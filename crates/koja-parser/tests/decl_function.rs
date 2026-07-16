//! Coverage for top-level `fn` declarations.
//!
//! Pins:
//! - bare fn with no params, no return type, no body
//! - parameter parsing (typed param, default value, self receiver)
//! - return-type parsing
//! - body presence detection (empty body, normal body, bodyless for
//!   `@extern` / `@intrinsic`, bodyless before a sibling `fn`)
//! - visibility (`fn` is Public, `priv fn` is Private)

use koja_ast::ast::{Function, ImplMember, Item, Param, TypeExpr, Visibility};

mod common;

use common::{first_extend, first_function, parse_clean};

fn nth_function(source: &str, idx: usize) -> Function {
    parse_clean(source)
        .items
        .into_iter()
        .filter_map(|item| match item {
            Item::Function(f) => Some(f),
            _ => None,
        })
        .nth(idx)
        .unwrap_or_else(|| panic!("fewer than {} functions parsed", idx + 1))
}

#[test]
fn bare_fn_no_params() {
    let f = first_function(
        "
        fn run
          42
        end
        ",
    );
    assert_eq!(f.name, "run");
    assert!(f.params.is_empty());
    assert!(f.return_type.is_none());
    assert!(f.body.is_some());
    assert_eq!(f.visibility, Visibility::Public);
}

#[test]
fn fn_with_typed_params() {
    let f = first_function(
        "
        fn add(a: Int, b: Int) -> Int
          a + b
        end
        ",
    );
    assert_eq!(f.params.len(), 2);
    match &f.params[0] {
        Param::Regular { name, .. } => {
            assert_eq!(name, "a");
        }
        other => panic!("expected Regular param, got {other:?}"),
    }
    assert!(matches!(f.return_type, Some(TypeExpr::Named { ref path, .. }) if path == &["Int"]));
}

#[test]
fn fn_with_default_param() {
    let f = first_function(
        "
        fn greet(name: String = \"world\") -> String
          name
        end
        ",
    );
    match &f.params[0] {
        Param::Regular { default, .. } => assert!(default.is_some()),
        other => panic!("expected Regular param, got {other:?}"),
    }
}

#[test]
fn fn_with_self_borrow() {
    let block = first_extend(
        "
        extend Counter
          fn value(self) -> Int
            self.value
          end
        end
        ",
    );
    let func = match &block.members[0] {
        ImplMember::Function(f) => f,
        other => panic!("expected function member, got {other:?}"),
    };
    match &func.params[0] {
        Param::Self_ { .. } => {}
        other => panic!("expected Self_ param, got {other:?}"),
    }
}

#[test]
fn fn_empty_body_is_some_with_no_statements() {
    let f = first_function(
        "
        fn noop
        end
        ",
    );
    assert!(f.body.as_ref().is_some_and(|b| b.is_empty()));
}

#[test]
fn fn_extern_has_no_body() {
    let f = first_function(
        "
        @extern \"C\"
        fn libc_abs(n: Int32) -> Int32
        ",
    );
    assert!(f.body.is_none());
    assert_eq!(f.annotations.len(), 1);
}

#[test]
fn fn_intrinsic_has_no_body() {
    let f = first_function(
        "
        @intrinsic \"List.len\"
        fn list_len(items: List<Int>) -> Int
        ",
    );
    assert!(f.body.is_none());
}

#[test]
fn priv_fn_is_private() {
    let f = first_function(
        "
        priv fn helper
          1
        end
        ",
    );
    assert_eq!(f.visibility, Visibility::Private);
}

#[test]
fn fn_followed_by_fn_each_has_body() {
    let src = "
        fn first
          1
        end

        fn second
          2
        end
        ";
    let a = nth_function(src, 0);
    let b = nth_function(src, 1);
    assert_eq!(a.name, "first");
    assert_eq!(b.name, "second");
    assert!(a.body.is_some());
    assert!(b.body.is_some());
}

#[test]
fn fn_with_type_parameters() {
    let f = first_function(
        "
        fn identity<T>(x: T) -> T
          x
        end
        ",
    );
    assert_eq!(f.type_params.len(), 1);
    assert_eq!(f.type_params[0].name, "T");
}
