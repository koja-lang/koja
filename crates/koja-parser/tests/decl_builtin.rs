//! Coverage for `builtin` declarations.
//!
//! Pins:
//! - empty bodies, inline functions (`fn`, `priv fn`, `@annotation fn`)
//! - generic builtins (`builtin List<T>`)
//! - field-shaped lines and nested types are parse errors

use koja_ast::ast::Visibility;

mod common;

use common::{first_builtin, parse_failing_with};

#[test]
fn empty_builtin() {
    let b = first_builtin(
        "
        builtin String
        end
        ",
    );
    assert_eq!(b.name(), "String");
    assert!(b.functions.is_empty());
    assert!(b.type_params.is_empty());
    assert_eq!(b.visibility, Visibility::Public);
}

#[test]
fn builtin_with_type_params() {
    let b = first_builtin(
        "
        builtin Map<K, V>
        end
        ",
    );
    assert_eq!(b.name(), "Map");
    assert_eq!(b.type_params.len(), 2);
    assert_eq!(b.type_params[0].name, "K");
    assert_eq!(b.type_params[1].name, "V");
}

#[test]
fn builtin_with_inline_functions() {
    let b = first_builtin(
        "
        builtin List<T>
          fn length(self) -> Int
            0
          end

          priv fn helper() -> Int
            1
          end
        end
        ",
    );
    assert_eq!(b.functions.len(), 2);
    assert_eq!(b.functions[0].name, "length");
    assert_eq!(b.functions[1].name, "helper");
    assert_eq!(b.functions[1].visibility, Visibility::Private);
}

#[test]
fn builtin_with_annotated_function() {
    let b = first_builtin(
        "
        builtin String
          @doc \"the number of bytes\"
          fn length(self) -> Int
            0
          end
        end
        ",
    );
    assert_eq!(b.functions.len(), 1);
    assert_eq!(b.functions[0].annotations[0].name, "doc");
}

#[test]
fn builtin_with_top_level_annotation() {
    let b = first_builtin(
        "
        @doc \"an immutable UTF-8 string\"
        builtin String
        end
        ",
    );
    assert_eq!(b.annotations.len(), 1);
    assert_eq!(b.annotations[0].name, "doc");
}

#[test]
fn builtin_rejects_fields() {
    parse_failing_with(
        "
        builtin String
          length: Int
        end
        ",
        &["expected a function declaration in builtin block"],
    );
}

#[test]
fn builtin_rejects_nested_types() {
    parse_failing_with(
        "
        builtin String
          struct Inner
            x: Int
          end
        end
        ",
        &["expected a function declaration in builtin block"],
    );
}

#[test]
fn priv_builtin_parses() {
    // Visibility is recorded at parse time. Typecheck rejects it.
    let b = first_builtin(
        "
        priv builtin String
        end
        ",
    );
    assert_eq!(b.visibility, Visibility::Private);
}
