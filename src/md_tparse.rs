use crate::anki::{NoteId, add_cloze_note, update_cloze_note};
use log::{debug, error};
use std::{cmp::Ordering, fmt::Write, fs, io, path::Path, process::Stdio};
use tokio::{io::AsyncWriteExt, process::Command};
use tparse::*;

// file
Or! {FileElement, ClozeLines = ClozeLines, Heading = Heading, Tag = Tag,
Code = Code, Math = Math, Link = Link, Char = char}
type File = AllConsumed<Vec<FileElement>>;

// newline
Or! {Newline, Cr = TStr<"\r">, Lf = TStr<"\n">, CrLF = TStr<"\r\n">}

// heading
type Heading = (
    VecN<1, TStr<"#">>,
    TStr<" ">,
    Vec<(IsNot<Newline>, char)>,
    Newline,
);

// tag
type Tag = (
    TStr<"#">,
    (IsNot<NotTagSpacing>, char),
    VecN<1, (IsNot<DisallowedInTag>, char)>,
    Newline,
);
Or! {NotTagSpacing, HashTag = TStr<"#">, Space = TStr<" ">, Newline = Newline}
Or! {DisallowedInTag, Space = TStr<" ">, Newline = Newline}

// Cloze
Or! {Element, Code = Code, Math = Math, Link = Link, Char = char}
type Cloze = (
    TStr<"==">,
    VecN<1, (IsNot<TStr<"==">>, Element)>,
    TStr<"==">,
);

Or! {ClozeOrNewline, Cloze = Cloze, Newline = Newline}
Or! {NotNewlineElementOrCloze, NotNewlineElement = (IsNot<Newline>, Element), Cloze = Cloze}
type ClozeLines = (
    Vec<(IsNot<ClozeOrNewline>, Element)>,
    Cloze,
    Vec<NotNewlineElementOrCloze>,
    Option<NoteIdComment>,
);

// note id comment
const NOTE_ID_COMMENT_START: &str = "<!--NoteID:";
const NOTE_ID_COMMENT_END: &str = "-->";
type NoteIdComment = (
    Newline,
    TStr<NOTE_ID_COMMENT_START>,
    VecN<10, RangedChar<'0', '9'>>,
    TStr<NOTE_ID_COMMENT_END>,
    Option<Newline>,
);

// code
Or! {Code, Inline = InlineCode, Multiline = MultilineCode}
// inline code
type InlineCode = (TStr<"`">, VecN<1, (IsNot<TStr<"`">>, char)>, TStr<"`">);
// display code
type MultilineCode = (
    TStr<"```">,
    VecN<1, (IsNot<TStr<"```">>, char)>,
    TStr<"```">,
);

// math
Or! {Math, Inline = InlineMath, Display = DisplayMath}
// inline math
type InlineMath = (TStr<"$">, VecN<1, (IsNot<TStr<"$">>, char)>, TStr<"$">);
// display math
type DisplayMath = (TStr<"$$">, VecN<1, (IsNot<TStr<"$$">>, char)>, TStr<"$$">);

// Link
Or! {DisallowedInLink, ClosingBrackets = TStr<"]]">, Newline = Newline, Pipe = TStr<"|">}
type Link = (
    TStr<"[[">,
    VecN<1, (IsNot<DisallowedInLink>, char)>,
    Option<LinkRename>,
    TStr<"]]">,
);
// LinkRename
Or! {DisallowedInLinkRename, ClosingBrackets = TStr<"]]">, Newline = Newline}
type LinkRename = VecN<1, (IsNot<DisallowedInLinkRename>, char)>;

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
        let inline_fn = |code: &(TStr<_>, VecN<1, (IsNot<TStr<_>>, char)>, TStr<_>)| {
            format!(
                "{}{}{}",
                code.0.str(),
                code.1.0.iter().map(|char| char.1).collect::<String>(),
                code.2.str()
            )
        };
        let multiline_fn = |code: &(TStr<_>, VecN<1, (IsNot<TStr<_>>, char)>, TStr<_>)| {
            format!(
                "{}{}{}",
                code.0.str(),
                code.1.0.iter().map(|char| char.1).collect::<String>(),
                code.2.str()
            )
        };

        match self {
            Code::Inline(inline) => inline_fn(inline),
            Code::Multiline(multiline) => multiline_fn(multiline),
        }
    }
}

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

    for (_, element) in cloze_lines.0 {
        match element {
            Element::Code(code) => string.push_str(&code.to_string()),
            Element::Math(math) => string.push_str(&math.convert().await.unwrap()), // TODO: handle errors
            Element::Link(link) => string.push_str(&link_to_string(link)),
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

fn link_to_string(link: Link) -> String {
    fn to_string<T: TParse>(vec: VecN<1, (IsNot<T>, char)>) -> String {
        vec.0.into_iter().map(|char| char.1).collect::<String>()
    }
    if let Some(rename) = link.2 {
        to_string(rename)
    } else {
        to_string(link.1)
    }
}

impl Math {
    /// Convert from Obsidian latex/typst to anki latex
    async fn convert(&self) -> io::Result<String> {
        // extract inner math
        fn extract<T>(math: &(T, VecN<1, (T, char)>, T)) -> String {
            math.1.0.iter().map(|char| char.1).collect()
        }
        let inner = match self {
            Self::Inline(inner) => extract(inner),
            Self::Display(inner) => extract(inner),
        };
        let typst_style_math = match self {
            Self::Inline(_) => format!("${inner}$"),
            Self::Display(_) => format!("$ {inner} $"),
        };

        if is_typst(&typst_style_math).await? {
            typst_to_latex(&typst_style_math).await
        } else {
            Ok(match self {
                Self::Inline(_) => {
                    format!("\\({inner}\\)")
                }
                Self::Display(_) => {
                    format!("\\[{inner}\\]")
                }
            })
        }
        .map(|string| string.replace("}}", "} }")) // avoid confusing anki
    }
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
