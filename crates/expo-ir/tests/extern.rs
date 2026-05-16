//! Coverage for `@extern "C"` lowering in `src/lower/package.rs`:
//!
//! - a bodyless `@extern "C"` decl lowers to an [`IRFunction`] with
//!   [`FunctionKind::Extern(attrs)`] carrying the AST's `link_name` /
//!   `link_lib` payloads, and zero basic blocks (the LLVM backend
//!   declares the C symbol; eval refuses to call into them);
//! - per-function attrs survive lowering — `@link "lib:sym"` produces
//!   both `link_lib` and `link_name`; bare `@extern "C"` produces no
//!   link metadata;
//! - the program-level `link_libraries` is deduped + sorted across
//!   every `@extern "C"` function, regardless of which package or
//!   `@link "lib:sym"` shape contributed.

use expo_ast::util::dedent;
use expo_ir::{FunctionKind, IRType};

mod common;

const PACKAGE: &str = "TestApp";

#[test]
fn bare_extern_c_lowers_to_function_kind_extern_with_empty_blocks() {
    let source = "
        @extern \"C\"
        fn cosf(x: Float32) -> Float32
        ";

    let script = common::lower_script_source_in(PACKAGE, &dedent(source));
    let mangled = format!("{PACKAGE}.cosf");
    let function = script
        .function(&mangled)
        .unwrap_or_else(|| panic!("missing extern fn `{mangled}` in IRScript"));

    let FunctionKind::Extern(attrs) = &function.kind else {
        panic!(
            "expected FunctionKind::Extern for `{mangled}`; got {:?}",
            function.kind,
        );
    };
    assert_eq!(
        attrs.link_lib, None,
        "bare @extern \"C\" carries no library"
    );
    assert_eq!(
        attrs.link_name, None,
        "bare @extern \"C\" carries no symbol override",
    );
    assert!(
        function.blocks.is_empty(),
        "extern fn body should lower to zero blocks; got {}",
        function.blocks.len(),
    );
    assert_eq!(function.return_type, IRType::Float32);
    assert_eq!(function.params.len(), 1);
    assert_eq!(function.params[0].ty, IRType::Float32);
}

#[test]
fn link_lib_only_populates_link_lib_field() {
    let source = "
        @extern \"C\"
        @link \"m\"
        fn cosf(x: Float32) -> Float32
        ";

    let script = common::lower_script_source_in(PACKAGE, &dedent(source));
    let function = script
        .function(&format!("{PACKAGE}.cosf"))
        .expect("missing extern fn `cosf` in IRScript");

    let FunctionKind::Extern(attrs) = &function.kind else {
        panic!("expected FunctionKind::Extern; got {:?}", function.kind);
    };
    assert_eq!(attrs.link_lib.as_deref(), Some("m"));
    assert_eq!(attrs.link_name, None);
}

#[test]
fn link_lib_with_symbol_alias_populates_both_fields() {
    let source = "
        @extern \"C\"
        @link \"m:cos\"
        fn cosf(x: Float32) -> Float32
        ";

    let script = common::lower_script_source_in(PACKAGE, &dedent(source));
    let function = script
        .function(&format!("{PACKAGE}.cosf"))
        .expect("missing extern fn `cosf` in IRScript");

    let FunctionKind::Extern(attrs) = &function.kind else {
        panic!("expected FunctionKind::Extern; got {:?}", function.kind);
    };
    assert_eq!(attrs.link_lib.as_deref(), Some("m"));
    assert_eq!(attrs.link_name.as_deref(), Some("cos"));
}

#[test]
fn link_libraries_are_deduped_and_sorted() {
    // `crypto` referenced twice (with two different aliases) should
    // collapse into a single `-lcrypto`. `m` and `crypto` should
    // surface in lexical order.
    let source = "
        @extern \"C\"
        @link \"m:cos\"
        fn cosf(x: Float32) -> Float32

        @extern \"C\"
        @link \"crypto:SHA256_Init\"
        fn sha_init(ctx: CPtr<UInt8>) -> Int32

        @extern \"C\"
        @link \"crypto:SHA256_Update\"
        fn sha_update(ctx: CPtr<UInt8>) -> Int32

        @extern \"C\"
        fn unlibbed(x: Int32) -> Int32
        ";

    let script = common::lower_script_source_in(PACKAGE, &dedent(source));
    assert_eq!(
        script.link_libraries,
        vec!["crypto".to_string(), "m".to_string()],
        "expected sorted, deduped libraries; bare @extern \"C\" with no @link contributes nothing",
    );
}

#[test]
fn link_libraries_empty_without_any_extern_c() {
    let source = "
        fn helper -> Int
          1
        end

        helper()
        ";

    let script = common::lower_script_source_in(PACKAGE, &dedent(source));
    assert!(
        script.link_libraries.is_empty(),
        "no @extern \"C\" decls should leave link_libraries empty; got {:?}",
        script.link_libraries,
    );
}

#[test]
fn extern_c_with_cptr_param_lowers_pointee_into_irtype_cptr() {
    let source = "
        @extern \"C\"
        @link \"c\"
        fn malloc(size: UInt64) -> CPtr<UInt8>
        ";

    let script = common::lower_script_source_in(PACKAGE, &dedent(source));
    let function = script
        .function(&format!("{PACKAGE}.malloc"))
        .expect("missing extern fn `malloc`");
    assert_eq!(function.params.len(), 1);
    assert_eq!(function.params[0].ty, IRType::UInt64);
    assert_eq!(
        function.return_type,
        IRType::CPtr(Box::new(IRType::UInt8)),
        "CPtr<UInt8> should lower to IRType::CPtr carrying the pointee",
    );
}

#[test]
fn last_link_wins_across_multiple_link_annotations() {
    // `@link "a:foo"` then `@link "b:bar"` — the second should win
    // for both fields (mirrors v1 + the AST `AnnotationKind::Link`
    // walk in `IRExternAttrs::from_annotations`).
    let source = "
        @extern \"C\"
        @link \"a:foo\"
        @link \"b:bar\"
        fn handle -> Int32
        ";

    let script = common::lower_script_source_in(PACKAGE, &dedent(source));
    let function = script
        .function(&format!("{PACKAGE}.handle"))
        .expect("missing extern fn `handle`");
    let FunctionKind::Extern(attrs) = &function.kind else {
        panic!("expected FunctionKind::Extern; got {:?}", function.kind);
    };
    assert_eq!(attrs.link_lib.as_deref(), Some("b"));
    assert_eq!(attrs.link_name.as_deref(), Some("bar"));
    assert_eq!(script.link_libraries, vec!["b".to_string()]);
}
