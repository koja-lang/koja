# Union Types

Union types (`A | B`) are anonymous enums -- tagged union values that can hold
any one of their constituent types. They are a general-purpose type system
feature used for process mailbox typing, heterogeneous collections, error type
composition, and function parameters that accept related but distinct types.

---

## Anonymous unions

`A | B` is a composite type expression, usable anywhere a type is expected:
variable annotations, function parameters, return types, generic type arguments.

```
x: Post | Comment | Ad = get_feed_item()

fn render(content: Text | Image | Video) -> Html
  match content
    Text(t) -> render_text(t)
    Image(i) -> render_image(i)
    Video(v) -> render_video(v)
  end
end

fn get_item(id: Int) -> Post | Comment
  if is_post?(id) then Post.load(id) else Comment.load(id) end
end
```

Anonymous unions cannot have protocol implementations. They are composite types,
not base types. To use a protocol method, match to decompose into a concrete
type first:

```
item: Post | Comment = get_item(id)

# Compile error -- no Display impl for Post | Comment
# print(item.display())

# Match to decompose, then dispatch on the concrete type
match item
  Post(p) -> print(p.display())
  Comment(c) -> print(c.display())
end
```

---

## Named unions

The `type` keyword creates a named alias for a composite:

```
type FeedItem = Post | Comment | Ad
```

A named union is structurally identical to the anonymous version -- `FeedItem`
and `Post | Comment | Ad` are the same type. The name provides three things:

1. **A handle for protocol impls.** Named unions can have explicit `impl` blocks:

```
impl Display for FeedItem
  fn display(self) -> String
    match self
      Post(p) -> p.display()
      Comment(c) -> c.display()
      Ad(a) -> a.display()
    end
  end
end
```

2. **A documentation site.** `@doc` annotations on the `type` declaration.

3. **Clearer error messages.** "Expected `FeedItem`, got `String`" is more
   readable than "expected `Post | Comment | Ad`, got `String`."

### Name collision rule

Two aliases with the same name in scope must refer to the same composite.
If they don't, it's a compile error:

```
# module_a.expo
type FeedItem = Post | Comment | Ad

# module_b.expo
type FeedItem = Post | Comment | Ad    # same composite -- OK

# module_c.expo
type FeedItem = Post | Comment         # different composite -- compile error
```

---

## Explicit only, never inferred

Union types appear only at explicit annotation sites. The compiler never infers
a union type on its own. If branches return different types and no union type
is annotated, it's a type error:

```
# Compile error: branches return different types (Post vs Comment)
fn get_item(id: Int)
  if is_post?(id) then Post.load(id) else Comment.load(id) end
end

# Fix: add an explicit return type annotation
fn get_item(id: Int) -> Post | Comment
  if is_post?(id) then Post.load(id) else Comment.load(id) end
end
```

### Widening coercion

When the target type is a union and the source type is one of its constituents,
the compiler inserts a widening coercion automatically. This uses the same
mechanism as numeric coercion (`x: UInt8 = 4`):

```
x: Post | Comment | Ad = Post.new("hello")  # Post widens to Post | Comment | Ad

fn process(item: Post | Comment | Ad)
  ...
end

process(Post.new("hello"))  # Post widens at the call boundary
```

Widening only happens at annotated boundaries -- assignments, parameter passing,
and return expressions where the target type is known. The inference engine
never introduces subtyping. Widening is a directional coercion: `A` coerces to
`A | B`, not the other way around.

---

## Properties

- **Order-independent.** `A | B` and `B | A` are the same type.
- **Flattened.** `(A | B) | C` normalizes to `A | B | C`.
- **Deduplicated.** `A | A | B` normalizes to `A | B`.

These properties are enforced by canonicalizing the union into a sorted,
deduplicated set of constituent type IDs at the point of construction.

---

## Pattern matching and exhaustiveness

### Typed binding patterns

The `name: Type` syntax matches a union member by type and binds the unwrapped
value. The `:` is consistent with its meaning everywhere else in Expo --
"has type" -- in variable annotations (`x: Int`), function params (`n: Int`),
and future binary segments (`<<len: Int(16)>>`).

```
item: Post | Comment | Ad = get_item(id)

match item
  p: Post -> render_post(p)        # p has type Post
  c: Comment -> render_comment(c)  # c has type Comment
  a: Ad -> render_ad(a)            # a has type Ad
end
```

Match arms are checked against the union's constituent types. Exhaustiveness
uses the same logic as enum variant checking:

```
match item
  p: Post -> render_post(p)
  c: Comment -> render_comment(c)
  # Compile error: non-exhaustive match, missing Ad
end
```

Struct destructuring in match arms (`Post { title, body } -> ...`) is deferred.
With static typing and LSP autocomplete, `p.title` is just as convenient as
destructuring and more readable. Struct destructuring will arrive alongside
irrefutable destructuring (`Config{name, port} = load_config()`).

### Enum constituent unions

When constituent types are enums with their own variants, qualify to
disambiguate:

```expo
item: ServerMsg | LibResult = receive()

match item
  ServerMsg.Shutdown -> stop()
  ServerMsg.Status(s) -> report(s)
  LibResult.Success(data) -> process(data)
  LibResult.Error(e) -> handle_error(e)
end
```

---

## Protocols and composites

Composites are not base types. Protocol implementations are never auto-derived
for union types. If `A` and `B` both implement `Display`, `A | B` does not
automatically implement `Display`.

To attach protocol behavior to a union, use a named union with an explicit
`impl` block:

```expo
type FeedItem = Post | Comment | Ad

impl Display for FeedItem
  fn display(self) -> String
    match self
      Post(p) -> p.display()
      Comment(c) -> c.display()
      Ad(a) -> a.display()
    end
  end
end
```

The developer writes the dispatch logic and owns the `Self` return semantics.
No compiler-synthesized impls, no edge cases with `Self`-returning protocol
methods.

---

## Representation

Union types compile to tagged unions -- the same representation as named enums.
Tag assignment is determined at monomorphization time. Layout is
`{ tag, payload }` where payload size is the maximum of all constituent type
sizes.

For generic usage like `Process<ServerMsg | LibResult>`, the union type is
monomorphized like any other generic type argument. The monomorphized process
gets a mailbox with a tagged union layout for `ServerMsg | LibResult`.

---

## Numeric tower as first dogfood

The numeric tower can be defined in Expo using union types rather than
hardcoded as a compiler primitive:

```
type Int = Int8 | Int16 | Int32 | Int64
type Float = Float32 | Float64
```

The language says "`Int` is a number that could be any of these widths." The
compiler is free to optimize the representation -- widen to the largest variant,
elide the tag, whatever produces the best code. This is an implementation
detail, not a language guarantee.

When a developer needs a specific representation (FFI, binary protocols,
memory-critical inner loops), they reach for the specific type (`Int32`,
`Float64`). These aren't "lower-level" -- they're more specific types that the
language already defines.

This validates that union types are general enough to express the language's own
numeric relationships. It extends naturally to user-defined aliases like
`type SmallInt = Int8 | Int16`.

### Codegen: expected-type threading

For the compiler to "figure it out," codegen needs context. When compiling an
`Int` literal, it needs to know whether the surrounding context expects `Int32`,
`Int64`, or the full union. The mechanism for this is **expected-type threading**:
each expression compilation receives an optional expected type from its parent
(assignment annotation, function parameter type, return type, etc.).

This is the same infrastructure needed to resolve generic enum unit variants
(e.g. `Option.None` inside a method that re-parameterizes the enum). Currently
a targeted `return_type_hint` on the compiler handles the return-position case.
When the numeric tower ships, this should be generalized into a full
`expected: Option<&Type>` parameter on `compile_expr` and its callees, so
codegen can make representation decisions at every expression site.

---

## Implementation: canonical hashmap

The type checker maintains a hashmap of composite types, keyed by sorted,
deduplicated sets of constituent type IDs:

- **Insertion.** When a union type is encountered, its constituents are
  normalized (sorted, deduplicated, flattened) and looked up. If the entry
  exists, it's reused. If not, a new entry is created.
- **Equality.** Same hashmap entry = same type. O(1).
- **Widening check.** `A` widens to `A | B | C` if `{A}` is a subset of
  `{A, B, C}`. Set containment on the keys.
- **Named aliases.** The `type FeedItem = ...` declaration resolves to a
  hashmap entry and attaches the alias name as metadata. If another alias with
  the same name resolves to a different entry, compile error.
- **Exhaustiveness.** The match checker walks the set of constituent types
  from the hashmap entry.

This slots into the existing type interning infrastructure alongside struct
and enum type IDs.

---

## Use cases

**Process mailbox typing.** A process that receives messages from multiple
sources declares a union mailbox type:

```
fn main() : Process<ServerMsg | LibResult>
  lib_handle: Process<LibResult> = self()
  spawn(lib_worker(lib_handle))

  match receive()
    ServerMsg.Shutdown -> stop()
    LibResult.Success(data) -> print(data)
  end
end
```

**Heterogeneous collections.** A list of mixed types for API responses:

```
items: List<Post | Comment | Ad> = load_feed(user_id)
```

**Error type composition.** Combining error types from different sources
without manual wrapper enums:

```
fn create_user(input: Input) -> Result<User, ValidationError | DatabaseError>
  validated = validate(input)?
  save(validated)
end
```

**Function parameters.** Accepting related but distinct types:

```
fn render(content: Text | Image | Video) -> Html
  match content
    Text(t) -> render_text(t)
    Image(i) -> render_image(i)
    Video(v) -> render_video(v)
  end
end
```

---

## Recursive types

Structs and enums may reference themselves, directly or through other types:

```
struct Node
  value: Int
  next: Option<Node>
end

enum Tree
  Leaf(Int)
  Branch(Tree, Tree)
end

struct GNode<T>
  value: T
  next: Option<GNode<T>>
end
```

### No user-facing syntax

The compiler handles recursive types entirely behind the scenes. There is no
`Box`, `indirect`, or `ref` keyword. The developer writes the type as they
think about it; the compiler inserts the necessary indirection.

### Cycle detection

After type resolution and generic re-resolution, a DFS walks the type graph
built from struct fields and enum variant elements. Back edges identify fields
that create cycles. Those fields are wrapped in an internal `Type::Indirect`
marker.

### LLVM representation

`Type::Indirect` maps to an LLVM pointer type. Construction `malloc`s memory
for the inner value and stores through the pointer. Field access and match
payload extraction add an extra pointer load to dereference the indirection.

### Drop semantics

When a value containing `Indirect` fields is dropped, the compiler emits
`free` calls for each heap-allocated indirection before dropping the outer
value. Deep recursive structures free one level of indirection at drop time;
full recursive freeing is a future optimization.

### Monomorphization

`Type::Indirect` is preserved through generic substitution. When monomorphizing
a struct like `GNode<Int>`, the compiler sets the struct body (using pointer
types for `Indirect` fields) before ensuring inner types exist. This breaks
the circular dependency where `GNode<Int>` needs `Option<GNode<Int>>` which
needs `GNode<Int>`'s size.

---

## Open questions

- **Generic interaction with unions.** `List<A | B>` is clear (each element is
  `A` or `B`). Does `List<A> | List<B>` flatten? No -- it's a union of two
  list types, not a list of a union. But the interaction between union types
  and generic type parameter bounds needs exploration once trait bounds ship.

- **`Process<T>` contravariance.** A process that accepts `ServerMsg | LibResult`
  should be usable where `Process<LibResult>` is expected (the sender sends a
  subset of what the process accepts). This is contravariance on `Process<T>`.
  Whether this is a general rule for all generic types or specific to `Process`
  needs to be decided.

- **Union of enums vs flat union.** If `enum Color` has variants `Red`, `Green`,
  `Blue` and `enum Size` has `Small`, `Large` -- is `Color | Size` a union with
  two constituents (each an enum) or five (each a variant)? Two. You match on
  `Color` and `Size`, not on individual variants. The constituents are the types,
  not their internal structure.
