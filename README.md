My cli for converting Obsidian-style Markdown files to anki clozure flashcards

## Usage

This is currently somewhat hard-coded for my usage, feel free to open an issue / PR for anything you want to be configurable.
That being said, here's an outline of the aforementioned usage:

- Highlight (==text==) anything you want to make a cloze-style flashcard
  - For Obsidian you might want to include the `disable_highlight.css` snippet, to disable the default highlighting
- Setup AnkiConnect
- Have Anki open
- Run anksidian in a directory with Markdown files
- Anksidian will add the clozes to anki and insert html-comments with their note ids below
- Any files/directories mentioned in a .gitignore, as well as files unchanged since the last run will be ignored
Supported are:
- Multi-line flashcards
- Math blocks
  - style
    - inline ($..$)
    - display ($$..$$)
  - language
    - latex
    - typst
      - requires `typst` and `pandoc` to be installed.
- Obsidian-style hyperlinks, including renamed hyperlinks ([[link|SomeRename]])
  - Images can also be included like this: `![[image.jpg]]`
  - jpg, jpeg, jxl, png, gif, bmp, svg, webp, apng, ico, tif, tiff and avif extensions are currently recognised. Please open a PR or issue if the format of your choice isn't yet included.
  - Automatically converts .jxl files to normal jpeg, as anki doesn't yet support jpeg xl.
    - This requires `djxl` to be installed
  - Currently images are always shown on the backside of cards, although being able to configure this is planned.
- tags (#tag)
  - All tags in a file get added as Anki tags for all clozes in the file
  - Tags have to be at the start of separate lines

## Arguments

`anksidian [--track-seen] [--no-cache]`

`--track-seen`

- track whether notes were seen in unchanged files

`--no-cache`

- do not use the file cache (located at `~/.cache/anksidian/file_cache.json`)

Both `--track-seen` and `--no-cache` enable anksidian to detect when a note was deleted in your files, but not from Anki, and will ask to do so for you.

## Config

Anksidian's config is located at ~/.config/anksidian/config.json and will be created on the first run.
It currently contains two config values:

- ignore_paths:
  - is a list of all the paths (or regexes) anksidian should ignore
- path_to_deck:
  - a mapping of paths (or regexes) to anki decks, that anksidian should use
  - is evaluated in the order specifified in the config
- disable_typst:
  - is a bool to disable typst to latex conversion
- tag_to_deck:
  - a mapping of tags to anki decks, that anksidian should use
  - is evaluated in the order specifified in the config

- if there is a confict between the paths and tags it defaults to path

## Example

- can be found in `test.md`

## TODO

- tables
- choose where images are shown
- fix off-by-one somewhere in the heading logic

![](https://brainmade.org/black-logo.svg)
