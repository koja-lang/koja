//! Typecheck coverage for `fn (T, U) -> R` annotations.
//!
//! Function types lift to [`ResolvedType::Anonymous`] with an
//! [`AnonymousKind::Function`] payload carrying each parameter's
//! resolved type. The return type recurses through the same lifter.
//! These tests pin the surface for higher-order signatures
//! (`fn (T) -> U` parameters and returns) and cover the stdlib shape
//! that powers `list.map` / `list.filter`.

use koja_ast::identifier::{AnonymousKind, Resolution, ResolvedType, TypeParamIndex};
use koja_ast::util::dedent;

mod common;

use common::{
    PACKAGE, function_signature, global_leaf, registry_id, typecheck_script as typecheck,
};

#[test]
fn function_type_param_lifts_to_anonymous_function() {
    let source = "
        fn apply(f: fn (Int) -> Int, x: Int) -> Int
          x
        end
        ";

    let checked = typecheck(&dedent(source));
    let int = global_leaf(&checked, "Int");
    let signature = function_signature(&checked, PACKAGE, &["apply"]);

    let expected_callback = ResolvedType::Anonymous(AnonymousKind::Function {
        params: vec![int.clone()],
        ret: Box::new(int.clone()),
    });
    assert_eq!(signature.params[0].ty, expected_callback);
    assert_eq!(signature.params[1].ty, int);
    assert_eq!(signature.return_type, int);
}

#[test]
fn function_type_returning_function_type_nests() {
    // Pin the lifter on a function-of-function return annotation.
    // Closure-value bodies arrive in a later slice, so this fixture
    // is annotation-only with an `@intrinsic` declaration.
    let source = "
        @intrinsic
        fn make_pipeline -> fn (Int) -> fn (Int) -> Int
        ";

    let checked = typecheck(&dedent(source));
    let int = global_leaf(&checked, "Int");
    let signature = function_signature(&checked, PACKAGE, &["make_pipeline"]);

    let inner = ResolvedType::Anonymous(AnonymousKind::Function {
        params: vec![int.clone()],
        ret: Box::new(int.clone()),
    });
    let outer = ResolvedType::Anonymous(AnonymousKind::Function {
        params: vec![int],
        ret: Box::new(inner),
    });
    assert_eq!(signature.return_type, outer);
}

#[test]
fn dotted_type_in_signature_lifts_to_qualified_global() {
    // `fn touch(crypto: Crypto.SHA256) -> Crypto.SHA256`, without
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
    let signature = function_signature(&checked, PACKAGE, &["touch"]);
    let sha_id = registry_id(&checked, "Crypto", &["SHA256"]);
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
    let signature = function_signature(&checked, PACKAGE, &["map"]);
    let map_id = registry_id(&checked, PACKAGE, &["map"]);
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
    assert_eq!(params[0], t);
    assert_eq!(**ret, u);
    assert_eq!(signature.return_type, u);
}

#[test]
fn generic_function_body_passes_template_seal() {
    let source = "
        fn identity<T>(value: T) -> T
          value
        end

        identity(1)
        ";

    typecheck(&dedent(source));
}
