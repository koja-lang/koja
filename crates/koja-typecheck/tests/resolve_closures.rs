//! Typecheck coverage for closure expression resolution.
//!
//! Pins the resolver's behavior on the closure surface that drives
//! the higher-order stdlib (`list.map`/`filter`, `option.then`,
//! `result.map`): block (`fn x -> body end`) and short (`x -> body`)
//! forms, explicit param annotations vs context-driven inference,
//! capture of outer locals, nested closures, and "value-of-fn-type"
//! storage in a local binding.

use koja_ast::ast::{ClosureParam, ExprKind, Statement};
use koja_ast::identifier::{AnonymousKind, Resolution, ResolvedType};
use koja_ast::util::dedent;

mod common;

use common::{function_body, global_leaf, last_expr, typecheck_script as typecheck};

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
