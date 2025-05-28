#![feature(exit_status_error)]
#![feature(iter_map_windows)]

use std::{
    convert::identity,
    fs,
    io::{self, Write},
    mem,
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

const IGNORE_PATHS: [&str; 1] = ["./Excalidraw"];

const TEST_MD: &str = "## Definition
- Veränderung des Volumens/Länge bei Temperaturveränderung
## Formel
==$Delta V/L = V/L dot gamma/alpha dot Delta T$==
## Koeffizienten
==$gamma$== = ==Raum-==, ==$alpha$== = ==Längenausdehnungskoeffizient==
### Einheit
==$[gamma/alpha] = 1K^(-1)$==
## Random Latex
==$\\frac{3}{\\pi}$==
## Mixed
==Idkman $gamma$ me neither==";

#[derive(Debug)]
struct SubStrWithSurroundingNewlines {
    sub_str: String,
    previous_newline: usize,
    start: usize,
    end: usize,
    next_newline: usize,
}

fn main() {
    // TODO: remove
    {
        fs::write("/tmp/test", TEST_MD);
        handle_md(&PathBuf::from("/tmp/test"));
    }
}

fn traverse(dir: PathBuf) -> io::Result<()> {
    for entry in dir.read_dir()?.flatten() {
        let path = entry.path();
        // recurse
        if path.is_dir()
            && !IGNORE_PATHS
                .map(AsRef::<Path>::as_ref)
                .contains(&path.as_path())
        {
            traverse(path)?;
        // markdown file
        } else if path.is_file()
            && let Some(extension) = path.extension()
            && extension == "md"
        {
            handle_md(&path)?;
        }
    }

    Ok(())
}

#[derive(PartialEq, Clone, Copy)]
enum Math {
    Inline,
    Display,
}

fn handle_md(path: &Path) -> io::Result<()> {
    let file_contents = fs::read_to_string(path)?;

    let mut clozes = Vec::new(); // todo: handle ID
    let mut line = 0;
    let mut current_text = String::new();
    let mut math_text = String::new();
    let mut in_cloze = false;
    let mut line_contains_cloze = false;
    let mut num_cloze = 1;
    let mut math = None;

    // init iter
    let mut i = 0;
    // skip to the newline before the next cloze
    let skip_before_next_cloze = |file_contents: &str, i: &mut usize, line: &mut usize| {
        if let Some(next_cloze_offset) = file_contents
            .chars()
            .skip(*i)
            .map_windows(|chars| *chars == ['='; 2])
            .position(identity)
        {
            let (newline_before_offset, (newlines_skipped, _)) = file_contents
                .chars()
                .skip(*i)
                .take(next_cloze_offset)
                .enumerate()
                .filter(|(_, char)| *char == '\n')
                .enumerate()
                .last()
                .unwrap_or_default();
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
        let Some(char_a) = file_contents.chars().nth(i) else {
            break;
        };
        let char_b = file_contents.chars().nth(i + 1).unwrap_or_default();
        match [char_a, char_b] {
            ['\n', _] => {
                line += 1;
                if in_cloze || math.is_some() {
                    current_text.push('\n');
                } else {
                    if line_contains_cloze {
                        clozes.push(mem::take(&mut current_text));
                    } else {
                        current_text.clear();
                    }
                    line_contains_cloze = false;
                    num_cloze = 1;

                    if !skip_before_next_cloze(&file_contents, &mut i, &mut line) {
                        break;
                    }
                }
            }
            ['=', '='] if math.is_none() => {
                line_contains_cloze = true;

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
            ['$', '$'] => match math {
                None => math = Some(Math::Display),
                Some(math_type) => {
                    math = None;
                    let converted = convert_math(&mem::take(&mut math_text), math_type)?; // todo: adjust this fn
                    current_text.push_str(&converted);
                    if math_type == Math::Display {
                        i += 1
                    }
                }
            },
            ['$', _] => match math {
                None => math = Some(Math::Inline),
                Some(Math::Inline) => {
                    math = None;
                    let converted = convert_math(&mem::take(&mut math_text), Math::Inline)?; // todo: adjust this fn
                    current_text.push_str(&converted);
                }
                Some(Math::Display) => push_char('$', math, &mut math_text, &mut current_text),
            },
            [other, _] => push_char(other, math, &mut math_text, &mut current_text),
        }
        i += 1;
    }
    dbg!(clozes);
    Ok(())
}

/// Convert from Obsidian latex/typst to anki latex
fn convert_math(str: &str, math_type: Math) -> io::Result<String> {
    let typst_style_math = match math_type {
        Math::Inline => format!("${str}$"),
        Math::Display => format!("$ {str} $"),
    };
    if is_typst(&typst_style_math)? {
        typst_to_latex(&typst_style_math)
    } else {
        Ok(match math_type {
            Math::Inline => format!("\\({str}\\)"),
            Math::Display => format!("\\[{str}\\]"),
        })
    }
}

fn is_typst(math: &str) -> io::Result<bool> {
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
        .write_all(math.as_bytes())?;

    // success -> true
    Ok(child.wait()?.code() == Some(0))
}

fn typst_to_latex(typst: &str) -> io::Result<String> {
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
        .write_all(typst.as_bytes())?;

    let mut stdout = child
        .wait_with_output()?
        .exit_ok()
        .map_err(io::Error::other)?
        .stdout;
    // remove trailing newline
    stdout.truncate(stdout.len() - 1);

    String::from_utf8(stdout).map_err(io::Error::other)
}
