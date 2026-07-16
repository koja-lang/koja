//! Registration coverage for the auto-imported `Global` stdlib
//! surface, table-driven per root type. One typecheck of an empty
//! script must stamp every root and member below, plus smoke-call
//! coverage proving user code can reach each module without the
//! autoimport raising diagnostics.
//!
//! Behavioral seams that need more than a registry lookup (the
//! `Kernel.panic` `Never` rewrite, the universal-Debug fallback, the
//! `CPtr` concrete-impl pinning) keep dedicated tests at the end.

use koja_ast::identifier::{Identifier, Resolution, ResolvedType};
use koja_ast::util::dedent;
use koja_typecheck::GlobalKind;

mod common;

use common::{
    PACKAGE, assert_registered, assert_script_fails_with, function_signature, global_id, int_type,
    trailing_resolution, typecheck_script as typecheck,
};

/// Every root registers under `Global`, and every listed member
/// (function, nested type, or `@extern "C"` shim) registers under it.
const SURFACE: &[(&str, &[&str])] = &[
    (
        "Binary",
        &["at", "byte_size", "format", "slice", "to_bits", "to_string"],
    ),
    ("Bits", &["format", "to_binary"]),
    ("Bool", &["eq", "format", "hash"]),
    (
        "CPtr",
        &[
            "alloc",
            "borrow",
            "copy",
            "free",
            "null",
            "null?",
            "offset",
            "read",
            "strlen",
            "to_binary",
            "to_cstring",
            "write",
        ],
    ),
    ("CString", &["ConversionError", "free", "to_string"]),
    ("Debug", &[]),
    (
        "Fd",
        &[
            "block",
            "close",
            "koja_fd_close",
            "koja_fd_read",
            "koja_fd_write",
            "koja_io_block",
            "koja_rt_unwatch_fd",
            "koja_rt_watch_fd",
            "read",
            "unwatch",
            "watch",
            "write",
        ],
    ),
    (
        "File",
        &[
            "Mode",
            "close",
            "delete",
            "exists?",
            "koja_file_delete",
            "koja_file_exists",
            "koja_file_open",
            "koja_file_read_all",
            "koja_file_rename",
            "koja_file_write_all",
            "open",
            "read",
            "rename",
            "write",
        ],
    ),
    ("Float", &["format", "parse"]),
    ("Float32", &["format"]),
    ("IO", &["Ready", "gets", "puts", "warn", "write"]),
    ("Int", &["eq", "format", "hash", "parse"]),
    ("Int8", &["eq", "format", "hash"]),
    ("Int16", &["eq", "format", "hash"]),
    ("Int32", &["eq", "format", "hash"]),
    ("Kernel", &["panic"]),
    ("List", &["format"]),
    ("Map", &["format"]),
    ("Option", &["format"]),
    ("Pair", &["format"]),
    ("Range", &[]),
    (
        "Random",
        &["bytes", "int", "koja_random_bytes", "koja_random_int"],
    ),
    ("Result", &["format"]),
    ("STDERR", &[]),
    ("STDIN", &[]),
    ("STDOUT", &[]),
    ("Set", &["format"]),
    (
        "String",
        &[
            "ConversionError",
            "alpha?",
            "at",
            "byte_length",
            "codepoints",
            "contains?",
            "digit?",
            "downcase",
            "empty?",
            "ends_with?",
            "eq",
            "escape_debug",
            "get",
            "graphemes",
            "hash",
            "join",
            "length",
            "replace",
            "reverse",
            "slice",
            "split",
            "starts_with?",
            "to_binary",
            "to_cstring",
            "to_float",
            "to_int",
            "trim",
            "trim_end",
            "trim_start",
            "upcase",
            "whitespace?",
        ],
    ),
    (
        "System",
        &[
            "cwd",
            "get_env",
            "hostname",
            "koja_cwd",
            "koja_get_env",
            "koja_hostname",
            "koja_set_env",
            "set_env",
        ],
    ),
    ("UInt8", &["eq", "format", "hash"]),
    ("UInt16", &["eq", "format", "hash"]),
    ("UInt32", &["eq", "format", "hash"]),
    ("UInt64", &["eq", "format", "hash"]),
];

#[test]
fn stdlib_surface_registers_after_autoimport() {
    let checked = typecheck("1\n");
    for (root, members) in SURFACE {
        assert_registered(&checked, &[root]);
        for member in *members {
            assert_registered(&checked, &[root, member]);
        }
    }
}

#[test]
fn user_code_can_call_file_apis() {
    typecheck(&dedent(
        "
        _ = File.write(\"out.txt\", \"hello\")
        File.read(\"out.txt\")
        File.exists?(\"out.txt\")
        File.open(\"out.txt\", File.Mode.Read)
        ",
    ));
}

#[test]
fn user_code_can_call_io_apis() {
    typecheck(&dedent(
        "
        IO.puts(\"hello\")
        IO.warn(\"oops\")
        IO.write(\"hello\")
        ",
    ));
}

#[test]
fn user_code_can_call_eq_and_hash_through_method_chain() {
    typecheck(&dedent(
        "
        1.eq(1)
        42.hash()
        ",
    ));
}

#[test]
fn user_code_can_call_random_apis() {
    typecheck(&dedent(
        "
        Random.int(0, 100)
        Random.bytes(16)
        ",
    ));
}

#[test]
fn user_code_can_call_string_apis() {
    typecheck(&dedent(
        "
        \"hello\".length()
        \"hello\".byte_length()
        \"hello\".eq(\"hello\")
        \"hello\".empty?()
        \"  hi  \".trim()
        \"hello world\".contains?(\"world\")
        ",
    ));
}

#[test]
fn user_code_can_call_system_apis() {
    typecheck(&dedent(
        "
        System.cwd()
        System.get_env(\"HOME\")
        System.set_env(\"FOO\", \"bar\")
        System.hostname()
        ",
    ));
}

#[test]
fn debug_protocol_registers_with_format_print_inspect() {
    let checked = typecheck("1\n");
    let id = Identifier::new("Global", vec!["Debug".to_string()]);
    let (_, entry) = checked
        .registry
        .lookup(&id)
        .expect("Debug protocol should be registered");
    let GlobalKind::Protocol(Some(definition)) = &entry.kind else {
        panic!("Debug should be a lifted protocol; got {:?}", entry.kind);
    };
    let names: Vec<&str> = definition.methods.iter().map(|m| m.name.as_str()).collect();
    for method in ["format", "print", "inspect"] {
        assert!(
            names.contains(&method),
            "Debug.{method} missing; got {names:?}",
        );
    }
}

#[test]
fn universal_debug_fallback_resolves_format_on_bare_type_param() {
    // `T` has no declared bound but `T.format()` resolves through
    // the universal-Debug fallback in
    // `koja-typecheck/src/pipeline/resolve/calls/bounded.rs`.
    let checked = typecheck(&dedent(
        "
        fn show<T>(value: T) -> String
          value.format()
        end

        show(1)
        0
        ",
    ));
    // Panics unless `show` carries a lifted signature.
    function_signature(&checked, PACKAGE, &["show"]);
}

#[test]
fn kernel_panic_registers_with_never_return() {
    let checked = typecheck("1\n");
    // The lift_signatures override rewrites the source's `Unit`
    // return into `Global.Never` so callers in match-arm tail
    // position propagate the surrounding arm's expected type instead
    // of mismatching against `Unit`.
    let signature = function_signature(&checked, "Global", &["Kernel", "panic"]);
    let ResolvedType::Named {
        resolution,
        type_args,
    } = &signature.return_type
    else {
        panic!(
            "Kernel.panic return should be a Named type pointing at `Never`; got {:?}",
            signature.return_type,
        );
    };
    assert!(
        type_args.is_empty(),
        "`Never` is nullary; got type_args={type_args:?}",
    );
    assert_eq!(
        resolution,
        &Resolution::Global(global_id(&checked, "Never")),
        "Kernel.panic return should resolve to `Global.Never`; got {resolution:?}",
    );
}

#[test]
fn kernel_panic_callable_in_arm_tail_with_polymorphic_return() {
    // `Option.unwrap` exercises the bidirectional propagation:
    // `Kernel.panic(...)` in the `None` arm tail must fit the `T`
    // expected by the surrounding match. Compiles only if the
    // `Never` rewrite + bidirectional inference are both wired.
    let checked = typecheck(&dedent(
        "
        Option.Some(7).unwrap()
        ",
    ));
    assert_eq!(
        trailing_resolution(&checked),
        int_type(&checked),
        "`Option.Some(7).unwrap()` should resolve to Int end-to-end",
    );
}

// Call-site coverage for `p: CPtr<UInt8> = CPtr.alloc(8)`,
// `alloc_bytes() -> CPtr<UInt8>`, and user-side `CString{ptr, len}` /
// `to_cstring` / `to_string` is intentionally not pinned here. Every
// such shape hits the deferred typed-local / return-bound
// bidirectional inference seam ("cannot infer type parameter `T` of
// `Global.CPtr` from the supplied arguments") and will round-trip
// end-to-end once that seam unblocks `Random.bytes`-style call sites.

#[test]
fn cptr_copy_static_resolves_with_unconstrained_receiver_param() {
    // `copy` is a static on `extend CPtr<UInt8>` whose signature never
    // mentions `T`. The concrete impl pinning must satisfy inference.
    typecheck(&dedent(
        "
        bytes = \"abc\".to_binary()
        owned = CPtr.copy(bytes)
        first = owned.read()
        owned.free()
        ",
    ));
}

#[test]
fn cptr_borrow_static_resolves_in_statement_position() {
    typecheck(&dedent(
        "
        bytes = \"abc\".to_binary()
        CPtr.borrow(bytes).read()
        ",
    ));
}

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
        ptr: CPtr<Int32> = CPtr.alloc(1)
        ptr.write(42)
        ptr.free()
        ",
    ));
}

#[test]
fn cptr_uint8_write_accepts_int_literal_arg() {
    typecheck(&dedent(
        "
        byte_ptr: CPtr<UInt8> = CPtr.alloc(2)
        byte_ptr.write(65)
        byte_ptr.offset(1).write(0)
        byte_ptr.free()
        ",
    ));
}

#[test]
fn cptr_int32_write_rejects_non_literal_int_value() {
    // The literal-widening tolerance only takes effect because the
    // post-substitute arg validator runs `check_compatible`, which
    // coerces a literal but rejects a non-literal `Int` value. The
    // diagnostic flips from "T cannot be both" to a cleaner
    // "argument expects `Int32`, got `Int`". Pin the new wording.
    assert_script_fails_with(
        "
        x: Int = 5
        ptr: CPtr<Int32> = CPtr.alloc(1)
        ptr.write(x)
        ptr.free()
        ",
        &["expects `Int32`", "got `Int`"],
    );
}
