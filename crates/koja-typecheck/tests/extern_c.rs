//! `@extern "C"` FFI signature gate. Mirrors v1's
//! `validate_ffi_signature` rules: bodyless declaration, no
//! `@intrinsic` mutex, no `self` receiver, and parameter / return
//! types restricted to the explicit-width numeric primitives,
//! `Bool`, `Unit`, or `CPtr<T>`. `@link` annotations are pure
//! linker metadata and pass through silently. The IR layer
//! consumes them via `AnnotationKind::Link`.

use koja_ast::identifier::Identifier;
use koja_ast::util::dedent;
use koja_typecheck::GlobalKind;

mod common;

use common::{PACKAGE, assert_script_fails_with, typecheck_script as typecheck};

#[test]
fn happy_path_explicit_width_primitives_typechecks() {
    let source = "
        @extern \"C\"
        fn cosf(x: Float32) -> Float32

        @extern \"C\"
        fn sleep(seconds: UInt32) -> Int32

        @extern \"C\"
        fn unit_effect

        @extern \"C\"
        fn pure_bool(flag: Bool) -> Bool
        ";

    let checked = typecheck(&dedent(source));

    for name in ["cosf", "sleep", "unit_effect", "pure_bool"] {
        let id = Identifier::new(PACKAGE, vec![name.to_string()]);
        let (_, entry) = checked
            .registry
            .lookup(&id)
            .unwrap_or_else(|| panic!("`{name}` should be registered"));
        assert!(
            matches!(entry.kind, GlobalKind::Function(Some(_))),
            "`{name}` should have a stamped signature; got {:?}",
            entry.kind,
        );
    }
}

#[test]
fn cptr_pointee_typechecks_for_any_t() {
    let source = "
        @extern \"C\"
        fn malloc(size: UInt64) -> CPtr<UInt8>

        @extern \"C\"
        fn free(ptr: CPtr<UInt8>)

        @extern \"C\"
        fn opaque_handle(p: CPtr<Float32>) -> CPtr<Bool>
        ";

    let checked = typecheck(&dedent(source));

    for name in ["malloc", "free", "opaque_handle"] {
        let id = Identifier::new(PACKAGE, vec![name.to_string()]);
        assert!(
            checked.registry.lookup(&id).is_some(),
            "`{name}` should be registered",
        );
    }
}

#[test]
fn link_annotations_are_accepted_metadata() {
    let source = "
        @extern \"C\"
        @link \"m\"
        fn cosf(x: Float32) -> Float32

        @extern \"C\"
        @link \"crypto:SHA256_Init\"
        fn sha256_init(ctx: CPtr<UInt8>) -> Int32
        ";

    let checked = typecheck(&dedent(source));

    for name in ["cosf", "sha256_init"] {
        let id = Identifier::new(PACKAGE, vec![name.to_string()]);
        assert!(
            checked.registry.lookup(&id).is_some(),
            "`{name}` should be registered alongside `@link`",
        );
    }
}

#[test]
fn extern_with_intrinsic_is_mutex_rejected() {
    let source = "
        @extern \"C\"
        @intrinsic
        fn cosf(x: Float32) -> Float32
        ";

    assert_script_fails_with(
        source,
        &["`@extern \"C\"` and `@intrinsic` are mutually exclusive"],
    );
}

// NOTE: the parser silently strips bodies from `@extern`-annotated
// functions ([`koja_parser::decl`] lines 482-498), so the
// "extern + body" path can't be exercised end-to-end through
// `parse_program`. The typecheck-side check stays in place as
// defense-in-depth for any caller that builds a [`Function`]
// programmatically.

#[test]
fn extern_with_self_receiver_is_rejected() {
    let source = "
        struct Handle
          tag: Bool
        end

        extend Handle
          @extern \"C\"
          fn close(self) -> Int32
        end
        ";

    assert_script_fails_with(
        source,
        &["`@extern \"C\"` functions cannot take a `self` receiver"],
    );
}

#[test]
fn extern_with_inferred_int_param_is_rejected() {
    let source = "
        @extern \"C\"
        fn read(fd: Int) -> Int32
        ";

    assert_script_fails_with(
        source,
        &[
            "`@extern \"C\"` parameter `fd`",
            "not an FFI-admissible C type",
        ],
    );
}

#[test]
fn extern_with_string_param_is_rejected() {
    let source = "
        @extern \"C\"
        fn label(name: String) -> Int32
        ";

    assert_script_fails_with(
        source,
        &[
            "`@extern \"C\"` parameter `name`",
            "not an FFI-admissible C type",
        ],
    );
}

#[test]
fn extern_with_inferred_float_return_is_rejected() {
    let source = "
        @extern \"C\"
        fn pi -> Float
        ";

    assert_script_fails_with(
        source,
        &["`@extern \"C\"` return type is not an FFI-admissible C type"],
    );
}

#[test]
fn extern_with_user_struct_param_is_rejected() {
    let source = "
        struct Vec2
          x: Float32
          y: Float32
        end

        @extern \"C\"
        fn length(v: Vec2) -> Float32
        ";

    assert_script_fails_with(
        source,
        &[
            "`@extern \"C\"` parameter `v`",
            "not an FFI-admissible C type",
        ],
    );
}
