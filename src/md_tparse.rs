use crate::anki::{NoteId, add_cloze_note, update_cloze_note};
use log::{debug, error};
use std::{cmp::Ordering, fmt::Write, fs, path::Path};
use tparse::*;

// file
Or! {FileElement, ClozeLines = ClozeLines, Heading = Heading, Tag = Tag,
Code = Code, Math = Math, Link = Link, Char = char}
type File = AllConsumed<Vec<FileElement>>;

// newline
Or! {Newline, Cr = TStr<"\r">, Lf = TStr<"\n">, CrLF = TStr<"\r\n">}

// heading
Concat! {NotNewline, IsNot<Newline>, char}
Concat! {Heading, VecN<1, TStr<"#">>, TStr<" ">, Vec<NotNewline>, Newline}

// tag
Concat! {Tag, TStr<"#">, TagSpacing, VecN<1, TagNameChar>, Newline}
// tag spacing
Or! {NotTagSpacing, HashTag = TStr<"#">, Space = TStr<" ">, Newline = Newline}
Concat! {TagSpacing, IsNot<NotTagSpacing>, char}
// tag name
Or! {NotTagContents, Space = TStr<" ">, Newline = Newline}
Concat! {TagNameChar, IsNot<NotTagContents>, char}

// Cloze
Or! {Element, Code = Code, Math = Math, Link = Link, Char = char}
Concat! {NotClozeEnd, IsNot<TStr<"==">>, Element}
Concat! {Cloze, TStr<"==">, VecN<1, NotClozeEnd>,TStr<"==">}

Or! {ClozeOrNewline, Cloze = Cloze, Newline = Newline}
Concat! {NotClozeOrNewline, IsNot<ClozeOrNewline>, Element}
Concat! {NotNewlineElement, IsNot<Newline>, Element}
Or! {NotNewlineElementOrCloze, NotNewlineElement = NotNewlineElement, Cloze = Cloze}
Concat! {ClozeLines, Vec<NotClozeOrNewline>, Cloze, Vec<NotNewlineElementOrCloze>, Option<NoteIdComment>}

// note id comment
const NOTE_ID_COMMENT_START: &str = "<!--NoteID:";
const NOTE_ID_COMMENT_END: &str = "-->";
Concat! {NoteIdComment, Newline, TStr<NOTE_ID_COMMENT_START>, VecN<10, RangedChar<'0', '9'>>, TStr<NOTE_ID_COMMENT_END>, Option<Newline>}

// code
Or! {Code, Inline = InlineCode, Multiline = MultilineCode}
// inline code
Concat! {NotInlineCodeEnd, IsNot<TStr<"`">>, char}
Concat! {InlineCode, TStr<"`">, VecN<1, NotInlineCodeEnd>, TStr<"`">}
// display code
Concat! {NotMultilineCodeEnd, IsNot<TStr<"```">>, char}
Concat! {MultilineCode, TStr<"```">, VecN<1, NotMultilineCodeEnd>, TStr<"```">}

// math
Or! {Math, Inline = InlineMath, Display = DisplayMath}
// inline math
Concat! {NotInlineMathEnd, IsNot<TStr<"$">>, char}
Concat! {InlineMath, TStr<"$">, VecN<1, NotInlineMathEnd>, TStr<"$">}
// display math
Concat! {NotDisplayMathEnd, IsNot<TStr<"$$">>, char}
Concat! {DisplayMath, TStr<"$$">, VecN<1, NotDisplayMathEnd>, TStr<"$$">}

// Link
Or! {DisallowedInLink, ClosingBrackets = TStr<"]]">, Newline = Newline, Pipe = TStr<"|">}
Concat! {AllowedInLink, IsNot<DisallowedInLink>, char}
Concat! {Link, TStr<"[[">, VecN<1, AllowedInLink>, Option<LinkRename>, TStr<"]]">}
// LinkRename
Or! {DisallowedInLinkRename, ClosingBrackets = TStr<"]]">, Newline = Newline}
Concat! {AllowedInLinkRename, IsNot<DisallowedInLinkRename>, char}
Concat! {LinkRename, VecN<1, AllowedInLinkRename>}

pub async fn handle_md(path: &Path, client: &reqwest::Client, deck: &str) {
    /// the approximate length of a note id comment in bytes.
    /// Right for the years 2001-2286
    const APPROX_LEN_NOTE_ID_COMMENT: usize = "<!--NoteID:0000000000000-->\n".len();

    let str = fs::read_to_string(path).expect("Reading file shouldnt fail");

    let parsed = File::tparse(&str).expect("Parsing file shouldn't fail");

    let mut path_str = path
        .iter()
        .skip(1)
        .map(|part| part.to_string_lossy().to_string())
        .intersperse(" > ".to_owned())
        .collect::<String>();
    path_str.truncate(path_str.len() - 3); // remove .md

    let mut tags = Vec::new();
    let mut headings = Vec::new();
    let mut clozes = Vec::new();

    for file_element in parsed.0.0 {
        match file_element {
            FileElement::ClozeLines(cloze_lines) => {
                handle_cloze_lines(cloze_lines, &headings, &mut clozes, &path_str).await
            }
            FileElement::Heading(heading) => handle_heading(heading, &mut headings),
            FileElement::Tag(tag) => {
                tags.push(tag.2.0.into_iter().map(|char| char.1).collect::<String>())
            }
            FileElement::Code(_)
            | FileElement::Math(_)
            | FileElement::Link(_)
            | FileElement::Char(_) => {}
        }
    }

    let mut last_read = 0;
    let mut out_string =
        String::with_capacity(str.len() + clozes.len() * APPROX_LEN_NOTE_ID_COMMENT);
    for (contents, note_id, end) in clozes {
        // update existing note
        if let Some(note_id) = note_id {
            let result = update_cloze_note(
                contents,
                NoteId(note_id),
                tags.iter().map(ToString::to_string).collect(),
                client,
            )
            .await;
            if let Err(e) = result {
                error!("{e}");
            }
        // add new note
        } else {
            match add_cloze_note(
                contents,
                tags.iter().map(ToString::to_string).collect(),
                deck.to_string(),
                client,
            )
            .await
            {
                Ok(note_id) => {
                    // insert note id comments by copying the old file and interleaving the comments
                    let index = str.len().min(end + 1);
                    out_string.push_str(&str[last_read..index]);
                    writeln!(out_string, "<!--NoteID:{}-->", note_id.0)
                        .expect("Writing to out_string shouldn't fail");

                    last_read = index;
                }
                Err(e) => error!("{e}"),
            }
        }
    }
    out_string.push_str(&str[last_read..]);
    fs::write(path, out_string).expect("Writing to file shouldn't fail");
}

fn handle_heading(heading: Heading, headings: &mut Vec<String>) {
    let level = heading.0.0.len();
    let contents = heading.2.into_iter().map(|char| char.1).collect::<String>();

    match level.cmp(&headings.len()) {
        Ordering::Less => {
            headings.pop();
            headings.truncate(level);
            headings[level - 1] = contents
        }
        Ordering::Equal => headings[level - 1] = contents,
        Ordering::Greater => {
            // empty headings will be filtered out when writing path
            for _ in 0..level - headings.len() {
                headings.push(Default::default());
            }
            headings.push(contents);
        }
    }
}

impl ToString for Code {
    fn to_string(&self) -> String {
        match &self {
            Code::Inline(inline_code) => inline_code.to_string(),
            Code::Multiline(multiline_code) => multiline_code.to_string(),
        }
    }
}
macro_rules! impl_to_string_for_code {
    ($($ty:ident),+) => {
        $(
            impl ToString for $ty {
                fn to_string(&self) -> String {
                    format!(
                        "{}{}{}",
                        self.0.str(),
                        self.1.0.iter().map(|char| char.1).collect::<String>(),
                        self.2.str()
                    )
                }
            }
        )+
    };
}
impl_to_string_for_code!(InlineCode, MultilineCode);

async fn handle_cloze_lines<'i>(
    cloze_lines: ClozeLines,
    headings: &[String],
    // (contents, id, end)
    clozes: &mut Vec<(String, Option<u64>, usize)>,
    path_str: &str,
) {
    let mut string = String::new();
    let mut cloze_num: u8 = 0;
    let mut note_id = None;

    for NotClozeOrNewline(_, element) in cloze_lines.0 {
        match element {
            Element::Code(code) => string.push_str(&code.to_string()),
            Element::Math(math) => string.push_str(&math.convert().await.unwrap()), // TODO: handle errors
            Element::Link(link) => string.push_str(&link.to_string()),
            Element::Char(char) => string.push(char),
        }
    }
    for pair in pair.into_inner() {
        match pair.as_rule() {
            Rule::cloze => {
                cloze_num += 1;

                write!(string, "{{{{c{cloze_num}::").unwrap();
                for inner_pair in pair.into_inner() {
                    match inner_pair.as_rule() {
                        Rule::math => string.push_str(&convert_math(inner_pair).await.unwrap()), // TODO: handle errors
                        Rule::link => string.push_str(handle_link(inner_pair)),
                        Rule::character => string.push_str(inner_pair.as_str()),
                        other => unreachable!("{other:?}"),
                    }
                }
                string.push_str("}}");
            }
            Rule::not_cloze_or_newline => {
                let inner_pair = pair
                    .into_inner()
                    .next()
                    .expect("not_cloze_or_newline should always have children");
                match inner_pair.as_rule() {
                    Rule::character | Rule::code => string.push_str(inner_pair.as_str()),
                    Rule::math => string.push_str(&convert_math(inner_pair).await.unwrap()), // TODO: handle errors
                    Rule::link => string.push_str(handle_link(inner_pair)),
                    other => unreachable!("{other:?}"),
                }
            }
            Rule::note_id_comment => {
                note_id = Some(
                    pair.into_inner()
                        .next()
                        .expect("note_id_comments always has an ascii_digits child")
                        .as_str()
                        .parse()
                        .expect("parsing note id shouldn't fail"),
                )
            }
            other => unreachable!("{other:?}"),
        }
    }

    // append path & headings
    string.push_str("<br>");
    string.push_str(path_str);
    for heading in headings {
        if !heading.is_empty() {
            write!(string, " > {heading}").unwrap();
        }
    }

    clozes.push((string, note_id, end));
}

fn handle_link<'i>(pair: Pair<'i, Rule>) -> &'i str {
    assert!(pair.as_rule() == Rule::link);

    let str = pair.as_str();

    if let Some(rename) = pair.into_inner().next() {
        &rename.as_str()[1..]
    } else {
        &str[2..str.len() - 2]
    }
}

/// Convert from Obsidian latex/typst to anki latex
async fn convert_math<'i>(pair: Pair<'i, Rule>) -> io::Result<String> {
    assert!(pair.as_rule() == Rule::math);

    let str = pair.as_str();

    let inline = pair
        .into_inner()
        .next()
        .expect("Math always has either a display or an inline child")
        .as_rule()
        == Rule::inline_math;

    // extract inner math
    let offset = if inline { 1 } else { 2 };
    let str = &str[offset..str.len() - offset];

    let typst_style_math = if inline {
        format!("${str}$")
    } else {
        format!("$ {str} $")
    };
    if is_typst(&typst_style_math).await? {
        typst_to_latex(&typst_style_math).await
    } else {
        Ok(if inline {
            format!("\\({str}\\)")
        } else {
            format!("\\[{str}\\]")
        })
    }
    .map(|string| string.replace("}}", "} }")) // avoid confusing anki
}

async fn is_typst(math: &str) -> io::Result<bool> {
    // spawn typst compiler
    let mut child = Command::new("typst")
        .args(["c", "-", "-f", "pdf", "/dev/null"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;

    // write math to stdin
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(math.as_bytes())
        .await?;

    // success -> true
    Ok(child.wait().await?.code() == Some(0))
}

async fn typst_to_latex(typst: &str) -> io::Result<String> {
    let mut child = Command::new("pandoc")
        .args(["-f", "typst", "-t", "latex"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(typst.as_bytes())
        .await?;

    let mut stdout = child
        .wait_with_output()
        .await?
        .exit_ok()
        .map_err(io::Error::other)?
        .stdout;
    // remove trailing newline
    stdout.truncate(stdout.len() - 1);

    String::from_utf8(stdout).map_err(io::Error::other)
}
