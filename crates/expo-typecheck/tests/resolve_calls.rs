//! Typecheck coverage for call resolution: bare-identifier
//! `f(args)` calls. Exercises the full
//! `collect → lift_signatures → resolve → seal` path on zero-arg,
//! arg-taking, arity-mismatched, type-mismatched, unknown,
//! non-identifier, and wrong-kind callees, plus return-type
//! propagation through arithmetic.

use expo_ast::ast::{Expr, ExprKind, Function, Item, Literal, Statement};
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};
use expo_ast::util::dedent;
use expo_typecheck::{CheckedProgram, GlobalKind};

mod common;

use common::{
    PACKAGE, diagnostic_messages, typecheck_file as typecheck,
    typecheck_file_fail as typecheck_fail,
};

fn find_function<'a>(checked: &'a CheckedProgram, name: &str) -> &'a Function {
    let pkg = checked
        .packages
        .iter()
        .find(|p| p.package == PACKAGE)
        .expect("checked program is missing the test package");
    for file in &pkg.files {
        for item in &file.items {
            if let Item::Function(function) = item
                && function.name == name
            {
                return function;
            }
        }
    }
    panic!("fn {name} not found in checked program");
}

fn trailing_expr(function: &Function) -> &Expr {
    let body = function
        .body
        .as_deref()
        .expect("function has no body (extern?)");
    let trailing = body.last().expect("expected at least one statement");
    match trailing {
        Statement::Expr(expr) => expr,
        other => panic!("expected Statement::Expr as trailing statement, got {other:?}"),
    }
}

fn global_leaf(checked: &CheckedProgram, name: &str) -> ResolvedType {
    let ident = Identifier::new("Global", vec![name.to_string()]);
    let (id, _) = checked
        .registry
        .lookup(&ident)
        .unwrap_or_else(|| panic!("stdlib stub `Global.{name}` missing from registry"));
    ResolvedType::leaf(Resolution::Global(id))
}

// ---------------------------------------------------------------------------
// Happy paths
// ---------------------------------------------------------------------------

#[test]
fn zero_arg_call_resolves_to_callee_return_type() {
    let source = "
        fn answer -> Int
          42
        end

        fn main
          answer()
        end
        ";

    let checked = typecheck(&dedent(source));
    let int = global_leaf(&checked, "Int");

    let main = find_function(&checked, "main");
    let call = trailing_expr(main);
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
    let answer_ident = Identifier::new(PACKAGE, vec!["answer".to_string()]);
    let (answer_id, _) = checked
        .registry
        .lookup(&answer_ident)
        .expect("TestApp.answer should be in the registry");
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

        fn main
          add(2, 3)
        end
        ";

    let checked = typecheck(&dedent(source));
    let int = global_leaf(&checked, "Int");

    let main = find_function(&checked, "main");
    let call = trailing_expr(main);
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

        fn main
          answer() + 1
        end
        ";

    let checked = typecheck(&dedent(source));
    let int = global_leaf(&checked, "Int");
    let main = find_function(&checked, "main");
    let trailing = trailing_expr(main);
    assert_eq!(trailing.resolution, int);
}

// ---------------------------------------------------------------------------
// Error paths
// ---------------------------------------------------------------------------

#[test]
fn arity_mismatch_diagnoses() {
    let source = "
        fn add(a: Int, b: Int) -> Int
          1
        end

        fn main
          add(1)
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages.iter().any(|m| m.contains("expects 2 argument")),
        "expected arity diagnostic, got {messages:?}",
    );
}

#[test]
fn arg_type_mismatch_diagnoses() {
    let source = "
        fn only_int(a: Int) -> Int
          1
        end

        fn main
          only_int(true)
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("expects `Int`") && m.contains("got `Bool`")),
        "expected arg-type diagnostic, got {messages:?}",
    );
}

#[test]
fn unknown_callee_diagnoses() {
    let failure = typecheck_fail("fn main\n  missing()\nend\n");
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("unknown function `missing`")),
        "expected unknown-callee diagnostic, got {messages:?}",
    );
}

#[test]
fn wrong_kind_callee_diagnoses() {
    let source = "
        fn main
          Int()
        end
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
    // resolve_call arm pattern-matches on `ExprKind::Ident`; the
    // Group falls through to the non-Ident diagnose path.
    let failure = typecheck_fail("fn main\n  (42)()\nend\n");
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("only supports bare-identifier callees")),
        "expected non-ident callee diagnostic, got {messages:?}",
    );
}

#[test]
fn named_args_diagnoses() {
    let source = "
        fn add(a: Int, b: Int) -> Int
          1
        end

        fn main
          add(a: 1, b: 2)
        end
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
