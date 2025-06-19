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
- Obsidian-style hyperlinks, including renamed hyperlinks ([[link|SomeRename]])
- tags (#tag)
  - All tags in a file get added as Anki tags for all clozes in the file
  - Tags have to be at the start of separate lines

## Example
- can be found in `test.md`

## TODO
- hash files
- warn on unclosed highlight
- parallel file handling (not really effective)

![](https://brainmade.org/black-logo.svg)
