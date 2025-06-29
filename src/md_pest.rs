use crate::anki::{NoteId, add_cloze_note, update_cloze_note};
use log::{debug, error};
use pest::{Parser, iterators::Pair};
use pest_derive::Parser;
use std::{cmp::Ordering, fmt::Write as _, fs, io, path::Path, process::Stdio};
use tokio::{io::AsyncWriteExt, process::Command};

#[derive(Parser)]
#[grammar = "anksidian.pest"]
struct AnksidianParser;

/// the length of a note id comment in bytes.
/// Only right for the years 2001-2286
const LEN_NOTE_ID_COMMENT: usize = "<!--NoteID:0000000000000-->\n".len();

pub async fn handle_md(path: &Path, client: &reqwest::Client, deck: &str) {
    let str = fs::read_to_string(path).expect("Reading file shouldnt fail");

    let parsed = AnksidianParser::parse(Rule::file, &str)
        .expect("Parsing file shouldn't fail")
        .next()
        .expect("There should always be one pair");

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

    for pair in parsed.into_inner() {
        match pair.as_rule() {
            Rule::heading => handle_heading(pair.as_str(), &mut headings),
            // remove trailing newline, then push
            Rule::tag => tags.push(pair.as_str().trim_end()),
            Rule::cloze_lines => handle_cloze_lines(pair, &headings, &mut clozes, &path_str).await,
            // ignore EOI, as well as code, math & links outside of cloze lines
            Rule::code | Rule::math | Rule::link | Rule::EOI => {}
            other => unreachable!("{other:?}"),
        }
    }

    let mut last_read = 0;
    let mut out_string = String::with_capacity(str.len() + clozes.len() * LEN_NOTE_ID_COMMENT);
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

fn handle_heading<'i>(str: &'i str, headings: &mut Vec<&'i str>) {
    // remove trailing newline
    let str = str.trim_end();
    let level = str.chars().take_while(|&char| char == '#').count();

    let contents = &str[level + 1..];

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

async fn handle_cloze_lines<'i>(
    pair: Pair<'i, Rule>,
    headings: &[&'i str],
    // (contents, id, end)
    clozes: &mut Vec<(String, Option<u64>, usize)>,
    path_str: &str,
) {
    assert!(pair.as_rule() == Rule::cloze_lines);
    let end = pair.as_span().end();

    let mut string = String::new();
    let mut cloze_num: u8 = 0;
    let mut note_id = None;

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
