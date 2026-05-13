//! Typecheck coverage for `fn (T, U) -> R` annotations.
//!
//! Function types lift to [`ResolvedType::Anonymous`] with an
//! [`AnonymousKind::Function`] payload. Per-parameter [`PassMode`]
//! travels on each [`FnParam`]; the return type recurses through the
//! same lifter. These tests pin the surface for higher-order
//! signatures (`fn (T) -> U` parameters and returns) and cover the
//! stdlib shape that powers `list.map` / `list.filter`.

use expo_alpha_typecheck::GlobalKind;
use expo_ast::ast::PassMode;
use expo_ast::identifier::{
    AnonymousKind, FnParam, Identifier, Resolution, ResolvedType, TypeParamIndex,
};
use expo_ast::util::dedent;

mod common;

use common::{PACKAGE, typecheck_file as typecheck};

fn lookup_function_signature<'a>(
    checked: &'a expo_alpha_typecheck::CheckedProgram,
    package: &str,
    path: &[&str],
) -> &'a expo_alpha_typecheck::FunctionSignature {
    let identifier = Identifier::new(package, path.iter().map(|s| s.to_string()).collect());
    let (_, entry) = checked
        .registry
        .lookup(&identifier)
        .unwrap_or_else(|| panic!("`{}` not registered", identifier));
    let GlobalKind::Function(Some(signature)) = &entry.kind else {
        panic!("`{}` should have a lifted signature", identifier);
    };
    signature
}

fn global_leaf(checked: &expo_alpha_typecheck::CheckedProgram, name: &str) -> ResolvedType {
    let (id, _) = checked
        .registry
        .lookup(&Identifier::new("Global", vec![name.to_string()]))
        .unwrap_or_else(|| panic!("`Global.{name}` not registered"));
    ResolvedType::leaf(Resolution::Global(id))
}

#[test]
fn function_type_param_lifts_to_anonymous_function() {
    let source = "
        fn apply(f: fn (Int) -> Int, x: Int) -> Int
          x
        end
        ";

    let checked = typecheck(&dedent(source));
    let int = global_leaf(&checked, "Int");
    let signature = lookup_function_signature(&checked, PACKAGE, &["apply"]);

    let expected_callback = ResolvedType::Anonymous(AnonymousKind::Function {
        params: vec![FnParam {
            mode: PassMode::Borrow,
            ty: int.clone(),
        }],
        ret: Box::new(int.clone()),
    });
    assert_eq!(signature.params[0].ty, expected_callback);
    assert_eq!(signature.params[1].ty, int);
    assert_eq!(signature.return_type, int);
}

#[test]
fn function_type_with_move_param_threads_pass_mode() {
    let source = "
        fn apply(f: fn (move String) -> String, s: String) -> String
          s
        end
        ";

    let checked = typecheck(&dedent(source));
    let string = global_leaf(&checked, "String");
    let signature = lookup_function_signature(&checked, PACKAGE, &["apply"]);

    let ResolvedType::Anonymous(AnonymousKind::Function { params, ret }) = &signature.params[0].ty
    else {
        panic!(
            "expected `f` to lift to AnonymousKind::Function, got {:?}",
            signature.params[0].ty
        );
    };
    assert_eq!(params.len(), 1);
    assert_eq!(params[0].mode, PassMode::Move);
    assert_eq!(params[0].ty, string);
    assert_eq!(**ret, string);
}

#[test]
fn function_type_returning_function_type_nests() {
    // Pin the lifter on a function-of-function return annotation;
    // closure-value bodies arrive in a later slice, so this fixture
    // is annotation-only with an `@intrinsic` declaration.
    let source = "
        @intrinsic
        fn make_pipeline -> fn (Int) -> fn (Int) -> Int
        ";

    let checked = typecheck(&dedent(source));
    let int = global_leaf(&checked, "Int");
    let signature = lookup_function_signature(&checked, PACKAGE, &["make_pipeline"]);

    let inner = ResolvedType::Anonymous(AnonymousKind::Function {
        params: vec![FnParam {
            mode: PassMode::Borrow,
            ty: int.clone(),
        }],
        ret: Box::new(int.clone()),
    });
    let outer = ResolvedType::Anonymous(AnonymousKind::Function {
        params: vec![FnParam {
            mode: PassMode::Borrow,
            ty: int,
        }],
        ret: Box::new(inner),
    });
    assert_eq!(signature.return_type, outer);
}

#[test]
fn dotted_type_in_signature_lifts_to_qualified_global() {
    // `fn touch(crypto: Crypto.SHA256) -> Crypto.SHA256` — without
    // an `alias` line. The lifter should walk the dotted path
    // through `resolve_path_to_global` and produce a
    // `Resolution::Global(Crypto.SHA256)` leaf for both the
    // parameter and the return type, identical to what the alias
    // form (`alias Crypto.SHA256 as Hasher` then `: Hasher`)
    // produces today.
    let source = "
        fn touch(crypto: Crypto.SHA256) -> Crypto.SHA256
          crypto
        end
        ";

    let checked = typecheck(&dedent(source));
    let signature = lookup_function_signature(&checked, PACKAGE, &["touch"]);
    let (sha_id, _) = checked
        .registry
        .lookup(&Identifier::new("Crypto", vec!["SHA256".to_string()]))
        .expect("`Crypto.SHA256` should be registered via ALPHA_QUALIFIED");
    let expected = ResolvedType::leaf(Resolution::Global(sha_id));
    assert_eq!(signature.params[0].ty, expected);
    assert_eq!(signature.return_type, expected);
}

#[test]
fn function_type_in_generic_context_carries_type_params() {
    // Mirrors stdlib `List<T>.map` / `Option<T>.then` etc.: the
    // closure parameter's `T` and `U` lift as `TypeParam` leaves with
    // the enclosing fn as their owner, so monomorphization
    // substitutes them like any other generic site.
    let source = "
        @intrinsic
        fn map<T, U>(value: T, f: fn (T) -> U) -> U
        ";

    let checked = typecheck(&dedent(source));
    let signature = lookup_function_signature(&checked, PACKAGE, &["map"]);
    let map_id = checked
        .registry
        .lookup(&Identifier::new(PACKAGE, vec!["map".to_string()]))
        .map(|(id, _)| id)
        .expect("map registered");
    let t = ResolvedType::leaf(Resolution::TypeParam {
        owner: map_id,
        index: TypeParamIndex::new(0),
    });
    let u = ResolvedType::leaf(Resolution::TypeParam {
        owner: map_id,
        index: TypeParamIndex::new(1),
    });

    assert_eq!(signature.params[0].ty, t);
    let ResolvedType::Anonymous(AnonymousKind::Function { params, ret }) = &signature.params[1].ty
    else {
        panic!("expected `f: fn (T) -> U` to lift to AnonymousKind::Function");
    };
    assert_eq!(params.len(), 1);
    assert_eq!(params[0].mode, PassMode::Borrow);
    assert_eq!(params[0].ty, t);
    assert_eq!(**ret, u);
    assert_eq!(signature.return_type, u);
}
