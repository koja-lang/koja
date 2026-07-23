# Type System Design

This document records the durable shape of Koja's type system. It describes
identity, composition, and dispatch rather than repeating syntax. See
[LANGUAGE.md](../LANGUAGE.md) for the current language and [GAPS.md](GAPS.md)
for known limitations.

## Foundations

Koja is statically typed. Type checking seals every expression with its
resolved type before IR lowering begins. Both backends consume that same sealed
program, so they cannot reinterpret source-level typing decisions.

The type system follows four principles.

- Named data types are nominal.
- Anonymous data types are structural.
- Every value follows the same value-semantics rules.
- Protocol dispatch is static through monomorphization.

## Products

Product types combine values that exist together.

### Structs

A `struct` is a named nominal product. Two structs with identical fields remain
different types. Structs define API boundaries, carry documentation, and may
own inline functions.

Fields participate in value semantics. Reading a field leaves the containing
struct usable. Updating a field produces a new independent value and rebinds
the target.

### Anonymous tuples

An anonymous tuple is a structural positional product with arity two or
greater. Its identity is the ordered sequence of element types. Field names are
intentionally absent. Code uses destructuring when it needs the elements.

Tuples are appropriate for local plumbing and multiple return values. A value
that needs durable field names or functions should be a struct.

The unit type `()` is the empty product. Koja has no one-element tuple.

### Anonymous records

Anonymous records are committed for 0.16 but are not implemented yet. They
complete the product model with a structural named shape.

Record identity is determined by field names and field types, independent of
declaration site or field order. Records are intended for lightweight boundary
data such as decoded JSON and query results. They do not define functions. A
shape that owns behavior or represents a stable domain concept should remain a
struct.

The record design must preserve explicit boundary conversion. Naming policies
such as camel case or snake case belong to encoders and boundary libraries,
not structural type identity.

## Sums

An `enum` is a named nominal sum. Its variants define the complete set of cases
and may carry payloads.

A union type is an anonymous structural sum written from its member types. A
named `type` declaration may give that union a stable name without changing its
member-based semantics. Values widen into a compatible union explicitly in the
sealed program, and pattern matching must remain exhaustive.

## Functions and closures

Named functions and closures share function types. A closure captures values
by value, using the same copy rules as arguments and assignments. Function
types describe parameter and return types without exposing whether a callable
has an environment.

Short closures rely on context for parameter inference. Stored closures use
the explicit block form so their complete type is visible.

## Generics and protocols

Generics are monomorphized. Each concrete instantiation is checked and lowered
without runtime type erasure.

Protocols define behavior rather than data identity. Bounds constrain generic
parameters, and protocol calls resolve statically. Koja has no dynamic protocol
objects or vtable-based dispatch.

`Self` names the implementing type inside protocol and implementation
contexts. Direct functions belong to type declarations or `extend` blocks.
Protocol conformance uses `impl Protocol for Type`.

## Aliases and package names

The `type` keyword gives a transparent name to any type expression, most
commonly a union. Package `alias` declarations
are file-local shorthands for qualified external types. Neither mechanism
creates a new nominal wrapper.

Package qualification is separate from type identity. Files in one package
share a namespace, while external declarations are reached through their
package name.

## Value semantics

All types obey one semantic rule. Bindings, parameters, returns, captures, and
fields are independent values. Heap sharing through reference counting and
copy-on-write is an implementation optimization and cannot be observed as
aliasing.

The complete runtime contract lives in
[MEMORY-MODEL.md](MEMORY-MODEL.md).

## Evolution

New type-system work must preserve the named versus anonymous distinction,
static dispatch, and value semantics. Concrete release commitments belong in
[ROADMAP.md](ROADMAP.md). Compiler gaps and undecided compatibility changes
remain in [GAPS.md](GAPS.md) until a design is selected.
