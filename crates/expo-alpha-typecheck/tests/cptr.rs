//! Surface-level coverage for the auto-imported `Global.cptr`
//! source. Pins that `CPtr<T>`'s 7-method generic surface registers
//! cleanly, that the `impl CPtr<UInt8>` concrete-pinned methods
//! stamp under the same struct, and that the impl-block's bare
//! `strlen` static call resolves through the `impl_args` mangling
//! introduced in `lift_signatures` (no `extern "C" priv` lookup
//! escapes into the global namespace).

use expo_alpha_typecheck::CheckedProgram;
use expo_ast::identifier::Identifier;
use expo_ast::util::dedent;

mod common;

use common::{
    diagnostic_messages, typecheck_file as typecheck, typecheck_file_fail as typecheck_fail,
};

fn assert_registered(checked: &CheckedProgram, segments: &[&str]) {
    let id = Identifier::new("Global", segments.iter().map(|s| s.to_string()).collect());
    assert!(
        checked.registry.lookup(&id).is_some(),
        "expected `{id}` to be registered after autoimport",
    );
}

#[test]
fn cptr_struct_and_generic_methods_register() {
    let checked = typecheck("fn main\n  1\nend\n");
    assert_registered(&checked, &["CPtr"]);
    for method in ["alloc", "null", "free", "offset", "read", "write", "null?"] {
        assert_registered(&checked, &["CPtr", method]);
    }
}

#[test]
fn cptr_uint8_concrete_impl_methods_register() {
    // `to_binary` and `to_string` live on `impl CPtr<UInt8>`, plus
    // the pure-Expo `to_cstring` and the private `strlen` extern.
    let checked = typecheck("fn main\n  1\nend\n");
    for method in ["to_binary", "to_string", "to_cstring", "strlen"] {
        assert_registered(&checked, &["CPtr", method]);
    }
}

// Surface-level call-site coverage (`p: CPtr<UInt8> = CPtr.alloc(8)`,
// `alloc_bytes() -> CPtr<UInt8>`) is intentionally not pinned here.
// Both shapes hit the deferred typed-local / return-bound bidirectional
// inference seam ("cannot infer type parameter `T` of `Global.CPtr`
// from the supplied arguments"). The dispatch path itself is still
// reachable through autoimported `cstring.expo` callers (whose
// `free` / `to_binary` calls land on the `CPtr<UInt8>` impl) — and
// will round-trip end-to-end once the typed-local seam unblocks
// `Random.bytes`-style call sites.

#[test]
fn cptr_int32_write_accepts_int_literal_arg() {
    // Pre-fix this surfaced "type parameter `T` of `Global.CPtr` cannot
    // be both `Int32` and `Int`": receiver-seeding pinned `T = Int32`
    // from `ptr: CPtr<Int32>`, then arg unification of `42: Int`
    // against the method template `T` collided. Literal coercion is
    // post-inference's job, so the per-arg unifier now tolerates a
    // sized-int slot widened by a default-`Int` literal arrival.
    typecheck(&dedent(
        "
        fn main
          ptr: CPtr<Int32> = CPtr.alloc(1)
          ptr.write(42)
          ptr.free()
        end
        ",
    ));
}

#[test]
fn cptr_uint8_write_accepts_int_literal_arg() {
    typecheck(&dedent(
        "
        fn main
          byte_ptr: CPtr<UInt8> = CPtr.alloc(2)
          byte_ptr.write(65)
          byte_ptr.offset(1).write(0)
          byte_ptr.free()
        end
        ",
    ));
}

#[test]
fn cptr_int32_write_rejects_non_literal_int_value() {
    // The literal-widening tolerance only takes effect because the
    // post-substitute arg validator runs `check_compatible`, which
    // coerces a literal but rejects a non-literal `Int` value. The
    // diagnostic flips from "T cannot be both" to a cleaner
    // "argument expects `Int32`, got `Int`" — pin the new wording.
    let failure = typecheck_fail(&dedent(
        "
        fn main
          x: Int = 5
          ptr: CPtr<Int32> = CPtr.alloc(1)
          ptr.write(x)
          ptr.free()
        end
        ",
    ));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("expects `Int32`") && m.contains("got `Int`")),
        "expected literal-coercion-style diagnostic for non-literal Int into Int32 slot, got {messages:?}",
    );
}
