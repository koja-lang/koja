//! Eval coverage for the pure Koja `Checksum` implementation.

use koja_ir_eval::Value;

mod common;

use common::evaluate_script;

#[test]
fn crc32_matches_the_standard_check_value() {
    let value = evaluate_script(r#"Checksum.crc32("123456789".to_binary())"#)
        .expect("CRC-32 should evaluate");
    assert_eq!(value, Value::Int(0xCBF4_3926));
}

#[test]
fn crc32c_matches_the_standard_check_value() {
    let value = evaluate_script(r#"Checksum.crc32c("123456789".to_binary())"#)
        .expect("CRC-32C should evaluate");
    assert_eq!(value, Value::Int(0xE306_9283));
}

#[test]
fn checksums_include_embedded_zero_bytes() {
    let crc32 = evaluate_script("Checksum.crc32(<<0, 1, 0, 255>>)")
        .expect("CRC-32 should include zero bytes");
    let crc32c = evaluate_script("Checksum.crc32c(<<0, 1, 0, 255>>)")
        .expect("CRC-32C should include zero bytes");

    assert_eq!(crc32, Value::Int(0x0D84_5AA6));
    assert_eq!(crc32c, Value::Int(0x405B_8AE8));
}
