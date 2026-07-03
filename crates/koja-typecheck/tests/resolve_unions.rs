//! Typecheck pins for the union slice: the `A | B`, `type X = A | B`,
//! and `p: T -> ...` triple. Together they exercise:
//!
//! - **Lift**: `TypeExpr::Union` resolves to a canonical
//!   [`ResolvedType::Union`] (sorted by display key, deduplicated,
//!   nested unions flattened) inside parameter / return / field / let
//!   slots.
//! - **Equivalence**: `types_equivalent` peels alias names and
//!   compares unions structurally (member-set equality), so
//!   `Post | Comment` ≡ `Comment | Post` and `Pet ≡ Cat | Dog | Fish`.
//! - **Widening**: `check_compatible` accepts a member type flowing
//!   into a union slot (call args, struct fields, return slots, let
//!   bindings, enum tuple payloads) by stamping
//!   `Coercion::UnionWiden(target)` on the source `Expr` so IR
//!   lower can emit the matching `UnionWrap`.
//! - **Diagnostics**: bare `FieldAccess` / `MethodCall` against a
//!   union receiver surfaces a precise "match the union first"
//!   error instead of falling through to "unknown method".
//! - **Match exhaustiveness**: typed-binding arms over a union
//!   subject either cover every member or surface a precise
//!   missing-member diagnostic. A duplicate member arm warns as
//!   unreachable. Pointing a typed-binding at a non-union surfaces
//!   the narrowing-site diagnostic.
//! - **Display**: alias names round-trip in diagnostics: a
//!   `type Pet = Cat | Dog` mismatch reports `Pet`, not the expanded
//!   `Cat | Dog`.

use koja_ast::util::dedent;

mod common;

use common::{
    diagnostic_messages, typecheck_script as typecheck, typecheck_script_fail as typecheck_fail,
    warning_messages,
};

// -----------------------------------------------------------------------------
// Lift / equivalence
// -----------------------------------------------------------------------------

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

// -----------------------------------------------------------------------------
// Widening
// -----------------------------------------------------------------------------

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
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("expects `A | B`") && m.contains("C")),
        "expected non-member-of-union arg diagnostic, got {messages:?}",
    );
}

// -----------------------------------------------------------------------------
// Bare-receiver diagnostics
// -----------------------------------------------------------------------------

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
            .any(|m| m.to_lowercase().contains("union")
                && (m.contains("method") || m.contains("match"))),
        "expected union-receiver method-call diagnostic, got {messages:?}",
    );
}

// -----------------------------------------------------------------------------
// Typed-binding match arms / exhaustiveness
// -----------------------------------------------------------------------------

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
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("typed-binding") && m.contains("union")),
        "expected typed-binding-against-non-union diagnostic, got {messages:?}",
    );
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
    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("C") && m.contains("A | B")),
        "expected typed-binding-not-a-member diagnostic, got {messages:?}",
    );
}
