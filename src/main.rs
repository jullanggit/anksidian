#![feature(exit_status_error)]

use std::{
    borrow::Cow,
    fs,
    io::{self, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
};

use reqwest::Client;

struct Config {
    ignore_paths: Vec<PathBuf>,
}

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

fn traverse(dir: PathBuf, config: &Config) -> io::Result<()> {
    for entry in dir.read_dir()?.flatten() {
        let path = entry.path();
        // recurse
        if path.is_dir() && !config.ignore_paths.contains(&path) {
            traverse(path, config)?;
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

fn handle_md(file: &Path) -> io::Result<()> {
    let contents = fs::read_to_string(file)?;
    let converted_math = convert_math(&contents)?;
    let removed_hyperlinks = converted_math.replace("[[", "").replace("]]", "");

    let highlighted = get_surrounded(&removed_hyperlinks, "==", false)
        .into_iter()
        .map(|(start, end)| SubStrWithSurroundingNewlines {
            sub_str: removed_hyperlinks[start..end].to_string(),
            previous_newline: removed_hyperlinks[0..start].rfind('\n').unwrap_or(0),
            start,
            end,
            next_newline: removed_hyperlinks[end..]
                .find('\n')
                .map_or(removed_hyperlinks.len(), |offset| end + offset),
        });

    let converted_math = highlighted.into_iter().map(|mut str_w_nl| {
        // remove hyperlink brackets
        str_w_nl.sub_str = convert_math(&str_w_nl.sub_str).unwrap();
        str_w_nl
    });

    for string in converted_math {
        let anki_formatted = format!(
            "{}{{{{c1::{}}}}}{}",
            &removed_hyperlinks[string.previous_newline + 1..string.start],
            string.sub_str,
            &removed_hyperlinks[string.end..string.next_newline]
        );
        let removed_highlights = anki_formatted.replace("==", "");
    }

    Ok(())
}

/// Get substrings surrounded by delimiter
fn get_surrounded(string: &str, delimiter: &str, with_delimiter: bool) -> Vec<(usize, usize)> {
    let mut pos = 0;
    let mut results = Vec::new();

    while let Some(Some(offset)) = string.get(pos..).map(|str| str.find(delimiter)) {
        let start = pos + offset + delimiter.len();

        if let Some(offset) = string[start..].find(delimiter) {
            let end = start + offset;
            pos = end + delimiter.len();

            results.push(if with_delimiter {
                (start - delimiter.len(), end + delimiter.len())
            } else {
                (start, end)
            });
        } else {
            break;
        }
    }

    results
}

/// Convert from Obsidian latex/typst to anki latex
fn convert_math(str: &str) -> io::Result<String> {
    let mut string = str.to_string();
    let maths = get_surrounded(str, "$", true);

    for (start, end) in maths {
        let math = &str[start..end];

        let replace = if is_typst(math)? {
            typst_to_latex(math)?
        } else {
            let mut temp = String::from("\\(");
            temp.push_str(&math[1..math.len() - 1]);
            temp.push_str("\\)");
            temp
        };

        string = string.replace(math, &replace);
    }

    Ok(string)
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
