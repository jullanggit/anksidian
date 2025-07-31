My cli for converting Obsidian-style Markdown files to anki clozure flashcards

## Usage
This is currently heavily hard-coded for my use-case, feel free to open an issue / PR for anything you want to be configurable.
That being said, here's an outline of the mentioned use-case:
- Highlight (==text==) anything you want to make a cloze-style flashcard
- Setup AnkiConnect
- Have Anki open
- Run the cli in a directory with Markdown files
- Anksidian will add the clozes to anki and insert html-comments with their note ids below
- Any files/directories mentioned in a .gitignore, as well as files unchanged since the last run will be ignored
Supported are:
- Multi-line flashcards
- Math blocks
  - style
    - inline ($..$)
    - display ($$..$$)
  - language
    - typst
      - requires `typst` and `pandoc` to be installed.
    - latex
      - currently also requires typst's dependencies, but this is planned to change.
- Obsidian-style hyperlinks, including renamed hyperlinks ([[link|SomeRename]])
  - Images can also be included like this: `![[image.jpg]]`
  - jpg, jpeg, jxl, png, gif, bmp, svg, webp, apng, ico, tif, tiff and avif extensions are currently recognised. Please open a PR or issue if the format of your choice isn't yet included.
  - Automatically converts .jxl files to normal jpeg, as anki doesn't yet support jpeg xl.
    - This requires `djxl` to be installed
  - Currently images are always shown on the backside of cards, although being able to choose this is planned.
- tags (#tag)
  - All tags in a file get added as Anki tags for all clozes in the file
  - Tags have to be at the start of separate lines

## Arguments
`anksidian DECK [--no-cache]`
`DECK`:
  - the deck to operate on
`--no-cache`:
  - do not use the file cache (located at `~/.cache/anksidian/file_cache.json`)
  - ask for deletion of unseen notes

## Example
- can be found in `test.md`

## TODO
- parallel file handling (not really effective)
- choose where images are shown
- dont require typst and pandoc if typst isnt used.
- italics & bold

![](https://brainmade.org/black-logo.svg)
