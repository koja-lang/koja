//! Slice 2.10 typecheck coverage: a generic function with a protocol
//! bound on its type-param can call the bound's method on a value
//! typed as that type-param. The call site reuses
//! [`resolve_bounded_method_call`], stamping the protocol method's
//! return type and validating arg arities against the protocol's
//! `non_self_params`. Mono later substitutes through to a concrete
//! receiver — that side lives in `expo-alpha-ir` tests.

use expo_alpha_typecheck::{CheckedProgram, GlobalKind};
use expo_ast::ast::{Expr, ExprKind, File, Function, Item, Statement};
use expo_ast::identifier::Identifier;
use expo_ast::util::dedent;

mod common;

use common::{
    PACKAGE, diagnostic_messages, typecheck_file as typecheck,
    typecheck_file_fail as typecheck_fail,
};

fn function<'a>(checked: &'a CheckedProgram, name: &str) -> &'a Function {
    for pkg in &checked.packages {
        for file in &pkg.files {
            if let Some(found) = find_function(file, name) {
                return found;
            }
        }
    }
    panic!("function `{name}` not found in checked program");
}

fn find_function<'a>(file: &'a File, name: &str) -> Option<&'a Function> {
    for item in &file.items {
        if let Item::Function(f) = item
            && f.name == name
        {
            return Some(f);
        }
    }
    None
}

fn body_trailing(function: &Function) -> &Expr {
    let body = function.body.as_ref().expect("function has a parsed body");
    let last = body.last().expect("body has at least one statement");
    match last {
        Statement::Expr(expr) => expr,
        Statement::Return {
            value: Some(expr), ..
        } => expr,
        other => panic!("trailing statement is not an expression: {other:?}"),
    }
}

#[test]
fn bounded_method_call_resolves_protocol_return_type() {
    // `fn show<T: Greeter>(value: T) -> String` calls `value.greet()`.
    // The receiver's type stays `TypeParam(show, 0)` until mono. The
    // call site's `expr.resolution` must be `String` (the protocol
    // method's return type), not `TypeParam` or `Unresolved`.
    let source = "
        protocol Greeter
          fn greet(self) -> String
        end

        fn show<T: Greeter>(value: T) -> String
          value.greet()
        end
        ";

    let checked = typecheck(&dedent(source));
    let show = function(&checked, "show");
    let trailing = body_trailing(show);
    let ExprKind::MethodCall { method, .. } = &trailing.kind else {
        panic!("expected MethodCall, got {:?}", trailing.kind);
    };
    assert_eq!(method, "greet");

    let string_id = checked
        .registry
        .lookup(&Identifier::new("Global", vec!["String".to_string()]))
        .map(|(id, _)| id)
        .expect("String exists in registry");
    let expo_ast::identifier::ResolvedType::Named {
        resolution: expo_ast::identifier::Resolution::Global(id),
        ..
    } = trailing.resolution
    else {
        panic!(
            "expected Named {{ Global(String), .. }}, got {:?}",
            trailing.resolution
        );
    };
    assert_eq!(id, string_id);
}

#[test]
fn bounded_method_call_with_no_bound_diagnoses() {
    // `T` has no bound; calling `value.greet()` is an error rather
    // than a silent dispatch failure. The universal-`Debug` fallback
    // augments every type parameter's bound list with the universal
    // protocols ([`registry::UNIVERSAL_PROTOCOLS`]), so the
    // diagnostic surface is `no bound provides it` (Debug is in
    // scope; greet is not on Debug) rather than the older
    // `no bounds declared` shape.
    let source = "
        fn show<T>(value: T) -> String
          value.greet()
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("greet") && m.contains("no bound provides it")),
        "expected `no bound provides it` diagnostic, got {messages:?}",
    );
}

#[test]
fn bounded_method_call_with_unrelated_bound_diagnoses() {
    // `T: Greeter` does not provide `unrelated_method`; the call
    // site fails with a "no bound provides it" diagnostic rather
    // than silently mapping to a wrong method.
    let source = "
        protocol Greeter
          fn greet(self) -> String
        end

        fn show<T: Greeter>(value: T) -> String
          value.unrelated_method()
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("unrelated_method") && m.contains("no bound provides it")),
        "expected `no bound provides it` diagnostic, got {messages:?}",
    );
}

#[test]
fn bounded_method_call_with_static_bound_method_diagnoses() {
    // Static methods on a bound protocol cannot be called through
    // a value of the bounded type-param — the receiver is a value,
    // not a type. This is the "use the protocol name to dispatch"
    // diagnostic.
    let source = "
        protocol Maker
          fn make() -> Int
        end

        fn show<T: Maker>(value: T) -> Int
          value.make()
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("static method") && m.contains("make")),
        "expected static-method diagnostic, got {messages:?}",
    );
}

#[test]
fn bounded_method_call_protocol_method_lifted_with_signature() {
    // Sanity: the registry actually carries a lifted protocol method
    // signature for `Greeter.greet` — that's what
    // `resolve_bounded_method_call` looks up via `collect_bound_providers`.
    let source = "
        protocol Greeter
          fn greet(self) -> String
        end
        ";

    let checked = typecheck(&dedent(source));
    let (_, entry) = checked
        .registry
        .lookup(&Identifier::new(PACKAGE, vec!["Greeter".to_string()]))
        .expect("Greeter exists");
    let GlobalKind::Protocol(Some(definition)) = &entry.kind else {
        panic!("expected lifted protocol, got {:?}", entry.kind);
    };
    assert_eq!(definition.methods.len(), 1);
    assert_eq!(definition.methods[0].name, "greet");
}
