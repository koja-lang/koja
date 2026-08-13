# Formatter

`koja format` rewrites source files into one canonical layout. This document
is normative for `koja-fmt`. It defines the contract the formatter must
uphold and the rules that decide layout and comment placement. When the
implementation and this document disagree, the implementation is wrong.

## Contract

1. Output parses. The formatter never writes a file the parser rejects.
2. Output preserves meaning. Formatting changes trivia, never the program.
3. Formatting is idempotent. One pass reaches the fixed point.
4. Output is canonical. Layout variants of the same program converge to one
   output.
5. No comment is lost. Every comment in the input appears in the output.

The corpus tests in `koja-fmt` enforce rules 3 and 4 over the standard
library and the language test suite. Rule 1 is enforced by a property test
that parses the output of formatting random parseable inputs. Rule 5 is
enforced three ways: a corpus test compares comment multisets before and
after formatting, a property test injects comments at random line
boundaries of corpus files, and the printer asserts that the comment
table is empty after printing. Rule 2 has no blanket enforcement yet. A
violation of any rule is a bug, never a judgment call.

## Layout basics

- Lines wrap at 80 columns. Indentation is 2 spaces per level.
- String literals never split. A line that only a string pushes past 80
  columns stays long.
- Broken field lists (constructions, parameter lists, enum struct variants)
  take one field per line and a trailing comma.
- Broken element lists (list, map, and set literals, and binary and bits
  literals) pack as many elements per line as fit, without a trailing
  comma. The delimiters sit on their own lines.
- Both stay inline without a trailing comma when they fit.
- Binary and bits patterns break like the literals. A segment
  (`value::spec`) is atomic and never splits around its `::`.
- Long operator chains pack operands per line and wrap with a 2 space
  continuation indent. Keyword operators (`and`, `or`) start the
  continuation line. Symbolic operators (`+`, `<>`, comparisons) end the
  previous line. An operand never splits around its operator.
- An assignment whose value renders as a multi-line block (`match`,
  `cond`, `if`, `receive`, a multi-line closure, or a call that contains
  one) breaks after `=` and indents the value 2 spaces. A value that fits
  on one line stays glued, including a call with an inline closure.
- String interpolation (`#{...}`) renders its expression flat. The
  segment never introduces a line break inside the string.
- Union types pack like symbolic operator chains: `|` ends the line and
  the continuation indents 2 spaces. This applies in type aliases,
  parameter types, and everywhere else a type renders.
- A generic parameter list (`<T: Hash & Equality, U>`) breaks like a
  parameter list: one entry per line inside the angle brackets. An entry
  keeps its bounds on one line.
- A fallible return (`-> T ! E`) wraps like a ternary. When the tail
  breaks, `-> T` and `! E` each take a continuation line that starts
  with its operator, indented 2 spaces.
- A method call on a collection literal breaks the literal's brackets
  first and hugs the call to the closing bracket.
- A zero-parameter signature drops its empty parens: `fn name -> X`.
- Annotation layout (`@extern "C" @link "m"` on one line or two) is the
  author's choice. Both spellings are stable.
- A ternary stays inline when it fits. A broken ternary keeps the
  condition on the head line and gives each branch a continuation line
  that starts with its `?` or `:`, indented 2 spaces. A broken ternary is
  still a line construct, so a trailing comment follows the last branch.
- Enum struct variants lay out like struct literals. Braces hug the variant
  name, and short field lists stay on one line.
- The formatter renders through a Wadler-style document algebra. A group
  collapses onto one line when it fits and breaks otherwise.

## Construct shapes

Comment placement follows from a two-way split of constructs.

A **line construct** has no interior. Examples include a struct field, a
`const`, an `alias`, a `type` alias, a protocol method signature, and an
arm written on one line.

A **block construct** has an interior and a terminator (`end` or a closing
delimiter). Examples include function bodies, `struct`/`enum`/`protocol`/
`impl`/`extend` bodies, `match`/`cond`/`receive`, block closures, and
broken collection literals.

Two refinements:

- **Arms classify by authored shape.** A `match`, `cond`, or `receive` arm
  written on one line (`1 -> 10`) is a line construct. An arm whose body
  spans multiple lines is a block. The construct kind does not decide, the
  shape the author wrote decides.
- **A block header is itself a line construct.** A function signature, a
  type declaration header, an arm head, and an `end` line each behave as a
  line construct for comment purposes, even though they open or close a
  block.

## Comment attachment

Before printing, the formatter assigns every comment to an owner slot in
one pass over the AST in source order. The printer consumes slots while it
renders, and an empty table after printing proves no comment was dropped.
Ownership follows three rules:

1. A comment on a construct's last line trails that construct.
2. Any other comment belongs to the innermost block that contains it.
   Inside that block it leads the next sibling child. When no child
   follows, it dangles before the block's terminator.
3. A comment between an annotation and its declaration hoists above the
   annotation.

Dangling comments before `else` or `after` render above that keyword at
the keyword's indent. The AST records no span for these keywords, so the
pass locates them in a token stream lexed from the same source.

## Comment rules

1. **A trailing comment stays glued to its line construct.** `x: Int # px`,
   `1 -> 10 # one`, `alias Process.Step # shorthand`, and
   `fn f(x: Int) -> Int # doubles x` all keep the comment on that line.
2. **A comment inside a block stays inside that block, where it was
   written.** The formatter never moves a comment across a block boundary in
   either direction. A comment between the last statement and `end` stays in
   the body.
3. **A line that carries a comment never merges with following code.** A
   `#` comment claims the rest of its physical line, so joining code after
   it changes the program. Every collapse decision must check for pending
   comments in the span first. This rule makes comment-corruption bugs
   impossible by construction.
4. **A block that contains a comment never collapses.** This follows from
   rule 3. Collapsing would destroy the block the comment lives in and force
   the comment to re-anchor with a different meaning.

### The authoring idiom

To annotate an inline arm, use a trailing comment. To annotate a block arm,
write the comment inside the block, after the arrow:

```koja
match x
  1 -> 10 # one
  _ ->
    # everything else lands here
    0
end
```

### Boundary cases

**A comment between two arms.** If the previous arm is a block, the comment
sits inside that block and stays with it as trailing body content. If the
previous arm is inline, it has no interior, so the comment leads the next
arm. Both placements keep the comment exactly where the author wrote it.

```koja
match x
  2 ->
    a = "t"
    a + "wo"
    # stays with arm 2 (previous arm is a block)
  _ -> "many"
end

match x
  1 -> 10
  # leads the wildcard arm (previous arm is inline)
  _ -> 0
end
```

**A comment before a block terminator.** It stays inside the block. A
comment between the last arm and `end` must not escape the `match`.

**Header and terminator lines.** A trailing comment on `struct Point`,
`fn f -> Int`, or `end` stays on that line. Nothing can follow `end` on its
line, so the placement is always safe.

**Comments inside bracketed constructs.** A comment inside a collection
literal, a construction, a call argument list, or a parameter list keeps
that construct broken (rule 4) and stays with its element. A broken field
list is one field per line, so the comment rides its field. In an element
list a comment breaks only its own line and packing resumes after it: a
trailing comment ends the packed line after its element, and a leading
comment takes its own line above the element that starts the next packed
run. Rule 3 holds because no code follows a comment on its line.

```koja
nums = [
  1, 2, # two
  3, 4, 5, 6,
  # header values start here
  7, 8
]
```

**Comments inside patterns.** A comment inside a list, tuple, enum,
struct, or binary pattern anchors to its element the same way, in match
and receive heads, destructuring assignments, and `for` loops. The
container breaks, the arm's `->` and any `when` guard glue to the
closing delimiter, and nested containers anchor their own comments. The
one exception is a comment between or-pattern alternatives, which moves
to the head line because the alternative list has no per-line layout.

```koja
match packet
  [
    # version byte
    version, flags
  ] -> decode(version, flags)
  _ -> drop()
end
```

**A comment between an annotation and its declaration.** It moves above
the annotation. An annotation glues to its declaration, so nothing can sit
between them:

```koja
# explains the function
@doc "Adds one."
fn add_one(x: Int32) -> Int32
```

## Non-goals

The formatter does not rewrite comments. It does not hoist trailing
comments onto their own line, reflow comment text, or normalize comment
markers. Comment content is preserved byte for byte.
