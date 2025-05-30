#![feature(exit_status_error)]
#![feature(array_windows)]
#![feature(string_into_chars)]

use std::{
    array,
    fmt::Write,
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

#[tokio::main]
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

    // clozes
    let mut contains_cloze = false;
    let mut in_cloze = false;
    let mut num_cloze = 1;
    // text
    let mut current_text = String::new();
    // math
    let mut math_text = String::new();
    let mut math = None;
    // code
    let mut in_code = false;
    // headings
    let mut possible_heading: u8 = 1;
    let mut capturing_heading = false;
    let mut heading_level = 0;
    let mut headings: Vec<String> = Vec::new();
    let mut new_heading = false;

    // push the character to current/math text, based on math
    let mut i = 0;
    loop {
        let chars = array::from_fn(|offset| file_contents.get(i + offset).cloned());
        match chars {
            [Some('\n'), _, _] | [None, _, _] => {
                if in_cloze || math.is_some() || in_code {
                    current_text.push_str("<br>"); // anki linebreak

                    // prevent infinite loop. Should enter the below path on the next loop
                    if chars[0].is_none() {
                        in_cloze = false;
                        math = None;
                    }
                // outside of any special blocks
                } else {
                    num_cloze = 1;

                    let mut current_text = mem::take(&mut current_text);
                    if contains_cloze && !current_text.is_empty() {
                        // append path & headings
                        current_text.push_str("<br>");
                        let path_str = path.to_string_lossy();
                        current_text.push_str(&path_str[2..path_str.len() - 3]); // remove ./ and .md
                        for heading in &headings {
                            if !heading.is_empty() {
                                write!(current_text, " > {heading}").unwrap();
                            }
                        }

                        // handle note id
                        let format_note_id =
                            |id: u64| format!("\n<!--NoteID:{id}-->").into_chars().collect();
                        let mock_note_id: Vec<char> = format_note_id(1000000000000); // should have the same length as normal ones

                        // if the potential id has the correct format
                        if let Some(potential_id) = file_contents.get(i..i + mock_note_id.len())
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
                                current_text,
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
                            match add_cloze_note(current_text, Vec::new(), client).await {
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

                    contains_cloze = false;
                    // headings
                    possible_heading = 1;
                    capturing_heading = false;
                    heading_level = 0;

                    if chars[0].is_none() {
                        break;
                    }
                }
            }
            // code
            [Some('`'), Some('`'), Some('`')] if math.is_none() => {
                in_code = !in_code;
                current_text.push('`'); // still push entire "```"
            }
            // cloze
            [Some('='), Some('='), _] if math.is_none() && !in_code => {
                if in_cloze {
                    current_text.push_str("}}");
                } else {
                    write!(current_text, "{{{{c{num_cloze}::").unwrap();
                    num_cloze += 1;
                }

                // skip second '='
                i += 1;
                in_cloze = !in_cloze;
                contains_cloze = true;
            }
            // math
            [Some('$'), Some('$'), _] if !in_code => match math {
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
            [Some('$'), _, _] if !in_code => match math {
                None => math = Some(Math::Inline),
                Some(Math::Inline) => {
                    math = None;
                    let converted = convert_math(&mem::take(&mut math_text), Math::Inline).await?;
                    current_text.push_str(&converted);
                }
                Some(Math::Display) => math_text.push('$'),
            },
            [Some('['), Some('['), _] | [Some(']'), Some(']'), _] if math.is_none() && !in_code => {
                i += 1
            }
            // headings
            [Some('#'), _, _] if possible_heading > 0 => {
                heading_level += 1;
                possible_heading = 2;
                new_heading = true;
            }
            [Some(' '), _, _] if heading_level > 0 && !capturing_heading => {
                capturing_heading = true;
            }
            [Some(other), _, _] => {
                if capturing_heading {
                    // adjust length
                    if heading_level > headings.len() {
                        for _ in 0..heading_level - headings.len() {
                            headings.push(Default::default());
                        }
                    } else {
                        headings.truncate(heading_level);
                    }
                    if new_heading {
                        headings[heading_level - 1].clear();
                        new_heading = false;
                    }
                    headings[heading_level - 1].push(other);
                }
                if math.is_some() {
                    &mut math_text
                } else {
                    &mut current_text
                }
                .push(other)
            }
        }
        i += 1;
        possible_heading.saturating_sub(1);
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
