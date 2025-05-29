#![feature(exit_status_error)]
#![feature(array_windows)]
#![feature(default_field_values)]
#![feature(string_into_chars)]

use std::{
    fs,
    io::{self},
    mem,
    path::{Path, PathBuf},
    process::Stdio,
};

use anki::{NoteId, add_cloze_note, update_cloze_note};
use log::{debug, error, trace};
use tokio::{io::AsyncWriteExt, process::Command};

mod anki;

const IGNORE_PATHS: [&str; 1] = ["./Excalidraw"];

#[tokio::main(flavor = "current_thread")]
async fn main() {
    env_logger::init();
    let client = reqwest::Client::new();
    traverse(PathBuf::from("."), &client).await.unwrap();
}

async fn traverse(dir: PathBuf, client: &reqwest::Client) -> io::Result<()> {
    trace!("Recursing into dir {}", dir.display());
    for entry in dir.read_dir()?.flatten() {
        let path = entry.path();
        // recurse
        if path.is_dir()
            && !IGNORE_PATHS
                .map(AsRef::<Path>::as_ref)
                .contains(&path.as_path())
        {
            Box::pin(traverse(path, client)).await?;
        // markdown file
        } else if path.is_file()
            && let Some(extension) = path.extension()
            && extension == "md"
        {
            handle_md(&path, client).await?;
        }
    }

    Ok(())
}

#[derive(PartialEq, Clone, Copy)]
enum Math {
    Inline,
    Display,
}

async fn handle_md(path: &Path, client: &reqwest::Client) -> io::Result<()> {
    debug!("Handling Markdown file {}", path.display());
    let mut file_contents = fs::read_to_string(path)?
        .into_chars()
        .collect::<Vec<char>>();
    let mut changed = false;

    let mut line = 0;
    let mut current_text = String::new();
    let mut math_text = String::new();
    let mut in_cloze = false;
    let mut num_cloze = 1;
    let mut math = None;

    let mut i = 0;
    // skip to the newline before the next cloze
    let skip_before_next_cloze = |file_contents: &[char], i: &mut usize, line: &mut usize| {
        if let Some(next_cloze_offset) = file_contents[*i..]
            .array_windows()
            .position(|chars| chars == &['='; 2])
        {
            let (newlines_skipped, (newline_before_offset, _)) = file_contents
                [*i..*i + next_cloze_offset]
                .iter()
                .enumerate()
                .filter(|(_, char)| **char == '\n')
                .enumerate()
                .last()
                .unwrap_or((0, (0, &'\0')));
            *i += newline_before_offset;
            *line += newlines_skipped;

            true
        // No more clozes in the file
        } else {
            false
        }
    };
    if !skip_before_next_cloze(&file_contents, &mut i, &mut line) {
        return Ok(());
    };
    // push the character to current/math text, based on math
    let push_char =
        |other: char, math: Option<Math>, math_text: &mut String, current_text: &mut String| {
            if math.is_some() {
                math_text
            } else {
                current_text
            }
            .push(other)
        };
    loop {
        let char_a = file_contents.get(i);
        let char_b = file_contents.get(i + 1);
        match [char_a, char_b] {
            [Some('\n'), _] | [None, _] => {
                line += 1;
                if in_cloze || math.is_some() {
                    current_text.push('\n');

                    // prevent infinite loop
                    if char_a.is_none() {
                        in_cloze = false;
                        math = None;
                    }
                } else {
                    num_cloze = 1;

                    if !current_text.is_empty() {
                        // handle note id
                        let format_note_id =
                            |id: u64| format!("\n<!--NoteID:{id}-->").into_chars().collect();
                        let mock_note_id: Vec<char> = format_note_id(1000000000000);

                        if let Some(potential_id) = file_contents.get(i..i + mock_note_id.len()) // index + 1 to skip newline
                            && potential_id[0..12] == mock_note_id[0..12]
                            && potential_id[25..] == mock_note_id[25..]
                        // update existing note
                        {
                            let note_id: u64 = potential_id[12..25]
                                .iter()
                                .collect::<String>()
                                .parse()
                                .unwrap();

                            let result = update_cloze_note(
                                mem::take(&mut current_text),
                                NoteId(note_id),
                                Vec::new(),
                                client,
                            )
                            .await;
                            if let Err(e) = result {
                                error!("{e}");
                            }

                            i += mock_note_id.len();
                        // add new note
                        } else {
                            match add_cloze_note(mem::take(&mut current_text), Vec::new(), client)
                                .await
                            {
                                Ok(note_id) => {
                                    let index = i.min(file_contents.len());
                                    file_contents.splice(index..index, format_note_id(note_id.0));

                                    changed = true;
                                    i += mock_note_id.len();
                                }
                                Err(e) => error!("{e}"),
                            }
                        }
                    }

                    if !skip_before_next_cloze(&file_contents, &mut i, &mut line) {
                        break;
                    }
                }
            }
            [Some('='), Some('=')] if math.is_none() => {
                if in_cloze {
                    current_text.push_str("}}");
                } else {
                    current_text.push_str(&format!("{{{{c{num_cloze}::")); // could be done without an allocation
                    num_cloze += 1;
                }

                // toggle in_cloze
                in_cloze = !in_cloze;

                // skip second '='
                i += 1;
            }
            [Some('$'), Some('$')] => match math {
                None => {
                    math = Some(Math::Display);
                    i += 1
                }
                Some(math_type) => {
                    math = None;
                    let converted = convert_math(&mem::take(&mut math_text), math_type).await?;
                    current_text.push_str(&converted);
                    if math_type == Math::Display {
                        i += 1
                    }
                }
            },
            [Some('$'), _] => match math {
                None => math = Some(Math::Inline),
                Some(Math::Inline) => {
                    math = None;
                    let converted = convert_math(&mem::take(&mut math_text), Math::Inline).await?;
                    current_text.push_str(&converted);
                }
                Some(Math::Display) => push_char('$', math, &mut math_text, &mut current_text),
            },
            [Some('['), Some('[')] | [Some(']'), Some(']')] if math.is_none() => i += 1,
            [Some(other), _] => push_char(*other, math, &mut math_text, &mut current_text),
        }
        i += 1;
    }
    if changed {
        fs::write(path, file_contents.into_iter().collect::<String>())
    } else {
        Ok(())
    }
}

/// Convert from Obsidian latex/typst to anki latex
async fn convert_math(str: &str, math_type: Math) -> io::Result<String> {
    let typst_style_math = match math_type {
        Math::Inline => format!("${str}$"),
        Math::Display => format!("$ {str} $"),
    };
    if is_typst(&typst_style_math).await? {
        typst_to_latex(&typst_style_math).await
    } else {
        Ok(match math_type {
            Math::Inline => format!("\\({str}\\)"),
            Math::Display => format!("\\[{str}\\]"),
        })
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
