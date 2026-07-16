//! Position checks on `CPtr.borrow` results: the borrowed pointer
//! may only be consumed within the statement that borrows it (call
//! argument, chained receiver). Binding, returning, or storing it
//! escapes the statement and is rejected with a teaching diagnostic.

use koja_ast::util::dedent;

mod common;

use common::{
    assert_file_fails_with, diagnostic_messages, typecheck_file, typecheck_script,
    typecheck_script_fail,
};

fn assert_escape_diagnostic(source: &str, opening: &str) {
    let failure = typecheck_script_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages.iter().any(|m| {
            m.contains(opening) && m.contains("only valid within the statement that borrows it")
        }),
        "expected borrow-escape diagnostic starting with `{opening}`, got {messages:?}",
    );
}

#[test]
fn borrow_as_call_argument_is_accepted() {
    typecheck_file(&dedent(
        "
        struct FFI
          @extern \"C\"
          fn strlen(s: CPtr<UInt8>) -> Int64
        end

        fn main
          bytes = \"abc\".to_binary()
          n = FFI.strlen(CPtr.borrow(bytes))
        end
        ",
    ));
}

#[test]
fn borrow_as_chained_receiver_is_accepted() {
    typecheck_script(&dedent(
        "
        bytes = \"abc\".to_binary()
        CPtr.borrow(bytes).offset(1).read()
        ",
    ));
}

#[test]
fn parenthesized_borrow_receiver_is_accepted() {
    typecheck_script(&dedent(
        "
        bytes = \"abc\".to_binary()
        (CPtr.borrow(bytes)).read()
        ",
    ));
}

#[test]
fn borrow_bound_to_a_local_is_rejected() {
    assert_escape_diagnostic(
        "
        bytes = \"abc\".to_binary()
        p = CPtr.borrow(bytes)
        ",
        "a borrowed pointer cannot be bound to `p`",
    );
}

#[test]
fn borrow_returned_from_a_function_is_rejected() {
    assert_file_fails_with(
        "
        struct Leaker
          fn leak(bytes: Binary) -> CPtr<UInt8>
            return CPtr.borrow(bytes)
          end
        end

        fn main
          1.print()
        end
        ",
        &["a borrowed pointer cannot be returned"],
    );
}

#[test]
fn borrow_as_implicit_tail_return_is_rejected() {
    assert_file_fails_with(
        "
        struct Leaker
          fn leak(bytes: Binary) -> CPtr<UInt8>
            CPtr.borrow(bytes)
          end
        end

        fn main
          1.print()
        end
        ",
        &["a borrowed pointer cannot be returned"],
    );
}

#[test]
fn borrow_in_a_struct_literal_field_is_rejected() {
    assert_escape_diagnostic(
        "
        struct Holder
          p: CPtr<UInt8>
        end

        bytes = \"abc\".to_binary()
        h = Holder{p: CPtr.borrow(bytes)}
        ",
        "a borrowed pointer cannot be stored",
    );
}

#[test]
fn borrow_in_a_list_literal_is_rejected() {
    assert_escape_diagnostic(
        "
        bytes = \"abc\".to_binary()
        ps = [CPtr.borrow(bytes)]
        ",
        "a borrowed pointer cannot be stored",
    );
}

#[test]
fn borrow_as_closure_tail_is_rejected() {
    assert_escape_diagnostic(
        "
        bytes = \"abc\".to_binary()
        f = fn (n: Int) -> CPtr<UInt8>
          CPtr.borrow(bytes)
        end
        ",
        "a borrowed pointer cannot be returned",
    );
}
