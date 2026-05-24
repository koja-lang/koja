//! Coverage for top-level `fn` declarations.
//!
//! Pins:
//! - bare fn with no params, no return type, no body
//! - parameter parsing (borrow, move, default value, self receiver)
//! - return-type parsing
//! - body presence detection (empty body, normal body, bodyless for
//!   `@extern` / `@intrinsic`, bodyless before a sibling `fn`)
//! - visibility (`fn` is Public, `priv fn` is Private)

use koja_ast::ast::{Item, Param, PassMode, TypeExpr, Visibility};
use koja_ast::util::dedent;

mod common;

use common::parse_clean;

fn first_function(source: &str) -> koja_ast::ast::Function {
    let file = parse_clean(source);
    for item in file.items {
        if let Item::Function(f) = item {
            return f;
        }
    }
    panic!("no function in parsed output");
}

fn nth_function(source: &str, idx: usize) -> koja_ast::ast::Function {
    let file = parse_clean(source);
    let mut funcs: Vec<_> = file
        .items
        .into_iter()
        .filter_map(|item| {
            if let Item::Function(f) = item {
                Some(f)
            } else {
                None
            }
        })
        .collect();
    assert!(idx < funcs.len(), "only {} functions parsed", funcs.len());
    funcs.swap_remove(idx)
}

#[test]
fn bare_fn_no_params() {
    let src = dedent(
        "
        fn run
          42
        end
        ",
    );
    let f = first_function(&src);
    assert_eq!(f.name, "run");
    assert!(f.params.is_empty());
    assert!(f.return_type.is_none());
    assert!(f.body.is_some());
    assert_eq!(f.visibility, Visibility::Public);
}

#[test]
fn fn_with_typed_params() {
    let src = dedent(
        "
        fn add(a: Int, b: Int) -> Int
          a + b
        end
        ",
    );
    let f = first_function(&src);
    assert_eq!(f.params.len(), 2);
    match &f.params[0] {
        Param::Regular { name, mode, .. } => {
            assert_eq!(name, "a");
            assert_eq!(*mode, PassMode::Borrow);
        }
        other => panic!("expected Regular param, got {other:?}"),
    }
    assert!(matches!(f.return_type, Some(TypeExpr::Named { ref path, .. }) if path == &["Int"]));
}

#[test]
fn fn_with_move_param() {
    let src = dedent(
        "
        fn consume(move s: String) -> String
          s
        end
        ",
    );
    let f = first_function(&src);
    match &f.params[0] {
        Param::Regular { name, mode, .. } => {
            assert_eq!(name, "s");
            assert_eq!(*mode, PassMode::Move);
        }
        other => panic!("expected Regular param, got {other:?}"),
    }
}

#[test]
fn fn_with_default_param() {
    let src = dedent(
        "
        fn greet(name: String = \"world\") -> String
          name
        end
        ",
    );
    let f = first_function(&src);
    match &f.params[0] {
        Param::Regular { default, .. } => assert!(default.is_some()),
        other => panic!("expected Regular param, got {other:?}"),
    }
}

#[test]
fn fn_with_self_borrow() {
    let src = dedent(
        "
        extend Counter
          fn value(self) -> Int
            self.value
          end
        end
        ",
    );
    let file = parse_clean(&src);
    let block = match &file.items[0] {
        Item::Extend(b) => b,
        other => panic!("expected extend block, got {other:?}"),
    };
    let func = match &block.members[0] {
        koja_ast::ast::ImplMember::Function(f) => f,
        other => panic!("expected function member, got {other:?}"),
    };
    match &func.params[0] {
        Param::Self_ { mode, .. } => assert_eq!(*mode, PassMode::Borrow),
        other => panic!("expected Self_ param, got {other:?}"),
    }
}

#[test]
fn fn_with_move_self() {
    let src = dedent(
        "
        extend Counter
          fn into_value(move self) -> Int
            self.value
          end
        end
        ",
    );
    let file = parse_clean(&src);
    let block = match &file.items[0] {
        Item::Extend(b) => b,
        other => panic!("expected extend block, got {other:?}"),
    };
    let func = match &block.members[0] {
        koja_ast::ast::ImplMember::Function(f) => f,
        other => panic!("expected function member, got {other:?}"),
    };
    match &func.params[0] {
        Param::Self_ { mode, .. } => assert_eq!(*mode, PassMode::Move),
        other => panic!("expected Self_ param, got {other:?}"),
    }
}

#[test]
fn fn_empty_body_is_some_with_no_statements() {
    let src = dedent(
        "
        fn noop
        end
        ",
    );
    let f = first_function(&src);
    assert!(f.body.as_ref().is_some_and(|b| b.is_empty()));
}

#[test]
fn fn_extern_has_no_body() {
    let src = dedent(
        "
        @extern \"C\"
        fn libc_abs(n: Int32) -> Int32
        ",
    );
    let f = first_function(&src);
    assert!(f.body.is_none());
    assert_eq!(f.annotations.len(), 1);
}

#[test]
fn fn_intrinsic_has_no_body() {
    let src = dedent(
        "
        @intrinsic \"List.len\"
        fn list_len(items: List<Int>) -> Int
        ",
    );
    let f = first_function(&src);
    assert!(f.body.is_none());
}

#[test]
fn priv_fn_is_private() {
    let src = dedent(
        "
        priv fn helper
          1
        end
        ",
    );
    let f = first_function(&src);
    assert_eq!(f.visibility, Visibility::Private);
}

#[test]
fn fn_followed_by_fn_each_has_body() {
    let src = dedent(
        "
        fn first
          1
        end

        fn second
          2
        end
        ",
    );
    let a = nth_function(&src, 0);
    let b = nth_function(&src, 1);
    assert_eq!(a.name, "first");
    assert_eq!(b.name, "second");
    assert!(a.body.is_some());
    assert!(b.body.is_some());
}

#[test]
fn fn_with_type_parameters() {
    let src = dedent(
        "
        fn identity<T>(x: T) -> T
          x
        end
        ",
    );
    let f = first_function(&src);
    assert_eq!(f.type_params.len(), 1);
    assert_eq!(f.type_params[0].name, "T");
}
