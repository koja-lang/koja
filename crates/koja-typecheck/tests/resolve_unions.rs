//! Typecheck pins for the union slice: the `A | B`, `type X = A | B`,
//! and `p: T -> ...` triple. Together they exercise:
//!
//! - **Lift**: `TypeExpr::Union` resolves to a canonical
//!   [`AnonymousKind::Union`] (sorted by display key, deduplicated,
//!   nested unions flattened) inside parameter / return / field / let
//!   slots.
//! - **Equivalence**: `types_equivalent` peels alias names and
//!   compares unions structurally (member-set equality), so
//!   `Post | Comment` ≡ `Comment | Post` and `Pet ≡ Cat | Dog | Fish`.
//! - **Widening**: `check_compatible` accepts a member type flowing
//!   into a union slot (call args, struct fields, return slots, let
//!   bindings, enum tuple payloads) by stamping
//!   `Coercion::UnionWiden(target)` on the source `Expr` so IR
//!   lower can emit the matching `UnionWrap`. A union whose members
//!   all belong to the slot's union widens the same way.
//! - **Diagnostics**: bare `FieldAccess` against a union receiver
//!   surfaces a precise "match the union first" error, and a
//!   `MethodCall` admits only the universal protocol functions
//!   (`equals?`, the `Debug` family, `hash`) before saying the same.
//! - **Bounds**: a union discharges a `T: Protocol` bound when every
//!   member conforms and the protocol dispatches on the receiver
//!   alone (no static function, no `Self` outside the receiver).
//! - **Match exhaustiveness**: typed-binding arms over a union
//!   subject either cover every member or surface a precise
//!   missing-member diagnostic. A duplicate member arm warns as
//!   unreachable. Pointing a typed-binding at a non-union surfaces
//!   the narrowing-site diagnostic.
//! - **Member patterns**: a variant or struct pattern on a union
//!   subject matches the member that its type names. Resolve records
//!   that member in a `UnionMember` around the pattern, and coverage
//!   names a missing variant or a missing member.
//! - **Display**: alias names round-trip in diagnostics: a
//!   `type Pet = Cat | Dog` mismatch reports `Pet`, not the expanded
//!   `Cat | Dog`.
//! - **Substitution**: a union that names a type parameter is
//!   canonical again after the parameter gets its type. `String | T`
//!   is `String` at `T = String`, and a flat, sorted union at
//!   `T = Int | Bool`.
//! - **Inference**: the expected type sets a type parameter before
//!   the value does, so a member widens in the payload of a generic
//!   variant constructor and in a tuple literal passed to a generic
//!   call. An expected union gives a generic struct literal, variant
//!   constructor, or call the one member that it can build.
//! - **Writes**: a reassignment of a local, a write to a field, and
//!   an element of a tuple literal on the right of a destructuring
//!   assignment widen a member into the type of the target.

use koja_ast::ast::{EnumConstructionData, Expr, ExprKind, Pattern, Statement};
use koja_ast::coercion::{Coercion, LiteralCoercion, NumericLiteralWidth};
use koja_ast::identifier::{AnonymousKind, Resolution, ResolvedType};
use koja_ast::util::dedent;
use koja_typecheck::{CheckedProgram, Substitution, peel_alias, substitute};

mod common;

use common::{
    PACKAGE, assert_script_fails_with, bool_type, diagnostic_messages, global_named, int_type,
    package_leaf, registry_id, script_body, string_type, struct_definition, trailing_expr,
    trailing_resolution, typecheck_script as typecheck, typecheck_script_fail as typecheck_fail,
    warning_messages,
};

#[test]
fn bare_union_in_param_signature_typechecks() {
    let source = "
        struct Post
          title: String
        end

        struct Comment
          body: String
        end

        fn describe(item: Post | Comment) -> String
          match item
            _ -> \"ok\"
          end
        end

          describe(Post{title: \"hi\"})
        ";
    typecheck(&dedent(source));
}

#[test]
fn union_member_order_is_canonical() {
    // `Comment | Post` and `Post | Comment` resolve to the same
    // canonical union, so passing a `Post | Comment` value into a
    // slot declared `Comment | Post` typechecks without any extra
    // coercion (no `Compatible::UnionWiden` needed, equality alone
    // suffices). Pinned so a future canonicalization regression
    // surfaces as a clean test failure.
    let source = "
        struct A
          x: Int
        end

        struct B
          y: Int
        end

        fn flip(v: A | B) -> B | A
          v
        end

          flip(A{x: 1})
        ";
    typecheck(&dedent(source));
}

#[test]
fn union_member_dedup_collapses_repeats() {
    // `A | A | B` canonicalizes to `A | B`. The duplicate member
    // is folded out at lift time. A return position annotated
    // `A | B` accepts a function whose RHS annotation is `A | A | B`.
    let source = "
        struct A
          x: Int
        end

        struct B
          y: Int
        end

        fn one(v: A | A | B) -> A | B
          v
        end

          one(A{x: 1})
        ";
    typecheck(&dedent(source));
}

#[test]
fn union_keeps_members_that_share_a_leaf_name() {
    // `A.Error` and `B.Error` share a leaf name. The display string
    // orders union members but is not their identity, so the two
    // stay distinct and a value of either widens into the union.
    // Before the structural dedup, `A.Error | B.Error` collapsed to
    // `A.Error` and the `B.Error` assignment below failed.
    let source = "
        struct A
        end

        enum A.Error
          One
        end

        struct B
        end

        enum B.Error
          Two
        end

        fn pick(flag: Bool) -> A.Error | B.Error
          if flag
            a: A.Error | B.Error = A.Error.One
            return a
          end

          b: A.Error | B.Error = B.Error.Two
          b
        end

          match pick(false)
            e: A.Error -> 1
            e: B.Error -> 2
          end
        ";
    typecheck(&dedent(source));
}

#[test]
fn union_mismatch_spells_nested_members_by_full_path() {
    // Every nested type renders by its full path, so a mismatch
    // between types that share a leaf name reads
    // `TestApp.A.Error | TestApp.B.Error` against `TestApp.C.Error`
    // and not `Error | Error` against `Error`.
    let source = "
        struct A
        end

        enum A.Error
          One
        end

        struct B
        end

        enum B.Error
          Two
        end

        struct C
        end

        enum C.Error
          Three
        end

        fn take(e: A.Error | B.Error) -> Int
          1
        end

          take(C.Error.Three)
        ";
    assert_script_fails_with(
        &dedent(source),
        &[
            "expects `TestApp.A.Error | TestApp.B.Error`",
            "got `TestApp.C.Error`",
        ],
    );
}

#[test]
fn nested_union_in_signature_typechecks() {
    // `(A | B) | C` is the parser's intermediate shape. The lifter
    // canonicalizes the nested members into a single flat
    // `{ A, B, C }` member set. A round trip through a function
    // signature confirms the lifted shape stays well-formed.
    let source = "
        struct A
          x: Int
        end

        struct B
          y: Int
        end

        struct C
          z: Int
        end

        fn flatten(v: A | B | C) -> A | B | C
          v
        end

          flatten(A{x: 1})
        ";
    typecheck(&dedent(source));
}

#[test]
fn member_type_widens_into_union_call_arg() {
    let source = "
        struct Post
          title: String
        end

        struct Comment
          body: String
        end

        fn take(item: Post | Comment) -> String
          match item
            _ -> \"ok\"
          end
        end

          p = Post{title: \"hi\"}
          take(p)
        ";
    typecheck(&dedent(source));
}

#[test]
fn member_type_widens_into_union_let_binding() {
    let source = "
        struct A
          x: Int
        end

        struct B
          y: Int
        end

          a = A{x: 1}
          v: A | B = a
          v
        ";
    typecheck(&dedent(source));
}

#[test]
fn member_type_widens_into_union_return_slot() {
    let source = "
        struct A
          x: Int
        end

        struct B
          y: Int
        end

        fn make_a() -> A | B
          A{x: 1}
        end

          make_a()
        ";
    typecheck(&dedent(source));
}

#[test]
fn non_member_into_union_diagnoses() {
    let source = "
        struct A
          x: Int
        end

        struct B
          y: Int
        end

        struct C
          z: Int
        end

        fn take(v: A | B) -> Int
          match v
            _ -> 0
          end
        end

          take(C{z: 0})
        ";
    assert_script_fails_with(
        source,
        &["expects `TestApp.A | TestApp.B`", "got `TestApp.C`"],
    );
}

#[test]
fn union_widens_into_superset_union() {
    // An `A | B` value flows into an `A | B | C` slot in a call
    // argument, a let binding, and a return slot. Each site stamps
    // `Coercion::UnionWiden` just as a single member would.
    let source = "
        struct A
          x: Int
        end

        struct B
          y: Int
        end

        struct C
          z: Int
        end

        fn narrow(flag: Bool) -> A | B
          if flag
            a: A | B = A{x: 1}
            return a
          end

          b: A | B = B{y: 2}
          b
        end

        fn take(v: A | B | C) -> Int
          match v
            _ -> 0
          end
        end

        fn widen(flag: Bool) -> A | B | C
          narrow(flag)
        end

          take(narrow(true))
          wide: A | B | C = narrow(false)
          take(wide)
          widen(true)
        ";
    typecheck(&dedent(source));
}

#[test]
fn union_with_outside_member_into_union_diagnoses() {
    // `A | C` does not widen into `A | B`. One stranger in the
    // source union is enough to reject the whole flow.
    let source = "
        struct A
          x: Int
        end

        struct B
          y: Int
        end

        struct C
          z: Int
        end

        fn take(v: A | B) -> Int
          match v
            _ -> 0
          end
        end

          mixed: A | C = A{x: 1}
          take(mixed)
        ";
    assert_script_fails_with(
        source,
        &[
            "expects `TestApp.A | TestApp.B`",
            "got `TestApp.A | TestApp.C`",
        ],
    );
}

#[test]
fn field_access_on_union_diagnoses() {
    // `v.title` where `v: Post | Comment` is illegal: typecheck
    // forces the user to discriminate via `match` first. Pinned so
    // a future naive auto-narrow doesn't silently fall back to
    // "unknown field".
    let source = "
        struct Post
          title: String
        end

        struct Comment
          body: String
        end

          v: Post | Comment = Post{title: \"hi\"}
          v.title
        ";
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.to_lowercase().contains("union")
                && (m.contains("field") || m.contains("match"))),
        "expected union-receiver field-access diagnostic, got {messages:?}",
    );
}

#[test]
fn method_call_on_union_diagnoses() {
    let source = "
        struct A
          x: Int
        end

        extend A
          fn label(self) -> String
            \"a\"
          end
        end

        struct B
          y: Int
        end

        extend B
          fn label(self) -> String
            \"b\"
          end
        end

          v: A | B = A{x: 1}
          v.label()
        ";
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("no function `label` on union type")),
        "expected union-receiver method-call diagnostic, got {messages:?}",
    );
}

#[test]
fn universal_protocol_calls_on_union_resolve() {
    // `equals?`, the `Debug` family, and `hash` (when every member
    // hashes) are the only functions a union receiver admits.
    let source = "
        struct A: Hash
          x: Int

          fn hash(self) -> Int
            self.x
          end
        end

        struct B: Hash
          y: Int

          fn hash(self) -> Int
            self.y
          end
        end

        v: A | B = A{x: 1}
        w: A | B = B{y: 2}
        (v == w).print()
        v.equals?(w).print()
        v.format().print()
        v.print()
        v.hash().print()
        ";
    typecheck(&dedent(source));
}

#[test]
fn union_hash_requires_every_member_to_hash() {
    let source = "
        struct A: Hash
          x: Int

          fn hash(self) -> Int
            self.x
          end
        end

        struct B
          y: Int
        end

        v: A | B = A{x: 1}
        v.hash().print()
        ";
    assert_script_fails_with(source, &["cannot hash unions containing `TestApp.B`"]);
}

/// Declarations for the union equality and arm tests. They are three
/// structs, the alias `Pet = Cat | Dog`, and one local of each type
/// in play.
const PET_PRELUDE: &str = "
    struct Cat
      name: String
    end

    struct Dog
      name: String
    end

    struct Bird
      name: String
    end

    type Pet = Cat | Dog

    pet: Pet = Cat{name: \"Tom\"}
    tom = Cat{name: \"Tom\"}
    bird = Bird{name: \"Tweety\"}
    wide: Cat | Dog | Bird = tom
    other: Dog | Bird = bird
    value: Int | String = 1
    ";

fn pet_script(tail: &str) -> String {
    format!("{}{}", dedent(PET_PRELUDE), dedent(tail))
}

#[test]
fn union_equality_takes_a_member_on_the_right() {
    let source = pet_script(
        "
        (pet == tom).print()
        (pet != tom).print()
        (pet == Cat{name: \"Tom\"}).print()
        pet.equals?(tom).print()
        (wide == pet).print()
        (value == 1).print()
        (value == \"koja\").print()
        ",
    );
    typecheck(&source);
}

#[test]
fn union_equality_takes_a_member_on_the_left() {
    let source = pet_script(
        "
        (tom != pet).print()
        (Cat{name: \"Tom\"} == pet).print()
        (pet == wide).print()
        (1 == value).print()
        (\"koja\" == value).print()
        tom == pet
        ",
    );
    let checked = typecheck(&source);
    // The union must be the `equals?` receiver, so the left value
    // binds first and the operands still evaluate left to right.
    let comparison = trailing_expr(&checked);
    assert!(
        matches!(comparison.kind, ExprKind::Match { .. }),
        "expected the member-left comparison to rewrite to a match, got {:?}",
        comparison.kind,
    );
    assert_eq!(comparison.resolution, bool_type(&checked));
}

#[test]
fn union_equality_rejects_a_value_that_is_not_a_member() {
    let source = pet_script("pet == bird\n");
    let failure = typecheck_fail(&source);
    let messages = diagnostic_messages(&failure);
    assert_eq!(
        messages,
        vec![
            "cannot compare `TestApp.Pet` with `TestApp.Bird`. `TestApp.Bird` is not a member \
             of `TestApp.Pet`"
        ],
    );
}

#[test]
fn union_equality_rejects_unions_that_only_overlap() {
    let source = pet_script("pet == other\n");
    let failure = typecheck_fail(&source);
    let messages = diagnostic_messages(&failure);
    assert_eq!(
        messages,
        vec![
            "cannot compare `TestApp.Pet` with `TestApp.Bird | TestApp.Dog`. \
             `TestApp.Bird | TestApp.Dog` has a member that is not a member of `TestApp.Pet`"
        ],
    );
}

#[test]
fn member_equals_call_rejects_a_union_argument() {
    // Only the operator widens. The explicit call names the member's
    // own `equals?`, and that function takes the member type.
    let source = pet_script("tom.equals?(pet)\n");
    let failure = typecheck_fail(&source);
    let messages = diagnostic_messages(&failure);
    assert_eq!(
        messages,
        vec!["argument `other` to `TestApp.Cat.equals?` expects `TestApp.Cat`, got `TestApp.Pet`"],
    );
}

#[test]
fn arms_that_disagree_widen_into_an_expected_union() {
    let source = pet_script(
        "
        flag = true
        dog = Dog{name: \"Rex\"}

        from_if: Pet = if flag
          tom
        else
          dog
        end

        from_cond: Pet = cond
          flag -> tom
          else -> dog
        end

        from_match: Pet = match flag
          true -> tom
          false -> dog
        end

        from_ternary: Pet = flag ? tom : dog
        ",
    );
    let checked = typecheck(&source);
    // Each arm carries the widening. The ternary already has the
    // union type, so it needs no coercion of its own.
    let Some(Statement::Assignment { value, .. }) = script_body(&checked).last() else {
        panic!("expected the script to end with an assignment");
    };
    let ExprKind::Ternary {
        then_expr,
        else_expr,
        ..
    } = &value.kind
    else {
        panic!("expected a ternary, got {:?}", value.kind);
    };
    let pet = package_leaf(&checked, "Pet");
    let widen_to_pet = Some(Coercion::UnionWiden(pet.clone()));
    assert_eq!(then_expr.coercion, widen_to_pet);
    assert_eq!(else_expr.coercion, widen_to_pet);
    assert_eq!(value.coercion, None);
    assert_eq!(value.resolution, pet);
}

#[test]
fn a_member_arm_joins_a_union_arm() {
    let source = pet_script(
        "
        flag = true
        dog = Dog{name: \"Rex\"}
        member_then_union: Pet = flag ? dog : pet
        union_then_member: Pet = flag ? pet : dog
        narrower: Cat | Dog | Bird = flag ? pet : bird
        scalar: Int | String = flag ? 1 : \"one\"
        ",
    );
    typecheck(&source);
}

#[test]
fn an_arm_that_diverges_does_not_stop_the_widening() {
    let source = pet_script(
        "
        fn pick(n: Int) -> Pet
          match n
            0 -> Cat{name: \"Tom\"}
            1 -> Dog{name: \"Rex\"}
            _ -> return Cat{name: \"Tim\"}
          end
        end

        pick(0)
        ",
    );
    typecheck(&source);
}

#[test]
fn an_arm_that_is_not_a_member_keeps_the_join_error() {
    let source = pet_script(
        "
        flag = true
        wrong: Pet = flag ? tom : bird
        ",
    );
    assert_arms_inconsistent(&source, "then=`TestApp.Cat`, else=`TestApp.Bird`");
}

#[test]
fn an_arm_union_that_only_overlaps_keeps_the_join_error() {
    let source = pet_script(
        "
        flag = true
        wrong: Pet = flag ? tom : other
        ",
    );
    assert_arms_inconsistent(
        &source,
        "then=`TestApp.Cat`, else=`TestApp.Bird | TestApp.Dog`",
    );
}

#[test]
fn arms_with_no_expected_type_stay_strict() {
    let source = pet_script(
        "
        flag = true
        dog = Dog{name: \"Rex\"}
        either = flag ? tom : dog
        ",
    );
    assert_arms_inconsistent(&source, "then=`TestApp.Cat`, else=`TestApp.Dog`");
}

#[test]
fn numeric_arms_stay_strict() {
    // Arms widen only into a union. A sized integer arm does not
    // widen to match an `Int` arm.
    let source = "
        flag = true
        small: Int32 = 1
        big = 2
        n: Int = flag ? small : big
        ";
    assert_arms_inconsistent(&dedent(source), "then=`Int32`, else=`Int`");
}

/// Assert that `source` fails with only the ternary join error, and
/// that the error names the two arm types in `arm_types`.
fn assert_arms_inconsistent(source: &str, arm_types: &str) {
    let failure = typecheck_fail(source);
    let messages = diagnostic_messages(&failure);
    assert_eq!(
        messages,
        vec![format!("ternary arms have inconsistent types. {arm_types}")],
    );
}

#[test]
fn union_satisfies_protocol_bound_when_every_member_conforms() {
    let source = "
        protocol Label
          fn label(self) -> String
        end

        struct A: Label
          x: Int

          fn label(self) -> String
            \"a\"
          end
        end

        struct B: Label
          y: Int

          fn label(self) -> String
            \"b\"
          end
        end

        type Item = A | B

        fn show<T: Label>(value: T) -> String
          value.label()
        end

        named: Item = A{x: 1}
        bare: A | B = B{y: 2}
        show(named).print()
        show(bare).print()
        ";
    typecheck(&dedent(source));
}

#[test]
fn union_bound_requires_every_member_to_conform() {
    let source = "
        protocol Label
          fn label(self) -> String
        end

        struct A: Label
          x: Int

          fn label(self) -> String
            \"a\"
          end
        end

        struct B
          y: Int
        end

        fn show<T: Label>(value: T) -> String
          value.label()
        end

        v: A | B = A{x: 1}
        show(v).print()
        ";
    assert_union_bound_fails(
        source,
        "does not implement protocol `TestApp.Label`",
        "`TestApp.B` does not implement `TestApp.Label`",
    );
}

#[test]
fn union_bound_rejects_protocol_with_static_function() {
    // A static function has no receiver, so no tag picks the impl.
    let source = "
        protocol Named
          fn name(self) -> String
          fn default_name -> String
        end

        struct A: Named
          x: Int

          fn name(self) -> String
            \"a\"
          end

          fn default_name -> String
            \"a\"
          end
        end

        struct B: Named
          y: Int

          fn name(self) -> String
            \"b\"
          end

          fn default_name -> String
            \"b\"
          end
        end

        fn show<T: Named>(value: T) -> String
          value.name()
        end

        v: A | B = A{x: 1}
        show(v).print()
        ";
    assert_union_bound_fails(
        source,
        "does not implement protocol `TestApp.Named`",
        "only when every function takes `self`",
    );
}

#[test]
fn union_bound_rejects_protocol_that_names_self_outside_the_receiver() {
    // `other: Self` could carry a different member than the receiver.
    let source = "
        protocol Merge
          fn merge(self, other: Self) -> Int
        end

        struct A: Merge
          x: Int

          fn merge(self, other: A) -> Int
            self.x + other.x
          end
        end

        struct B: Merge
          y: Int

          fn merge(self, other: B) -> Int
            self.y + other.y
          end
        end

        fn combine<T: Merge>(left: T, right: T) -> Int
          left.merge(right)
        end

        v: A | B = A{x: 1}
        combine(v, v).print()
        ";
    assert_union_bound_fails(
        source,
        "does not implement protocol `TestApp.Merge`",
        "names `Self` in no other position",
    );
}

/// Assert one diagnostic carries both the bound failure and the hint
/// that says why the union fails it.
fn assert_union_bound_fails(source: &str, message_needle: &str, hint_needle: &str) {
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure.diagnostics.iter().any(|d| {
            d.message.contains(message_needle)
                && d.hint.as_deref().is_some_and(|h| h.contains(hint_needle))
        }),
        "expected `{message_needle}` with hint `{hint_needle}`, got {:?}",
        failure.diagnostics,
    );
}

#[test]
fn typed_binding_arm_resolves_with_member_type() {
    // Inside the arm body, `p` has type `Post`, so `p.title`
    // resolves cleanly. Each arm's pattern stamps the local on the
    // match scope. The body sees only the narrowed member type.
    let source = "
        struct Post
          title: String
        end

        struct Comment
          body: String
        end

        fn describe(item: Post | Comment) -> String
          match item
            p: Post -> p.title
            c: Comment -> c.body
          end
        end

          describe(Post{title: \"hi\"})
        ";
    typecheck(&dedent(source));
}

#[test]
fn typed_binding_missing_member_diagnoses() {
    let source = "
        struct Post
          title: String
        end

        struct Comment
          body: String
        end

        struct Ad
          url: String
        end

        fn describe(item: Post | Comment | Ad) -> String
          match item
            p: Post -> p.title
            c: Comment -> c.body
          end
        end

          describe(Post{title: \"hi\"})
        ";
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("Ad") && m.to_lowercase().contains("missing")),
        "expected missing-union-member diagnostic mentioning `Ad`, got {messages:?}",
    );
}

#[test]
fn typed_binding_duplicate_member_warns_unreachable() {
    let source = "
        struct A
          x: Int
        end

        struct B
          y: Int
        end

        fn describe(item: A | B) -> Int
          match item
            a: A -> 1
            a: A -> 2
            _ -> 0
          end
        end

          describe(A{x: 0})
        ";
    let checked = typecheck(&dedent(source));
    let warnings = warning_messages(&checked);
    assert!(
        warnings.iter().any(|m| m.contains("unreachable")),
        "expected duplicate-union-member unreachable warning, got {warnings:?}",
    );
}

#[test]
fn typed_binding_not_in_union_diagnoses() {
    // Typed-binding arms only narrow over union subjects. Using one
    // against a primitive surfaces a precise narrowing diagnostic.
    let source = "
        struct Post
          id: Int
        end

          match 1
            p: Post -> p.id
            _ -> 0
          end
        ";
    assert_script_fails_with(source, &["typed-binding", "union"]);
}

#[test]
fn typed_binding_member_not_in_union_diagnoses() {
    // The member type in `p: T ->` must be a member of the
    // subject's canonical union. Naming a stranger surfaces a
    // precise "not a member" diagnostic at the pattern site rather
    // than letting the body diagnose later.
    let source = "
        struct A
          x: Int
        end

        struct B
          y: Int
        end

        struct C
          z: Int
        end

        fn describe(item: A | B) -> Int
          match item
            c: C -> c.z
            _ -> 0
          end
        end

          describe(A{x: 1})
        ";
    assert_script_fails_with(
        source,
        &["type `TestApp.C` is not a member of union `TestApp.A | TestApp.B`"],
    );
}

/// Declarations for the tests of a variant or struct pattern on a
/// union subject. They are two enums, two structs, and one generic
/// struct.
const MEMBER_PATTERN_PRELUDE: &str = "
    enum Parse
      BadDigit(Int)
      Empty
    end

    enum Net
      Timeout
    end

    struct Cat
      name: String
    end

    struct Dog
      name: String
      age: Int
    end

    struct Crate<T>
      item: T
    end
    ";

fn member_pattern_script(tail: &str) -> String {
    format!("{}{}", dedent(MEMBER_PATTERN_PRELUDE), dedent(tail))
}

/// The member type on each arm of the `match` that ends the script.
/// Panics when an arm pattern is not a `UnionMember`.
fn arm_member_types(checked: &CheckedProgram) -> Vec<&ResolvedType> {
    let ExprKind::Match { arms, .. } = &trailing_expr(checked).kind else {
        panic!("expected the script to end with a `match`");
    };
    arms.iter()
        .map(|arm| match &arm.pattern {
            Pattern::UnionMember { member_type, .. } => member_type,
            other => panic!("expected a `UnionMember` arm pattern, got {other:?}"),
        })
        .collect()
}

#[test]
fn variant_pattern_on_a_union_subject_records_its_member() {
    let source = member_pattern_script(
        "
        error: Parse | Net = Parse.Empty
        match error
          Parse.BadDigit(pos) -> pos
          Parse.Empty -> 0
          Net.Timeout -> 1
        end
        ",
    );
    let checked = typecheck(&source);
    let parse = package_leaf(&checked, "Parse");
    let net = package_leaf(&checked, "Net");
    assert_eq!(arm_member_types(&checked), [&parse, &parse, &net]);
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn struct_pattern_on_a_union_subject_records_its_member() {
    let source = member_pattern_script(
        "
        pet: Cat | Dog = Cat{name: \"Tom\"}
        match pet
          Cat{name: n} -> n
          Dog{name: n, age: 3} -> n
          Dog{name: n} -> n
        end
        ",
    );
    let checked = typecheck(&source);
    let cat = package_leaf(&checked, "Cat");
    let dog = package_leaf(&checked, "Dog");
    assert_eq!(arm_member_types(&checked), [&cat, &dog, &dog]);
    assert_eq!(trailing_resolution(&checked), string_type(&checked));
}

#[test]
fn variant_pattern_matches_a_union_in_a_variant_payload() {
    // The repro of issue #156. The payload of `Result.Err` is a
    // union, and each inner pattern names one member of it.
    let source = member_pattern_script(
        "
        fn route(outcome: Result<Int, Parse | Net>) -> Int
          match outcome
            Result.Ok(value) -> value
            Result.Err(Parse.BadDigit(pos)) -> pos
            Result.Err(Parse.Empty) -> 0
            Result.Err(Net.Timeout) -> 1
          end
        end

        route(Result.Ok(1))
        ",
    );
    typecheck(&source);
}

#[test]
fn variant_pattern_matches_a_member_of_a_union_alias() {
    let source = member_pattern_script(
        "
        type AppError = Parse | Net

        fn route(error: AppError) -> Int
          match error
            Parse.BadDigit(pos) -> pos
            Parse.Empty -> 0
            Net.Timeout -> 1
          end
        end

        route(Net.Timeout)
        ",
    );
    typecheck(&source);
}

#[test]
fn variant_pattern_shares_a_match_with_a_typed_binding() {
    let source = member_pattern_script(
        "
        fn route(error: Parse | Net) -> Int
          match error
            Parse.BadDigit(pos) -> pos
            Parse.Empty -> 0
            _net: Net -> 1
          end
        end

        route(Net.Timeout)
        ",
    );
    typecheck(&source);
}

#[test]
fn variant_patterns_that_miss_a_variant_name_the_variant() {
    let source = member_pattern_script(
        "
        fn route(error: Parse | Net) -> Int
          match error
            Parse.BadDigit(pos) -> pos
            Net.Timeout -> 1
          end
        end

        route(Net.Timeout)
        ",
    );
    assert_script_fails_with(
        &source,
        &["match is not exhaustive. Missing pattern(s) `TestApp.Parse.Empty`"],
    );
}

#[test]
fn variant_patterns_that_miss_a_member_name_the_member() {
    let source = member_pattern_script(
        "
        fn route(error: Parse | Net) -> Int
          match error
            Parse.BadDigit(pos) -> pos
            Parse.Empty -> 0
          end
        end

        route(Net.Timeout)
        ",
    );
    assert_script_fails_with(
        &source,
        &["match is not exhaustive. Missing pattern(s) `TestApp.Net`"],
    );
}

#[test]
fn variant_pattern_after_a_typed_binding_of_its_member_warns_unreachable() {
    let source = member_pattern_script(
        "
        fn route(error: Parse | Net) -> Int
          match error
            _parse: Parse -> 0
            Parse.Empty -> 1
            Net.Timeout -> 2
          end
        end

        route(Net.Timeout)
        ",
    );
    let checked = typecheck(&source);
    let warnings = warning_messages(&checked);
    assert!(
        warnings.iter().any(|m| m.contains("unreachable")),
        "expected an unreachable warning for `Parse.Empty`, got {warnings:?}",
    );
}

#[test]
fn variant_pattern_of_a_type_outside_the_union_diagnoses() {
    let source = member_pattern_script(
        "
        fn route(pet: Cat | Dog) -> Int
          match pet
            Net.Timeout -> 1
            _ -> 0
          end
        end

        route(Cat{name: \"Tom\"})
        ",
    );
    assert_script_fails_with(
        &source,
        &["type `TestApp.Net` is not a member of union `TestApp.Cat | TestApp.Dog`"],
    );
}

#[test]
fn struct_pattern_that_names_two_members_diagnoses() {
    // `Crate{...}` does not say which of the two `Crate` members it
    // matches. The hint names a typed binding, which does.
    let source = member_pattern_script(
        "
        fn open(item: Crate<Int> | Crate<String>) -> Int
          match item
            Crate{item: _} -> 1
          end
        end

        open(Crate{item: 1})
        ",
    );
    let failure = typecheck_fail(&source);
    assert!(
        failure.diagnostics.iter().any(|d| {
            d.message.contains(
                "`TestApp.Crate` names more than one member of union \
                 `TestApp.Crate<Int> | TestApp.Crate<String>`",
            ) && d
                .hint
                .as_deref()
                .is_some_and(|h| h.contains("`value: TestApp.Crate<Int> ->`"))
        }),
        "expected the ambiguous member error with a typed binding hint, got {:?}",
        failure.diagnostics,
    );
}

#[test]
fn or_pattern_rejects_a_variant_of_a_union_member() {
    let source = member_pattern_script(
        "
        fn route(error: Parse | Net) -> Int
          match error
            Parse.Empty | Net.Timeout -> 1
            _ -> 0
          end
        end

        route(Net.Timeout)
        ",
    );
    let failure = typecheck_fail(&source);
    assert!(
        failure.diagnostics.iter().any(|d| {
            d.message
                .contains("an or-pattern cannot hold a variant pattern when the subject is a union")
                && d.hint
                    .as_deref()
                    .is_some_and(|h| h.contains("write one arm for each alternative"))
        }),
        "expected the or-pattern error with the one arm hint, got {:?}",
        failure.diagnostics,
    );
}

#[test]
fn variant_pattern_of_an_unknown_variant_reports_one_error() {
    // The member is found, so the only error is the one for the
    // variant. No union mismatch follows it.
    let source = member_pattern_script(
        "
        fn route(error: Parse | Net) -> Int
          match error
            Parse.Nope -> 1
            _ -> 0
          end
        end

        route(Net.Timeout)
        ",
    );
    let failure = typecheck_fail(&source);
    let messages = diagnostic_messages(&failure);
    assert_eq!(messages, ["`TestApp.Parse` has no variant `Nope`"]);
}

#[test]
fn variant_pattern_matches_a_member_of_a_generic_union() {
    // `Option<Int> | T` holds `Option<Int>` for every `T`, so the
    // variant patterns name that member in the generic body.
    let source = "
        struct Holder<T>
          value: Option<Int> | T
        end

        fn pick<T>(holder: Holder<T>) -> Int
          match holder.value
            Option.Some(n) -> n
            Option.None -> -1
            _other: T -> 0
          end
        end

          text: Holder<String> = Holder{value: \"a\"}
          same: Holder<Option<Int>> = Holder{value: Option.None}
          pick(text) + pick(same)
        ";
    typecheck(&dedent(source));
}

const HOLDER_PRELUDE: &str = "
    struct Holder<T>
      value: String | T
    end
    ";

fn holder_script(tail: &str) -> String {
    format!("{}{}", dedent(HOLDER_PRELUDE), dedent(tail))
}

#[test]
fn substituted_union_collapses_an_equal_member() {
    // `String | T` at `T = String` is `String`, not `String | String`.
    let source = holder_script(
        "
        same: Holder<String> = Holder{value: \"a\"}
        text: String = same.value
        same.value
        ",
    );
    let checked = typecheck(&source);
    assert_eq!(trailing_resolution(&checked), string_type(&checked));
}

#[test]
fn substituted_union_flattens_a_union_argument() {
    // `String | T` at `T = Int | Bool` is the flat, sorted union. A
    // member of the argument is then a member of the field type.
    let source = holder_script(
        "
        wide: Holder<Int | Bool> = Holder{value: true}
        value: Bool | Int | String = wide.value
        wide.value
        ",
    );
    let checked = typecheck(&source);
    assert_eq!(
        trailing_resolution(&checked),
        ResolvedType::Anonymous(AnonymousKind::Union {
            members: vec![
                bool_type(&checked),
                int_type(&checked),
                string_type(&checked),
            ],
        }),
    );
}

#[test]
fn substituted_union_peels_an_alias_argument() {
    let source = holder_script(
        "
        type Small = Int | Bool

        small: Small = true
        ",
    );
    let checked = typecheck(&source);
    let template = &struct_definition(&checked, "Holder").fields[0].ty;
    let holder_id = registry_id(&checked, PACKAGE, &["Holder"]);
    let subst = Substitution::from_args(holder_id, &[package_leaf(&checked, "Small")]);
    assert_eq!(
        substitute(template, &subst, &checked.registry),
        ResolvedType::Anonymous(AnonymousKind::Union {
            members: vec![
                bool_type(&checked),
                int_type(&checked),
                string_type(&checked),
            ],
        }),
    );
}

#[test]
fn substituted_union_is_canonical_inside_a_type_argument() {
    let source = "
        struct Bag<T>
          items: List<String | T>
        end

          items: List<Int | String> = []
          bag: Bag<Int> = Bag{items: items}
          bag.items
        ";
    let checked = typecheck(&dedent(source));
    let member = ResolvedType::Anonymous(AnonymousKind::Union {
        members: vec![int_type(&checked), string_type(&checked)],
    });
    assert_eq!(
        trailing_resolution(&checked),
        global_named(&checked, "List", vec![member]),
    );
}

#[test]
fn match_on_a_collapsed_field_takes_one_arm() {
    // The field type is `String`, so the typed binding names the
    // whole subject and one arm is exhaustive.
    let source = holder_script(
        "
        same: Holder<String> = Holder{value: \"a\"}
        match same.value
          s: String -> s
        end
        ",
    );
    let checked = typecheck(&source);
    assert_eq!(trailing_resolution(&checked), string_type(&checked));
}

#[test]
fn generic_return_union_collapses_at_the_call() {
    // `Int | T` at `T = Int` is `Int`, so the call fits an `Int` slot.
    let source = "
        fn pick<T>(item: T, flag: Bool) -> Int | T
          if flag
            return item
          end
          0
        end

          n: Int = pick(7, true)
          pick(7, true)
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn substituted_union_takes_a_recursive_alias_argument() {
    // `String | T` at `T = Value` is the union that `Value` names,
    // because `String` is already a member of it.
    let source = holder_script(
        "
        type Value = Bool | Int | String | List<Value>

        items: List<Value> = []
        holder: Holder<Value> = Holder{value: items}
        value: Value = holder.value
        holder.value
        ",
    );
    let checked = typecheck(&source);
    assert_eq!(
        trailing_resolution(&checked),
        peel_alias(&package_leaf(&checked, "Value"), &checked.registry),
    );
}

#[test]
fn variant_payload_widens_a_member_under_the_expected_type() {
    // The annotation sets `T` to `Pet` before the payload has a say,
    // so the payload is a member that widens into its slot.
    let source = pet_script("some: Option<Pet> = Option.Some(tom)\n");
    let checked = typecheck(&source);
    let Some(Statement::Assignment { value, .. }) = script_body(&checked).last() else {
        panic!("expected the script to end with an assignment");
    };
    let ExprKind::EnumConstruction {
        data: EnumConstructionData::Tuple(payload),
        ..
    } = &value.kind
    else {
        panic!(
            "expected a tuple variant construction, got {:?}",
            value.kind
        );
    };
    let pet = package_leaf(&checked, "Pet");
    assert_eq!(payload[0].coercion, Some(Coercion::UnionWiden(pet.clone())));
    assert_eq!(
        value.resolution,
        global_named(&checked, "Option", vec![pet])
    );
}

#[test]
fn variant_payload_widens_in_each_position_that_names_the_union() {
    let source = pet_script(
        "
        struct Home
          pet: Option<Pet>
        end

        enum Box<T>
          Full(T)
          Named{item: T}
        end

        fn wrap(cat: Cat) -> Option<Pet>
          Option.Some(cat)
        end

        fn adopt(cat: Cat) -> Result<Pet, String>
          Result.Ok(cat)
        end

        fn refuse(cat: Cat) -> Result<Int, Pet>
          Result.Err(cat)
        end

        fn count(found: Option<Pet>) -> Int
          1
        end

        count(Option.Some(tom))
        home = Home{pet: Option.Some(tom)}
        all: List<Option<Pet>> = [Option.Some(tom), Option.None]
        full: Box<Pet> = Box.Full(tom)
        named: Box<Pet> = Box.Named{item: tom}
        text: Option<Int | String> = Option.Some(\"a\")
        number: Option<Int | String> = Option.Some(7)
        found: Option<Pet> = Option.Some(tom)
        found == Option.Some(tom)
        ",
    );
    let checked = typecheck(&source);
    assert_eq!(trailing_resolution(&checked), bool_type(&checked));
}

#[test]
fn variant_payload_that_conflicts_with_the_expected_type_decides() {
    // `Int` does not widen into `String`, so the payload sets `T` and
    // the annotation reports the two types.
    let source = pet_script("text: Option<String> = Option.Some(1)\n");
    let failure = typecheck_fail(&source);
    assert_eq!(
        diagnostic_messages(&failure),
        vec![
            "type annotation on `text` says `Option<String>`, but the right-hand side has \
             type `Option<Int>`"
        ],
    );
}

#[test]
fn built_variant_does_not_widen_its_type_argument() {
    // Only the constructor widens its payload. A value that already
    // has the type `Option<Cat>` is not an `Option<Pet>`.
    let source = pet_script(
        "
        narrow: Option<Cat> = Option.Some(tom)
        some: Option<Pet> = narrow
        ",
    );
    let failure = typecheck_fail(&source);
    assert_eq!(
        diagnostic_messages(&failure),
        vec![
            "type annotation on `some` says `Option<TestApp.Pet>`, but the right-hand side \
             has type `Option<TestApp.Cat>`"
        ],
    );
}

#[test]
fn tuple_literal_argument_widens_a_member_in_a_generic_call() {
    // The receiver sets `T` to `(String, Pet)`. The tuple literal
    // arrives as `(String, Cat)`, and its second element widens.
    let source = pet_script(
        "
        fn add<T>(items: List<T>, item: T) -> List<T>
          items.append(item)
        end

        homes: Map<String, (Int, Pet)> = Map.new()
        homes = homes.put(\"a\", (1, tom))
        deep: List<(Int, (String, Pet))> = []
        deep = deep.append((1, (\"tom\", tom)))
        pets: List<(String, Pet)> = []
        pets = add(pets, (\"tom\", tom))
        pets = pets.append((\"tom\", tom))
        ",
    );
    let checked = typecheck(&source);
    let Some(Statement::Assignment { value, .. }) = script_body(&checked).last() else {
        panic!("expected the script to end with an assignment");
    };
    let ExprKind::MethodCall { args, .. } = &value.kind else {
        panic!("expected a method call, got {:?}", value.kind);
    };
    let ExprKind::Tuple { elements } = &args[0].value.kind else {
        panic!("expected a tuple literal, got {:?}", args[0].value.kind);
    };
    let pet = package_leaf(&checked, "Pet");
    assert_eq!(elements[0].coercion, None);
    assert_eq!(elements[1].coercion, Some(Coercion::UnionWiden(pet)));
}

#[test]
fn tuple_value_argument_does_not_widen_in_a_generic_call() {
    // Only a tuple literal widens its elements. A tuple value of type
    // `(String, Cat)` is not a `(String, Pet)`.
    let source = pet_script(
        "
        entry = (\"tom\", tom)
        pets: List<(String, Pet)> = []
        pets = pets.append(entry)
        ",
    );
    let failure = typecheck_fail(&source);
    assert_eq!(
        diagnostic_messages(&failure),
        vec![
            "argument `item` to `List.append` expects `(String, TestApp.Pet)`, got \
             `(String, TestApp.Cat)`"
        ],
    );
}

const BOX_PRELUDE: &str = "
    struct Box<T>
      value: T
    end
    ";

/// [`pet_script`] with the generic struct `Box<T>` before the tail.
fn box_script(tail: &str) -> String {
    format!(
        "{}{}{}",
        dedent(PET_PRELUDE),
        dedent(BOX_PRELUDE),
        dedent(tail)
    )
}

/// `Box<arg>` for the struct of [`box_script`].
fn box_of(checked: &CheckedProgram, arg: ResolvedType) -> ResolvedType {
    ResolvedType::Named {
        resolution: Resolution::Global(registry_id(checked, PACKAGE, &["Box"])),
        type_args: vec![arg],
    }
}

/// A raw union. The members must already be in canonical order.
fn union_of(members: Vec<ResolvedType>) -> ResolvedType {
    ResolvedType::Anonymous(AnonymousKind::Union { members })
}

#[test]
fn struct_literal_takes_its_type_arguments_from_a_union_member() {
    // `Box<Int32>` is the one member that a `Box` literal can build,
    // so it sets `T` and the literal `3` is an `Int32`. The built
    // value then widens into the union.
    let source = box_script("boxed: String | Box<Int32> = Box{value: 3}\n");
    let checked = typecheck(&source);
    let value = last_assignment_value(&checked);
    let member = box_of(&checked, global_named(&checked, "Int32", Vec::new()));
    assert_eq!(value.resolution, member);
    assert_eq!(
        value.coercion,
        Some(Coercion::UnionWiden(union_of(vec![
            string_type(&checked),
            member
        ])))
    );
}

#[test]
fn variant_constructor_takes_its_type_arguments_from_a_union_member() {
    // The payload of `Result.Ok` cannot set `E`, and `Option.None`
    // has no payload. The member is the only source in each case.
    let source = pet_script(
        "
        done: Int | Result<Int, String> = Result.Ok(3)
        empty: String | Option<Int> = Option.None
        ",
    );
    let checked = typecheck(&source);
    let value = last_assignment_value(&checked);
    let member = global_named(&checked, "Option", vec![int_type(&checked)]);
    assert_eq!(value.resolution, member);
    assert_eq!(
        value.coercion,
        Some(Coercion::UnionWiden(union_of(vec![
            member,
            string_type(&checked)
        ])))
    );
}

#[test]
fn generic_call_takes_its_type_arguments_from_a_union_member() {
    // The return type of the call has the head of one member, so the
    // member sets the type parameters that no argument can set.
    let source = box_script(
        "
        fn none<T> -> Option<T>
          Option.None
        end

        fn wrap<T>(item: T) -> Box<T>
          Box{value: item}
        end

        found: String | Option<Int> = none()
        boxed: String | Box<Int32> = wrap(3)
        table: String | Map<String, Int> = Map.new()
        items: String | List<Int> = List.new()
        ",
    );
    let checked = typecheck(&source);
    let value = last_assignment_value(&checked);
    let member = global_named(&checked, "List", vec![int_type(&checked)]);
    assert_eq!(value.resolution, member);
    assert_eq!(
        value.coercion,
        Some(Coercion::UnionWiden(union_of(vec![
            member,
            string_type(&checked)
        ])))
    );
}

#[test]
fn union_member_sets_type_arguments_in_each_position_that_names_the_union() {
    let source = box_script(
        "
        type Slot = String | Box<Int32>
        type Tree = Int | Box<Tree>

        struct Shelf
          slot: Slot
        end

        fn make -> String | Box<Int32>
          Box{value: 3}
        end

        fn count(slot: String | Box<Int32>) -> Int
          1
        end

        count(Box{value: 3})
        shelf = Shelf{slot: Box{value: 3}}
        shelf.slot = Box{value: 4}
        slot: Slot = Box{value: 3}
        slot = Box{value: 5}
        slots: List<Slot> = [Box{value: 3}, \"a\"]
        home: String | Box<Pet> = Box{value: tom}
        tree: Tree = Box{value: 3}
        ",
    );
    let checked = typecheck(&source);
    // The member of a recursive alias names the alias, so the built
    // value is a `Box<Tree>` and the literal `3` widens into `Tree`.
    let value = last_assignment_value(&checked);
    let tree = package_leaf(&checked, "Tree");
    assert_eq!(value.resolution, box_of(&checked, tree.clone()));
    assert_eq!(value.coercion, Some(Coercion::UnionWiden(tree)));
}

#[test]
fn two_members_with_the_same_head_leave_the_values_to_decide() {
    let source = box_script("boxed: Box<Int> | Box<String> = Box{value: \"a\"}\n");
    let checked = typecheck(&source);
    assert_eq!(
        last_assignment_value(&checked).resolution,
        box_of(&checked, string_type(&checked)),
    );
}

#[test]
fn two_members_with_the_same_head_give_a_unit_variant_no_type_arguments() {
    let source = pet_script("empty: Option<Int> | Option<String> = Option.None\n");
    let failure = typecheck_fail(&source);
    assert_eq!(
        diagnostic_messages(&failure),
        vec!["typecheck cannot infer type parameter `T` of `Option` from unit variant `None`"],
    );
}

#[test]
fn union_with_no_member_of_the_built_type_keeps_the_mismatch() {
    let source = box_script("boxed: Int | String = Box{value: 3}\n");
    let failure = typecheck_fail(&source);
    assert_eq!(
        diagnostic_messages(&failure),
        vec![
            "type annotation on `boxed` says `Int | String`, but the right-hand side has type \
             `TestApp.Box<Int>`"
        ],
    );
}

#[test]
fn value_that_conflicts_with_the_union_member_reports_the_member() {
    // The member sets `T` to `Int32`, so the errors are the same as
    // for a position that expects `Box<Int32>` alone.
    let source = box_script("boxed: String | Box<Int32> = Box{value: \"a\"}\n");
    let failure = typecheck_fail(&source);
    assert_eq!(
        diagnostic_messages(&failure),
        vec![
            "type parameter `T` of `TestApp.Box` cannot be both `Int32` and `String`",
            "field `value` of `TestApp.Box` expects `Int32`, got `String`",
        ],
    );
}

/// The value of the assignment that ends the script.
fn last_assignment_value(checked: &CheckedProgram) -> &Expr {
    let Some(Statement::Assignment { value, .. }) = script_body(checked).last() else {
        panic!("expected the script to end with an assignment");
    };
    value
}

/// The elements of the tuple literal on the right of the
/// destructuring assignment that ends the script.
fn last_destructure_elements(checked: &CheckedProgram) -> &[Expr] {
    let Some(Statement::Destructure { value, .. }) = script_body(checked).last() else {
        panic!("expected the script to end with a destructuring assignment");
    };
    tuple_elements(value)
}

fn tuple_elements(expr: &Expr) -> &[Expr] {
    let ExprKind::Tuple { elements } = &expr.kind else {
        panic!("expected a tuple literal, got {:?}", expr.kind);
    };
    elements
}

/// The error for a write of `actual` to the local `name` of type
/// `declared`.
fn cannot_reassign(name: &str, declared: &str, actual: &str) -> String {
    format!(
        "cannot reassign `{name}` from `{declared}` to `{actual}` because local types are fixed \
         at declaration"
    )
}

#[test]
fn reassignment_widens_a_member_into_the_local() {
    let source = pet_script("pet = Dog{name: \"Rex\"}\n");
    let checked = typecheck(&source);
    let pet = package_leaf(&checked, "Pet");
    assert_eq!(
        last_assignment_value(&checked).coercion,
        Some(Coercion::UnionWiden(pet))
    );
}

#[test]
fn field_write_widens_a_member_into_the_field() {
    let source = pet_script(
        "
        struct Home
          pet: Pet
        end

        home = Home{pet: tom}
        home.pet = Dog{name: \"Rex\"}
        ",
    );
    let checked = typecheck(&source);
    let pet = package_leaf(&checked, "Pet");
    assert_eq!(
        last_assignment_value(&checked).coercion,
        Some(Coercion::UnionWiden(pet))
    );
}

#[test]
fn reassignment_widens_a_narrower_union() {
    let source = pet_script("wide = pet\n");
    let checked = typecheck(&source);
    let coercion = &last_assignment_value(&checked).coercion;
    assert!(
        matches!(coercion, Some(Coercion::UnionWiden(_))),
        "expected `pet` to widen into `Cat | Dog | Bird`, got {coercion:?}",
    );
}

#[test]
fn reassignment_widens_a_member_in_a_tuple_literal() {
    let source = pet_script(
        "
        pair: (Pet, Int) = (tom, 0)
        pair = (Dog{name: \"Rex\"}, 1)
        ",
    );
    let checked = typecheck(&source);
    let elements = tuple_elements(last_assignment_value(&checked));
    let pet = package_leaf(&checked, "Pet");
    assert_eq!(elements[0].coercion, Some(Coercion::UnionWiden(pet)));
    assert_eq!(elements[1].coercion, None);
}

#[test]
fn reassignment_does_not_narrow_a_union() {
    let source = pet_script("tom = pet\n");
    let failure = typecheck_fail(&source);
    assert_eq!(
        diagnostic_messages(&failure),
        vec![cannot_reassign("tom", "TestApp.Cat", "TestApp.Pet")],
    );
}

#[test]
fn reassignment_rejects_a_value_that_is_not_a_member() {
    let source = pet_script("pet = 1\n");
    let failure = typecheck_fail(&source);
    assert_eq!(
        diagnostic_messages(&failure),
        vec![cannot_reassign("pet", "TestApp.Pet", "Int")],
    );
}

#[test]
fn local_with_no_annotation_keeps_the_type_of_its_first_value() {
    // `tom` has the type `Cat`, not a union, so a `Dog` has no union
    // to widen into.
    let source = pet_script("tom = Dog{name: \"Rex\"}\n");
    let failure = typecheck_fail(&source);
    assert_eq!(
        diagnostic_messages(&failure),
        vec![cannot_reassign("tom", "TestApp.Cat", "TestApp.Dog")],
    );
}

#[test]
fn destructure_widens_a_member_from_a_tuple_literal() {
    let source = pet_script(
        "
        count = 0
        (pet, count) = (Dog{name: \"Rex\"}, 1)
        ",
    );
    let checked = typecheck(&source);
    let elements = last_destructure_elements(&checked);
    let pet = package_leaf(&checked, "Pet");
    assert_eq!(elements[0].coercion, Some(Coercion::UnionWiden(pet)));
    assert_eq!(elements[1].coercion, None);
}

#[test]
fn destructure_widens_a_narrower_union_from_a_tuple_literal() {
    let source = pet_script(
        "
        count = 0
        (wide, count) = (pet, 1)
        ",
    );
    let checked = typecheck(&source);
    let coercion = &last_destructure_elements(&checked)[0].coercion;
    assert!(
        matches!(coercion, Some(Coercion::UnionWiden(_))),
        "expected `pet` to widen into `Cat | Dog | Bird`, got {coercion:?}",
    );
}

#[test]
fn destructure_widens_next_to_a_new_name() {
    let source = pet_script(
        "
        (pet, fresh) = (Dog{name: \"Rex\"}, 1)
        fresh
        ",
    );
    let checked = typecheck(&source);
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn destructure_gives_a_new_name_the_type_of_its_element() {
    let source = pet_script(
        "
        count = 0
        (copy, count) = (pet, 1)
        copy
        ",
    );
    let checked = typecheck(&source);
    assert_eq!(trailing_resolution(&checked), package_leaf(&checked, "Pet"));
}

#[test]
fn destructure_widens_through_a_nested_tuple_literal() {
    let source = pet_script(
        "
        small: Int32 = 1
        count = 0
        ((pet, small), count) = ((Dog{name: \"Rex\"}, 7), 1)
        ",
    );
    let checked = typecheck(&source);
    let inner = tuple_elements(&last_destructure_elements(&checked)[0]);
    let pet = package_leaf(&checked, "Pet");
    assert_eq!(inner[0].coercion, Some(Coercion::UnionWiden(pet)));
    assert_eq!(
        inner[1].literal_coercion,
        Some(LiteralCoercion::NumericLiteralWidth(
            NumericLiteralWidth::Int32
        ))
    );
}

#[test]
fn destructure_widens_a_tuple_literal_into_a_tuple_local() {
    let source = pet_script(
        "
        pair: (Pet, Int) = (tom, 0)
        count = 0
        (pair, count) = ((Dog{name: \"Rex\"}, 1), 2)
        ",
    );
    let checked = typecheck(&source);
    let inner = tuple_elements(&last_destructure_elements(&checked)[0]);
    let pet = package_leaf(&checked, "Pet");
    assert_eq!(inner[0].coercion, Some(Coercion::UnionWiden(pet)));
}

#[test]
fn destructure_does_not_narrow_a_union() {
    let source = pet_script(
        "
        count = 0
        (tom, count) = (pet, 1)
        ",
    );
    let failure = typecheck_fail(&source);
    assert_eq!(
        diagnostic_messages(&failure),
        vec![cannot_reassign("tom", "TestApp.Cat", "TestApp.Pet")],
    );
}

#[test]
fn destructure_from_a_call_does_not_widen() {
    // Only a tuple literal pairs each name with an expression. A
    // call gives one value of type `(Dog, Int)`.
    let source = pet_script(
        "
        fn step -> (Dog, Int)
          (Dog{name: \"Rex\"}, 1)
        end

        count = 0
        (pet, count) = step()
        ",
    );
    let failure = typecheck_fail(&source);
    assert_eq!(
        diagnostic_messages(&failure),
        vec![cannot_reassign("pet", "TestApp.Pet", "TestApp.Dog")],
    );
}

#[test]
fn destructure_from_a_nested_tuple_value_does_not_widen() {
    let source = pet_script(
        "
        small: Int32 = 1
        count = 0
        inner = (Dog{name: \"Rex\"}, 7)
        ((pet, small), count) = (inner, 1)
        ",
    );
    let failure = typecheck_fail(&source);
    assert_eq!(
        diagnostic_messages(&failure),
        vec![
            cannot_reassign("pet", "TestApp.Pet", "TestApp.Dog"),
            cannot_reassign("small", "Int32", "Int"),
        ],
    );
}

#[test]
fn destructure_from_arms_does_not_widen() {
    let source = pet_script(
        "
        flag = true
        count = 0
        (pet, count) = flag ? (Dog{name: \"Rex\"}, 1) : (tom, 2)
        ",
    );
    assert_arms_inconsistent(
        &source,
        "then=`(TestApp.Dog, Int)`, else=`(TestApp.Cat, Int)`",
    );
}
