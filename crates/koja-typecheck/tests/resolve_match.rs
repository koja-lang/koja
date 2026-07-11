//! Typecheck coverage for `match`. Pin the contract:
//!
//! - subject + arm bodies resolve under the same rules as anywhere else
//! - the surface expression's type is the join of every reaching arm tail
//!   (with `Never` as the lattice bottom: divergent arms don't constrain it)
//! - a wildcard / binding catch-all is required, except for enum subjects
//!   with full structural variant coverage and `Bool` subjects with both
//!   `true` and `false` literal coverage
//! - missing-variant and missing-catch-all errors carry suggestion hints
//! - `pattern when expr -> body` guards resolve `expr` against `Bool` in
//!   the post-pattern-bind scope, and a guarded arm does not contribute
//!   to catch-all detection or enum variant coverage
//! - struct destructure patterns (`Type{ field: x, ... }` for plain
//!   structs, `Type.Variant{ field: x, ... }` for struct-variant enums)
//!   resolve each named field against the declared roster, accept any
//!   nested pattern shape (wildcard / binding / literal / nested
//!   struct / nested enum / or-alternatives), and stamp a `LocalId`
//!   on every binding. Omitted fields are implicit wildcards. The arm
//!   counts as a catch-all only when every listed field's own
//!   coverage is catch-all
//! - constructor shorthand (`Some(x)` / `None` against an enum subject)
//!   rewrites in place to the corresponding `EnumTuple` / `EnumUnit`,
//!   reusing every downstream invariant
//! - unsupported pattern shapes (`Pattern::List`, `Pattern::TypedBinding`,
//!   `Pattern::Binary`) and literal patterns over non-primitive subjects
//!   diagnose feature gaps
//! - bindings stamp a `LocalId` on the AST node
//! - reachability/redundancy fires warning-severity diagnostics for arms
//!   following an unguarded catch-all, duplicate enum-variant or literal
//!   arms, and overlapping alternatives within an or-pattern

use koja_ast::ast::{ExprKind, Item, Pattern, Statement};
use koja_ast::identifier::{Identifier, Resolution, ResolvedType};
use koja_ast::util::dedent;
use koja_typecheck::CheckedProgram;

mod common;

use common::{
    PACKAGE, typecheck_script as typecheck, typecheck_script_fail as typecheck_fail,
    warning_messages,
};

fn script_body(checked: &CheckedProgram) -> &[Statement] {
    let pkg = checked
        .packages
        .iter()
        .find(|p| p.package == PACKAGE)
        .expect("checked program is missing the test package");
    let file = pkg.files.first().expect("package has no files");
    file.body
        .as_deref()
        .expect("script-mode file must keep statements on File.body")
}

fn trailing_resolution(checked: &CheckedProgram) -> ResolvedType {
    let trailing = script_body(checked)
        .last()
        .expect("expected at least one statement");
    match trailing {
        Statement::Expr(expr) => expr.resolution.clone(),
        other => panic!("expected Statement::Expr as trailing statement, got {other:?}"),
    }
}

fn primitive_type(checked: &CheckedProgram, name: &str) -> ResolvedType {
    let ident = Identifier::new("Global", vec![name.to_string()]);
    let (id, _) = checked
        .registry
        .lookup(&ident)
        .unwrap_or_else(|| panic!("stdlib stub `Global.{name}` missing from registry"));
    ResolvedType::leaf(Resolution::Global(id))
}

fn int_type(checked: &CheckedProgram) -> ResolvedType {
    primitive_type(checked, "Int")
}

fn string_type(checked: &CheckedProgram) -> ResolvedType {
    primitive_type(checked, "String")
}

fn assert_missing_variant(source: &str, variant: &str) {
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure.diagnostics.iter().any(|diagnostic| {
            diagnostic.message.contains("not exhaustive") && diagnostic.message.contains(variant)
        }),
        "expected missing-{variant} diagnostic, got: {:?}",
        failure.diagnostics,
    );
}

#[test]
fn match_int_literal_arms_resolve_to_int() {
    let source = "
          match 1
            1 -> 10
            2 -> 20
            _ -> 30
          end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn match_string_literal_arms_resolve_to_int() {
    let source = "
          match \"hi\"
            \"hi\" -> 1
            _ -> 0
          end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn match_binding_arm_resolves_to_subject_type() {
    let source = "
          match 7
            x -> x
          end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn match_binding_stamps_local_id() {
    let source = "
          match 7
            x -> x
          end
        ";
    let checked = typecheck(&dedent(source));
    let Statement::Expr(match_expr) = script_body(&checked)
        .last()
        .expect("missing trailing match-expr")
    else {
        panic!("expected trailing Statement::Expr");
    };
    let ExprKind::Match { arms, .. } = &match_expr.kind else {
        panic!("expected ExprKind::Match");
    };
    let Pattern::Binding { local_id, name, .. } = &arms[0].pattern else {
        panic!("expected Pattern::Binding for arm 0");
    };
    assert_eq!(name, "x");
    assert!(
        local_id.is_some(),
        "binding `x` should carry a stamped LocalId"
    );
}

#[test]
fn match_with_diverging_arms_resolves_to_else_type() {
    let source = "
        fn pick -> Int
          match 1
            1 -> return 10
            _ -> 20
          end
        end

          pick()
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn match_without_catch_all_diagnoses() {
    let source = "
          match 1
            1 -> 10
            2 -> 20
          end
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("must include a wildcard")),
        "expected catch-all diagnostic, got: {:?}",
        failure.diagnostics,
    );
}

#[test]
fn match_with_mismatched_arms_diagnoses() {
    let source = "
          match 1
            1 -> 10
            _ -> false
          end
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("match arms have inconsistent types")),
        "expected mismatch diagnostic, got: {:?}",
        failure.diagnostics,
    );
}

#[test]
fn match_with_literal_type_mismatch_diagnoses() {
    let source = "
          match 1
            \"hi\" -> 10
            _ -> 20
          end
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("does not match subject type")),
        "expected literal-vs-subject diagnostic, got: {:?}",
        failure.diagnostics,
    );
}

#[test]
fn match_guard_resolves_against_pattern_locals() {
    let source = "
          match 7
            x when x > 0 -> 10
            _ -> 20
          end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
    let Statement::Expr(match_expr) = script_body(&checked).last().unwrap() else {
        panic!("expected trailing Statement::Expr");
    };
    let ExprKind::Match { arms, .. } = &match_expr.kind else {
        panic!("expected ExprKind::Match");
    };
    let Pattern::Binding { local_id, .. } = &arms[0].pattern else {
        panic!("expected Pattern::Binding for arm 0");
    };
    let binding_id = local_id.expect("binding should carry a stamped LocalId");
    let guard = arms[0].guard.as_ref().expect("arm 0 should carry a guard");
    let ExprKind::Binary { left, .. } = &guard.kind else {
        panic!("expected guard to be a Binary expression");
    };
    let ExprKind::Ident { resolution, .. } = &left.kind else {
        panic!("expected guard's left operand to be an Ident");
    };
    assert_eq!(
        *resolution,
        Resolution::Local(binding_id),
        "guard ident `x` should resolve to the arm's pattern binding",
    );
}

#[test]
fn match_guard_non_bool_diagnoses() {
    let source = "
          match 1
            x when x -> 10
            _ -> 20
          end
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure.diagnostics.iter().any(|d| d
            .message
            .contains("`match arm guard` condition must be `Bool`")),
        "expected non-Bool guard diagnostic, got: {:?}",
        failure.diagnostics,
    );
}

#[test]
fn match_guarded_catch_all_does_not_close_chain() {
    let source = "
          match 1
            _ when 1 > 0 -> 10
          end
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure.diagnostics.iter().any(|d| d
            .message
            .contains("must include a wildcard `_` or binding catch-all arm")),
        "expected missing-catch-all diagnostic, got: {:?}",
        failure.diagnostics,
    );
}

#[test]
fn match_guarded_enum_arm_does_not_count_as_coverage() {
    let source = "
        enum Color
          Red
          Green
        end

        fn pick(c: Color) -> Int
          match c
            Color.Red when 1 > 0 -> 1
            Color.Green -> 2
          end
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("not exhaustive") && d.message.contains("Red")),
        "expected missing-Red diagnostic, got: {:?}",
        failure.diagnostics,
    );
}

#[test]
fn match_string_subject_resolves() {
    let source = "
          match \"hi\"
            \"hi\" -> \"yes\"
            _ -> \"no\"
          end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), string_type(&checked));
}

#[test]
fn match_unit_only_enum_no_catch_all_resolves() {
    let source = "
        enum Color
          Red
          Green
        end

        fn pick(c: Color) -> Int
          match c
            Color.Red -> 1
            Color.Green -> 2
          end
        end

          pick(Color.Red)
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn match_enum_missing_variant_diagnoses() {
    let source = "
        enum Color
          Red
          Green
          Blue
        end

        fn pick(c: Color) -> Int
          match c
            Color.Red -> 1
            Color.Green -> 2
          end
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("not exhaustive") && d.message.contains("Blue")),
        "expected missing-variant diagnostic, got: {:?}",
        failure.diagnostics,
    );
}

#[test]
fn match_enum_tuple_binding_stamps_local_id() {
    let source = "
        enum Box
          Some(Int)
          None
        end

        fn unwrap(b: Box) -> Int
          match b
            Box.Some(x) -> x
            Box.None -> 0
          end
        end

          unwrap(Box.Some(7))
        ";
    let checked = typecheck(&dedent(source));
    let pkg = checked
        .packages
        .iter()
        .find(|p| p.package == PACKAGE)
        .expect("missing test package");
    let file = pkg.files.first().expect("package has no files");
    let unwrap_fn = file
        .items
        .iter()
        .find_map(|item| match item {
            Item::Function(f) if f.name == "unwrap" => Some(f),
            _ => None,
        })
        .expect("missing fn unwrap");
    let body = unwrap_fn.body.as_deref().expect("missing fn unwrap body");
    let Statement::Expr(match_expr) = body.last().expect("missing trailing match") else {
        panic!("expected trailing Statement::Expr");
    };
    let ExprKind::Match { arms, .. } = &match_expr.kind else {
        panic!("expected ExprKind::Match");
    };
    let Pattern::EnumTuple { elements, .. } = &arms[0].pattern else {
        panic!("expected Pattern::EnumTuple for arm 0");
    };
    let Pattern::Binding { local_id, name, .. } = &elements[0] else {
        panic!("expected Pattern::Binding inside the tuple");
    };
    assert_eq!(name, "x");
    assert!(
        local_id.is_some(),
        "payload binding `x` should carry a stamped LocalId"
    );
}

#[test]
fn match_enum_tuple_arity_mismatch_diagnoses() {
    let source = "
        enum Box
          Pair(Int, Int)
          None
        end

        fn first(b: Box) -> Int
          match b
            Box.Pair(x) -> x
            Box.None -> 0
          end
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("expects 2 positional element")),
        "expected tuple arity diagnostic, got: {:?}",
        failure.diagnostics,
    );
}

#[test]
fn match_or_of_strings_resolves() {
    let source = "
          match \"a\"
            \"a\" | \"b\" | \"c\" -> 1
            _ -> 0
          end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn match_or_of_enum_units_resolves() {
    let source = "
        enum Lifecycle
          Shutdown
          Interrupt
          Reload
        end

        fn classify(e: Lifecycle) -> Int
          match e
            Lifecycle.Shutdown | Lifecycle.Interrupt -> 0
            Lifecycle.Reload -> 1
          end
        end

          classify(Lifecycle.Reload)
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn match_or_with_binding_diagnoses() {
    let source = "
          match 1
            x | 2 -> 0
            _ -> 0
          end
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure.diagnostics.iter().any(|d| d
            .message
            .contains("only admits literal / enum-unit alternatives")),
        "expected or-with-binding diagnostic, got: {:?}",
        failure.diagnostics,
    );
}

#[test]
fn match_arm_binding_does_not_leak_to_following_arm() {
    let source = "
          match 1
            x -> 0
            y -> y
          end
        ";
    let checked = typecheck(&dedent(source));
    let Statement::Expr(match_expr) = script_body(&checked)
        .last()
        .expect("missing trailing match")
    else {
        panic!("expected trailing Statement::Expr");
    };
    let ExprKind::Match { arms, .. } = &match_expr.kind else {
        panic!("expected ExprKind::Match");
    };
    let Pattern::Binding { local_id: x_id, .. } = &arms[0].pattern else {
        panic!("arm 0 should be a binding");
    };
    let Pattern::Binding { local_id: y_id, .. } = &arms[1].pattern else {
        panic!("arm 1 should be a binding");
    };
    assert!(x_id.is_some(), "binding `x` should have a LocalId");
    assert!(y_id.is_some(), "binding `y` should have a LocalId");
    assert_ne!(
        x_id, y_id,
        "per-arm bindings must mint distinct LocalIds (the snapshot/restore unwound `x`)",
    );
}

#[test]
fn match_struct_destructure_binds_resolve_against_field_types() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

          match Point{x: 1, y: 2}
            Point{x: a, y: b} -> a + b
          end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
    let Statement::Expr(match_expr) = script_body(&checked).last().unwrap() else {
        panic!("expected trailing Statement::Expr");
    };
    let ExprKind::Match { arms, .. } = &match_expr.kind else {
        panic!("expected ExprKind::Match");
    };
    let Pattern::Struct { fields, .. } = &arms[0].pattern else {
        panic!("expected Pattern::Struct for arm 0");
    };
    assert_eq!(fields.len(), 2);
    for field in fields {
        let Pattern::Binding { local_id, .. } = &field.pattern else {
            panic!("expected Pattern::Binding inside the struct destructure");
        };
        assert!(
            local_id.is_some(),
            "field-pattern binding `{}` should carry a stamped LocalId",
            field.name,
        );
    }
}

#[test]
fn match_struct_destructure_unknown_field_diagnoses() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

          match Point{x: 1, y: 2}
            Point{z: a} -> a
          end
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("has no field `z`")),
        "expected unknown-field diagnostic, got: {:?}",
        failure.diagnostics,
    );
}

#[test]
fn match_struct_destructure_literal_field_resolves() {
    // Inverted from the old "non-binding field diagnoses": literal
    // patterns inside struct fields are accepted post Phase 4
    // follow-on. The arm's coverage is `Other` (not `CatchAll`) so
    // the outer match needs an explicit catch-all to typecheck.
    let source = "
        struct Point
          x: Int
          y: Int
        end

          match Point{x: 1, y: 2}
            Point{x: 1, y: b} -> b
            _other -> 0
          end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
    let Statement::Expr(match_expr) = script_body(&checked).last().unwrap() else {
        panic!("expected trailing Statement::Expr");
    };
    let ExprKind::Match { arms, .. } = &match_expr.kind else {
        panic!("expected ExprKind::Match");
    };
    let Pattern::Struct { fields, .. } = &arms[0].pattern else {
        panic!("expected Pattern::Struct for arm 0");
    };
    assert_eq!(fields.len(), 2);
    assert!(
        matches!(fields[0].pattern, Pattern::Literal { .. }),
        "field `x` should preserve the literal pattern shape",
    );
    let Pattern::Binding { local_id: y_id, .. } = &fields[1].pattern else {
        panic!("field `y` should be a binding");
    };
    assert!(y_id.is_some(), "binding `y` should have a stamped LocalId");
}

#[test]
fn match_struct_partial_omitted_fields_match_anything() {
    // `Point{x: 5}` lists only `x`, so `y` is an implicit wildcard.
    // The empty `Point{}` arm is the explicit full catch-all.
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn classify(p: Point) -> Int
          match p
            Point{x: 5} -> 1
            Point{y: 9} -> 2
            Point{} -> 3
          end
        end

          classify(Point{x: 5, y: 1})
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn match_nested_struct_inside_enum_tuple_resolves() {
    // `Option.Some(Point{x: 5})` nests a partial struct pattern
    // inside the tuple payload of an enum variant. Resolver must
    // recurse through both layers with the correct types.
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn label(op: Option<Point>) -> Int
          match op
            Option.Some(Point{x: 5}) -> 1
            Option.Some(Point{x: x, y: y}) -> x + y
            Option.None -> 0
          end
        end

          label(Option.Some(Point{x: 5, y: 9}))
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn match_enum_tuple_literal_payload_resolves() {
    // String + int literal payloads inside enum tuple patterns.
    // Mirrors the `nested_enum_pattern_literal` lang golden's
    // outer/inner discrimination shape.
    let source = "
        enum TokenKind
          Ident(String)
          Number(Int)
        end

        fn classify(t: TokenKind) -> Int
          match t
            TokenKind.Ident(\"and\") -> 1
            TokenKind.Ident(_n) -> 2
            TokenKind.Number(0) -> 3
            TokenKind.Number(_v) -> 4
          end
        end

          classify(TokenKind.Ident(\"and\"))
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn match_enum_tuple_with_literal_payload_requires_full_variant_coverage() {
    let source = "
        fn classify(op: Option<Int>) -> Int
          match op
            Option.Some(5) -> 1
            Option.None -> 0
          end
        end

          classify(Option.Some(5))
        ";
    assert_missing_variant(source, "Some");
}

#[test]
fn match_enum_struct_with_literal_field_requires_full_variant_coverage() {
    let source = "
        enum Shape
          Rect{w: Int, h: Int}
          Circle{r: Int}
        end

        fn classify(s: Shape) -> Int
          match s
            Shape.Rect{w: 1, h: 2} -> 10
            Shape.Circle{r: r} -> r
          end
        end

          classify(Shape.Circle{r: 7})
        ";
    assert_missing_variant(source, "Rect");
}

#[test]
fn match_nested_enum_payload_requires_full_outer_variant_coverage() {
    let source = "
        enum Color
          Red
          Green
        end

        fn classify(op: Option<Color>) -> Int
          match op
            Option.Some(Color.Red) -> 1
            Option.None -> 0
          end
        end

          classify(Option.Some(Color.Green))
        ";
    assert_missing_variant(source, "Some");
}

#[test]
fn match_guarded_full_payload_does_not_exhaust_outer_variant() {
    let source = "
        fn classify(op: Option<Int>) -> Int
          match op
            Option.Some(value) when value > 0 -> value
            Option.None -> 0
          end
        end

          classify(Option.Some(1))
        ";
    assert_missing_variant(source, "Some");
}

#[test]
fn match_some_with_distinct_enum_payloads_does_not_warn_unreachable() {
    // Narrowing inner patterns under the same outer variant
    // (`Some(Color.Red)` / `Some(Color.Green)`) used to trip the
    // cross-arm reachability check because variant coverage only
    // tracked the outer tag.
    let source = "
        enum Color
          Red
          Green
        end

        fn classify(op: Option<Color>) -> Int
          match op
            Option.Some(Color.Red) -> 1
            Option.Some(Color.Green) -> 2
            Option.None -> 0
            _ -> 99
          end
        end

          classify(Option.Some(Color.Red))
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
    let warnings = warning_messages(&checked);
    assert!(
        warnings.is_empty(),
        "narrowing arms under the same outer variant should not warn, got: {warnings:?}",
    );
}

#[test]
fn match_some_binding_then_none_exhausts_without_catch_all() {
    let source = "
        fn classify(op: Option<Int>) -> Int
          match op
            Option.Some(_) -> 1
            Option.None -> 0
          end
        end

          classify(Option.None)
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
    let warnings = warning_messages(&checked);
    assert!(
        warnings.is_empty(),
        "Some(_) + None should be exhaustive without warnings, got: {warnings:?}",
    );
}

#[test]
fn match_some_binding_then_narrow_some_warns_unreachable() {
    // `Some(x)` is a full witness for `Some` (binding is a
    // catch-all), so the narrower `Some(Color.Red)` arm is dead.
    let source = "
        enum Color
          Red
          Green
        end

        fn classify(op: Option<Color>) -> Int
          match op
            Option.Some(x) -> 1
            Option.Some(Color.Red) -> 2
            _ -> 0
          end
        end

          classify(Option.Some(Color.Red))
        ";
    let checked = typecheck(&dedent(source));
    let warnings = warning_messages(&checked);
    assert!(
        warnings
            .iter()
            .any(|m| m.contains("unreachable") && m.contains("variant")),
        "expected unreachable-variant warning after Some(x), got: {warnings:?}",
    );
}

#[test]
fn match_distinct_some_literal_payloads_require_full_variant_coverage() {
    // `Some(1)` and `Some(2)` narrow on disjoint primitive
    // literals but do not cover every possible `Some`.
    let source = "
        fn classify(op: Option<Int>) -> Int
          match op
            Option.Some(1) -> 1
            Option.Some(2) -> 2
            Option.None -> 0
          end
        end

          classify(Option.Some(1))
        ";
    assert_missing_variant(source, "Some");
}

#[test]
fn match_enum_tuple_with_bindings_only_still_covers_variant() {
    // `Option.Some(x)` with a plain binding does cover every
    // Some, since bindings are catch-alls. The match is exhaustive
    // without an extra catch-all arm.
    let source = "
        fn classify(op: Option<Int>) -> Int
          match op
            Option.Some(x) -> x
            Option.None -> 0
          end
        end

          classify(Option.Some(5))
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn match_struct_with_only_literal_fields_is_not_catch_all() {
    // `Point{x: 5, y: 2}` is no longer treated as a catch-all
    // (Phase 3 coverage refinement). With no explicit catch-all the
    // match must diagnose missing coverage.
    let source = "
        struct Point
          x: Int
          y: Int
        end

          match Point{x: 5, y: 2}
            Point{x: 5, y: 2} -> 1
          end
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("catch-all") || d.message.contains("exhaustive")),
        "expected missing-catch-all diagnostic, got: {:?}",
        failure.diagnostics,
    );
}

#[test]
fn match_enum_struct_destructure_binds_resolve() {
    let source = "
        enum Shape
          Rect{w: Int, h: Int}
          Circle{r: Int}
        end

        fn area(s: Shape) -> Int
          match s
            Shape.Rect{w: w, h: h} -> w * h
            Shape.Circle{r: r} -> r * r
          end
        end

          area(Shape.Rect{w: 3, h: 4})
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn match_enum_struct_destructure_against_tuple_variant_diagnoses() {
    let source = "
        enum Box
          Some(Int)
          None
        end

          match Box.Some(7)
            Box.Some{x: x} -> x
            Box.None -> 0
          end
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("not a struct variant")),
        "expected struct-vs-tuple variant diagnostic, got: {:?}",
        failure.diagnostics,
    );
}

#[test]
fn match_struct_destructure_acts_as_catch_all() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

          match Point{x: 1, y: 2}
            Point{x: a, y: _} -> a
          end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

// --- Phase 5: reachability / hint / Bool exhaustiveness ---

#[test]
fn match_arm_after_catch_all_warns_unreachable() {
    let source = "
          match 1
            _ -> 10
            2 -> 20
          end
        ";
    let checked = typecheck(&dedent(source));
    let warnings = warning_messages(&checked);
    assert!(
        warnings
            .iter()
            .any(|m| m.contains("unreachable") && m.contains("matches every value")),
        "expected arm-after-catch-all warning, got: {warnings:?}",
    );
}

#[test]
fn match_duplicate_literal_arm_warns_unreachable() {
    let source = "
          match 1
            1 -> 10
            1 -> 11
            _ -> 20
          end
        ";
    let checked = typecheck(&dedent(source));
    let warnings = warning_messages(&checked);
    assert!(
        warnings
            .iter()
            .any(|m| m.contains("unreachable") && m.contains("literal")),
        "expected duplicate-literal warning, got: {warnings:?}",
    );
}

#[test]
fn match_duplicate_enum_variant_arm_warns_unreachable() {
    let source = "
        enum Color
          Red
          Green
        end

        fn pick(c: Color) -> Int
          match c
            Color.Red -> 1
            Color.Red -> 11
            Color.Green -> 2
          end
        end

          pick(Color.Red)
        ";
    let checked = typecheck(&dedent(source));
    let warnings = warning_messages(&checked);
    assert!(
        warnings
            .iter()
            .any(|m| m.contains("unreachable") && m.contains("variant")),
        "expected duplicate-variant warning, got: {warnings:?}",
    );
}

#[test]
fn match_overlapping_or_alternative_warns_unreachable() {
    let source = "
          match 1
            1 | 1 -> 10
            _ -> 20
          end
        ";
    let checked = typecheck(&dedent(source));
    let warnings = warning_messages(&checked);
    assert!(
        warnings
            .iter()
            .any(|m| m.contains("or-pattern alternative is unreachable")),
        "expected overlapping or-alternative warning, got: {warnings:?}",
    );
}

#[test]
fn match_multiple_catch_alls_warns_on_each_extra() {
    let source = "
          match 1
            _ -> 10
            _ -> 20
            _ -> 30
          end
        ";
    let checked = typecheck(&dedent(source));
    let warnings = warning_messages(&checked);
    let unreachable_count = warnings
        .iter()
        .filter(|m| m.contains("unreachable") && m.contains("matches every value"))
        .count();
    assert_eq!(
        unreachable_count, 2,
        "expected one warning per extra catch-all, got: {warnings:?}",
    );
}

#[test]
fn match_missing_variant_diagnostic_carries_hint() {
    let source = "
        enum Color
          Red
          Green
          Blue
        end

        fn pick(c: Color) -> Int
          match c
            Color.Red -> 1
            Color.Green -> 2
          end
        end

          pick(Color.Red)
        ";
    let failure = typecheck_fail(&dedent(source));
    let with_hint = failure
        .diagnostics
        .iter()
        .find(|d| d.message.contains("not exhaustive") && d.message.contains("Blue"));
    let diag = with_hint.expect("expected missing-variant diagnostic");
    let hint = diag
        .hint
        .as_deref()
        .expect("missing-variant diagnostic should carry a hint");
    assert!(
        hint.contains("`_ -> ...`") && hint.contains("Blue"),
        "hint should suggest catch-all and list the missing variant, got: {hint:?}",
    );
}

#[test]
fn match_missing_catch_all_diagnostic_carries_hint_with_subject_type() {
    let source = "
          match 1
            1 -> 10
            2 -> 20
          end
        ";
    let failure = typecheck_fail(&dedent(source));
    let with_hint = failure
        .diagnostics
        .iter()
        .find(|d| d.message.contains("must include a wildcard"));
    let diag = with_hint.expect("expected missing-catch-all diagnostic");
    let hint = diag
        .hint
        .as_deref()
        .expect("missing-catch-all diagnostic should carry a hint");
    assert!(
        hint.contains("`Int`") && hint.contains("`_ -> ...`"),
        "hint should mention the subject type and catch-all syntax, got: {hint:?}",
    );
}

#[test]
fn match_bool_exhaustive_without_catch_all_typechecks() {
    let source = "
          match true
            true -> 1
            false -> 0
          end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
    let warnings = warning_messages(&checked);
    assert!(
        warnings.is_empty(),
        "exhaustive Bool match should not emit warnings, got: {warnings:?}",
    );
}

#[test]
fn match_bool_only_true_arm_still_requires_catch_all() {
    let source = "
          match true
            true -> 1
          end
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("must include a wildcard")),
        "expected missing-catch-all diagnostic for partial Bool match, got: {:?}",
        failure.diagnostics,
    );
}

// --- Constructor shorthand + deferred-shape feature gaps ---

#[test]
fn match_constructor_unit_resolves() {
    let source = "
        enum Box
          Some(Int)
          None
        end

        fn pick(b: Box) -> Int
          match b
            None -> 0
            Box.Some(x) -> x
          end
        end

          pick(Box.None)
        ";
    let checked = typecheck(&dedent(source));
    let pkg = checked
        .packages
        .iter()
        .find(|p| p.package == PACKAGE)
        .expect("missing test package");
    let file = pkg.files.first().expect("package has no files");
    let pick_fn = file
        .items
        .iter()
        .find_map(|item| match item {
            Item::Function(f) if f.name == "pick" => Some(f),
            _ => None,
        })
        .expect("missing fn pick");
    let body = pick_fn.body.as_deref().expect("missing fn pick body");
    let Statement::Expr(match_expr) = body.last().expect("missing trailing match") else {
        panic!("expected trailing Statement::Expr");
    };
    let ExprKind::Match { arms, .. } = &match_expr.kind else {
        panic!("expected ExprKind::Match");
    };
    assert!(
        matches!(arms[0].pattern, Pattern::EnumUnit { .. }),
        "expected `None` constructor to be rewritten to Pattern::EnumUnit, got: {:?}",
        arms[0].pattern,
    );
}

#[test]
fn match_constructor_tuple_resolves_and_binds() {
    let source = "
        enum Box
          Some(Int)
          None
        end

        fn unwrap(b: Box) -> Int
          match b
            Some(x) -> x
            None -> 0
          end
        end

          unwrap(Box.Some(7))
        ";
    let checked = typecheck(&dedent(source));
    let pkg = checked
        .packages
        .iter()
        .find(|p| p.package == PACKAGE)
        .expect("missing test package");
    let file = pkg.files.first().expect("package has no files");
    let unwrap_fn = file
        .items
        .iter()
        .find_map(|item| match item {
            Item::Function(f) if f.name == "unwrap" => Some(f),
            _ => None,
        })
        .expect("missing fn unwrap");
    let body = unwrap_fn.body.as_deref().expect("missing fn unwrap body");
    let Statement::Expr(match_expr) = body.last().expect("missing trailing match") else {
        panic!("expected trailing Statement::Expr");
    };
    let ExprKind::Match { arms, .. } = &match_expr.kind else {
        panic!("expected ExprKind::Match");
    };
    let Pattern::EnumTuple { elements, .. } = &arms[0].pattern else {
        panic!(
            "expected `Some(x)` constructor to rewrite to Pattern::EnumTuple, got: {:?}",
            arms[0].pattern,
        );
    };
    let Pattern::Binding { local_id, name, .. } = &elements[0] else {
        panic!("expected payload binding inside the rewritten EnumTuple");
    };
    assert_eq!(name, "x");
    assert!(
        local_id.is_some(),
        "constructor-shorthand payload binding `x` should carry a stamped LocalId",
    );
}

#[test]
fn match_constructor_unknown_variant_diagnoses() {
    let source = "
        enum Box
          Some(Int)
          None
        end

        fn unwrap(b: Box) -> Int
          match b
            Foo(x) -> x
            _ -> 0
          end
        end

          unwrap(Box.None)
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("has no variant `Foo`")),
        "expected unknown-variant diagnostic, got: {:?}",
        failure.diagnostics,
    );
}

#[test]
fn match_constructor_arity_mismatch_diagnoses() {
    let source = "
        enum Box
          Some(Int)
          None
        end

        fn unwrap(b: Box) -> Int
          match b
            Some(x, y) -> x + y
            _ -> 0
          end
        end

          unwrap(Box.None)
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("expects 1 positional element")),
        "expected arity-mismatch diagnostic, got: {:?}",
        failure.diagnostics,
    );
}

#[test]
fn match_constructor_struct_variant_diagnoses() {
    let source = "
        enum Shape
          Rect{w: Int, h: Int}
          Circle(Int)
        end

        fn area(s: Shape) -> Int
          match s
            Rect(w) -> w
            _ -> 0
          end
        end

          area(Shape.Circle(3))
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("is a struct variant")
                && d.message.contains("Shape.Rect{...}")),
        "expected struct-variant shorthand redirect diagnostic, got: {:?}",
        failure.diagnostics,
    );
}

#[test]
fn match_constructor_unit_variant_with_payload_diagnoses() {
    let source = "
        enum Box
          Some(Int)
          None
        end

        fn unwrap(b: Box) -> Int
          match b
            None(x) -> x
            _ -> 0
          end
        end

          unwrap(Box.None)
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("is a unit variant and takes no payload")),
        "expected unit-variant-with-payload diagnostic, got: {:?}",
        failure.diagnostics,
    );
}

#[test]
fn match_constructor_non_enum_subject_diagnoses() {
    let source = "
          match 1
            Some(x) -> x
            _ -> 0
          end
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("requires an enum subject")),
        "expected non-enum-subject diagnostic, got: {:?}",
        failure.diagnostics,
    );
}

#[test]
fn match_list_pattern_still_diagnoses_feature_gap() {
    let source = "
          match 1
            [a, b] -> a + b
            _ -> 0
          end
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure
            .diagnostics
            .iter()
            .any(|d| d.message.contains("does not yet support list patterns")),
        "expected list-pattern feature-gap diagnostic, got: {:?}",
        failure.diagnostics,
    );
}

#[test]
fn match_typed_binding_against_non_union_diagnoses() {
    // Typed-binding patterns (`p: T -> ...`) only narrow over union
    // subjects. Pointing one at a non-union (here: `Int`) emits a
    // precise diagnostic naming the actual subject type rather than
    // the legacy "feature gap" stub the resolver carried
    // before unions landed. The companion missing-binding error
    // for `p` falls out of the failed declaration. Both diagnostics
    // are pinned so a future binding-rescue rewrite still flags the
    // narrowing site directly.
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
    assert!(
        failure.diagnostics.iter().any(|d| d
            .message
            .contains("typed-binding pattern requires a union subject")
            && d.message.contains("Int")),
        "expected typed-binding-against-non-union diagnostic, got: {:?}",
        failure.diagnostics,
    );
}
