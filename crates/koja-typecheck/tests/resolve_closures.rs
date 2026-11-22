//! Typecheck coverage for closure expression resolution.
//!
//! Pins the resolver's behavior on the closure surface that drives
//! the higher-order stdlib (`list.map`/`filter`, `option.then`,
//! `result.map`): block (`fn x -> body end`) and short (`x -> body`,
//! `() -> body`) forms, explicit param annotations vs context-driven inference,
//! capture of outer locals, nested closures, and "value-of-fn-type"
//! storage in a local binding.

use koja_ast::ast::{ClosureParam, Expr, ExprKind, Statement};
use koja_ast::coercion::Coercion;
use koja_ast::identifier::{AnonymousKind, Resolution, ResolvedType};
use koja_ast::util::dedent;
use koja_typecheck::CheckedProgram;

mod common;

use common::{
    assert_script_fails_with, diagnostic_messages, function_body, global_leaf, last_expr,
    package_leaf, script_body, typecheck_script as typecheck,
    typecheck_script_fail as typecheck_fail,
};

fn fn_type(params: Vec<ResolvedType>, ret: ResolvedType) -> ResolvedType {
    ResolvedType::Anonymous(AnonymousKind::Function {
        params,
        ret: Box::new(ret),
    })
}

#[test]
fn block_closure_with_annotated_params_resolves_to_function_type() {
    let source = "
        fn make -> Int
          f = fn (x: Int) -> Int
            x + 1
          end
          5
        end
        ";

    let checked = typecheck(&dedent(source));
    let int = global_leaf(&checked, "Int");
    let body = function_body(&checked, "make");
    let Statement::Assignment { value, .. } = &body[0] else {
        panic!("expected assignment, got {:?}", body[0]);
    };
    let expected = fn_type(vec![int.clone()], int);
    assert_eq!(value.resolution, expected);

    let ExprKind::Closure { params, .. } = &value.kind else {
        panic!("expected ExprKind::Closure, got {:?}", value.kind);
    };
    let ClosureParam::Name { local_id, .. } = &params[0] else {
        panic!("expected named closure param, got {:?}", params[0]);
    };
    assert!(local_id.is_some(), "closure param missing local_id stamp");
}

#[test]
fn short_closure_with_unannotated_param_uses_context() {
    // `f: fn (Int) -> Int` lets `x -> x + 1` infer `x: Int`.
    let source = "
        fn apply(f: fn (Int) -> Int, value: Int) -> Int
          value
        end

        fn make -> Int
          apply(x -> x + 1, 5)
        end
        ";

    let checked = typecheck(&dedent(source));
    let int = global_leaf(&checked, "Int");
    let body = function_body(&checked, "make");
    let trailing = last_expr(body);
    let ExprKind::Call { args, .. } = &trailing.kind else {
        panic!("expected Call, got {:?}", trailing.kind);
    };
    let closure = &args[0].value;
    let expected = fn_type(vec![int.clone()], int);
    assert_eq!(closure.resolution, expected);
}

#[test]
fn zero_param_short_closure_resolves_to_thunk_type() {
    let source = "
        fn run(f: fn () -> Int) -> Int
          f()
        end

        fn make -> Int
          run(() -> 42)
        end
        ";

    let checked = typecheck(&dedent(source));
    let int = global_leaf(&checked, "Int");
    let body = function_body(&checked, "make");
    let trailing = last_expr(body);
    let ExprKind::Call { args, .. } = &trailing.kind else {
        panic!("expected Call, got {:?}", trailing.kind);
    };
    let closure = &args[0].value;
    assert_eq!(closure.resolution, fn_type(Vec::new(), int));
}

#[test]
fn zero_param_short_closure_against_unary_slot_reports_count() {
    let source = "
        fn apply(f: fn (Int) -> Int, value: Int) -> Int
          f(value)
        end

        fn make -> Int
          apply(() -> 42, 5)
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("closure expects 1 parameter, got 0")),
        "expected parameter count diagnostic, got {messages:?}",
    );
}

#[test]
fn closure_captures_outer_local_with_local_resolution() {
    // The body's reference to outer `factor` resolves to
    // `Resolution::Local(<factor's local id>)`. Capture analysis at
    // IR time will derive the capture set from this.
    let source = "
        fn make_scaler -> Int
          factor = 3
          scaler = fn (x: Int) -> Int
            x * factor
          end
          5
        end
        ";

    let checked = typecheck(&dedent(source));
    let outer = function_body(&checked, "make_scaler");
    let Statement::Assignment { value: closure, .. } = &outer[1] else {
        panic!("expected scaler assignment");
    };
    let ExprKind::Closure { body, .. } = &closure.kind else {
        panic!("expected Closure, got {:?}", closure.kind);
    };
    let trailing = last_expr(body);
    let ExprKind::Binary { right, .. } = &trailing.kind else {
        panic!("expected Binary in closure body, got {:?}", trailing.kind);
    };
    let ExprKind::Ident { resolution, name } = &right.kind else {
        panic!("expected Ident on Binary RHS, got {:?}", right.kind);
    };
    assert_eq!(name, "factor");
    assert!(
        matches!(resolution, Resolution::Local(_)),
        "expected Local resolution for captured `factor`, got {resolution:?}",
    );
}

#[test]
fn nested_closures_resolve_through_layers() {
    let source = "
        fn apply(f: fn (Int) -> Int, value: Int) -> Int
          value
        end

        fn make -> Int
          outer = 10
          stage1 = fn (x: Int) -> Int
            x + outer
          end
          apply(stage1, 5)
        end
        ";

    typecheck(&dedent(source));
}

#[test]
fn closure_value_stored_in_local_resolves_to_function_type() {
    let source = "
        fn make -> Int
          f = fn (x: Int) -> Int
            x * 2
          end
          5
        end
        ";

    let checked = typecheck(&dedent(source));
    let int = global_leaf(&checked, "Int");
    let body = function_body(&checked, "make");
    let Statement::Assignment { value, .. } = &body[0] else {
        panic!("expected assignment, got {:?}", body[0]);
    };
    let expected = fn_type(vec![int.clone()], int);
    assert_eq!(value.resolution, expected);
}

#[test]
fn closure_param_local_ids_stamped_for_seal() {
    let source = "
        fn caller -> Int
          adder = fn (a: Int, b: Int) -> Int
            a + b
          end
          5
        end
        ";

    let checked = typecheck(&dedent(source));
    let body = function_body(&checked, "caller");
    let Statement::Assignment { value, .. } = &body[0] else {
        panic!("expected assignment");
    };
    let ExprKind::Closure { params, .. } = &value.kind else {
        panic!("expected Closure");
    };
    for (idx, param) in params.iter().enumerate() {
        let ClosureParam::Name { local_id, .. } = param else {
            panic!("param {idx} is not Name");
        };
        assert!(local_id.is_some(), "param {idx} missing local_id");
    }
}

#[test]
fn block_closure_return_annotation_threads_to_trailing_expr() {
    // `Result.Ok(v)` in a closure whose return type is annotated
    // `Result<Int, Int>` must let the `E` slot fill from the
    // surrounding annotation. Without the return-hint plumbing
    // used to fire "cannot infer type parameter `E` of
    // `Global.Result` from the supplied `Ok` payload" inside
    // `result.then`-style higher-order callers.
    typecheck(&dedent(
        "
        fn run -> Int
          step = fn (v: Int) -> Result<Int, Int>
            Result.Ok(v * 3)
          end
          step
          0
        end
        ",
    ));
}

#[test]
fn closure_capture_of_heap_local_resolves_through_string() {
    let source = "
        fn apply(f: fn (Int) -> String, value: Int) -> String
          \"x\"
        end

        fn make -> String
          prefix = \"hello\"
          apply(x -> prefix, 1)
        end
        ";

    let checked = typecheck(&dedent(source));
    let string = global_leaf(&checked, "String");
    let int = global_leaf(&checked, "Int");
    let body = function_body(&checked, "make");
    let trailing = last_expr(body);
    let ExprKind::Call { args, .. } = &trailing.kind else {
        panic!("expected Call, got {:?}", trailing.kind);
    };
    let closure = &args[0].value;
    let expected = fn_type(vec![int], string);
    assert_eq!(closure.resolution, expected);
}

/// The last expression of the block closure that the last statement
/// of the script assigns to a local.
fn closure_last_expr(checked: &CheckedProgram) -> &Expr {
    let Some(Statement::Assignment { value, .. }) = script_body(checked).last() else {
        panic!("expected the script to end with an assignment");
    };
    let ExprKind::Closure { body, .. } = &value.kind else {
        panic!("expected a block closure, got {:?}", value.kind);
    };
    last_expr(body)
}

#[test]
fn annotated_closure_widens_a_member_into_its_union_return() {
    let source = "
        struct Cat
          name: String
        end

        struct Dog
          name: String
        end

        type Pet = Cat | Dog

        adopt = fn (name: String) -> Pet
          Cat{name: name}
        end
        ";

    let checked = typecheck(&dedent(source));
    let pet = package_leaf(&checked, "Pet");
    assert_eq!(
        closure_last_expr(&checked).coercion,
        Some(Coercion::UnionWiden(pet)),
    );
}

#[test]
fn annotated_closure_widens_a_sized_number_into_its_return() {
    let source = "
        widen = fn (n: Int32) -> Int
          n
        end
        ";

    let checked = typecheck(&dedent(source));
    let int = global_leaf(&checked, "Int");
    assert_eq!(
        closure_last_expr(&checked).coercion,
        Some(Coercion::NumericWiden(int)),
    );
}

#[test]
fn annotated_closure_rejects_a_last_expression_of_another_type() {
    // A closure has no name, so the message has no `on` part. There
    // is one diagnostic, because the local still gets the annotated
    // function type.
    let source = "
        label = fn (n: Int) -> String
          n
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    assert_eq!(
        diagnostic_messages(&failure),
        vec!["return type mismatch. Expected `String`, found `Int`"],
    );
}

#[test]
fn annotated_closure_with_an_empty_body_diagnoses() {
    let source = "
        nothing = fn () -> Int
        end
        ";

    assert_script_fails_with(source, &["return type mismatch", "empty body"]);
}

#[test]
fn annotated_closure_that_ends_with_an_assignment_diagnoses() {
    let source = "
        bump = fn (n: Int) -> Int
          m = n + 1
        end
        ";

    assert_script_fails_with(source, &["return type mismatch", "non-expression"]);
}

#[test]
fn annotated_closure_that_ends_with_an_if_without_else_diagnoses() {
    let source = "
        pick = fn (flag: Bool) -> Int
          if flag
            1
          end
        end
        ";

    assert_script_fails_with(source, &["return type mismatch", "found `Unit`"]);
}

#[test]
fn annotated_closure_can_end_with_a_return() {
    let source = "
        answer = fn () -> Int
          return 42
        end
        ";

    typecheck(&dedent(source));
}

#[test]
fn annotated_closure_can_end_with_a_call_that_never_returns() {
    let source = "
        stop = fn (n: Int) -> Int
          Kernel.panic(\"no\")
        end
        ";

    typecheck(&dedent(source));
}

#[test]
fn unit_closure_discards_its_last_value() {
    let source = "
        ignore = fn (n: Int) -> ()
          n
        end
        ";

    typecheck(&dedent(source));
}
