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
- An argument, for a parameter of the union type.
- A returned value, for a function that returns the union.
- A field in a struct literal.
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

A union has no fields and no functions. Code reads it with `match` and a
typed binding for each member, and the `match` must cover every member.

```koja
match pet
  cat: Cat -> cat.name
  dog: Dog -> dog.name
end
```

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

- `canonical_union` builds the canonical form of rule 1.
- `check_compatible_stamping` applies rules 3 and 4. It records
  `Coercion::UnionWiden` on the expression that widens.
- `narrow_union_hint` picks the member for a literal.
- `recursive_alias` finds the alias for the union of rule 7.

In the IR, a union is `IRType::Union` with its members, and its name comes
from the members. The union of a recursive alias is `IRType::NamedUnion`
with the name of the alias, and one declaration holds its members.

A union value is a tag and a payload as wide as the widest member. A struct
or an enum that holds its own union inline gets a box on that field, the
same as every other recursive type. [MEMORY-MODEL.md](MEMORY-MODEL.md)
describes the box.

## Open issues

A case that the rules do not predict is a bug, and a bug is a GitHub issue.
This list holds the open issues against the rules.

- [#185](https://github.com/koja-lang/koja/issues/185): a member does not
  widen inside a variant payload or a tuple argument. This is a missing
  position under rule 4.
- [#186](https://github.com/koja-lang/koja/issues/186): a reassignment and
  a field write do not widen a member. This is a missing position under
  rule 4.
- [#192](https://github.com/koja-lang/koja/issues/192): a union alias does
  not flatten a union alias that the file declares later. This breaks
  rule 1.
- [#193](https://github.com/koja-lang/koja/issues/193): a generic struct
  literal or variant constructor does not take its type arguments from a
  member of the expected union. This is a missing position under rule 4.
- [#194](https://github.com/koja-lang/koja/issues/194): a generic function
  that widens into a union with a type parameter panics in IR lowering.
  Rule 4 holds in the typecheck, and the record of the widening is wrong
  after monomorphization.
- [#195](https://github.com/koja-lang/koja/issues/195): a union with a type
  parameter is not canonical after the parameter gets its type. This breaks
  rule 1.

One limit is a decision and not a bug. A direct call on a union value is an
error under rule 6, and [GAPS.md](GAPS.md) records the open question.
