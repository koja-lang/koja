# Miscellaneous Ideas

This file is a memory aid for ideas that are worth retaining but do not yet
warrant a design document or roadmap commitment. An entry here is not an
accepted design, scheduled work, or a release promise.

When an idea becomes concrete, move it into a focused design document. Add it
to [ROADMAP.md](ROADMAP.md) only when its release scope is known. Delete ideas
that no longer serve the language.

## C-compatible structs

Support an explicitly C-compatible struct layout for passing records by value
across the FFI boundary. A future design must define field mapping, alignment,
padding, nested layouts, target ABI differences, and which Koja field types are
valid.

The old `@compat "C"` sketch is preserved in
[archive/20260722-FFI.md](archive/20260722-FFI.md). The annotation name and
surface syntax are not decided.

## C callbacks

Support passing Koja callables where a C API expects a function pointer. Bare
noncapturing functions are the narrowest possible starting point because a C
function pointer has no environment.

Capturing closures require a trampoline and explicit answers for:

- captured environment lifetime
- C userdata representation
- callbacks retained after the original foreign call returns
- entry from foreign OS threads
- Koja process and scheduler context
- non-atomic process-local reference counts
- panic containment at the C boundary

No callback may unwind a Koja panic through C. The runtime also cannot execute
ordinary process-local Koja code on an unattached foreign thread. A design
should begin from the needs of a real wrapper package rather than promise
general closure conversion.
