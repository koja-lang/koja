# Testing

**Status: design accepted (2026-09-12), implemented in 0.19.** This
document is normative for the `test` declaration, the `assert` statement,
the `Test` package, and the `koja test` runner. [LANGUAGE.md](../LANGUAGE.md)
documents the shipped surface. The `@test` contract described there stays
valid through 0.19 and is removed in 0.20. The assertion helper sketch in
[ERROR-HANDLING.md](ERROR-HANDLING.md#koja-test-rides-along) is superseded
by this document.

## Summary

A test is a declaration, not an annotated function:

```koja
test "pops in reverse order"
  stack = Stack.new().push(1).push(2)
  assert stack.pop() == Option.Some(2)
end
```

- `test "description"` declares a test at the top level or inside a struct.
  Its body has no parameters, a unit success type, and the error channel
  `Test.Failure`. Nothing else travels on it: setup errors enter through
  `try Test.require(...)`, and a string literal after `fail` converts
  through the `StringLiteral` protocol.
- `assert cond` and `assert cond, "message"` are statements that fail the
  test when `cond` is false. The compiler stamps the source text of the
  expression, the source line, the file, the line, and for comparison forms
  the `Debug` rendering of both operands.
- The `Test` package ships the `Test.Failure` enum and three functions:
  `require` for setup, `skip` for a test that cannot run here, and
  `crashes` for a body that must panic. The loader links it only when tests
  are included, so
  application code cannot name `Test.Failure` and therefore cannot use
  `assert`.
- The `Test` package is also the runner. `Test.Runner` executes each test
  in its own process with a deadline and feeds a `Test.Reporter`. Three
  reporters ship: `dots`, `trace`, and `json`. The human reporters follow
  the pretty and short styles of compiler diagnostics, and any package can
  add a reporter.
- Structs are suites and nested structs are the hierarchy. There is no
  `describe` keyword.

## Motivation

The 0.18.3 contract made a test a `fn ... ! String` inside a wrapper struct.
`fail message` is the assertion verb and `try` propagates setup failures.
That removed most of the ceremony, but three costs remain.

First, every comparison is four lines:

```koja
if result != 5
  fail "expected 5, got #{result}"
end
```

The stdlib has about 380 tests, 817 `fail` lines, and 517 `if x != y` lines.
Each one restates the comparison in prose, and each prose message drifts from
the code it describes.

Second, the failure carries a string. The runner cannot show a diff, cannot
point at the failing line, and cannot hand an editor a location. The
[ERROR-HANDLING.md](ERROR-HANDLING.md#assertion-helpers) sketch of `try
assert_eq(a, b)` helpers would render both values but still no expression
text and no line, because without macros a library function cannot see
either.

Third, the harness runs every test in one process. A crash ends the run, a
hang hits one global 60 second timeout, and per-test output is lost. The
harness is also LLVM-only, so every run pays a native compile and link
even though `koja run` starts on the interpreter, and the interpreter
cannot run a project with its own externs anyway.

Koja can fix all three because the compiler owns tests. It holds the span
and the source text at parse time, it owns the error channel that carries
the failure, and the runtime already has process isolation, monitors, and
timers. Languages that bolted testing on later, or that have no macros, could
not make these choices. Koja can, and the shape below spends that freedom on
the failure output and the runtime, not on novel syntax. Most Koja tests are
written by agents trained on existing conventions, so the syntax reads like
Zig and pytest on purpose.

## Surface

### The `test` declaration

```koja
test "description"
  statements
end
```

- `test` is a keyword. The description is a string literal and is required.
- A test may appear at the top level of a file or as a member wherever a
  `fn` gets a concrete owner type: a `struct`, `enum`, `impl`, `extend`, or
  `builtin` body. A member test becomes a method on that type, so it can
  reach the type's `priv` members. A `protocol` body rejects `test`, since
  a protocol has no concrete type to call. Its tests belong in the `impl`
  block of a conforming type.
- The body has no parameters and no `self`. The success type is `()` and
  the error channel is `Test.Failure`. There is no `! E` form. A domain
  error enters the channel through `try Test.require(...)`, which renders it
  through `Debug`, so the channel type is the same for every test and the
  runner needs no per-test adaptation.
- Two tests in one scope may share a description. The runner disambiguates
  by line.
- `test` declarations are stripped before typecheck whenever the project
  loads without tests (`koja build`, `koja run`, `koja doc`). They may live
  in `src/` and never reach a build.

Helpers stay ordinary functions. A helper that asserts declares
`! Test.Failure` and is called with `try`:

```koja
struct RouterTest
  priv fn assert_route(path: String, expected: String) ! Test.Failure
    assert Router.match(path) == expected, "route for #{path}"
  end

  test "matches static paths"
    try RouterTest.assert_route("/health", "health")
    try RouterTest.assert_route("/", "root")
  end
end
```

### The `assert` statement

```koja
assert cond
assert cond, "message"
```

- `assert` is a keyword and is valid only in statement position, like
  `fail`. Embedding it in an expression is a parse error.
- The statement type checks only when `Test.Failure` is on the enclosing
  channel. Otherwise the compiler reports an error and a hint:

  ```
  error: `assert` needs `Test.Failure` on the error channel
  help: use it inside a `test` body or a helper that declares `! Test.Failure`
  ```

- When `cond` is false, the statement returns
  `Result.Err(Test.Failure.Assertion(...))` from the enclosing function.
  Assertions are hard: the first failure ends the test. There is no soft
  form that continues.
- The optional message is any `String` expression. It is evaluated only on
  failure.

The compiler recognizes six comparison forms and renders both sides:

| Form            | `left`       | `right`      |
| --------------- | ------------ | ------------ |
| `assert a == b` | `a.format()` | `b.format()` |
| `assert a != b` | `a.format()` | `b.format()` |
| `assert a < b`  | `a.format()` | `b.format()` |
| `assert a <= b` | `a.format()` | `b.format()` |
| `assert a > b`  | `a.format()` | `b.format()` |
| `assert a >= b` | `a.format()` | `b.format()` |

Both operands are bound to locals once, so a side effect runs one time and
the rendered values are the compared values. Rendering uses the same
synthesized `.format()` call that string interpolation uses. `Debug` is a
universal protocol, so the forms add no bound beyond what `==` already
requires. Any other expression records only its source text, and `left` and
`right` are `Option.None`.

A pass after parsing captures the source text of `cond` and the full
source line that holds it, by slicing the file at the node's span. The
parser sees tokens only and the typecheck phase does not keep source, and
the test binary has no file access at run time, so this is the one point
where both the AST and the text are in hand. `file` and `line` come from
the span as well.

### The `Test` package

`lib/test` is a stdlib package named `Test`. Like every stdlib package
other than `Global`, its names are qualified: `Test.Failure`,
`Test.require`. A file that uses them often can `alias` them.
The project loader links it only when it loads with tests included, which
today means `koja test` and `koja check`. `koja build` and `koja run` never
see it. This is the whole enforcement: application code cannot name
`Test.Failure`, so it cannot put it on a channel, so `assert` does not type
check there. `Kernel.panic` remains the crash verb for application code.

```koja
enum Failure
  Assertion(Assertion)   # from assert
  Error(String)          # a domain error or a message
  Skipped(String)        # from Test.skip
end

struct Assertion
  expression: String
  source_line: String
  file: String
  line: Int
  left: Option<String>
  right: Option<String>
  message: Option<String>
end

fn require<T, E: Debug>(outcome: Result<T, E>) -> T ! Failure
fn skip(reason: String) ! Failure
fn crashes<R>(body: fn () -> R) -> Bool
```

`Failure` is the only type on a test's channel, so one enum describes every
way a test can end short of a crash, and the runner matches on it.

`require` is for setup, and its name says so. A test calls it with `try`
on a step that must succeed before the claim under test, and it returns the
unwrapped value. It takes a `Result`, and a domain error becomes `Error`
with its `Debug` rendering:

```koja
test "decodes a payload"
  conn = try Test.require(connect_trust())
  row = try Test.require(conn.query_one("SELECT 1"))
  assert row.cell(0) == Option.Some("1")
end
```

There is no `Option` form. Same-name functions at distinct arities are
deferred (see MISC.md), and an `Option` in a test is one of three things
with a home already. A claim about it is an `assert`, as above. A gate on
it is `Test.skip`. A fixture bug is `unwrap`, which panics with the line
under process isolation.

The name is deliberate. An earlier draft called this `Test.ok`, and
`try Test.ok(connect_trust())` read as the assertion under test when it was
the precondition. Swift Testing's `try #require(...)` is the same verb for
the same job.

There is no extractor for the expected-failure pattern, because `Result`
already answers the question as a value and `assert` renders the answer:

```koja
test "rejects malformed input"
  assert Base.decode64("Zm9vYg").err() == Option.Some(Base.Error.Truncated)
end
```

This needs `Equality` on `E` only, not on `T`. `assert result.err?()` is
the form when the variant does not matter.

`crashes` is the one claim that needs a closure, because a panic is not a
value. It runs the body in a child process, monitors it, and returns `true`
on an abnormal exit:

```koja
test "rejects an index past the end"
  assert Test.crashes(fn () list.get(5).unwrap() end)
end
```

A body that returns is `false`. A body that returns a `Result.Err` is also
`false`, because an error on the channel is not a crash, and tests keep the
two channels as distinct as application code does. A body that hangs is
bounded by the case deadline, since the runner kills the case's process
tree, so `crashes` has no timeout of its own. Nothing in the stdlib
asserts that a function panics today, because nothing could observe one.

`Failure` conforms to `StringLiteral` with `from_string` producing `Error`,
so a string literal after `fail` converts through the ordinary contextual
literal rule and the unconditional form keeps its current spelling:

```koja
match shape
  Shape.Circle(_) -> ()
  _ -> fail "unexpected variant #{shape}"
end
```

Interpolated strings use the same protocol. A `String` variable does not
convert, which is the protocol's normal rule, and `fail Test.Failure.Error(message)`
is the explicit form for that rare case. No compiler rule is specific to
tests here.

`require` carries no caller line until a caller-location intrinsic exists.
Its failure names the value or the label, and the runner names the test.
`Failure` conforms to `Debug` so a helper that renders it by hand still
works.

### Skipping

There is no static skip. A test that is marked to never run is code that
never runs, so the only skip is a call from the body, in the Go and Zig
style, whose condition is re-evaluated on every run:

```koja
test "round-trips through a live database"
  if System.get_env("DATABASE_URL") == Option.None
    try Test.skip("DATABASE_URL is not set")
  end
  ...
end
```

A known failure is `try Test.skip("#214: returns the wrong sign")` as the
first line, with the issue in the reason. Every summary lists it, which is
what keeps it from being forgotten. There is no `pending` form that runs
the body and inverts the verdict: that is a modifier on a case, not a way
for a case to end, and the design has no modifier mechanism on purpose.
`Skipped` is a variant of `Failure` and needs no second channel type and
no message to the runner.

Selection is by structure, not by tags. `--filter` on the struct path and a
second directory in the manifest cover "run the fast ones" and "run the
ones that need Postgres" with no parallel classification to misspell.

### Grouping

The struct is the suite. Nested types, which Koja has had since 0.16, give
the hierarchy:

```koja
struct ParserTest
  struct Literals
    test "parses a decimal integer"
      assert parse("42") == Literal.Int(42)
    end
  end

  struct Strings
    test "keeps interpolation parts in order"
      ...
    end
  end
end
```

The runner names a test by its owner and description:
`ParserTest.Literals` then `parses a decimal integer`. A struct, enum, or
builtin test groups under the type path, an `impl` test under
`Type: Protocol`, an `extend` test under the target type, and a top-level
test under its file path. This reuses a namespace unit the language
already has, keeps discovery static, and reads like Swift Testing suites to
a model. A `describe "text"` keyword would add only a sentence in place of a
PascalCase group name. It stays deferred until nested structs have been used
enough to know whether that matters.

## Runner

### Discovery

`koja test` parses `src` and `test`, then collects every `test` item at the
top level and inside structs, recursively through nested structs. Discovery
is static, which the LSP code lens and the filters depend on. During 0.19
the runner also discovers `@test` functions so both forms run together.

Filters, which ship on a branch after phase 3:

- `--filter <text>` keeps tests whose description or struct path contains
  the text.
- `--only <path>:<line>` runs the one test declared at that location. The
  LSP run-test code lens uses this.

A filter that matches no test exits 1 with a message, so a stale code lens
location or a mistyped path is not a silent pass. A project with no tests
at all still prints `no tests found` and exits 0.

### The `Test` package is the runner

The compiler owns discovery and the `Test` package owns execution and
reporting. The generated harness is a registration list and nothing else.
It is an entry process, because `Process.monitor` is only callable from a
`Process` whose message type includes `Process.ExitSignal`:

```koja
struct KojaTestHarness
end

impl Process<(), Process.ExitSignal, ()> for KojaTestHarness
  fn run(self) -> Process.StopReason
    specs: List<Test.Spec> = [
      Test.Spec{
        description: "pops in reverse order",
        file: "test/stack_test.koja",
        group: "StackTest",
        line: 14,
        run: &StackTest.__test_stack_test_14/0,
      },
    ]
    runner = Test.run(Test.Plan{specs: specs}, Test.Options.from_env())
    Process.monitor(runner.pid())

    receive
      envelope: (Process.ExitSignal, Option<ReplyTo<()>>) ->
        (signal, _) = envelope

        match signal.reason
          Process.ExitReason.Normal -> Process.StopReason.Normal
          _ -> Process.StopReason.Shutdown
        end
    end
  end
end
```

One registered test is a `Test.Spec`, not a `Test.Case`, because
`Global.Case` is reserved for string casing. `Test.Spec.run` is
`fn () -> Result<(), Test.Failure>` for every spec, because every test
body has that exact channel and function types carry no `!`. The harness
emits a plain function reference for a `test` block. For an `@test`
function on another error type it emits a closure that maps `Err(e)` to
`Test.Failure.Error` with `e` interpolated, and that adapter leaves with
`@test` in 0.20.

`Test.run` spawns a `Test.Runner` for the reporter the options name and
returns its handle. The runner is a real Koja program that uses processes,
monitors, `receive ... after`, function references, and `JSON.encode`. It
ships to every user, which makes it the most exercised Koja library in
existence. That is the point: the roadmap says real applications validate
the language, and this one cannot be skipped.

The driver keeps compile, exec, and the exit status. It resolves the flags
and the diagnostics style, hands them to the program as `KOJA_TEST_*`
environment variables, and `Test.Options.from_env()` reads them through
`System.get_env`. On the interpreter the driver sets them in its own
process before the program starts. No argument parsing happens in Koja.

### Interpreter by default

`koja test` runs on the interpreter. `--backend llvm` compiles and links a
native binary instead, the way `koja run` already works. The interpreter
run starts in well under a second, which is what the run-test code lens
and an agent loop need, and the native run is the release check. The CI
recipes run both, so every test suite doubles as a parity check between
the two backends, an invariant the language suite fixtures check today on
their own.

The driver settles the backend for `koja test` the way it does for
`koja run`. After lowering it resolves every `@extern "C"` the program
declares. A symbol with a hand-written shim in the interpreter uses the
shim. Any other symbol is looked up through the dynamic loader, first in
the `@link` library as a shared library under the project root, then on
the loader's own search path, then in the running process, which covers
libc and libm. When every symbol resolves the run is interpreted and each
foreign call goes through libffi with the declared signature. When one
does not resolve, a bare `koja test` compiles through LLVM instead, and
`--backend interpreter` lists every unresolved symbol and exits. The
extern surface is explicit-width primitives, `Bool`, `CPtr<T>`, and `()`,
with no structs by value, callbacks, or variadics, so libffi's primitive
types cover it. The non-finite float check at extern call sites applies on
both backends. The shims for files, sockets, and TLS stay as overrides
keyed by symbol, because they park the calling process on the cooperative
reactor instead of blocking. A plain C call that blocks still blocks the
interpreter's scheduler, the same way it blocks one worker natively.

The one shape that still compiles is a project whose `@link` library
exists only as a static archive, since the loader cannot open a `.a`.
Building the library as a shared library next to it is enough to run the
tests on the interpreter.

### One process per test

`Test.Runner` spawns each spec in a `Test.SpecProcess` and monitors it.
The spec process runs the body and casts `Test.SpecDone{result}` back to
the runner, so the runner's message type is
`Test.SpecDone | Process.ExitSignal`.

```koja
enum Outcome
  Crashed(String, Duration)        # the crash reason, then the elapsed time
  Failed(Test.Failure, Duration)   # Assertion, Error, or Skipped
  Passed(Duration)
  TimedOut(Duration)               # the deadline that was missed
end
```

- A `SpecDone` with `Result.Ok` is `Passed`.
- A `SpecDone` with `Result.Err(failure)` is `Failed` with the variant.
  The runner reads `Skipped` out of it for the summary count and the exit
  status.
- An `ExitSignal` before any `SpecDone` is `Crashed`. `ExitReason.Crashed`
  supplies the panic message and, when the runtime has one, the
  backtrace. The interpreter's backtrace is empty. A `Killed` or
  `Shutdown` exit before the result is `Crashed` with a fixed reason. The
  runner survives and continues.
- Each spec has a deadline, 60 seconds by default and settable with
  `--timeout <ms>`. The runner waits with `receive ... after`, kills a
  spec that misses its deadline, and reports `TimedOut`. There is no
  global timeout, and `--trace` no longer changes any timeout.
- The spec process is the parent of everything the body spawns, so the
  kill cascade tears the tree down when the spec ends. The runner waits
  for the spec's own `ExitSignal` before it starts the next spec, so a
  registered name from one test cannot collide with the next.

Specs run one at a time in 0.19. Parallel execution is deferred until the
process-per-test model has run for a while. Elapsed time comes from
`Instant` and travels as a `Duration`. The human reporters pick the unit
from the size, `452µs`, `3ms`, or `1.2s`, and the `json` reporter emits
integer microseconds.

### Panics in tests

A panic in a test body is `Crashed`, one red block with the panic message
and the run continues. That changes what counts as good style in a test.
`value = option.unwrap()` is not a smell when each test is its own process:
the failure is contained and the message names the cause. The crash line
waits on a caller-location intrinsic, and so does `try Test.require(...)`,
since `require` carries no line either. Use `require` when the domain
error's rendering matters, and `unwrap` when the panic message does.
`Test.crashes` is the same isolation applied on purpose: it is the one
place a test wants the panic.

### Reporters

Every runner that lasted solved output the same way: one event stream and
pluggable formatters. ExUnit formatters receive `test_finished` messages,
Go emits `-json` for `test2json` to reshape, and Rust's libtest has
`--format json` and JUnit behind a flag. Koja does the same with a
protocol. Reporters are values, so they thread through the run the way a
connection does:

```koja
protocol Reporter
  fn started(self, plan: Test.Plan) -> Self
  fn spec_started(self, spec: Test.Spec) -> Self
  fn spec_finished(self, spec: Test.Spec, outcome: Test.Outcome) -> Self
  fn finished(self, summary: Test.Summary) -> Self
end
```

`Test.Runner<R: Reporter>` is generic over the reporter, so each built-in
reporter is a separate monomorphization and no dispatch enum sits between
the runner and the protocol.

Three reporters ship in 0.19, selected with `--reporter <name>`:

- `dots` is the default: one dot or `X` per spec, `s` for a skip, a
  failures block, and a summary line. This is what a developer expects
  from `koja test`.
- `trace` prints a group header and one line per spec with its location
  and elapsed time. `--trace` is an alias for `--reporter trace`. This is
  what a developer reaches for when something is confusing.
- `json` writes one event per line. This is what CI, the LSP code lens,
  and agents consume:

```
{"event":"started","total":13}
{"event":"spec_started","id":"test/stack_test.koja:14","group":"StackTest","description":"pops in reverse order"}
{"event":"spec_finished","id":"test/stack_test.koja:14","outcome":"passed","microseconds":3120}
{"event":"spec_finished","id":"test/stack_test.koja:22","outcome":"failed","microseconds":1045,"failure":{"kind":"assertion","expression":"popped == 3","file":"test/stack_test.koja","line":23,"column":12,"source_line":"    assert popped == 3","left":"2","right":"3","message":null}}
{"event":"spec_finished","id":"test/stack_test.koja:30","outcome":"crashed","microseconds":12400,"reason":"index 5 out of bounds"}
{"event":"spec_finished","id":"test/stack_test.koja:38","outcome":"skipped","microseconds":18,"reason":"DATABASE_URL is not set"}
{"event":"spec_finished","id":"test/stack_test.koja:46","outcome":"timed_out","microseconds":60000000}
{"event":"finished","passed":10,"failed":2,"skipped":1,"crashed":0,"timed_out":0,"microseconds":75211}
```

`microseconds` is an integer so the value survives every JSON parser
exactly. Go and cargo emit float seconds and the JavaScript runners emit
milliseconds, and both lose digits on the way through. For a `timed_out`
spec it is the deadline. The `failure` object carries a `kind` of `assertion` or `error`. `Skipped`
is its own `outcome` value with a `reason`, because CI counts skips apart
from failures.

Machine reporters write to stderr by default, or to a file with
`--out <path>`, so a test that calls `IO.puts` cannot corrupt the stream.
Human reporters write to stdout. The `json` event shapes are part of the
`Test` package's public surface and follow its versioning.

A user reporter is any type that conforms to `Test.Reporter`. On a branch
after phase 3, `--reporter MyPkg.Junit` makes the generated harness name
that type, and the compiler checks the conformance. That is a plugin
system with no dynamic loading, and a JUnit XML reporter is its first
proof.

### Output style

The `dots` and `trace` reporters follow the compiler's diagnostics style so
a failing test and a compile error look like one tool. The driver resolves
the style the same way it does for diagnostics, the `--diagnostics` flag,
then `KOJA_DIAGNOSTICS`, then whether stderr is a terminal, plus
`--no-color` and `NO_COLOR`, and passes the result to the binary. The two
renderers share a visual specification, not code. The drift risk is
accepted, because the harness already printed dots from Koja before this
design.

Pretty draws a source snippet at the assertion line from
`Test.Assertion.source_line`, so no source file access is needed at run
time. The glyphs are the ones `koja check` draws. The expression is
underlined, both operands are labels, and the message is a help row:

```
failure: assertion failed
   ╭─ test/stack_test.koja:23:12
   │
23 │     assert popped == 3
   │            ──────────
   │            left:  2
   │            right: 3
   = help: pops the most recent value
```

Short prints one line per failure for pipes, editors, and agents:

```
test/stack_test.koja:23:12: failure: assert popped == 3 (left: 2, right: 3)
```

In short style `dots` prints no dots, only the failure lines and the
summary, which matches `koja check` printing nothing when nothing is wrong.
`trace` in short style prints one line per spec with its location and
result, which is a complete greppable log:

```
test/stack_test.koja:14: ok: pops in reverse order (3ms)
test/stack_test.koja:23:12: failure: assert popped == 3 (left: 2, right: 3)
test/stack_test.koja:30: crash: index 5 out of bounds
test/stack_test.koja:46: timeout: no result after 60.0s
```

An `Error` failure, a crash, and a timeout print a severity line and a
location row with no snippet, and a crash adds its backtrace under the
gutter when the runtime has one. A skipped test prints one line with its
reason under a `Skipped:` header, not in the failures block. Both human
reporters end with the summary line, `12 successful tests. 0 failures.`,
with crashed, timed out, and skipped counts appended only when non-zero.

The exit status is 1 when any spec fails, crashes, or times out, and 0
otherwise. Skipped tests do not affect it.

## Compatibility and migration

- `@test "description"` on a `fn ... ! String` keeps working through 0.19.
  The compiler reports a deprecation warning at the annotation, with a hint
  that names the `test` declaration. The warning landed after the stdlib
  migrated, so a clean stdlib build carries no warnings. The annotation and
  its runner path are removed in 0.20.
- Old and new tests coexist in one run and one struct.
- No formatter rewrite and no migration command. Flattening a wrapper struct
  into `test` blocks changes name resolution for helpers, which is not a
  trivia change, and turning `if x != y` then `fail` into `assert x == y`
  loses or reshapes the message. The stdlib's tests, the examples, and the
  `koja new` scaffold migrated by hand in the 0.19 cycle.
- `test` and `assert` are keywords. Neither was used as an identifier in
  the stdlib or the language suite. The annotation parser keeps accepting
  `@test` while the token exists.

## Tooling

The following surfaces change with the keywords. Later implementation plans
must cover each one.

- Lexer: `test` and `assert` tokens.
- Parser: `Item::Test` and struct-member tests, `ExprKind::Assert`, `@test`
  still parsed as an annotation. The parser slices the expression text and
  source line from its own source while it builds the node.
- Typecheck: channel construction for `test`, `assert` desugaring beside
  `fail` in the error channel resolver, the `@test` deprecation warning,
  `test` items stripped when tests are not loaded, completion `KEYWORDS`.
- Driver: `koja test` generates the registration harness, settles the
  backend from `--backend`, resolves `--reporter`, `--trace`, `--timeout`,
  `--out`, and the diagnostics style, and passes them to the program as
  `KOJA_TEST_*` environment. `--filter` and `--only` follow phase 3.
- Formatter: printing for both constructs.
- LSP: folding, document symbols, traversal, and later the run-test code
  lens and failures as diagnostics. The LSP project loader today bundles
  `src/` and the open file. It must link `Test` and include the other files
  under the test directories when the open file is a test, or every
  `assert` in the editor is a red squiggle.
- `koja doc` highlighter keyword list. `koja shell` block-depth counter,
  since `test` opens an `end` block.
- `grammar.ebnf`, `LANGUAGE.md`, `CHANGELOG.md`, the `koja new` scaffold.
- Sibling repos after the release: tree-sitter-koja, zed-koja, vscode-koja,
  vim-koja, and the kojalang.org lexer.

## Rejected alternatives

**Library helpers only.** `try assert_eq(a, b)` functions in a `Test`
package need no language change and were the first design. Without macros
they cannot see the source expression or the line, so every failure reads
"expected 3, found 2" with no pointer to the code. Tests are a large share
of what agents write and read, and the failure text is what steers them.
The keyword is the one place where the no-macros rule costs something, and
it pays it back.

**`assert` anywhere as a crash.** D, Python, and Rust allow `assert` in
application code as a crash. Koja already has `Kernel.panic` for that, and
a word that means "crash" in one file and "test failure" in another is two
meanings for one name. Typed `try` validators are the production tool.
Tying `assert` to `Test.Failure` on the channel makes the restriction fall
out of the type system with no special rule.

**`Test.Failure | E` on the test channel.** An earlier draft let a test
declare `! E` so setup errors could propagate with a bare `try`. Then the
channel type differed per test, `Test.Spec.run` needed a per-test adapter,
the runner needed an `Errored` outcome beside `Failed`, and a skip had no
clean way to travel. One `Test.Failure` enum with an `Error` variant costs
`Test.require(...)` around each setup call and removes all four problems.
An implicit `try` that mapped `E` into `Error` inside test bodies was
rejected as a special rule for one construct.

**A `pending` form.** pytest `xfail` and RSpec `pending` run a known
failure and report an error when it starts passing. In this design that is
a modifier on a case, not a way for a case to end, and the first draft put
it in `Failure` as a variant with a signature that returned before the body
ran. Making it correct needs a closure form and a child process, for the
one benefit of noticing a fix without a human. `Test.skip` with the issue
number in the reason is listed in every summary, which is enough.

**A `describe` keyword now.** Nested structs give the hierarchy today with
no new construct. A `describe "text"` block that desugars to an anonymous
nested struct remains possible later.

**Soft assertions.** Swift `#expect` and Go `t.Errorf` continue after a
failure and collect several per test. That needs a mutable failure list in
the runner and a second verb. Hard assertions match ExUnit and Rust and keep
`assert` a plain early return.

**Matcher DSL.** `expect(x).to eq(y)` vocabularies are large to learn and
fit poorly in a language that values one obvious way. The six comparison
forms plus `not` cover the same ground with operators the reader already
knows.

**Formatter-driven migration.** The formatter's contract is to change trivia
and never the program. Moving functions out of a struct and rewriting
conditionals into assertions violates that contract.

**Tests as runtime values.** Jest, Dart, and RSpec register tests by calling
functions with closures. Discovery then requires executing the file, which
blocks a static code lens and static filters.

**Rendering in the driver.** An earlier draft had the harness emit events
on stdout and the Rust driver parse them and render through
`diagnostics.rs`, so failures and compile errors shared one renderer and
the driver's source table drew the snippets. That put output logic in two
languages anyway, since the harness already printed dots, and it made the
driver classify every stdout line as event or user output. Capturing
`source_line` at parse time removes the need for the source table, and a
`Reporter` protocol in Koja gives CI its `json` output, gives users an
extension point, and makes the runner a real Koja program. The shared
visual style is kept as a specification.

## Deferred

- **Per-test suite instances.** A struct whose fields all have defaults can
  be constructed fresh before each test, and the body gets `self`. Field
  defaults are re-evaluated at each construction, so fields become per-test
  setup with no new syntax. `self` is reserved in test bodies for this.
- **`describe` sugar** over anonymous nested structs.
- **Parallel tests.** The process model makes it natural. It waits on
  experience with process-per-test.
- **Doctests.** Compile and run the `koja` code blocks in `@doc` strings as
  tests.
- **Caller-location intrinsic**, so `require` and helpers can report the
  caller's line the way Go `t.Helper()` and Rust `#[track_caller]` do.
- **Per-test deadlines.** A call from the body, in the style of
  `Test.skip`, that tells the runner to wait longer than `--timeout` for
  this one case. Unlike a skip it has to reach the runner before the body
  ends, so it needs a message rather than a `Failure` variant.
- **Tags.** Selection by struct path and directory is the 0.19 answer. A
  tag system is a parallel classification to misspell and is not planned.
- **Suite-level setup.** Nothing runs once before a struct's tests. A
  database created per test is slow, and a fixed helper that every test
  calls is the workaround. A struct-level `before` hook waits on
  experience with per-test suite instances.
- **Operand truncation.** A `Debug` rendering of a large list or map fills
  the terminal. Reporters could elide the middle above a size and print
  the full value with `--trace`. The rendering is captured whole in
  `Test.Assertion` either way.
- **A JUnit XML reporter**, as the fourth built-in or the first package
  reporter. It is an afternoon once `json` exists, and it proves the
  `--reporter MyPkg.Type` extension point.
- **Per-test output capture.** Attributing `IO.puts` output to the running
  case needs the runtime to redirect a process's output. Until then human
  reporters interleave user output and machine reporters avoid stdout.
- **Coverage.** `koja test --coverage` instruments the sealed IR, not LLVM.
  Lowering inserts a relaxed atomic counter increment at the start of each
  block in the project's `src/` files and keeps a table from counter to
  span. Spans marked synthetic are excluded, so derived impls, desugared
  loops, and `assert` expansions never appear as uncovered lines, which is
  the property LLVM's own instrumentation cannot give. Match arms are
  blocks, so arm coverage is free. After the last case `Test.Runner` calls
  a `Runtime.coverage_snapshot()` intrinsic, fills `Summary.coverage`, and
  writes `build/coverage/lcov.info`. Reporters need no new method, `json`
  carries the numbers for CI, and `--coverage-min <percent>` is the CI
  gate. The counter-to-span table is a static fact about the program and
  belongs in the query crate planned for the LSP, so the report, the
  editor gutter, and any HTML view agree on what counts as executable. The
  counts are run data and stay outside that crate. Depends on phase 3.

## Phases

Five phases, on four branches. Phases 1 and 2 share a branch because
`assert` is usable from `@test` on the day it lands and `test` is a
desugaring on top of it, so the old harness carries both. Phase 3 is its
own branch because it is the largest piece and the one most likely to
raise runtime questions. Phase 4 is its own branch because it touches the
interpreter's extern dispatch and the driver's backend selection and
nothing in the test surface. Phase 5 is one pull
request per package after the surface is on `main`. Each phase is one or
more commits at its boundary, so a bisect can name the phase.

1. **`Test` package and `assert`.** `lib/test` with `Test.Failure`,
   `Test.Assertion`, `require`, `skip`, `crashes`, and the
   `StringLiteral` and `Debug` conformances. The loader links `Test` only
   when `include_tests` is set: today every qualified stdlib package is
   linked into every build, so this is a filter with a test that
   `koja build` cannot name `Test.Failure`. The `assert` keyword, the
   parser node with `expression` and `source_line` sliced from the
   parser's own source, the error-channel resolver arm beside `fail`, the
   formatter, and LSP traversal. The existing harness
   accepts `@test fn ... ! Test.Failure` and renders the failure through
   `Debug`. About 700 lines of Rust and 200 of Koja.
2. **`test` declaration.** The `test` keyword, `Item::Test` at the top
   level and as a member of `struct`, `enum`, `impl`, `extend`, and
   `builtin` bodies, and a pass before `collect` that desugars each test
   into a synthesized `fn __test_<file stem>_<line> ! Test.Failure` or
   strips it when tests are not loaded, so typecheck, IR, and both
   backends never learn a new item kind. The stem keeps two files with a
   test on the same line from colliding. Top-level tests are
   package-private and member tests are public, because `priv` on a
   method is type-private and the harness lives outside the type. The
   protocol-member check in an `impl` block skips desugared tests, since
   they exist only under `koja test` and widen nothing. Discovery in
   `koja-test` walks every body through nested types and groups `impl`
   tests as `Type: Protocol`. The formatter, LSP symbols and folding, the
   shell block-depth counter, and the three keyword tables. About 700
   lines of Rust.
3. **Runner and reporters.** `Test.Runner`, `Spec`, `Plan`, `Summary`,
   `Outcome`, `Options.from_env`, the `Reporter` protocol, and the `dots`,
   `trace`, and `json` reporters in both output styles. The runner is a
   `Process` whose `M` includes `Process.ExitSignal`, and
   `ExitReason.Crashed(CrashInfo)` supplies the crash message. The
   generated harness shrinks to the registration list, the driver passes
   flags and the diagnostics style as environment, and `koja test` gains
   `--backend`, `--reporter`, `--timeout`, and `--out`. The two checks
   resolved as yes: a parent's exit force-kills its children
   transitively, and the test binary inherits stderr so the `json`
   stream passes through the exec path unchanged. Follow-up branch:
   `--filter`, `--only`, and the `--reporter MyPkg.Type` extension point.
   About 1,200 lines of Koja and 250 of Rust.
4. **Interpreter C FFI.** A `dlopen` per `@link` library and a libffi call
   for the extern surface, with the hand-written shims kept as overrides
   by symbol. The `libffi` crate links the system libffi, which the
   `koja` binary already loads through LLVM on every supported platform,
   so the release build did not change. The driver resolves every extern
   after lowering and settles the backend on the result, so `koja run` and
   `koja test` interpret an FFI project whose libraries are shared
   libraries and compile one that ships only a static archive. The
   language suite runs the FFI fixtures on both backends. Independent of
   the first three phases. About 500 lines of Rust.
5. **Migration.** Done. The stdlib's tests and the `koja new` scaffold
   moved to `test` and `assert`, each package run on both backends with
   its test count unchanged. Migrating the stdlib exposed one compiler
   gap: `assert` bound its operands before the resolver saw the `==`, so
   the right operand lost the left's type. The desugaring now resolves
   the left operand first and hands its type to the right. Then the
   `@test` deprecation warning, `LANGUAGE.md`, and `CHANGELOG.md`.
   `grammar.ebnf` already carried both constructs. The examples and the
   sibling repos follow after 0.19 ships.
