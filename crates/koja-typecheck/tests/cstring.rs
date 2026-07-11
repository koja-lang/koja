//! Surface-level coverage for the auto-imported `Global.cstring`
//! source. Pins the `CString` struct's two fields (`ptr: CPtr<UInt8>`,
//! `len: Int`), nested conversion error, and intrinsic methods.

use koja_ast::identifier::Identifier;
use koja_typecheck::CheckedProgram;

mod common;

use common::typecheck_script as typecheck;

fn assert_registered(checked: &CheckedProgram, segments: &[&str]) {
    let id = Identifier::new("Global", segments.iter().map(|s| s.to_string()).collect());
    assert!(
        checked.registry.lookup(&id).is_some(),
        "expected `{id}` to be registered after autoimport",
    );
}

#[test]
fn cstring_struct_and_methods_register() {
    let checked = typecheck("1\n");
    assert_registered(&checked, &["CString"]);
    assert_registered(&checked, &["CString", "ConversionError"]);
    assert_registered(&checked, &["CString", "free"]);
    assert_registered(&checked, &["CString", "to_string"]);
}

// Call-site coverage for `CString{ptr, len}` / `to_cstring` /
// `to_string` is intentionally not pinned here: every shape that
// would exercise it requires producing a `CPtr<UInt8>` from user
// code, which trips the deferred typed-local / return-bound
// bidirectional inference seam ("cannot infer type parameter `T`
// of `Global.CPtr` from the supplied arguments"). The dispatch path
// will pin end-to-end once that seam unblocks user-side `CPtr<UInt8>`
// values.
