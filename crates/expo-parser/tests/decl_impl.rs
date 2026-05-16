//! Coverage for `impl` blocks.
//!
//! Pins:
//! - inherent vs trait impl shape (`impl Foo` vs `impl Trait for Foo`)
//! - inline `type Alias = TypeExpr` members
//! - generic impls
//! - the diagnostic that fires when the body holds something other
//!   than a function or type alias

use expo_ast::ast::{ImplMember, Item, TypeExpr};
use expo_ast::util::dedent;

mod common;

use common::{assert_message_contains, parse_clean, parse_failing};

fn first_impl(source: &str) -> expo_ast::ast::ImplBlock {
    let file = parse_clean(source);
    for item in file.items {
        if let Item::Impl(b) = item {
            return b;
        }
    }
    panic!("no impl block in parsed output");
}

#[test]
fn inherent_impl() {
    let src = dedent(
        "
        impl Point
          fn origin() -> Point
            Point { x: 0, y: 0 }
          end
        end
        ",
    );
    let block = first_impl(&src);
    assert!(matches!(block.target, TypeExpr::Named { ref path, .. } if path == &["Point"]));
    assert!(block.trait_expr.is_none());
    assert_eq!(block.members.len(), 1);
    assert!(matches!(block.members[0], ImplMember::Function(_)));
}

#[test]
fn trait_impl_for_type() {
    let src = dedent(
        "
        impl Show for Point
          fn show(self) -> String
            \"point\"
          end
        end
        ",
    );
    let block = first_impl(&src);
    let trait_path = match block.trait_expr.as_ref() {
        Some(TypeExpr::Named { path, .. }) => path.clone(),
        other => panic!("expected Named trait, got {other:?}"),
    };
    assert_eq!(trait_path, vec!["Show"]);
    assert!(matches!(block.target, TypeExpr::Named { ref path, .. } if path == &["Point"]));
}

#[test]
fn impl_with_type_alias_member() {
    let src = dedent(
        "
        impl Counter
          type Snapshot = Int

          fn snapshot(self) -> Snapshot
            self.value
          end
        end
        ",
    );
    let block = first_impl(&src);
    assert_eq!(block.members.len(), 2);
    assert!(matches!(block.members[0], ImplMember::TypeAlias(_)));
    assert!(matches!(block.members[1], ImplMember::Function(_)));
}

#[test]
fn impl_with_generic_target() {
    let src = dedent(
        "
        impl Box<Int>
          fn unwrap(self) -> Int
            self.inner
          end
        end
        ",
    );
    let block = first_impl(&src);
    assert!(matches!(
        &block.target,
        TypeExpr::Generic { path, .. } if path == &["Box"]
    ));
}

#[test]
fn impl_body_rejects_non_function_non_alias() {
    let src = dedent(
        "
        impl Point
          struct Nested
          end
        end
        ",
    );
    let result = parse_failing(&src);
    assert_message_contains(&result, "expected function or type alias in impl block");
}
