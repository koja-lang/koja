//! End-to-end smoke test for the typecheck pipeline.
//!
//! Drives `parse_program → check_program` on `fn main; 2 + 2; end` and
//! asserts the pipeline succeeds, the registry holds `TestApp.main`,
//! and the body's `2 + 2` resolves into the preloaded `Global.Int`
//! stdlib stub.

use expo_ast::ast::{Expr, ExprKind, Item, Statement};
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};
use expo_ast::util::dedent;
use expo_typecheck::{CheckedProgram, GlobalKind};

mod common;

use common::{PACKAGE, typecheck_file as typecheck, typecheck_file_fail as typecheck_fail};

fn main_body(checked: &CheckedProgram) -> &[Statement] {
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
    main.body
        .as_deref()
        .expect("`fn main` has no body — extern fn cannot be the entry point")
}

#[test]
fn fn_main_two_plus_two_typechecks_to_int() {
    let source = "
        fn main
          2 + 2
        end
        ";

    let checked = typecheck(&dedent(source));

    let main_id = Identifier::new(PACKAGE, vec!["main".to_string()]);
    assert!(
        checked.registry.lookup(&main_id).is_some(),
        "registry is missing `{main_id}`; registry: {:?}",
        checked.registry,
    );

    let int_ident = Identifier::new("Global", vec!["Int".to_string()]);
    let (int_id, int_entry) = checked
        .registry
        .lookup(&int_ident)
        .expect("Global.Int stub is missing from the registry");
    assert_eq!(
        int_entry.identifier, int_ident,
        "Global.Int registry entry identifier drifted",
    );

    let body = main_body(&checked);
    assert_eq!(body.len(), 1, "expected exactly one statement in main");
    let Statement::Expr(expr) = &body[0] else {
        panic!("expected Statement::Expr at body[0], got {:?}", body[0]);
    };

    assert!(
        expr.resolution.is_resolved(),
        "top-level `2 + 2` has an unresolved annotation: {:?}",
        expr.resolution,
    );
    assert_eq!(
        expr.resolution,
        ResolvedType::leaf(Resolution::Global(int_id)),
        "top-level `2 + 2` did not resolve to Global.Int",
    );

    let ExprKind::Binary { left, right, .. } = &expr.kind else {
        panic!("expected ExprKind::Binary, got {:?}", expr.kind);
    };
    assert_int(left, int_id);
    assert_int(right, int_id);
}

#[test]
fn intrinsic_fn_typechecks_without_body_and_lifts_signature() {
    // Bodyless `@intrinsic` decls flow through collect /
    // lift_signatures / resolve / seal exactly like regular fns;
    // the only surface-level difference is `function.body == None`
    // and the parser refusing to consume a body. The registry
    // still ends up with a `Function(Some(_))` entry once
    // `lift_signatures` runs, so call-site resolution finds the
    // signature.
    let source = "
        @intrinsic
        fn print(s: String)
        ";

    let checked = typecheck(&dedent(source));

    let print_id = Identifier::new(PACKAGE, vec!["print".to_string()]);
    let (_, entry) = checked
        .registry
        .lookup(&print_id)
        .expect("intrinsic should be registered in the GlobalRegistry");
    let GlobalKind::Function(Some(signature)) = &entry.kind else {
        panic!(
            "expected Function(Some(_)) for intrinsic; got {:?}",
            entry.kind
        );
    };
    assert_eq!(signature.params.len(), 1);
    assert_eq!(signature.params[0].name, "s");

    let string_id = Identifier::new("Global", vec!["String".to_string()]);
    let (string_global_id, _) = checked
        .registry
        .lookup(&string_id)
        .expect("Global.String stub should be registered");
    assert_eq!(
        signature.params[0].ty,
        ResolvedType::leaf(Resolution::Global(string_global_id)),
    );

    let unit_id = Identifier::new("Global", vec!["Unit".to_string()]);
    let (unit_global_id, _) = checked
        .registry
        .lookup(&unit_id)
        .expect("Global.Unit stub should be registered");
    assert_eq!(
        signature.return_type,
        ResolvedType::leaf(Resolution::Global(unit_global_id)),
    );
}

#[test]
fn duplicate_fn_in_same_file_emits_diagnostic() {
    let source = "
        fn main
          1
        end

        fn main
          2
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    assert_eq!(
        failure.diagnostics.len(),
        1,
        "expected exactly one diagnostic, got {failure}",
    );
    let diag = &failure.diagnostics[0];
    assert!(
        diag.message.contains("`TestApp.main`") && diag.message.contains("already defined"),
        "unexpected diagnostic message: {}",
        diag.message,
    );
}

fn assert_int(expr: &Expr, int_id: expo_ast::identifier::GlobalRegistryId) {
    assert_eq!(
        expr.resolution,
        ResolvedType::leaf(Resolution::Global(int_id)),
        "operand did not resolve to Global.Int: {expr:?}",
    );
}
