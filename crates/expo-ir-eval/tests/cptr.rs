//! Eval coverage for the `CPtr<T>` / `CString` family. Eval backs
//! `CPtr<T>` with a raw [`*mut u8`] so the same FFI paths the LLVM
//! backend emits run in-process: `CPtr.null` / `null?` / `alloc` /
//! `free` / `offset` / `read` / `write` route through libc, and
//! `String.to_cstring` + `CString.to_string` round-trip a String
//! through a null-terminated `malloc` copy.
//!
//! `CPtr<UInt8>.to_string` reads the v1 length-prefixed Expo string
//! ABI; we exercise it via `Random.bytes` (see the `random.rs` test
//! suite) since constructing a header-prefixed buffer manually from
//! pure Expo is awkward.

use expo_alpha_ir_eval::Value;
use expo_ast::util::dedent;

mod common;

use common::evaluate_script;

#[test]
fn cptr_null_returns_a_null_pointer() {
    // Type-annotated binding pins `T = UInt8` for `CPtr.null` —
    // alpha's bidirectional inference reads it off the lhs.
    let outcome = evaluate_script(&dedent(
        r#"
        p: CPtr<UInt8> = CPtr.null()
        p.null?()
        "#,
    ))
    .expect("CPtr.null<UInt8>() should evaluate");
    assert_eq!(outcome, Value::Bool(true));
}

#[test]
fn cptr_alloc_then_free_round_trips_without_panicking() {
    // The script returns `true` if everything ran. We can't assert
    // pointer values (they're entropy-dependent), but allocating a
    // buffer and freeing it without panicking pins the libc seam.
    let outcome = evaluate_script(&dedent(
        r#"
        ptr: CPtr<UInt8> = CPtr.alloc(16)
        is_null = ptr.null?()
        ptr.free()
        not is_null
        "#,
    ))
    .expect("alloc → null? → free chain should evaluate cleanly");
    assert_eq!(outcome, Value::Bool(true));
}

#[test]
fn cptr_write_then_read_round_trips_a_byte() {
    // Allocate a single-byte buffer, write a UInt8 marker, read it
    // back. Round-tripping a primitive value through a raw pointer
    // pins the `CPtr.read` / `CPtr.write` size-aware shims. The
    // write goes through a helper fn so the `byte: UInt8` arg slot
    // exercises narrow-int call-site coercion on the literal.
    let outcome = evaluate_script(&dedent(
        r#"
        fn poke(p: CPtr<UInt8>, byte: UInt8)
          p.write(byte)
        end

        ptr: CPtr<UInt8> = CPtr.alloc(1)
        poke(ptr, 0x42)
        v = ptr.read()
        ptr.free()
        v
        "#,
    ))
    .expect("write → read → free chain should evaluate cleanly");
    assert_eq!(outcome, Value::Int(0x42));
}

#[test]
fn cptr_offset_steps_by_element_width_for_uint8() {
    // `UInt8`'s element size is 1, so `ptr.offset(3)` lands 3 bytes
    // past the base. Write distinct markers at each step, then
    // verify the byte at offset 3 matches what we poked there.
    // Eval reads UInt8 into `Value::Int` (no distinct narrow
    // variant), so a successful round-trip lands as `Int(40)`.
    let outcome = evaluate_script(&dedent(
        r#"
        fn poke(p: CPtr<UInt8>, byte: UInt8)
          p.write(byte)
        end

        ptr: CPtr<UInt8> = CPtr.alloc(4)
        poke(ptr, 10)
        poke(ptr.offset(1), 20)
        poke(ptr.offset(2), 30)
        poke(ptr.offset(3), 40)
        v = ptr.offset(3).read()
        ptr.free()
        v
        "#,
    ))
    .expect("offset / read / write across 4 bytes should evaluate");
    assert_eq!(outcome, Value::Int(40));
}

#[test]
fn string_to_cstring_then_to_string_round_trips_utf8() {
    // `String.to_cstring` allocates a null-terminated copy and
    // wraps `(ptr, byte_len)` in a CString; `CString.to_string`
    // reads `byte_len` bytes back into a fresh String. Pin the
    // round-trip for an ASCII string (every byte is its own
    // codepoint, so byte_len == char count).
    let outcome = evaluate_script(&dedent(
        r#"
        cstr = "hello".to_cstring()
        copy = cstr.to_string()
        cstr.free()
        copy
        "#,
    ))
    .expect("to_cstring → to_string → free chain should evaluate");
    assert_eq!(outcome, Value::String("hello".into()));
}

#[test]
fn string_to_cstring_round_trips_multibyte_utf8() {
    // Non-ASCII codepoints take >1 byte each. `to_cstring` carries
    // byte_len, so the round-trip preserves the original payload
    // verbatim — `to_string` slices off exactly byte_len bytes.
    let outcome = evaluate_script(&dedent(
        r#"
        cstr = "héllo".to_cstring()
        copy = cstr.to_string()
        cstr.free()
        copy
        "#,
    ))
    .expect("multibyte to_cstring → to_string round-trip");
    assert_eq!(outcome, Value::String("héllo".into()));
}
