//! `@extern "C"` FFI signature gate. Mirrors v1's
//! `validate_ffi_signature` rules: bodyless declaration, no
//! `@intrinsic` mutex, no `self` receiver, and parameter / return
//! types restricted to the explicit-width numeric primitives,
//! `Bool`, `Unit`, or `CPtr<T>`. `@link` annotations are pure
//! linker metadata and pass through silently — the IR layer
//! consumes them via `AnnotationKind::Link`.

use expo_alpha_typecheck::GlobalKind;
use expo_ast::identifier::Identifier;
use expo_ast::util::dedent;

mod common;

use common::{
    PACKAGE, diagnostic_messages, typecheck_file as typecheck,
    typecheck_file_fail as typecheck_fail,
};

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

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("`@extern \"C\"` and `@intrinsic` are mutually exclusive")),
        "expected mutex diagnostic, got: {messages:?}",
    );
}

// NOTE: the parser silently strips bodies from `@extern`-annotated
// functions ([`expo_parser::decl`] lines 482-498), so the
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

        impl Handle
          @extern \"C\"
          fn close(self) -> Int32
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("`@extern \"C\"` functions cannot take a `self` receiver")),
        "expected self-receiver rejection, got: {messages:?}",
    );
}

#[test]
fn extern_with_inferred_int_param_is_rejected() {
    let source = "
        @extern \"C\"
        fn read(fd: Int) -> Int32
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages.iter().any(|m| {
            m.contains("`@extern \"C\"` parameter `fd`")
                && m.contains("not an FFI-admissible C type")
        }),
        "expected inferred-Int rejection, got: {messages:?}",
    );
}

#[test]
fn extern_with_string_param_is_rejected() {
    let source = "
        @extern \"C\"
        fn label(name: String) -> Int32
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages.iter().any(|m| {
            m.contains("`@extern \"C\"` parameter `name`")
                && m.contains("not an FFI-admissible C type")
        }),
        "expected String rejection, got: {messages:?}",
    );
}

#[test]
fn extern_with_inferred_float_return_is_rejected() {
    let source = "
        @extern \"C\"
        fn pi -> Float
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("`@extern \"C\"` return type is not an FFI-admissible C type")),
        "expected inferred-Float return rejection, got: {messages:?}",
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

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages.iter().any(|m| {
            m.contains("`@extern \"C\"` parameter `v`")
                && m.contains("not an FFI-admissible C type")
        }),
        "expected user-struct rejection, got: {messages:?}",
    );
}
