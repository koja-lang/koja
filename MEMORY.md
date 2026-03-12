# Expo Memory Strategy

## Three layers

### 1. Stack (automatic)

Primitives (`u8`..`u64`, `i8`..`i64`, `f32`, `f64`, `bool`), small fixed-size
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
- If you need to return data, return owned values. Clone where Rust would use a
  lifetime.
- No `Box`, `Rc`, `Arc` in user code. The compiler handles heap placement.
- No `mut` keyword: if you own a value, you can mutate it. Borrows are always
  read-only.

**Typical patterns in this repo:**

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

The arena is used in this repo for:

- `delete_expired_sessions` / `delete_expired_api_keys` -- phase 1 collects
  expiry index entries, phase 2 processes them. The collection is temporary.

## What this means in practice

Most code in this repo uses plain ownership + borrow. You write normal code,
pass values to functions (borrowed by default), clone when you need a copy. The
compiler tells you when something needs to be cloned.

When a type needs to express "this contains a reference, not an owned value," use
`ref<T>` syntax. This appears in return types (`-> ref<Database>`) and inside
generics (`Option<ref<String>>`).

Function references use bare names without any sigil. The compiler distinguishes
calls from references by the presence of parentheses: `foo()` calls the function,
`foo` references it. This works because Expo has no function overloading.

The `&` symbol does not exist in Expo.

Arena shows up in database operations where there's a clear "collect then
process" boundary. Framework code (like an HTTP server) might offer implicit
per-request arenas, but that's a framework concern, not a language concern.

There is no garbage collector. There are no lifetime annotations.
