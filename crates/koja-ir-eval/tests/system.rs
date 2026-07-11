use std::env;
use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

use koja_ast::util::dedent;
use koja_ir_eval::RuntimeError;

mod common;

use common::evaluate_script;

#[test]
fn get_env_panics_on_non_utf8_host_value() {
    let key = "KOJA_TEST_NON_UTF8_VALUE";
    unsafe { env::set_var(key, OsStr::from_bytes(b"\xff\xfe")) };
    let error = evaluate_script(&format!("System.get_env(\"{key}\")\n"))
        .expect_err("System.get_env must panic on a non-UTF-8 host value");
    assert_eq!(
        error,
        RuntimeError::Panicked {
            message: format!("System.get_env value for `{key}` is not valid UTF-8"),
        },
    );
}

#[test]
fn get_env_rejects_interior_nul_with_clear_message() {
    let error = evaluate_script(&dedent(
        r#"
        key = <<75, 0, 69, 89>>.to_string().unwrap()
        System.get_env(key)
        "#,
    ))
    .expect_err("System.get_env must reject interior NUL");
    assert_eq!(
        error,
        RuntimeError::Panicked {
            message: "System.get_env key cannot contain U+0000".to_string(),
        },
    );
}

#[test]
fn set_env_rejects_interior_nul_with_clear_message() {
    let error = evaluate_script(&dedent(
        r#"
        value = <<86, 0, 65, 76>>.to_string().unwrap()
        System.set_env("KOJA_TEST", value)
        "#,
    ))
    .expect_err("System.set_env must reject interior NUL");
    assert_eq!(
        error,
        RuntimeError::Panicked {
            message: "System.set_env value cannot contain U+0000".to_string(),
        },
    );
}
