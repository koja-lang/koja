//! Coverage for `extend Type ... end` blocks (inherent methods).
//!
//! Pins:
//! - bare extend on a named target
//! - generic targets (`extend Box<Int>`)
//! - dotted targets (`extend Net.TCPSocket`)
//! - `priv fn` members
//! - inline `type Alias = TypeExpr` members

use koja_ast::ast::{ExtendBlock, ImplMember, Item, TypeExpr, Visibility};
use koja_ast::util::dedent;

mod common;

use common::parse_clean;

fn first_extend(source: &str) -> ExtendBlock {
    let file = parse_clean(source);
    for item in file.items {
        if let Item::Extend(b) = item {
            return b;
        }
    }
    panic!("no extend block in parsed output");
}

#[test]
fn extend_block_parses() {
    let src = dedent(
        "
        extend Point
          fn origin() -> Point
            Point { x: 0, y: 0 }
          end
        end
        ",
    );
    let block = first_extend(&src);
    assert!(matches!(block.target, TypeExpr::Named { ref path, .. } if path == &["Point"]));
    assert_eq!(block.members.len(), 1);
    assert!(matches!(block.members[0], ImplMember::Function(_)));
}

#[test]
fn extend_with_priv_fn_parses() {
    let src = dedent(
        "
        extend Counter
          priv fn helper(self) -> Int
            self.value
          end
        end
        ",
    );
    let block = first_extend(&src);
    let ImplMember::Function(function) = &block.members[0] else {
        panic!("expected function member, got {:?}", block.members[0]);
    };
    assert_eq!(function.visibility, Visibility::Private);
    assert_eq!(function.name, "helper");
}

#[test]
fn extend_with_generic_target() {
    let src = dedent(
        "
        extend Box<Int>
          fn unwrap(self) -> Int
            self.inner
          end
        end
        ",
    );
    let block = first_extend(&src);
    assert!(matches!(
        &block.target,
        TypeExpr::Generic { path, .. } if path == &["Box"]
    ));
}

#[test]
fn extend_with_dotted_target() {
    // Cross-package extends use the dotted form to name the target's
    // owning package: `extend Net.TCPSocket` adds methods to the
    // `TCPSocket` struct defined in the `Net` package.
    let src = dedent(
        "
        extend Net.TCPSocket
          fn read_line(self) -> String
            \"line\"
          end
        end
        ",
    );
    let block = first_extend(&src);
    match &block.target {
        TypeExpr::Named { path, .. } => {
            assert_eq!(path, &vec!["Net".to_string(), "TCPSocket".to_string()]);
        }
        other => panic!("expected dotted Named target, got {other:?}"),
    }
}

#[test]
fn extend_with_type_alias_member() {
    let src = dedent(
        "
        extend Counter
          type Snapshot = Int

          fn snapshot(self) -> Snapshot
            self.value
          end
        end
        ",
    );
    let block = first_extend(&src);
    assert_eq!(block.members.len(), 2);
    assert!(matches!(block.members[0], ImplMember::TypeAlias(_)));
    assert!(matches!(block.members[1], ImplMember::Function(_)));
}
