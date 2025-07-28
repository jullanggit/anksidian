use crate::anki::{LockNotesError, NOTES, add_cloze_note, update_cloze_note};
use log::error;
use std::{
    cmp::Ordering,
    fmt::{Display, Write},
    fs, io,
    path::{Path, PathBuf},
    process::{ExitStatusError, Stdio},
    string::FromUtf8Error,
};
use thiserror::Error;
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
Or! {Element, Code = Code, Math = Math, Link = Link, Char = char}
type Heading = (
    VecN<1, TStr<"#">>,
    TStr<" ">,
    Vec<(IsNot<Newline>, Element)>,
    Newline,
);

// tag
type Tag = (TStr<"#">, VecN<1, (IsNot<DisallowedInTag>, char)>);
Or! {DisallowedInTag, HashTag = TStr<"#">, Space = TStr<" ">, Newline = Newline}

// Cloze
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

#[derive(Debug, Error)]
pub enum HandleMdError {
    #[error("Reading/writing file ({file}) failed: {error}")]
    ReadWriteFile { file: PathBuf, error: io::Error },
    #[error("Failed to lock NOTES: {0}")]
    Lock(#[from] LockNotesError),
    #[error("Failed to convert math: {0}")]
    MathConvert(#[from] MathConvertError),
}
pub async fn handle_md(path: &Path) -> Result<(), HandleMdError> {
    /// the approximate length of a note id comment in bytes.
    /// Right for the years 2001-2286
    const APPROX_LEN_NOTE_ID_COMMENT: usize = "<!--NoteID:0000000000000-->\n".len();

    let str = fs::read_to_string(path).map_err(|error| HandleMdError::ReadWriteFile {
        file: path.to_path_buf(),
        error,
    })?;

    let parsed = File::tparse(&str)
        .expect("Parsing file can't fail, as it includes a Vec<char> option, that always matches");

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
                handle_cloze_lines(cloze_lines, &headings, &mut clozes, &path_str).await?
            }
            FileElement::Heading(heading) => handle_heading(heading, &mut headings).await?,
            FileElement::Tag(tag) => tags.push(
                tag.0
                    .str()
                    .chars()
                    .chain(tag.1.0.into_iter().map(|char| char.1))
                    .collect::<String>(),
            ),
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
            .lock()?
            .iter_mut()
            .find(|(note, _)| {
                note_id.is_some_and(|id| id == note.id.0) || note.fields["Text"] == contents
            })
            .map(|(note, seen)| {
                *seen = true;
                note.id
            });

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
    fs::write(path, out_string).map_err(|error| HandleMdError::ReadWriteFile {
        file: path.to_path_buf(),
        error,
    })
}

async fn handle_heading(
    heading: Heading,
    headings: &mut Vec<String>,
) -> Result<(), MathConvertError> {
    let level = heading.0.0.len();
    let mut contents = String::new();
    for (_, element) in heading.2 {
        contents.push_str(&element.into_string().await?);
    }

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
    Ok(())
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

impl Element {
    async fn into_string(self) -> Result<String, MathConvertError> {
        Ok(match self {
            Element::Code(code) => code.to_string(),
            Element::Math(math) => math.convert().await?,
            Element::Link(link) => link_to_string(link),
            Element::Char(char) => char.to_string(),
        })
    }
}

async fn handle_cloze_lines(
    cloze_lines: ClozeLines,
    headings: &[String],
    // (contents, id, remaining_length)
    clozes: &mut Vec<(String, Option<u64>, usize)>,
    path_str: &str,
) -> Result<(), MathConvertError> {
    let mut string = String::new();
    for (_, element) in cloze_lines.0 {
        string.push_str(&element.into_string().await?);
    }

    let mut cloze_num: u8 = 0;
    let mut note_id = None;

    async fn add_cloze(
        cloze: Cloze,
        string: &mut String,
        cloze_num: &mut u8,
    ) -> Result<(), MathConvertError> {
        *cloze_num += 1;

        write!(string, "{{{{c{cloze_num}::").expect("Writing to string shouldn't fail");
        for (_, element) in cloze.1.0 {
            string.push_str(&element.into_string().await?);
        }
        string.push_str("}}");
        Ok(())
    }
    add_cloze(cloze_lines.1, &mut string, &mut cloze_num).await?;

    for element_or_cloze in cloze_lines.2 {
        match element_or_cloze {
            NotNewlineClozeOrElement::NotNewlineElement((_, element)) => {
                string.push_str(&element.into_string().await?);
            }
            NotNewlineClozeOrElement::Cloze(cloze) => {
                add_cloze(cloze, &mut string, &mut cloze_num).await?
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
            write!(string, " > {heading}").expect("Writing to string shouldn't fail");
        }
    }

    let remaining_length = cloze_lines.4.0;

    clozes.push((string, note_id, remaining_length));
    Ok(())
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

#[derive(Error, Debug)]
pub enum MathConvertError {
    #[error("Checking if math is typst failed: {0}")]
    IsTypst(#[from] IsTypstError),
    #[error("Converting typst to latex failed: {0}")]
    TypstToLatex(#[from] TypstToLatexError),
}
impl Math {
    /// Convert from Obsidian latex/typst to anki latex
    async fn convert(&self) -> Result<String, MathConvertError> {
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

        Ok(if is_typst(&typst_style_math).await? {
            typst_to_latex(&typst_style_math).await?
        } else {
            match self {
                Self::Inline(_) => {
                    format!("\\({inner}\\)")
                }
                Self::Display(_) => {
                    format!("\\[{inner}\\]")
                }
            }
        }
        .replace("}", "} ")) // avoid confusing anki with }}
    }
}

#[derive(Error, Debug)]
pub enum IsTypstError {
    #[error("Failed to spawn typst process: {0}")]
    Spawn(tokio::io::Error),
    #[error("Failed to write to typst process stdin: {0}")]
    StdinWrite(tokio::io::Error),
    #[error("Failed to wait for typst process: {0}")]
    Wait(tokio::io::Error),
}
async fn is_typst(math: &str) -> Result<bool, IsTypstError> {
    // spawn typst compiler
    let mut child = Command::new("typst")
        .args(["c", "-", "-f", "pdf", "/dev/null"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(IsTypstError::Spawn)?;

    // write math to stdin
    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(math.as_bytes())
        .await
        .map_err(IsTypstError::StdinWrite)?;

    // success -> true
    Ok(child.wait().await.map_err(IsTypstError::Wait)?.success())
}

#[derive(Error, Debug)]
pub enum TypstToLatexError {
    #[error("Failed to spawn pandoc process: {0}")]
    Spawn(tokio::io::Error),
    #[error("Failed to write to pandoc process stdin: {0}")]
    StdinWrite(tokio::io::Error),
    #[error("Failed to wait for pandoc process: {0}")]
    Wait(tokio::io::Error),
    #[error("Pandoc failed: {0}")]
    ErrExit(#[from] ExitStatusError),
    #[error("Pandoc output not utf8: {0}")]
    Utf8(#[from] FromUtf8Error),
}
async fn typst_to_latex(typst: &str) -> Result<String, TypstToLatexError> {
    let mut child = Command::new("pandoc")
        .args(["-f", "typst", "-t", "latex"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(TypstToLatexError::Spawn)?;

    child
        .stdin
        .take()
        .expect("stdin is piped")
        .write_all(typst.as_bytes())
        .await
        .map_err(TypstToLatexError::StdinWrite)?;

    let mut stdout = child
        .wait_with_output()
        .await
        .map_err(TypstToLatexError::Wait)?
        .exit_ok()?
        .stdout;
    // remove trailing newline
    stdout.truncate(stdout.len() - 1);

    String::from_utf8(stdout).map_err(TypstToLatexError::Utf8)
}
