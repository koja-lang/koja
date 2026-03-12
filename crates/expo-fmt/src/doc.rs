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

pub fn nil() -> Doc {
    Doc::Nil
}

pub fn text(s: impl Into<String>) -> Doc {
    Doc::Text(s.into())
}

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

pub fn if_break(flat_doc: Doc, break_doc: Doc) -> Doc {
    Doc::IfBreak(Box::new(flat_doc), Box::new(break_doc))
}

/// Trailing comma: "," in break mode, nothing in flat mode.
pub fn trailing_comma() -> Doc {
    if_break(nil(), text(","))
}

pub fn concat(docs: Vec<Doc>) -> Doc {
    Doc::Concat(docs)
}

pub fn indent(n: u32, doc: Doc) -> Doc {
    Doc::Indent(n, Box::new(doc))
}

pub fn group(doc: Doc) -> Doc {
    Doc::Group(Box::new(doc))
}

pub fn fill(docs: Vec<Doc>) -> Doc {
    Doc::Fill(docs)
}

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

pub fn join_hardline(docs: Vec<Doc>) -> Doc {
    intersperse(docs, hardline())
}

pub fn space() -> Doc {
    text(" ")
}

// =========================================================================
// Renderer
// =========================================================================

const DEFAULT_WIDTH: u32 = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Flat,
    Break,
}

pub fn render(doc: &Doc, width: u32) -> String {
    let mut out = String::new();
    let mut col: u32 = 0;
    // Stack entries: (indent_level, mode, doc_ref)
    let mut stack: Vec<(u32, Mode, &Doc)> = vec![(0, Mode::Break, doc)];

    while let Some((ind, mode, d)) = stack.pop() {
        match d {
            Doc::Nil => {}
            Doc::Text(s) => {
                out.push_str(s);
                col += s.len() as u32;
            }
            Doc::Hardline => {
                out.push('\n');
                for _ in 0..ind {
                    out.push(' ');
                }
                col = ind;
            }
            Doc::Line => match mode {
                Mode::Flat => {
                    out.push(' ');
                    col += 1;
                }
                Mode::Break => {
                    out.push('\n');
                    for _ in 0..ind {
                        out.push(' ');
                    }
                    col = ind;
                }
            },
            Doc::Softline => match mode {
                Mode::Flat => {}
                Mode::Break => {
                    out.push('\n');
                    for _ in 0..ind {
                        out.push(' ');
                    }
                    col = ind;
                }
            },
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
                if fits(width.saturating_sub(col), &[(ind, Mode::Flat, inner)]) {
                    stack.push((ind, Mode::Flat, inner));
                } else {
                    stack.push((ind, Mode::Break, inner));
                }
            }
            Doc::Fill(items) => {
                render_fill(&mut out, &mut col, &mut stack, ind, items, width);
            }
        }
    }

    out
}

/// Fill rendering: pack items left-to-right, breaking only when an item
/// doesn't fit on the current line.
///
/// Each item in `items` is expected to be a single "element" (possibly
/// preceded by a separator like ", "). We try to fit each item flat;
/// if it doesn't fit, we emit a line break and then the item flat.
fn render_fill(
    out: &mut String,
    col: &mut u32,
    _stack: &mut Vec<(u32, Mode, &Doc)>,
    ind: u32,
    items: &[Doc],
    width: u32,
) {
    // We process fill items in order (not via the main stack, since we need
    // per-item fit decisions). Push remaining main-stack work back after.
    for (i, item) in items.iter().enumerate() {
        let remaining = width.saturating_sub(*col);
        if i == 0 {
            // First item: always emit flat (or break if it contains hardlines)
            render_doc_into(out, col, ind, Mode::Flat, item, width);
        } else if fits(remaining, &[(ind, Mode::Flat, item)]) {
            render_doc_into(out, col, ind, Mode::Flat, item, width);
        } else {
            // Break: newline + indent, then emit the item flat
            out.push('\n');
            for _ in 0..ind {
                out.push(' ');
            }
            *col = ind;
            render_doc_into(out, col, ind, Mode::Flat, item, width);
        }
    }
}

/// Render a single doc node directly into the output buffer.
/// Used by fill rendering to process items outside the main stack.
fn render_doc_into(out: &mut String, col: &mut u32, ind: u32, mode: Mode, doc: &Doc, width: u32) {
    let mut local_stack: Vec<(u32, Mode, &Doc)> = vec![(ind, mode, doc)];
    while let Some((ind, mode, d)) = local_stack.pop() {
        match d {
            Doc::Nil => {}
            Doc::Text(s) => {
                out.push_str(s);
                *col += s.len() as u32;
            }
            Doc::Hardline => {
                out.push('\n');
                for _ in 0..ind {
                    out.push(' ');
                }
                *col = ind;
            }
            Doc::Line => match mode {
                Mode::Flat => {
                    out.push(' ');
                    *col += 1;
                }
                Mode::Break => {
                    out.push('\n');
                    for _ in 0..ind {
                        out.push(' ');
                    }
                    *col = ind;
                }
            },
            Doc::Softline => match mode {
                Mode::Flat => {}
                Mode::Break => {
                    out.push('\n');
                    for _ in 0..ind {
                        out.push(' ');
                    }
                    *col = ind;
                }
            },
            Doc::IfBreak(flat_doc, break_doc) => match mode {
                Mode::Flat => local_stack.push((ind, mode, flat_doc)),
                Mode::Break => local_stack.push((ind, mode, break_doc)),
            },
            Doc::Concat(docs) => {
                for d in docs.iter().rev() {
                    local_stack.push((ind, mode, d));
                }
            }
            Doc::Indent(n, inner) => {
                local_stack.push((ind + n, mode, inner));
            }
            Doc::Group(inner) => {
                if fits(width.saturating_sub(*col), &[(ind, Mode::Flat, inner)]) {
                    local_stack.push((ind, Mode::Flat, inner));
                } else {
                    local_stack.push((ind, Mode::Break, inner));
                }
            }
            Doc::Fill(items) => {
                render_fill(out, col, &mut local_stack, ind, items, width);
            }
        }
    }
}

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

pub fn render_default(doc: &Doc) -> String {
    render(doc, DEFAULT_WIDTH)
}
