# Union Types

This document states the model for union types in one place. Seven rules
decide how a union behaves, and a case the rules do not predict is a bug.
See [LANGUAGE.md](../LANGUAGE.md) for the syntax and [TYPES.md](TYPES.md) for
how unions fit the rest of the type system.

The examples use these declarations.

```koja
struct Cat
  name: String
end

struct Dog
  name: String
end

type Pet = Cat | Dog
```

## The rules

### 1. A union is a set of types

Order does not matter, equal members collapse, and a union inside a union
flattens. A union of one member is that member.

`Cat | Dog` and `Dog | Cat` are one type. `Cat | Cat` is `Cat`. `Cat | Pet`
is `Cat | Dog`.

The compiler keeps one canonical form for each set, with the members flat,
sorted, and distinct. Two unions are the same type exactly when their
canonical forms are equal.

A union that names a type parameter follows the same rule when the
parameter gets its type. `String | T` is `String` at `T = String`, and it is
`Bool | Int | String` at `T = Bool | Int`. In a generic body, a `match` on
`Int | T` can have the arms `n: Int ->` and `t: T ->`. At `T = Int` the two
arms name the same type, and the first one runs.

### 2. A `type` alias is a name and not a type

`type Pet = Cat | Dog` gives the union a name. It does not make a new type.
A second alias with the same members is the same type, and so is the union
written out by hand.

An alias carries no functions and no conformance of its own. Everything
that is true of `Pet` is true of `Cat | Dog`. Rule 7 is the one exception.

### 3. A value widens and never narrows

A member flows into its union. A union flows into a wider union that holds
every one of its members.

```koja
cat = Cat{name: "Tom"}
pet: Pet = cat
wide: Cat | Dog | String = pet
```

The other direction is always an error. A `Pet` does not flow into a `Cat`,
and a `Cat | Dog | String` does not flow into a `Pet`. A `match` is the
only way to get a member out of a union.

Widening is explicit in the compiled program. The typecheck records it on
the expression, and the backends build the union value from that record. No
backend decides for itself that a value must widen.

### 4. Widening needs a position that expects the union

A value widens only where the surrounding code names the union. These
positions name it.

- A declaration with a type annotation.
- A reassignment of a local that has the union type.
- A write to a field of the union type.
- An element of a tuple literal on the right of a destructuring assignment,
  for a name that has the union type.
- An argument, for a parameter of the union type.
- A returned value, for a function that returns the union.
- A field in a struct literal.
- A payload of an enum variant.
- A constant.
- An element of a tuple, list, or map literal.
- An arm of an `if`, a `cond`, a `match`, a `receive`, or a `?:`.
- An operand of `==` or `!=`, when the other operand is the union.
- A failure, for a function that declares the union as its error type. This
  covers `fail` and the error of a callee under `try`.

With no expected union, the compiler does not invent one. The arms of a
`match` in a position with no expected type must all have the same type.
A union type always comes from something the program wrote.

A literal follows the same rule. In a position that expects a union, a
literal builds one member, and the choice is static. The member that is the
default type of the literal wins. Without that member, the one member that
implements the literal protocol gets the literal. Two such members is an
error.

### 5. A type argument does not widen

`List<Cat>` does not flow into `List<Pet>`, and `Option<Cat>` does not flow
into `Option<Pet>`. Generic types are invariant in their arguments.

A `Cat` and a `Pet` that holds a `Cat` do not have the same layout, because
the union adds a tag. A `List<Pet>` is therefore a different buffer from a
`List<Cat>`, and a conversion must build it. The language does not hide
that work behind an assignment.

A list literal still takes mixed members, because each element is its own
position under rule 4. To convert a list that exists, build the new one.

```koja
pets: List<Pet> = [Cat{name: "Tom"}, Dog{name: "Rex"}]

cats: List<Cat> = [Cat{name: "Tom"}]
others: List<Pet> = []

for cat in cats
  others = others.append(cat)
end
```

### 6. A union owns no functions

A union has no fields and no functions. Code reads it with `match` and
typed bindings, and the `match` must cover every member.

```koja
match pet
  cat: Cat -> cat.name
  dog: Dog -> dog.name
end
```

A typed binding names one member, or a union of some of the members. An arm
that names a union matches each of its members, and the binding has that
smaller union as its type.

```koja
wide: Cat | Dog | String = Cat{name: "Tom"}

match wide
  text: String -> text
  pet: Pet -> pet.format()
end
```

A variant pattern or a struct pattern reads a union too. The type that the
pattern names picks the member, so the arm needs no typed binding first.

```koja
error: Parse | Net = Parse.Empty

match error
  Parse.BadDigit(pos) -> pos
  Parse.Empty -> 0
  _net: Net -> 1
end
```

The pattern must pick exactly one member. `Option.Some(n)` picks no member of
`Option<Int> | Option<String>`, so a typed binding must come first there. A
member counts as covered when its own patterns cover it, and the `match`
above is exhaustive.

A union does conform to protocols, through its members.

- `Debug`, `Equality`, and `Hash` are built in. A union has `format()`,
  `print()`, `==`, and `hash()` when every member has them.
- A union satisfies a bound `T: P` when every member implements `P` and `P`
  dispatches on the receiver alone. [TYPES.md](TYPES.md) gives the reason
  for the second condition.
- A bound list such as `T: P & Q` is checked one bound at a time. The union
  must satisfy each one.

A call through a bound is a switch on the tag with one member call in each
arm. It is a `match` that the compiler writes, and not a vtable.

The same call written on the union value is an error. The bound is the
place where the program states that every member must conform.

### 7. A recursive alias is a type of its own

An alias can name itself in a type argument of `List`, `Map`, `Set`, or a
generic struct or enum. A value of such a type has a fixed size, so the
union that holds it has a fixed size too.

```koja
type Value = Bool | Int | String | List<Value> | Map<String, Value>
```

Two aliases can name each other the same way. An alias cannot name itself
as a direct member, or through a tuple, a function type, or a `CPtr`. Only
an alias of a union can be recursive.

A type declaration also cannot be a member of a union inside its own
definition, as in `type Tree = Int | List<Float | Tree>`. Rule 1 has no
answer for that union. The flat form of `Float | Tree` holds
`List<Float | Tree>`, which holds `Float | Tree` again. The same is true
when the path back to the type declaration goes through other type
declarations. The union inside the type argument needs a type declaration
of its own.

```koja
type Tree = Int | List<Item>
type Item = Float | Tree
```

`Item` is `Float | Int | List<Item>`, and `Tree` is `Int | List<Item>`.
Both hold in each order of the two declarations.

A recursive alias breaks rule 2 on purpose. The members of the union name
the alias, so a comparison by structure would never end. The alias is
therefore the identity of the type.

- The union of a recursive alias written out by hand is the alias.
- Two recursive aliases with the same shape are different types. A value of
  one does not flow into a position that expects the other.
- Every other rule holds. The alias is a set of members, a member widens
  into it, and it conforms through its members.

Conformance for a recursive alias asks a question that contains itself.
`Value` implements a protocol when `List<Value>` does, and `List<Value>`
does when `Value` does. A check that meets its own question again takes the
answer as yes. The answer is then decided by the members that do not name
the alias.

## In the compiler

The typecheck owns every decision about unions. The IR and the backends
carry the result.

- `canonical_union` builds the canonical form of rule 1. `substitute` calls
  it for each union, so the form holds after a type parameter gets its type.
  The IR uses the same `substitute` and has no copy of its own.
- `check_compatible_stamping` applies rules 3 and 4. It records
  `Coercion::UnionWiden` on the expression that widens.
- `narrow_union_hint` picks the member for a literal.
- `recursive_alias` finds the alias for the union of rule 7.
- `resolve_through_union` picks the member for a variant or struct pattern
  under rule 6. It puts a `Pattern::UnionMember` that holds the member around
  the pattern.

The typecheck records a decision in one of two forms. A conversion that
always succeeds is an annotation on the expression, as `Coercion::UnionWiden`
is. A test that can fail is a node, as `Pattern::UnionMember` is. Coverage
must count the test and the IR must branch on it, so each of them reads it
from the tree.

In the IR, a union is `IRType::Union` with a name, and a declaration with
that name holds its members. The name of an anonymous union comes from the
members. Each package that names the union declares it, and the declarations
are equal. The union of a recursive alias has the name of the alias, and the
package of the alias holds its one declaration.

A union value is a tag and a payload as wide as the widest member. A struct
or an enum that holds its own union inline gets a box on that field, the
same as every other recursive type. [MEMORY-MODEL.md](MEMORY-MODEL.md)
describes the box.

## Open issues

A case that the rules do not predict is a bug, and a bug is a GitHub issue.
This list holds the open issues against the rules.

- [#193](https://github.com/koja-lang/koja/issues/193): a generic struct
  literal or variant constructor does not take its type arguments from a
  member of the expected union. This is a missing position under rule 4.
- [#204](https://github.com/koja-lang/koja/issues/204): a destructuring
  assignment from a call does not widen an element into its local. Only a
  tuple literal on the right pairs each name with an expression. This is a
  missing position under rule 4.

One limit is a decision and not a bug. A direct call on a union value is an
error under rule 6, and [GAPS.md](GAPS.md) records the open question.
