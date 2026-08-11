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
library and the language test suite. Rules 1, 2, and 5 have no blanket
enforcement yet. A violation of any rule is a bug, never a judgment call.

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
that construct broken (rule 4) and stays with its element. In an element
list a comment also disables packing, because a packed line cannot carry an
interior comment (rule 3). The commented list takes one element per line.

## Non-goals

The formatter does not rewrite comments. It does not hoist trailing
comments onto their own line, reflow comment text, or normalize comment
markers. Comment content is preserved byte for byte.
