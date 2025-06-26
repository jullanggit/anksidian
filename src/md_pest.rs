use std::fs;

use pest::Parser;
use pest_derive::Parser;

#[derive(Parser)]
#[grammar = "anksidian.pest"]
struct AnksidianParser;

fn parse(file: &str) {
    let parsed = AnksidianParser::parse(Rule::file, file);
    dbg!(parsed);
}

#[test]
fn test_pest() {
    let file = fs::read_to_string("test.md").unwrap();
    parse(&file);
    panic!()
}
