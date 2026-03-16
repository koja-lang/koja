# Expo Memory Strategy

## Three layers

### 1. Stack (automatic)

Primitives (`UInt8`..`UInt64`, `Int8`..`Int`, `Float32`, `Float`, `Bool`), small fixed-size
structs, and temporaries live on the stack. The programmer never thinks about
this -- the compiler decides what fits.

### 2. Ownership + move (the default)

Every heap-allocated value has exactly one owner. When the owner goes out of
scope, the value is dropped (memory freed, file handles closed, etc).

**Rules:**

- Assignment **moves** by default: `b = a` makes `b` the new owner; `a` is no
  longer usable.
- Function parameters **borrow by default** (read-only). Use `move` to take
  ownership explicitly.
- Borrows are scoped to the function call -- no lifetime annotations.
- Borrows are **always read-only**. This is a permanent design commitment, not
  a current limitation. Expo has exactly two access modes: "I own it and can do
  anything" or "I'm borrowing it and can only read." There is no `&mut T` and
  there never will be. Concurrent in-place mutation is handled through
  ownership splitting (`split_owned`) instead of mutable borrows -- see
  `CONCURRENCY.md`.
- If you need to return data, return owned values. Clone where Rust would use a
  lifetime.
- No `Box`, `Rc`, `Arc` in user code. The compiler handles heap placement.
- No `mut` keyword: if you own a value, you can mutate it.

**Typical patterns:**

```
# Borrow -- params borrow by default, no annotation needed
fn get_session(self, token: String) -> Result<Option<SessionToken>, DatabaseError>

# Move -- use `move` keyword when a function takes ownership
fn new(move db: Database) -> AuthStateMachine

# Move -- fields transferred into the new struct, no clone needed
session_token = SessionToken{
  subject_id: req.subject_id,   # moved from req
  metadata: req.metadata,       # moved from req
  ...
}

# Clone -- when you need the data to outlive the borrow
id: s.id.clone()
```

**At concurrency boundaries:**

Ownership rules extend to tasks and actors (see `CONCURRENCY.md` for full
details):

- **Tasks** can borrow from their parent scope. Structured concurrency
  guarantees the data outlives the task, so read-only borrows are safe without
  lifetime annotations.
- **Actors** must move or clone data across their boundary. Actors have
  isolated memory -- no borrowing across actors. Messages transfer ownership
  (zero-copy).
- When an actor crashes, all its owned values are dropped deterministically.
  The supervisor starts a fresh instance with clean state -- no leaked memory,
  no zombie state. This is the same cleanup guarantee Erlang gets from
  per-process heaps, achieved through ownership instead of garbage collection.

### 3. Arena (explicit opt-in)

The `arena...end` block bump-allocates everything inside it and bulk-frees at
block exit. The compiler ensures nothing allocated inside an arena escapes
without being cloned out.

**When to use it:**

- Collecting intermediate results that only live for a phase of work.
- Batch processing with many temporaries and a small output.
- Any "gather then process" pattern where intermediates aren't needed afterward.

**Syntax:**

```
expired = arena
  txn = self.begin_read()?
  table = txn.open_table(SESSION_EXPIRY)
  result = []

  for (key, value) in table.iter()
    match expiry_key_ms(key)
      Some(ms) when ms <= now_ms -> result.push((key.clone(), value.clone()))
      _ -> break
    end
  end

  result  # these cloned strings escape; everything else is freed
end
```

Arenas are useful in database operations and batch processing where there's a
clear "collect then process" boundary. Framework code (like an HTTP server)
might offer implicit per-request arenas, but that's a framework concern, not a
language concern.

## Why this isn't FP (and why that matters)

Expo's ownership model looks superficially like functional programming:
immutable borrows, no `&mut`, values flow through functions. But the key
difference is that **owners can mutate in place**. When you own a struct, you
can write to its fields directly -- no copying, no path-copying, no
persistent data structures.

This avoids the deep-nested-update pain that plagues pure FP languages like
Elixir/Erlang. In Elixir, updating a field 5 levels deep means copying every
intermediate map/struct on the path. In Expo, you move ownership to a helper,
mutate directly, and return the owned value:

```
fn update_deep_config(move config: AppConfig) -> AppConfig
  config.database.pool.max_connections = 50
  config
end

config = update_deep_config(config)  # moved in, mutated, moved back
```

No copies. No lenses. No `put_in(config, [:database, :pool, :max_connections], 50)`.
The compiler tracks ownership so this is safe -- nobody else can read `config`
while the function holds it.

**Concurrent mutation of shared structures** is handled through actors. An
actor owns the data structure and processes mutations sequentially through its
mailbox -- effectively a mutex with better ergonomics and crash isolation. This
is the Erlang/OTP pattern, but with zero-copy message passing (ownership
transfer) instead of deep-copy serialization.

## What this means in practice

Most Expo code uses plain ownership + borrow. You write normal code, pass
values to functions (borrowed by default), clone when you need a copy. The
compiler tells you when something needs to be cloned.

When a type needs to express "this contains a reference, not an owned value,"
use `ref T` syntax. This appears in return types (`-> ref Database`) and
inside generics (`Option<ref String>`).

Function references use bare names without any sigil. The compiler
distinguishes calls from references by the presence of parentheses: `foo()`
calls the function, `foo` references it. This works because Expo has no
function overloading.

The `&` symbol does not exist in Expo.

There is no garbage collector. There are no lifetime annotations.

---

See `CONCURRENCY.md` for how ownership interacts with tasks, actors, message
passing, and crash recovery.
