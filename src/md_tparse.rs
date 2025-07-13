use tparse::*;

// file
Or! {FileElement, ClozeLines = ClozeLines, Heading = Heading, Tag = Tag,
Code = Code, Math = Math, Link = Link, Char = char}
type File = AllConsumed<Vec<FileElement>>;

// newline
Or! {Newline, Cr = TStr<"\r">, Lf = TStr<"\n">, CrLF = TStr<"\r\n">}

// heading
Concat! {NotNewline, IsNot<Newline>, char}
Concat! {Heading, VecN<1, TStr<"#">>, TStr<" ">, Vec<NotNewline>, Newline}

// tag
Concat! {Tag, TStr<"#">, TagSpacing, VecN<1, TagNameChar>, Newline}
// tag spacing
Or! {NotTagSpacing, HashTag = TStr<"#">, Space = TStr<" ">, Newline = Newline}
Concat! {TagSpacing, IsNot<NotTagSpacing>, char}
// tag name
Or! {NotTagContents, Space = TStr<" ">, Newline = Newline}
Concat! {TagNameChar, IsNot<NotTagContents>, char}

// Cloze
Or! {Element, Code = Code, Math = Math, Link = Link, Char = char}
Concat! {NotClozeEnd, IsNot<TStr<"==">>, Element}
Concat! {Cloze, TStr<"==">, VecN<1, NotClozeEnd>,TStr<"==">}

Or! {ClozeOrNewline, Cloze = Cloze, Newline = Newline}
Concat! {NotClozeOrNewline, IsNot<ClozeOrNewline>, Element}
Concat! {NotNewlineElement, IsNot<Newline>, Element}
Or! {NotNewlineElementOrCloze, NotNewlineElement = NotNewlineElement, Cloze = Cloze}
Concat! {ClozeLines, Vec<NotClozeOrNewline>, Cloze, Vec<NotNewlineElementOrCloze>, Option<NoteIdComment>}

// note id comment
const NOTE_ID_COMMENT_START: &str = "<!--NoteID:";
const NOTE_ID_COMMENT_END: &str = "-->";
Concat! {NoteIdComment, Newline, TStr<NOTE_ID_COMMENT_START>, VecN<10, RangedChar<'0', '9'>>, TStr<NOTE_ID_COMMENT_END>, Option<Newline>}

// code
Or! {Code, Inline = InlineCode, Multiline = MultilineCode}
// inline code
Concat! {NotInlineCodeEnd, IsNot<TStr<"`">>, char}
Concat! {InlineCode, TStr<"`">, VecN<1, NotInlineCodeEnd>, TStr<"`">}
// display code
Concat! {NotMultilineCodeEnd, IsNot<TStr<"```">>, char}
Concat! {MultilineCode, TStr<"```">, VecN<1, NotMultilineCodeEnd>, TStr<"```">}

// math
Or! {Math, Inline = InlineMath, Display = DisplayMath}
// inline math
Concat! {NotInlineMathEnd, IsNot<TStr<"$">>, char}
Concat! {InlineMath, TStr<"$">, VecN<1, NotInlineMathEnd>, TStr<"$">}
// display math
Concat! {NotDisplayMathEnd, IsNot<TStr<"$$">>, char}
Concat! {DisplayMath, TStr<"$$">, VecN<1, NotDisplayMathEnd>, TStr<"$$">}

// Link
Or! {DisallowedInLink, ClosingBrackets = TStr<"]]">, Newline = Newline, Pipe = TStr<"|">}
Concat! {AllowedInLink, IsNot<DisallowedInLink>, char}
Concat! {Link, TStr<"[[">, VecN<1, AllowedInLink>, Option<LinkRename>, TStr<"]]">}
// LinkRename
Or! {DisallowedInLinkRename, ClosingBrackets = TStr<"]]">, Newline = Newline}
Concat! {AllowedInLinkRename, IsNot<DisallowedInLinkRename>, char}
Concat! {LinkRename, VecN<1, AllowedInLinkRename>}
