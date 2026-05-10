//! Surface-level coverage for the auto-imported `Global.cptr`
//! source. Pins that `CPtr<T>`'s 7-method generic surface registers
//! cleanly, that the `impl CPtr<UInt8>` concrete-pinned methods
//! stamp under the same struct, and that the impl-block's bare
//! `strlen` static call resolves through the `impl_args` mangling
//! introduced in `lift_signatures` (no `extern "C" priv` lookup
//! escapes into the global namespace).

use expo_alpha_typecheck::CheckedProgram;
use expo_ast::identifier::Identifier;

mod common;

use common::typecheck_file as typecheck;

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
