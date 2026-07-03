//! Coverage for `impl Protocol for Type` blocks (protocol
//! conformance). Inherent methods now live in `extend Type` blocks.
//! See `decl_extend.rs`.
//!
//! Pins:
//! - `for` is required: bare `impl Type` is rejected with a migration
//!   diagnostic pointing to `extend Type`
//! - inline `type Alias = TypeExpr` members
//! - generic protocol heads + targets
//! - the diagnostic that fires when the body holds something other
//!   than a function or type alias

use koja_ast::ast::{ImplMember, Item, TypeExpr};
use koja_ast::util::dedent;

mod common;

use common::{assert_message_contains, parse_clean, parse_failing};

fn first_impl(source: &str) -> koja_ast::ast::ImplBlock {
    let file = parse_clean(source);
    for item in file.items {
        if let Item::Impl(b) = item {
            return b;
        }
    }
    panic!("no impl block in parsed output");
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
    let trait_path = match &block.trait_expr {
        TypeExpr::Named { path, .. } => path.clone(),
        other => panic!("expected Named trait, got {other:?}"),
    };
    assert_eq!(trait_path, vec!["Show"]);
    assert!(matches!(block.target, TypeExpr::Named { ref path, .. } if path == &["Point"]));
    assert_eq!(block.members.len(), 1);
    assert!(matches!(block.members[0], ImplMember::Function(_)));
}

#[test]
fn trait_impl_with_type_alias_member() {
    let src = dedent(
        "
        impl Counted for Counter
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
fn trait_impl_with_generic_target() {
    let src = dedent(
        "
        impl Show for Box<Int>
          fn show(self) -> String
            \"box\"
          end
        end
        ",
    );
    let block = first_impl(&src);
    assert!(matches!(
        &block.target,
        TypeExpr::Generic { path, .. } if path == &["Box"]
    ));
    assert!(matches!(
        &block.trait_expr,
        TypeExpr::Named { path, .. } if path == &["Show"]
    ));
}

#[test]
fn impl_body_rejects_non_function_non_alias() {
    let src = dedent(
        "
        impl Show for Point
          struct Nested
          end
        end
        ",
    );
    let result = parse_failing(&src);
    assert_message_contains(&result, "expected function or type alias in block body");
}

#[test]
fn bare_impl_emits_migration_diagnostic() {
    // `impl Type` without `for` is reserved for protocol impls. The
    // parser should emit a diagnostic that points the user to
    // `extend Type` instead, with a hint mentioning both the
    // replacement and the protocol-impl form.
    let src = dedent(
        "
        impl Point
          fn origin() -> Point
            Point { x: 0, y: 0 }
          end
        end
        ",
    );
    let result = parse_failing(&src);
    assert_message_contains(
        &result,
        "bare `impl Type` is not supported. Use `extend Type` for inherent methods",
    );
    // The hint should mention both the replacement keyword and the
    // protocol-impl form so a confused user can self-correct
    // regardless of which form they were trying to write.
    let diagnostic = result
        .errors
        .iter()
        .find(|d| d.message.contains("bare `impl Type`"))
        .expect("migration diagnostic present");
    let hint = diagnostic
        .hint
        .as_ref()
        .expect("migration diagnostic carries a hint");
    assert!(
        hint.contains("replace `impl` with `extend`"),
        "hint should suggest the replacement: {hint}"
    );
    assert!(
        hint.contains("impl Protocol for Type"),
        "hint should mention the protocol-impl form: {hint}"
    );
}
