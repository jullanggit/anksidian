My cli for converting Obsidian Markdown files to anki clozure flashcards

## Usage
This is currently heavily hard-coded for my exact use-case, feel free to open an issue / PR for anything you want to be configurable.
That being said, here's an outline of the mentioned use-case:
- Highlight (==text==) anything you want to make a cloze-style flashcard
- Setup AnkiConnect
- Have Anki open
- Run the cli in a directory with Markdown files
- Anksidian will add the clozes to anki and insert html-comments with their note ids below
- Any files/directories mentioned in a .gitignore will be ignored
Supported are:
- Multi-line flashcards
- Math blocks
  - style
    - inline ($..$)
    - display ($$..$$)
  - language
    - typst
    - latex
- Obsidian-style hyperlinks (just get removed)

## Example
- can be found in `test.md`

## TODO
- hash files
- handle renamed hyperlinks
- warn on unclosed highlight
- parallel file handeling

![](https://brainmade.org/black-logo.svg)
