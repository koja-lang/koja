//! Typecheck coverage for `match`. Pin the contract:
//!
//! - subject + arm bodies resolve under the same rules as anywhere else
//! - the surface expression's type is the join of every reaching arm tail
//!   (with `Never` as the lattice bottom — divergent arms don't constrain it)
//! - a wildcard / binding catch-all is required, except for enum subjects
//!   with full structural variant coverage and `Bool` subjects with both
//!   `true` and `false` literal coverage
//! - missing-variant and missing-catch-all errors carry suggestion hints
//! - `pattern when expr -> body` guards resolve `expr` against `Bool` in
//!   the post-pattern-bind scope, and a guarded arm does not contribute
//!   to catch-all detection or enum variant coverage
//! - struct destructure patterns (`Type{ field: x, ... }` for plain
//!   structs, `Type.Variant{ field: x, ... }` for struct-variant enums)
//!   resolve each named field against the declared roster, restrict
//!   field elements to wildcard / binding, and stamp a `LocalId` on
//!   each binding; plain-struct destructure counts as a catch-all
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

use expo_alpha_typecheck::CheckedProgram;
use expo_ast::ast::{ExprKind, Function, Item, Pattern, Statement};
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};
use expo_ast::util::dedent;

mod common;

use common::{
    PACKAGE, typecheck_file as typecheck, typecheck_file_fail as typecheck_fail, warning_messages,
};

fn trailing_resolution(checked: &CheckedProgram) -> ResolvedType {
    let pkg = checked
        .packages
        .iter()
        .find(|p| p.package == PACKAGE)
        .expect("checked program is missing the test package");
    let file = pkg.files.first().expect("package has no files");
    let main = file
        .items
        .iter()
        .find_map(|item| match item {
            Item::Function(function) if function.name == "main" => Some(function),
            _ => None,
        })
        .expect("file is missing `fn main`");
    let body = main
        .body
        .as_deref()
        .expect("`fn main` has no body — extern fn cannot be the entry point");
    let trailing = body.last().expect("expected at least one statement");
    match trailing {
        Statement::Expr(expr) => expr.resolution.clone(),
        other => panic!("expected Statement::Expr as trailing statement, got {other:?}"),
    }
}

fn main_fn(checked: &CheckedProgram) -> &Function {
    let pkg = checked
        .packages
        .iter()
        .find(|p| p.package == PACKAGE)
        .expect("checked program is missing the test package");
    let file = pkg.files.first().expect("package has no files");
    file.items
        .iter()
        .find_map(|item| match item {
            Item::Function(function) if function.name == "main" => Some(function),
            _ => None,
        })
        .expect("file is missing `fn main`")
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

#[test]
fn match_int_literal_arms_resolve_to_int() {
    let source = "
        fn main
          match 1
            1 -> 10
            2 -> 20
            _ -> 30
          end
        end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn match_string_literal_arms_resolve_to_int() {
    let source = "
        fn main
          match \"hi\"
            \"hi\" -> 1
            _ -> 0
          end
        end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn match_binding_arm_resolves_to_subject_type() {
    let source = "
        fn main
          match 7
            x -> x
          end
        end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn match_binding_stamps_local_id() {
    let source = "
        fn main
          match 7
            x -> x
          end
        end
        ";
    let checked = typecheck(&dedent(source));
    let pkg = checked
        .packages
        .iter()
        .find(|p| p.package == PACKAGE)
        .expect("missing test package");
    let file = pkg.files.first().expect("package has no files");
    let main = file
        .items
        .iter()
        .find_map(|item| match item {
            Item::Function(f) if f.name == "main" => Some(f),
            _ => None,
        })
        .expect("missing fn main");
    let body = main.body.as_deref().expect("missing fn main body");
    let Statement::Expr(match_expr) = body.last().expect("missing trailing match-expr") else {
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

        fn main
          pick()
        end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn match_without_catch_all_diagnoses() {
    let source = "
        fn main
          match 1
            1 -> 10
            2 -> 20
          end
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
        fn main
          match 1
            1 -> 10
            _ -> false
          end
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
        fn main
          match 1
            \"hi\" -> 10
            _ -> 20
          end
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
        fn main
          match 7
            x when x > 0 -> 10
            _ -> 20
          end
        end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
    let main = main_fn(&checked);
    let Statement::Expr(match_expr) = main.body.as_deref().unwrap().last().unwrap() else {
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
        fn main
          match 1
            x when x -> 10
            _ -> 20
          end
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
        fn main
          match 1
            _ when 1 > 0 -> 10
          end
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
        fn main
          match \"hi\"
            \"hi\" -> \"yes\"
            _ -> \"no\"
          end
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

        fn main
          pick(Color.Red)
        end
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

        fn main
          unwrap(Box.Some(7))
        end
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
        fn main
          match \"a\"
            \"a\" | \"b\" | \"c\" -> 1
            _ -> 0
          end
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

        fn main
          classify(Lifecycle.Reload)
        end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

#[test]
fn match_or_with_binding_diagnoses() {
    let source = "
        fn main
          match 1
            x | 2 -> 0
            _ -> 0
          end
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
        fn main
          match 1
            x -> 0
            y -> y
          end
        end
        ";
    let checked = typecheck(&dedent(source));
    let pkg = checked
        .packages
        .iter()
        .find(|p| p.package == PACKAGE)
        .expect("missing test package");
    let file = pkg.files.first().expect("package has no files");
    let main = file
        .items
        .iter()
        .find_map(|item| match item {
            Item::Function(f) if f.name == "main" => Some(f),
            _ => None,
        })
        .expect("missing fn main");
    let body = main.body.as_deref().expect("missing fn main body");
    let Statement::Expr(match_expr) = body.last().expect("missing trailing match") else {
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

        fn main
          match Point{x: 1, y: 2}
            Point{x: a, y: b} -> a + b
          end
        end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
    let main = main_fn(&checked);
    let Statement::Expr(match_expr) = main.body.as_deref().unwrap().last().unwrap() else {
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

        fn main
          match Point{x: 1, y: 2}
            Point{z: a} -> a
          end
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
fn match_struct_destructure_non_binding_field_diagnoses() {
    let source = "
        struct Point
          x: Int
          y: Int
        end

        fn main
          match Point{x: 1, y: 2}
            Point{x: 1, y: b} -> b
          end
        end
        ";
    let failure = typecheck_fail(&dedent(source));
    assert!(
        failure.diagnostics.iter().any(|d| d
            .message
            .contains("only admits wildcard / binding patterns inside")),
        "expected field-element feature-gap diagnostic, got: {:?}",
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

        fn main
          area(Shape.Rect{w: 3, h: 4})
        end
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

        fn main
          match Box.Some(7)
            Box.Some{x: x} -> x
            Box.None -> 0
          end
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

        fn main
          match Point{x: 1, y: 2}
            Point{x: a, y: _} -> a
          end
        end
        ";
    let checked = typecheck(&dedent(source));
    assert_eq!(trailing_resolution(&checked), int_type(&checked));
}

// --- Phase 5: reachability / hint / Bool exhaustiveness ---

#[test]
fn match_arm_after_catch_all_warns_unreachable() {
    let source = "
        fn main
          match 1
            _ -> 10
            2 -> 20
          end
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
        fn main
          match 1
            1 -> 10
            1 -> 11
            _ -> 20
          end
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

        fn main
          pick(Color.Red)
        end
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
        fn main
          match 1
            1 | 1 -> 10
            _ -> 20
          end
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
        fn main
          match 1
            _ -> 10
            _ -> 20
            _ -> 30
          end
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

        fn main
          pick(Color.Red)
        end
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
        fn main
          match 1
            1 -> 10
            2 -> 20
          end
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
        fn main
          match true
            true -> 1
            false -> 0
          end
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
        fn main
          match true
            true -> 1
          end
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

        fn main
          pick(Box.None)
        end
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

        fn main
          unwrap(Box.Some(7))
        end
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

        fn main
          unwrap(Box.None)
        end
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

        fn main
          unwrap(Box.None)
        end
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

        fn main
          area(Shape.Circle(3))
        end
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

        fn main
          unwrap(Box.None)
        end
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
        fn main
          match 1
            Some(x) -> x
            _ -> 0
          end
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
        fn main
          match 1
            [a, b] -> a + b
            _ -> 0
          end
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
    // the legacy "feature gap" stub the alpha resolver carried
    // before unions landed. The companion missing-binding error
    // for `p` falls out of the failed declaration; both diagnostics
    // are pinned so a future binding-rescue rewrite still flags the
    // narrowing site directly.
    let source = "
        struct Post
          id: Int
        end

        fn main
          match 1
            p: Post -> p.id
            _ -> 0
          end
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
