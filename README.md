My cli for converting Obsidian Markdown files to anki clozure flashcards

## Usage
This is currently heavily hard-coded for my exact use-case, feel free to open an issue / PR for anything you want to be configurable.
That being said, here's an outline of the mentioned use-case:
- Highlight (surround with "==") anything you want to make a cloze-style flashcard
- Setup AnkiConnect
- Have Anki open
- Run the cli in a directory with Markdown files
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
```file.md
# Thermal expansion
## Definition
- Change of ==[[volume]]/length== in response to ==change of
[[temperature]]==
## Formula
==$$Delta V/L = V/L dot gamma/alpha dot Delta T$$==
## Coefficients
==$gamma$== = ==linear==, ==$alpha$== = ==volumetric coefficient==
### Unit
==$\left\lbrack \frac \gamma \alpha \right\rbrack = 1K^{-1}\$==
```

## TODO
- fix multiline
- track note id


![](https://brainmade.org/black-logo.svg)
