/// Wadler-Lindig document algebra extended with Fill for dense packing.
#[derive(Debug, Clone)]
pub enum Doc {
    Nil,
    Text(String),
    Hardline,
    /// " " in flat mode, newline+indent in break mode.
    Line,
    /// "" in flat mode, newline+indent in break mode.
    Softline,
    Concat(Vec<Doc>),
    Indent(u32, Box<Doc>),
    Group(Box<Doc>),
    Fill(Vec<Doc>),
    /// Emits first doc in flat mode, second in break mode.
    IfBreak(Box<Doc>, Box<Doc>),
}

/// The empty document.
pub fn nil() -> Doc {
    Doc::Nil
}

/// A literal text fragment that is never broken across lines.
pub fn text(s: impl Into<String>) -> Doc {
    Doc::Text(s.into())
}

/// An unconditional line break that always emits a newline.
pub fn hardline() -> Doc {
    Doc::Hardline
}

/// " " in flat mode, newline in break mode.
pub fn line() -> Doc {
    Doc::Line
}

/// "" in flat mode, newline in break mode.
pub fn softline() -> Doc {
    Doc::Softline
}

/// Emits `flat_doc` when the enclosing group fits on one line,
/// `break_doc` when it breaks.
pub fn if_break(flat_doc: Doc, break_doc: Doc) -> Doc {
    Doc::IfBreak(Box::new(flat_doc), Box::new(break_doc))
}

/// Trailing comma: "," in break mode, nothing in flat mode.
pub fn trailing_comma() -> Doc {
    if_break(nil(), text(","))
}

/// Joins a list of documents sequentially with no separator.
pub fn concat(docs: Vec<Doc>) -> Doc {
    Doc::Concat(docs)
}

/// Increases the indentation level by `n` spaces for the inner document.
pub fn indent(n: u32, doc: Doc) -> Doc {
    Doc::Indent(n, Box::new(doc))
}

/// Tries to lay out the inner document on a single line (flat mode).
/// Falls back to break mode if it doesn't fit within the page width.
pub fn group(doc: Doc) -> Doc {
    Doc::Group(Box::new(doc))
}

/// Dense packing: each item is placed flat, breaking to a new line only
/// when an item would exceed the page width.
pub fn fill(docs: Vec<Doc>) -> Doc {
    Doc::Fill(docs)
}

/// Joins documents with `sep` inserted between each pair.
pub fn intersperse(docs: Vec<Doc>, sep: Doc) -> Doc {
    let mut result = Vec::new();
    for (i, doc) in docs.into_iter().enumerate() {
        if i > 0 {
            result.push(sep.clone());
        }
        result.push(doc);
    }
    Doc::Concat(result)
}

/// Joins documents with unconditional line breaks between each pair.
pub fn join_hardline(docs: Vec<Doc>) -> Doc {
    intersperse(docs, hardline())
}

/// A single space character.
pub fn space() -> Doc {
    text(" ")
}

// =========================================================================
// Renderer
// =========================================================================

/// The page width the formatter targets.
pub const DEFAULT_WIDTH: u32 = 80;

/// Layout mode for the renderer's stack entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Lay out on a single line (spaces instead of newlines).
    Flat,
    /// Lay out with line breaks and indentation.
    Break,
}

/// Renders a document tree to a string, wrapping lines at `width` columns.
pub fn render(doc: &Doc, width: u32) -> String {
    let mut out = String::new();
    let mut col: u32 = 0;
    render_doc_into(&mut out, &mut col, 0, Mode::Break, doc, width);
    out
}

/// Emits a newline followed by `ind` spaces of indentation.
fn emit_newline(out: &mut String, col: &mut u32, ind: u32) {
    out.push('\n');
    for _ in 0..ind {
        out.push(' ');
    }
    *col = ind;
}

/// Fill rendering: pack items left-to-right, breaking only when an item
/// doesn't fit on the current line.
///
/// Each item carries any separator as a *trailing* suffix (e.g. `", "` or
/// `" and "`), so a break lands on a fresh line at the fill indent with no
/// stray leading space. The dangling trailing separator is removed by the
/// final per-line `trim_end`.
fn render_fill(out: &mut String, col: &mut u32, ind: u32, items: &[Doc], width: u32) {
    for (i, item) in items.iter().enumerate() {
        if i > 0 && !fits(width.saturating_sub(*col), &[(ind, Mode::Flat, item)]) {
            emit_newline(out, col, ind);
        }
        render_doc_into(out, col, ind, Mode::Flat, item, width);
    }
}

/// Renders a single doc node into the output buffer using a local stack.
fn render_doc_into(out: &mut String, col: &mut u32, ind: u32, mode: Mode, doc: &Doc, width: u32) {
    let mut stack: Vec<(u32, Mode, &Doc)> = vec![(ind, mode, doc)];
    while let Some((ind, mode, d)) = stack.pop() {
        match d {
            Doc::Nil => {}
            Doc::Text(s) => {
                out.push_str(s);
                *col += s.len() as u32;
            }
            Doc::Hardline => emit_newline(out, col, ind),
            Doc::Line => match mode {
                Mode::Flat => {
                    out.push(' ');
                    *col += 1;
                }
                Mode::Break => emit_newline(out, col, ind),
            },
            Doc::Softline => {
                if mode == Mode::Break {
                    emit_newline(out, col, ind);
                }
            }
            Doc::IfBreak(flat_doc, break_doc) => match mode {
                Mode::Flat => stack.push((ind, mode, flat_doc)),
                Mode::Break => stack.push((ind, mode, break_doc)),
            },
            Doc::Concat(docs) => {
                for d in docs.iter().rev() {
                    stack.push((ind, mode, d));
                }
            }
            Doc::Indent(n, inner) => {
                stack.push((ind + n, mode, inner));
            }
            Doc::Group(inner) => {
                if fits(width.saturating_sub(*col), &[(ind, Mode::Flat, inner)]) {
                    stack.push((ind, Mode::Flat, inner));
                } else {
                    stack.push((ind, Mode::Break, inner));
                }
            }
            Doc::Fill(items) => {
                render_fill(out, col, ind, items, width);
            }
        }
    }
}

/// Returns `true` if the documents on `stack` can be rendered flat
/// without exceeding `remaining` columns.
fn fits(mut remaining: u32, stack: &[(u32, Mode, &Doc)]) -> bool {
    let mut work: Vec<(u32, Mode, &Doc)> = stack.iter().rev().cloned().collect();
    while let Some((ind, mode, d)) = work.pop() {
        match d {
            Doc::Nil => {}
            Doc::Text(s) => {
                let len = s.len() as u32;
                if len > remaining {
                    return false;
                }
                remaining -= len;
            }
            Doc::Hardline => return true,
            Doc::Line => match mode {
                Mode::Flat => {
                    if remaining == 0 {
                        return false;
                    }
                    remaining -= 1;
                }
                Mode::Break => return true,
            },
            Doc::Softline => match mode {
                Mode::Flat => {}
                Mode::Break => return true,
            },
            Doc::IfBreak(flat_doc, _break_doc) => {
                work.push((ind, Mode::Flat, flat_doc));
            }
            Doc::Concat(docs) => {
                for d in docs.iter().rev() {
                    work.push((ind, mode, d));
                }
            }
            Doc::Indent(n, inner) => {
                work.push((ind + n, mode, inner));
            }
            Doc::Group(inner) => {
                work.push((ind, Mode::Flat, inner));
            }
            Doc::Fill(items) => {
                for item in items.iter().rev() {
                    work.push((ind, Mode::Flat, item));
                }
            }
        }
    }
    true
}

/// Renders a document tree using the default line width (80 columns).
pub fn render_default(doc: &Doc) -> String {
    render(doc, DEFAULT_WIDTH)
}
