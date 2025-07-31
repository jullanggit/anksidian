use crate::anki::{LockNotesError, NOTES, NoteId, add_cloze_note, update_cloze_note};
use log::error;
use serde::Serialize;
use std::{
    cmp::Ordering,
    env::temp_dir,
    ffi::OsStr,
    fmt::{Display, Write as _},
    fs::{self, create_dir_all},
    io::{self, Write as _},
    path::{Path, PathBuf},
    process::{Command, ExitStatusError, Stdio},
    string::FromUtf8Error,
};
use thiserror::Error;

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
    Option<TStr<"!">>, // display
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

pub struct ClozeData {
    pub contents: String,
    pub note_id: Option<NoteId>,
    pub pictures: Vec<Picture>,
    remaining_length: usize,
}

#[derive(Debug, Error)]
pub enum HandleMdError {
    #[error("Reading/writing file ({file}) failed: {error}")]
    ReadWriteFile { file: PathBuf, error: io::Error },
    #[error("Failed to lock NOTES: {0}")]
    Lock(#[from] LockNotesError),
    #[error("Failed to convert math: {0}")]
    MathConvert(#[from] MathConvertError),
}
pub fn handle_md(path: &Path) -> Result<(), HandleMdError> {
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
                handle_cloze_lines(cloze_lines, &headings, &mut clozes, &path_str)?
            }
            FileElement::Heading(heading) => {
                handle_heading(heading, &mut headings, &mut Vec::new())?
            }
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
    for cloze in clozes {
        let actual_note_id = NOTES
            .lock()?
            .iter_mut()
            .find(|(note, _)| {
                cloze.note_id.is_some_and(|id| id == note.id)
                    || note.fields["Text"] == cloze.contents
            })
            .map(|(note, seen)| {
                *seen = true;
                note.id
            });

        let note_id = cloze.note_id;
        let index = str.len() - cloze.remaining_length;

        let final_id = match actual_note_id {
            // update existing note
            Some(note_id) => {
                let result =
                    update_cloze_note(cloze, tags.iter().map(ToString::to_string).collect());
                if let Err(e) = result {
                    error!("{e}");
                    None
                } else {
                    Some(note_id)
                }
            }
            // add new note
            None => match add_cloze_note(cloze, tags.iter().map(ToString::to_string).collect()) {
                Ok(note_id) => Some(note_id),
                Err(e) => {
                    error!("{e}");
                    None
                }
            },
        };

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
                let previous_id_string = previous_id.0.to_string();
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

fn handle_heading(
    heading: Heading,
    headings: &mut Vec<String>,
    pictures: &mut Vec<Picture>,
) -> Result<(), MathConvertError> {
    let level = heading.0.0.len();
    let mut contents = String::new();
    for (_, element) in heading.2 {
        contents.push_str(&element.into_string(pictures)?);
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
    fn into_string(self, pictures: &mut Vec<Picture>) -> Result<String, MathConvertError> {
        Ok(match self {
            Element::Code(code) => code.to_string(),
            Element::Math(math) => math.convert()?,
            Element::Link(link) => link_to_string(link, pictures),
            Element::Char(char) => char.to_string(),
        })
    }
}

fn handle_cloze_lines(
    cloze_lines: ClozeLines,
    headings: &[String],
    clozes: &mut Vec<ClozeData>,
    path_str: &str,
) -> Result<(), MathConvertError> {
    let mut string = String::new();
    let mut pictures = Vec::new();
    for (_, element) in cloze_lines.0 {
        string.push_str(&element.into_string(&mut pictures)?);
    }

    let mut cloze_num: u8 = 0;
    let mut note_id = None;

    fn add_cloze(
        cloze: Cloze,
        string: &mut String,
        cloze_num: &mut u8,
        pictures: &mut Vec<Picture>,
    ) -> Result<(), MathConvertError> {
        *cloze_num += 1;

        write!(string, "{{{{c{cloze_num}::").expect("Writing to string shouldn't fail");
        for (_, element) in cloze.1.0 {
            string.push_str(&element.into_string(pictures)?);
        }
        string.push_str("}}");
        Ok(())
    }
    add_cloze(cloze_lines.1, &mut string, &mut cloze_num, &mut pictures)?;

    for element_or_cloze in cloze_lines.2 {
        match element_or_cloze {
            NotNewlineClozeOrElement::NotNewlineElement((_, element)) => {
                string.push_str(&element.into_string(&mut pictures)?);
            }
            NotNewlineClozeOrElement::Cloze(cloze) => {
                add_cloze(cloze, &mut string, &mut cloze_num, &mut pictures)?
            }
        }
    }
    if let Some(note_id_comment) = cloze_lines.3 {
        note_id = Some(NoteId(note_id_comment.2.0.into_iter().fold(
            0u64,
            |acc, digit| {
                acc * 10
                    + digit
                        .0
                        .to_digit(10)
                        .expect("We use RangedChar 0..=9, so there are only valid digits")
                        as u64
            },
        )));
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

    clozes.push(ClozeData {
        contents: string,
        note_id,
        remaining_length,
        pictures,
    });
    Ok(())
}

#[derive(Clone, Debug, Serialize)]
pub struct Picture {
    pub path: PathBuf,
    pub filename: String,
    fields: String,
}
impl Picture {
    pub fn new(path: PathBuf, filename: String) -> Self {
        Self {
            path,
            filename,
            fields: String::from("Back Extra"), // TODO: maybe support both front and back
        }
    }
}
fn link_to_string(link: Link, pictures: &mut Vec<Picture>) -> String {
    fn to_string<T: TParse>(vec: VecN<1, (IsNot<T>, char)>) -> String {
        vec.0.into_iter().map(|char| char.1).collect::<String>()
    }
    let contents = if let Some(rename) = link.3 {
        to_string(rename.1)
    } else {
        to_string(link.2)
    };
    // handle images only if they are displayed
    if link.0.is_some() && maybe_handle_image(Path::new(&contents), pictures).is_some() {
        // dont display anything on the front, back will be handled by the anki module
        String::new()
    } else {
        contents
    }
}

/// Check if path is an image and if so handle it. Returns the string to be embedded into the cloze
// Returns Option<()> to enable ?
fn maybe_handle_image(path: &Path, pictures: &mut Vec<Picture>) -> Option<()> {
    const IMAGE_EXTENSIONS: [&str; 13] = [
        "jpg", "jpeg", "jxl", "png", "gif", "bmp", "svg", "webp", "apng", "ico", "tif", "tiff",
        "avif",
    ];
    for extension in IMAGE_EXTENSIONS {
        if path.extension() == Some(OsStr::new(extension)) && path.exists() {
            // convert jxl to jpeg
            let (path, filename) = if extension == "jxl" {
                let mut out_path = temp_dir().join(path);
                out_path.set_extension("jpg");

                if let Some(parent) = out_path.parent() {
                    let _ = create_dir_all(parent);
                }

                Command::new("djxl")
                    .arg(path)
                    .arg(&out_path)
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .spawn()
                    .ok()?
                    .wait()
                    .ok()?
                    .exit_ok()
                    .ok()?;

                let mut filename = path.to_path_buf();
                filename.set_extension("jpg");

                (
                    out_path.canonicalize().ok()?,
                    filename.to_str()?.to_string(),
                )
            } else {
                (path.canonicalize().ok()?, path.to_str()?.to_string())
            };
            pictures.push(Picture::new(path, filename));
            return Some(());
        }
    }
    None
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
    fn convert(&self) -> Result<String, MathConvertError> {
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

        Ok(if is_typst(&typst_style_math)? {
            typst_to_latex(&typst_style_math)?
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
    Spawn(std::io::Error),
    #[error("Failed to write to typst process stdin: {0}")]
    StdinWrite(std::io::Error),
    #[error("Failed to wait for typst process: {0}")]
    Wait(std::io::Error),
}
fn is_typst(math: &str) -> Result<bool, IsTypstError> {
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
        .map_err(IsTypstError::StdinWrite)?;

    // success -> true
    Ok(child.wait().map_err(IsTypstError::Wait)?.success())
}

#[derive(Error, Debug)]
pub enum TypstToLatexError {
    #[error("Failed to spawn pandoc process: {0}")]
    Spawn(std::io::Error),
    #[error("Failed to write to pandoc process stdin: {0}")]
    StdinWrite(std::io::Error),
    #[error("Failed to wait for pandoc process: {0}")]
    Wait(std::io::Error),
    #[error("Pandoc failed: {0}")]
    ErrExit(#[from] ExitStatusError),
    #[error("Pandoc output not utf8: {0}")]
    Utf8(#[from] FromUtf8Error),
}
fn typst_to_latex(typst: &str) -> Result<String, TypstToLatexError> {
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
        .map_err(TypstToLatexError::StdinWrite)?;

    let mut stdout = child
        .wait_with_output()
        .map_err(TypstToLatexError::Wait)?
        .exit_ok()?
        .stdout;
    // remove trailing newline
    stdout.truncate(stdout.len() - 1);

    String::from_utf8(stdout).map_err(TypstToLatexError::Utf8)
}
