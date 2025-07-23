use crate::anki::{NOTES, add_cloze_note, update_cloze_note};
use log::error;
use std::{
    cmp::Ordering,
    fmt::{Display, Write},
    fs, io,
    path::Path,
    process::Stdio,
};
use tokio::{io::AsyncWriteExt, process::Command};
use tparse::*;

// grammar

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
type Tag = (TStr<"#">, VecN<1, (IsNot<DisallowedInTag>, char)>, Newline);
Or! {DisallowedInTag, HashTag = TStr<"#">, Space = TStr<" ">, Newline = Newline}

// Cloze
Or! {Element, Code = Code, Math = Math, Link = Link, Char = char}
type Cloze = (
    TStr<"==">,
    VecN<1, (IsNot<TStr<"==">>, Element)>,
    TStr<"==">,
);

Or! {ClozeOrNewline, Cloze = Cloze, Newline = Newline}
Or! {NotNewlineClozeOrElement, Cloze = Cloze, NotNewlineElement = (IsNot<Newline>, Element)}
type ClozeLines = (
    Vec<(IsNot<ClozeOrNewline>, Element)>,
    Cloze,
    Vec<NotNewlineClozeOrElement>,
    Option<NoteIdComment>,
    RemainingLength,
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
type LinkRenameSeparator = TStr<"|">;
Or! {DisallowedInLink, ClosingBrackets = TStr<"]]">, Newline = Newline, LinkRenameSeparator = LinkRenameSeparator}
type Link = (
    TStr<"[[">,
    VecN<1, (IsNot<DisallowedInLink>, char)>,
    Option<LinkRename>,
    TStr<"]]">,
);
// LinkRename
Or! {DisallowedInLinkRename, ClosingBrackets = TStr<"]]">, Newline = Newline}
type LinkRename = (
    LinkRenameSeparator,
    VecN<1, (IsNot<DisallowedInLinkRename>, char)>,
);

pub async fn handle_md(path: &Path) {
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
                tags.push(tag.1.0.into_iter().map(|char| char.1).collect::<String>())
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
    for (contents, note_id, remaining_length) in clozes {
        let actual_note_id = NOTES
            .iter()
            .find(|note| {
                note_id.is_some_and(|id| id == note.id.0) || note.fields["Text"] == contents
            })
            .map(|note| note.id);

        let final_id = match actual_note_id {
            // update existing note
            Some(note_id) => {
                let result = update_cloze_note(
                    contents,
                    note_id,
                    tags.iter().map(ToString::to_string).collect(),
                )
                .await;
                if let Err(e) = result {
                    error!("{e}");
                    None
                } else {
                    Some(note_id)
                }
            }
            // add new note
            None => {
                match add_cloze_note(contents, tags.iter().map(ToString::to_string).collect()).await
                {
                    Ok(note_id) => Some(note_id),
                    Err(e) => {
                        error!("{e}");
                        None
                    }
                }
            }
        };
        let index = str.len() - remaining_length;
        out_string.push_str(&str[last_read..index]);
        last_read = index;
        match (note_id, final_id) {
            // dont change anything
            (_, None) => {}
            // write new id
            (None, Some(id_to_write)) => {
                write!(
                    out_string,
                    "\n{}{}{}",
                    NOTE_ID_COMMENT_START, id_to_write.0, NOTE_ID_COMMENT_END
                )
                .expect("Writing to out_string shouldn't fail");
            }
            // replace old id
            (Some(previous_id), Some(new_id)) => {
                let previous_id_string = previous_id.to_string();
                let start_previous_id = out_string
                    .rfind(&previous_id_string)
                    .expect("Previous ID should be present");
                out_string.replace_range(
                    start_previous_id..start_previous_id + previous_id_string.len(),
                    &new_id.0.to_string(),
                );
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

impl Display for Code {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Code::Inline(code) => {
                write!(
                    f,
                    "{}{}{}",
                    code.0.str(),
                    code.1.0.iter().map(|char| char.1).collect::<String>(),
                    code.2.str()
                )
            }
            Code::Multiline(code) => {
                write!(
                    f,
                    "{}{}{}",
                    code.0.str(),
                    code.1.0.iter().map(|char| char.1).collect::<String>(),
                    code.2.str()
                )
            }
        }
    }
}

async fn handle_cloze_lines(
    cloze_lines: ClozeLines,
    headings: &[String],
    // (contents, id, remaining_length)
    clozes: &mut Vec<(String, Option<u64>, usize)>,
    path_str: &str,
) {
    async fn handle_element(element: Element, string: &mut String) {
        match element {
            Element::Code(code) => string.push_str(&code.to_string()),
            Element::Math(math) => string.push_str(&math.convert().await.unwrap()), // TODO: handle errors
            Element::Link(link) => string.push_str(&link_to_string(link)),
            Element::Char(char) => string.push(char),
        }
    }

    let mut string = String::new();
    for (_, element) in cloze_lines.0 {
        handle_element(element, &mut string).await
    }

    let mut cloze_num: u8 = 0;
    let mut note_id = None;

    async fn add_cloze(cloze: Cloze, string: &mut String, cloze_num: &mut u8) {
        *cloze_num += 1;

        write!(string, "{{{{c{cloze_num}::").unwrap();
        for (_, element) in cloze.1.0 {
            handle_element(element, string).await
        }
        string.push_str("}}");
    }
    add_cloze(cloze_lines.1, &mut string, &mut cloze_num).await;

    for element_or_cloze in cloze_lines.2 {
        match element_or_cloze {
            NotNewlineClozeOrElement::NotNewlineElement((_, element)) => {
                handle_element(element, &mut string).await
            }
            NotNewlineClozeOrElement::Cloze(cloze) => {
                add_cloze(cloze, &mut string, &mut cloze_num).await
            }
        }
    }
    if let Some(note_id_comment) = cloze_lines.3 {
        note_id = Some(note_id_comment.2.0.into_iter().fold(0u64, |acc, digit| {
            acc * 10
                + digit
                    .0
                    .to_digit(10)
                    .expect("We use RangedChar 0..=9, so there are only valid digits")
                    as u64
        }));
    }

    // append path & headings
    string.push_str("<br>");
    string.push_str(path_str);
    for heading in headings {
        if !heading.is_empty() {
            write!(string, " > {heading}").unwrap();
        }
    }

    let remaining_length = cloze_lines.4.0;

    clozes.push((string, note_id, remaining_length));
}

fn link_to_string(link: Link) -> String {
    fn to_string<T: TParse>(vec: VecN<1, (IsNot<T>, char)>) -> String {
        vec.0.into_iter().map(|char| char.1).collect::<String>()
    }
    if let Some(rename) = link.2 {
        to_string(rename.1)
    } else {
        to_string(link.1)
    }
}

impl Math {
    /// Convert from Obsidian latex/typst to anki latex
    async fn convert(&self) -> io::Result<String> {
        // extract inner math
        fn extract<T, U, V>(math: &(T, VecN<1, (U, char)>, V)) -> String {
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
