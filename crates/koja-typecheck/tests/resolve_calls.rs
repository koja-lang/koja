//! Typecheck coverage for call resolution: bare-identifier
//! `f(args)` calls. Exercises the full
//! `collect -> lift_signatures -> resolve -> seal` path on zero-arg,
//! arg-taking, arity-mismatched, type-mismatched, unknown,
//! non-identifier, and wrong-kind callees, plus return-type
//! propagation through arithmetic.

use koja_ast::ast::{ExprKind, Literal};
use koja_ast::identifier::{Identifier, Resolution, ResolvedType};
use koja_ast::util::dedent;
use koja_typecheck::GlobalKind;

mod common;

use common::{
    PACKAGE, assert_script_fails_with, diagnostic_messages, global_leaf, registry_id,
    trailing_expr, typecheck_script as typecheck, typecheck_script_fail as typecheck_fail,
};

#[test]
fn zero_arg_call_resolves_to_callee_return_type() {
    let source = "
        fn answer -> Int
          42
        end

        answer()
        ";

    let checked = typecheck(&dedent(source));
    let int = global_leaf(&checked, "Int");

    let call = trailing_expr(&checked);
    assert_eq!(
        call.resolution, int,
        "call site should resolve to `answer`'s return type (Int)",
    );

    let ExprKind::Call { callee, args, .. } = &call.kind else {
        panic!("expected Call expression, got {:?}", call.kind);
    };
    assert!(args.is_empty(), "zero-arg call should carry zero args");

    let ExprKind::Ident {
        name, resolution, ..
    } = &callee.kind
    else {
        panic!("expected bare-Ident callee, got {:?}", callee.kind,);
    };
    assert_eq!(name, "answer");
    let answer_id = registry_id(&checked, PACKAGE, &["answer"]);
    assert_eq!(*resolution, Resolution::Global(answer_id));

    // Outer callee Expr.resolution stays Unresolved (carve-out).
    assert_eq!(callee.resolution, ResolvedType::unresolved());
}

#[test]
fn arg_taking_call_resolves_and_registers_signature() {
    let source = "
        fn add(a: Int, b: Int) -> Int
          1
        end

        add(2, 3)
        ";

    let checked = typecheck(&dedent(source));
    let int = global_leaf(&checked, "Int");

    let call = trailing_expr(&checked);
    assert_eq!(call.resolution, int);

    let ExprKind::Call { args, .. } = &call.kind else {
        panic!("expected Call expression");
    };
    assert_eq!(args.len(), 2);
    for (arg, expected_text) in args.iter().zip(["2", "3"]) {
        match &arg.value.kind {
            ExprKind::Literal {
                value: Literal::Int(text),
            } => {
                assert_eq!(text, expected_text);
            }
            other => panic!("expected Int literal arg, got {other:?}"),
        }
        assert_eq!(arg.value.resolution, int);
    }

    let add_ident = Identifier::new(PACKAGE, vec!["add".to_string()]);
    let (_, add_entry) = checked
        .registry
        .lookup(&add_ident)
        .expect("TestApp.add missing from registry");

    match &add_entry.kind {
        GlobalKind::Function(Some(sig)) => {
            assert_eq!(sig.params.len(), 2);
            assert_eq!(sig.params[0].name, "a");
            assert_eq!(sig.params[1].name, "b");
            assert_eq!(sig.params[0].ty, int);
            assert_eq!(sig.params[1].ty, int);
            assert_eq!(sig.return_type, int);
        }
        other => panic!("expected Function(Some(sig)), got {other:?}"),
    }
}

#[test]
fn return_type_propagates_through_arithmetic() {
    // Exercises `resolve_call` returning a `ResolvedType` that the
    // surrounding expression (`+ 1`) then type-checks against.
    let source = "
        fn answer -> Int
          42
        end

        answer() + 1
        ";

    let checked = typecheck(&dedent(source));
    let int = global_leaf(&checked, "Int");
    let trailing = trailing_expr(&checked);
    assert_eq!(trailing.resolution, int);
}

#[test]
fn arity_mismatch_diagnoses() {
    let source = "
        fn add(a: Int, b: Int) -> Int
          1
        end

        add(1)
        ";

    assert_script_fails_with(source, &["expects 2 argument"]);
}

#[test]
fn arg_type_mismatch_diagnoses() {
    let source = "
        fn only_int(a: Int) -> Int
          1
        end

        only_int(true)
        ";

    assert_script_fails_with(source, &["expects `Int`", "got `Bool`"]);
}

#[test]
fn unknown_callee_diagnoses() {
    assert_script_fails_with("missing()\n", &["unknown function `missing`"]);
}

#[test]
fn wrong_kind_callee_diagnoses() {
    let source = "
        Int()
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    // `Int` is a struct (stdlib stub), so calling it should fail
    // the wrong-kind branch. The resolver sees `Int` as an Ident
    // resolving to `Global.Int` under the current lookup, but
    // since we only look up in `PACKAGE`, this actually surfaces
    // as unknown-callee (no `TestApp.Int`). That's the expected
    // behaviour for this slice: primitives are only visible to
    // the signature resolver. The test documents that `Int()` is
    // diagnosed one way or another.
    assert!(
        messages
            .iter()
            .any(|m| m.contains("unknown function `Int`") || m.contains("not a function")),
        "expected wrong-kind-or-unknown diagnostic, got {messages:?}",
    );
}

#[test]
fn non_ident_callee_diagnoses() {
    // `(42)()` parses as Call { callee: Group { Literal } }. The
    // resolve_call arm pattern-matches on `ExprKind::Ident`. The
    // Group falls through to the non-Ident diagnose path.
    assert_script_fails_with("(42)()\n", &["only supports bare-identifier callees"]);
}

#[test]
fn named_args_diagnoses() {
    let source = "
        fn add(a: Int, b: Int) -> Int
          1
        end

        add(a: 1, b: 2)
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages.iter().any(|m| m.contains("named arguments")),
        "expected named-args diagnostic, got {messages:?}",
    );
    // Expect one diagnostic per named arg (2 in this test).
    let named_count = messages
        .iter()
        .filter(|m| m.contains("named arguments"))
        .count();
    assert_eq!(
        named_count, 2,
        "one diagnostic per named arg; got {messages:?}"
    );
}
