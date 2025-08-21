use crate::{
    CONFIG,
    anki::{LockNotesError, NOTES, NoteId, add_cloze_note, update_cloze_note},
};
use log::error;
use serde::Serialize;
use std::{
    cmp::Ordering,
    env::temp_dir,
    ffi::OsStr,
    fmt::Write as _,
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
type FileElement = Or<(ClozeLines, Heading, Tag, Code, Math, Link, char)>;
type File = AllConsumed<Vec<FileElement>>;

// newline
type Newline = Or<(TStr<"\r">, TStr<"\n">, TStr<"\r\n">)>;

// heading
type Element = Or<(Code, Math, Link, char)>;
type Heading = (
    VecN<1, TStr<"#">>,
    TStr<" ">,
    Vec<(IsNot<Newline>, Element)>,
    Newline,
);

// tag
type Tag = (
    TStr<"#">,
    VecN<1, (IsNot<Or<(TStr<"#">, TStr<" ">, Newline)>>, char)>,
);

// Cloze
type Cloze = (
    TStr<"==">,
    VecN<1, (IsNot<TStr<"==">>, Element)>,
    TStr<"==">,
);

type ClozeLines = (
    Vec<(IsNot<Or<(Cloze, Newline)>>, Element)>,
    Cloze,
    Vec<Or<(Cloze, (IsNot<Newline>, Element))>>,
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
type Code = Or<(InlineCode, MultilineCode)>;
// inline code
type InlineCode = (TStr<"`">, VecN<1, (IsNot<TStr<"`">>, char)>, TStr<"`">);
// display code
type MultilineCode = (
    TStr<"```">,
    VecN<1, (IsNot<TStr<"```">>, char)>,
    TStr<"```">,
);

// math
type Math = Or<(InlineMath, DisplayMath)>;
// inline math
type InlineMath = (TStr<"$">, VecN<1, (IsNot<TStr<"$">>, char)>, TStr<"$">);
// display math
type DisplayMath = (TStr<"$$">, VecN<1, (IsNot<TStr<"$$">>, char)>, TStr<"$$">);

// Link
type LinkRenameSeparator = TStr<"|">;
type Link = (
    Option<TStr<"!">>, // display
    TStr<"[[">,
    VecN<1, (IsNot<Or<(TStr<"]]">, Newline, LinkRenameSeparator)>>, char)>,
    Option<LinkRename>,
    TStr<"]]">,
);
// LinkRename
type LinkRename = (
    LinkRenameSeparator,
    VecN<1, (IsNot<Or<(TStr<"]]">, Newline)>>, char)>,
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
    #[error("No matching anki deck found for path {0}")]
    DeckLookup(PathBuf),
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

    let mut tags: Vec<String> = Vec::new();
    let mut headings: Vec<String> = Vec::new();
    let mut clozes: Vec<ClozeData> = Vec::new();

    for file_element in parsed.0.0 {
        let matcher: Matcher<_, _, _, _> = file_element.matcher::<_, Result<(), HandleMdError>>((
            &mut headings,
            &mut clozes,
            &path_str,
            &mut tags,
        ));
        let matcher = AddMatcher::<0>::add_matcher(
            matcher,
            |cloze_lines, (headings, clozes, path_str, _)| {
                Ok(handle_cloze_lines(
                    *cloze_lines,
                    headings,
                    clozes,
                    path_str,
                )?)
            },
        );
        let matcher = AddMatcher::<1>::add_matcher(matcher, |heading, (headings, _, _, _)| {
            Ok(handle_heading(*heading, headings, &mut Vec::new())?)
        });
        let matcher = AddMatcher::<2>::add_matcher(matcher, |tag, (_, _, _, tags)| {
            #[expect(clippy::unit_arg)]
            Ok(tags.push(
                tag.0
                    .str()
                    .chars()
                    .chain(tag.1.0.into_iter().map(|char| char.1))
                    .collect::<String>(),
            ))
        });
        let matcher = AddMatcher::<3>::add_matcher(matcher, |_, _| Ok(()));
        let matcher = AddMatcher::<4>::add_matcher(matcher, |_, _| Ok(()));
        let matcher = AddMatcher::<5>::add_matcher(matcher, |_, _| Ok(()));
        let matcher = AddMatcher::<6>::add_matcher(matcher, |_, _| Ok(()));
        matcher.do_match()?;
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
            None => {
                let deck = &CONFIG
                    .path_to_deck
                    .iter()
                    .find(|mapping| mapping.path.is_match(&path.to_string_lossy()))
                    .ok_or_else(|| HandleMdError::DeckLookup(path.to_path_buf()))?
                    .deck;
                match add_cloze_note(cloze, tags.iter().map(ToString::to_string).collect(), deck) {
                    Ok(note_id) => Some(note_id),
                    Err(e) => {
                        error!("{e}");
                        None
                    }
                }
            }
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
        contents.push_str(&element_to_string(element, pictures)?);
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

fn code_to_string(code: Code) -> String {
    let matcher = code.matcher::<(), String>(());
    let matcher = AddMatcher::<0>::add_matcher(matcher, |code, _| {
        format!(
            "{}{}{}",
            code.0.str(),
            code.1.0.iter().map(|char| char.1).collect::<String>(),
            code.2.str()
        )
    });
    let matcher = matcher.add_matcher(|code, _| {
        format!(
            "{}{}{}",
            code.0.str(),
            code.1.0.iter().map(|char| char.1).collect::<String>(),
            code.2.str()
        )
    });
    matcher.do_match()
}

fn element_to_string(
    element: Element,
    pictures: &mut Vec<Picture>,
) -> Result<String, MathConvertError> {
    let matcher = element.matcher(pictures);
    let matcher = AddMatcher::<0>::add_matcher(matcher, |code, _| Ok(code_to_string(*code)));
    let matcher = AddMatcher::<1>::add_matcher(matcher, |math, _| convert_math(*math));
    let matcher = AddMatcher::<2>::add_matcher(matcher, |link, pictures| {
        Ok(link_to_string(*link, pictures))
    });
    let matcher = matcher.add_matcher(|char, _| Ok(char.to_string()));
    matcher.do_match()
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
        string.push_str(&element_to_string(element, &mut pictures)?);
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
            string.push_str(&element_to_string(element, pictures)?);
        }
        string.push_str("}}");
        Ok(())
    }
    add_cloze(cloze_lines.1, &mut string, &mut cloze_num, &mut pictures)?;

    for element_or_cloze in cloze_lines.2 {
        let matcher = element_or_cloze.matcher((&mut string, &mut pictures, &mut cloze_num));
        let matcher =
            AddMatcher::<0>::add_matcher(matcher, |cloze, (string, pictures, cloze_num)| {
                add_cloze(*cloze, string, cloze_num, pictures)
            });
        let matcher = matcher.add_matcher(|element, (string, pictures, _)| {
            #[expect(clippy::unit_arg)]
            Ok(string.push_str(&element_to_string(element.1, pictures)?))
        });
        matcher.do_match()?;
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
/// Convert from Obsidian latex/typst to anki latex
fn convert_math(math: Math) -> Result<String, MathConvertError> {
    // extract inner math
    fn extract<T, U, V>(math: &(T, VecN<1, (U, char)>, V)) -> String {
        math.1.0.iter().map(|char| char.1).collect()
    }
    let matcher = math.matcher(());
    let matcher = AddMatcher::<0>::add_matcher(matcher, |inner, _| {
        let inner = extract(&inner);
        (format!("${inner}$"), format!("\\({inner}\\)"))
    });
    let matcher = matcher.add_matcher(|inner, _| {
        let inner = extract(&inner);
        (format!("$ {inner} $"), format!("\\[{inner}\\]"))
    });
    let (typst_style_math, latex_style_math) = matcher.do_match();

    Ok(if is_typst(&typst_style_math)? {
        typst_to_latex(&typst_style_math)?
    } else {
        latex_style_math
    }
    .replace("}", "} ")) // avoid confusing anki with }}
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
