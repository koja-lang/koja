//! Coverage for the `infer_return_modes` pass (`src/pipeline/return_mode`).
//!
//! Asserts the [`ReturnMode`] stamped onto each function's
//! [`FunctionSignature`]: fresh heap / `move`-through / owned-returning
//! callees are `Owned`; aliasing views (field access, borrowed
//! intrinsics), plain literals, and recursive cycles are `Borrowed`.

use koja_ast::ast::ReturnMode;
use koja_ast::identifier::Identifier;
use koja_ast::util::dedent;
use koja_typecheck::{CheckedProgram, GlobalKind};

mod common;

use common::{PACKAGE, typecheck_file as typecheck};

fn return_mode(checked: &CheckedProgram, path: &[&str]) -> ReturnMode {
    let identifier = Identifier::new(PACKAGE, path.iter().map(|s| s.to_string()).collect());
    let (_, entry) = checked
        .registry
        .lookup(&identifier)
        .unwrap_or_else(|| panic!("`{identifier}` not registered"));
    let GlobalKind::Function(Some(signature)) = &entry.kind else {
        panic!("`{identifier}` should have a lifted signature");
    };
    signature.return_mode
}

#[test]
fn concat_result_is_owned() {
    let checked = typecheck(&dedent(
        "
        fn joined() -> String
          \"a\" <> \"b\"
        end
        ",
    ));
    assert_eq!(return_mode(&checked, &["joined"]), ReturnMode::Owned);
}

#[test]
fn plain_literal_is_borrowed() {
    let checked = typecheck(&dedent(
        "
        fn greeting() -> String
          \"hi\"
        end
        ",
    ));
    assert_eq!(return_mode(&checked, &["greeting"]), ReturnMode::Borrowed);
}

#[test]
fn move_param_through_is_owned() {
    let checked = typecheck(&dedent(
        "
        fn identity(move s: String) -> String
          s
        end
        ",
    ));
    assert_eq!(return_mode(&checked, &["identity"]), ReturnMode::Owned);
}

#[test]
fn borrowed_param_through_is_borrowed() {
    let checked = typecheck(&dedent(
        "
        fn passthrough(s: String) -> String
          s
        end
        ",
    ));
    assert_eq!(
        return_mode(&checked, &["passthrough"]),
        ReturnMode::Borrowed
    );
}

#[test]
fn field_getter_is_borrowed() {
    let checked = typecheck(&dedent(
        "
        struct Box
          raw: String
        end

        extend Box
          fn raw_field(self) -> String
            self.raw
          end
        end
        ",
    ));
    assert_eq!(
        return_mode(&checked, &["Box", "raw_field"]),
        ReturnMode::Borrowed,
    );
}

#[test]
fn clone_intrinsic_result_is_owned() {
    let checked = typecheck(&dedent(
        "
        fn duplicate(s: String) -> String
          s.clone()
        end
        ",
    ));
    assert_eq!(return_mode(&checked, &["duplicate"]), ReturnMode::Owned);
}

#[test]
fn owned_callee_propagates_to_caller() {
    let checked = typecheck(&dedent(
        "
        fn fresh() -> String
          \"a\" <> \"b\"
        end

        fn relay() -> String
          fresh()
        end
        ",
    ));
    assert_eq!(return_mode(&checked, &["fresh"]), ReturnMode::Owned);
    assert_eq!(return_mode(&checked, &["relay"]), ReturnMode::Owned);
}

#[test]
fn recursive_cycle_is_borrowed() {
    let checked = typecheck(&dedent(
        "
        fn spin(n: Int) -> String
          spin(n)
        end
        ",
    ));
    assert_eq!(return_mode(&checked, &["spin"]), ReturnMode::Borrowed);
}

#[test]
fn struct_construction_is_owned() {
    let checked = typecheck(&dedent(
        "
        struct Point
          x: Int
          y: Int
        end

        fn make() -> Point
          Point { x: 1, y: 2 }
        end
        ",
    ));
    assert_eq!(return_mode(&checked, &["make"]), ReturnMode::Owned);
}
