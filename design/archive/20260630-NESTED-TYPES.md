# Nested type names

Let a struct/enum be declared in the namespace of an owning type, referenced
as `Owner.Nested` — completing Koja's existing "types are namespaces" rule.
This is a **prerequisite for [SUPERVISION.md](../SUPERVISION.md)**: that work
introduces ~10 related types (`Process.Identifier`, `Process.ExitSignal`,
`Supervisor.ChildSpec`, …) that should group under their owner rather than
flood the auto-imported `Global` package.

**Status: implemented** for the qualified-name declaration form. Construction,
patterns, type-position resolution, generic nested types (`Owner.Nested<T>`),
`extend`/`impl` on nested targets, aliases, mangling, and `Debug` surface-name
rendering all work across the eval and LLVM backends. The lexical (in-body)
declaration form is deferred (see [Declaration syntax](#declaration-syntax)).

## Why now

`Global` auto-imports every public top-level type, so each new one is a
globally visible name. A subsystem that adds a cluster of related types both
pollutes that namespace and loses the grouping that signals the types belong
together. Supervision is the forcing function; the registry/IO subsystems
will want the same.

## What the language already has

Two of the three namespacing cases exist; this feature is the third:

1. **A type namespaces functions.** Static functions and FFI externs live
   under a type: `DateTime.now()`, `Task.async`, `ChildSpec.of`. LANGUAGE.md
   states it directly — "types are namespaces."
2. **A package namespaces types.** Qualified type paths already resolve and
   mangle: `Net.TCPSocket`, `JSON.Encoder`, with `alias Net.TCPSocket` /
   `alias JSON.Encoder as JSONEncoder` for a file-local shorthand.
3. **Missing: a type namespaces a _type_** — `Owner.Nested`, where `Nested`
   is a struct or enum. This feature, and (per the model below) it is the
   generalization of case (2)+variants, not a new construct beside them.

Because (1) and (2) already exist, the machinery to extend is name
resolution and mangling of _qualified paths_, not a greenfield namespace
system — the cost is incremental.

## Resolution model

A variant _is_ conceptually a nested member of its enum — the enum is a
pseudo type-union over its variants. So nested types do not fight with
variants; they **generalize** what variants already are: members reached by a
dotted path. The resolver already walks dotted paths and distinguishes
`Package.Type` from `Enum.Variant`; nested types are simply a third member
kind on the same machinery.

**Unified dotted-path resolution.** A _namespace_ is a package or any type
(struct, enum, protocol). A namespace holds members with names unique within
it:

- static functions / FFI externs (snake_case),
- enum variants (PascalCase; enums only),
- nested types (PascalCase).

`A.B.C` resolves left-to-right: resolve `A` (a binding in scope — a package or
type), look up `B` in `A`'s namespace, then `C` in `B`'s, to **arbitrary
depth**. This is exactly how `ExitReason.Crashed` and `Net.TCPSocket` resolve
today; `Process.ExitSignal` and `Process.ExitReason.Crashed` (protocol →
nested enum → variant) resolve by the same walk.

There is **no enum-owner restriction and no special-case disambiguation.**
The resolver always knows each segment's kind because it resolved the
previous one. The PascalCase `Owner.Member` "ambiguity" (variant vs nested
type) is only ever a _reader_ question — answered the same way variants are
today, by knowing what `Owner` is — never a resolver question. The single
rule is the one variants already obey: **names are unique within a
namespace** (a nested type may not collide with a variant or static fn of the
same owner).

Nesting is therefore **namespacing only**, not type-level association:
`Owner.Nested` is one concrete type, not a per-implementer slot. The leaf
name is not auto-imported — callers qualify it or `alias Owner.Nested` per
file (reusing the package-path alias mechanism); field and variable names
stay short (`pid: Process.Identifier`). No IR/backend semantics change:
purely declaration, path resolution, mangling, and `Debug` name rendering.

## Non-goals

- **Associated types / type families.** A nested type that varies per `impl`
  (e.g. `Self.Output`) is a different feature; this is static namespacing.
- **Owner-generic capture.** A nested type does not implicitly capture the
  owner's type parameters (`Process.ExitSignal` is not generic over
  `C, M, R`). Revisit only if a concrete need appears.
- **Variants-as-types.** Treating each variant as a standalone type (true
  union types) is the conceptual lens that justifies the unified model, but
  promoting variants to first-class types is out of scope.

## Declaration syntax

- **Qualified-name top-level decl** (implemented) — `struct Process.ExitSignal
… end`. Works for any owner kind and attaches a nested type from outside the
  owner's body, exactly as `extend Type` adds methods from outside the body:

  ```koja
  struct Process.ExitSignal
    pid: Process.Identifier
    reason: Process.ExitReason
  end
  ```

- **Lexical** (deferred) — declaring a type inside the owner's body. Same
  resolved member, just sugar for the common same-file case. Not yet parsed:

  ```koja
  struct Supervisor
    enum Strategy
      OneForOne
      OneForAll
      RestForOne
    end
  end
  ```

The owner must be a registered type in the **same package** as the nested
decl. This is enforced at collect time, and a nested type may not collide with
one of the owner's enum variants. Injecting a type into a foreign namespace is
more invasive than adding a method, so it stays disallowed.

## Resolved questions

1. **Generics in paths.** A nested type may be generic (`Owner.Nested<T>`). The
   nested type owns its own type params and does not capture the owner's.
   Chain-with-args (`A.B<T>.C`) is unneeded and not supported.
2. **Mangling & Debug.** Symbols carry the full owner path (`Pkg.Owner.Nested.method`),
   so a nested type never collides with a top-level type of the same leaf name.
   `Debug` renders the qualified surface name (`Owner.Nested{…}`,
   `Enum.Variant`) with the package stripped.
3. **Visibility.** `priv` on a nested type is package-private, as for any type.
   No new owner-private axis.

## Sequencing

Prerequisite for SUPERVISION.md **P2** (the first nested supervision types);
must land before those types are coined. SUPERVISION.md **P1** (crash
unwind, pure runtime) is independent and proceeds in parallel, so this never
gates the start of supervision work.
