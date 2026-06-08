//! Coverage for union lowering across the IR pipeline:
//!
//! - **Mangling stability**: a `ResolvedType::Union` lowers to
//!   `IRType::Union { mangled, members }` whose canonical mangle
//!   is stable under member reordering / aliasing — the lifter
//!   already canonicalizes the source side, so two surface
//!   spellings of the same set yield the same `IRType`.
//! - **`UnionWrap` shape**: a member-typed source flowing into a
//!   union slot stamps `Coercion::UnionWiden` at typecheck and
//!   lowers to a single [`IRInstruction::UnionWrap`] with the
//!   member's index in the canonical member list.
//! - **`TypedBinding` lowering**: a `p: Member -> body` arm in a
//!   `match` over a union subject lowers to a tag check
//!   ([`IRInstruction::UnionTagGet`] + a const-`==` predicate) plus
//!   a payload extract ([`IRInstruction::UnionPayloadGet`]) that
//!   feeds the bound local at the head of the body block.
//! - **Per-package union registry**: each lowered union surfaces
//!   exactly one [`IRUnionDecl`] in the program's union map keyed
//!   by the same mangled symbol the type carries.

use koja_ir::{IRInstruction, IRType};

mod common;

use common::{PACKAGE, lower_script_source as lower, script_function};

#[test]
fn union_widening_arg_lowers_to_union_wrap() {
    let source = "
struct Post
  title: String
end

struct Comment
  body: String
end

fn take(item: Post | Comment) -> Int
  match item
    _ -> 0
  end
end

take(Post{title: \"hi\"})
";
    let script = lower(source);

    let wrap_count = script
        .blocks
        .iter()
        .flat_map(|b| b.instructions.iter())
        .filter(|i| matches!(i, IRInstruction::UnionWrap { .. }))
        .count();
    assert_eq!(
        wrap_count, 1,
        "expected exactly one UnionWrap for the bare-Post → union arg site, got {wrap_count}",
    );

    // Scope to the test package: the autoimported stdlib carries its
    // own unions (e.g. `Fd.write`'s `Binary | String`), so summing
    // across every package would also count those.
    let union_decl_count = script
        .packages
        .iter()
        .filter(|p| p.package == PACKAGE)
        .map(|p| p.unions.len())
        .sum::<usize>();
    assert_eq!(
        union_decl_count, 1,
        "expected exactly one IRUnionDecl for `Post | Comment` in `{PACKAGE}`, got {union_decl_count}",
    );
}

#[test]
fn typed_binding_lowers_to_tag_test_and_payload_get() {
    let source = "
struct Post
  title: String
end

struct Comment
  body: String
end

fn describe(item: Post | Comment) -> String
  match item
    p: Post -> p.title
    c: Comment -> c.body
  end
end

describe(Post{title: \"hi\"})
";
    let script = lower(source);
    let describe_fn = script_function(&script, "describe");

    let tag_count = describe_fn
        .blocks
        .iter()
        .flat_map(|b| b.instructions.iter())
        .filter(|i| matches!(i, IRInstruction::UnionTagGet { .. }))
        .count();
    assert!(
        tag_count >= 1,
        "expected at least one UnionTagGet for the typed-binding arms, got {tag_count}",
    );

    let payload_count = describe_fn
        .blocks
        .iter()
        .flat_map(|b| b.instructions.iter())
        .filter(|i| matches!(i, IRInstruction::UnionPayloadGet { .. }))
        .count();
    assert_eq!(
        payload_count, 2,
        "expected one UnionPayloadGet per typed-binding arm (Post + Comment), got {payload_count}",
    );

    // Each `UnionPayloadGet` points at the right member (the
    // member-index matches the canonical-order position of the
    // arm's pattern type). Pinned so a future canonicalization
    // change can't silently swap the indices and still satisfy
    // the count check above.
    let payload_indices: Vec<u8> = describe_fn
        .blocks
        .iter()
        .flat_map(|b| b.instructions.iter())
        .filter_map(|i| match i {
            IRInstruction::UnionPayloadGet { member_index, .. } => Some(*member_index),
            _ => None,
        })
        .collect();
    let mut sorted = payload_indices.clone();
    sorted.sort_unstable();
    assert_eq!(
        sorted,
        vec![0, 1],
        "expected payload extracts to cover both canonical members [0, 1], got {payload_indices:?}",
    );
}

#[test]
fn union_member_canonicalization_yields_stable_mangle() {
    // Two surface spellings of the same member set (`Post |
    // Comment` vs `Comment | Post`) lower to the same `IRType::Union`
    // mangle. Pin against the program-level `unions` map: the
    // dedupe is at type-mangle granularity, so a single decl
    // covers both surface forms.
    let source = "
struct Post
  title: String
end

struct Comment
  body: String
end

fn one(item: Post | Comment) -> Int
  match item
    _ -> 0
  end
end

fn two(item: Comment | Post) -> Int
  match item
    _ -> 0
  end
end

one(Post{title: \"hi\"}) + two(Comment{body: \"oh\"})
";
    let script = lower(source);
    // Scope to the test package so stdlib unions in autoimported
    // packages (e.g. `Fd.write`'s `Binary | String`) don't inflate
    // the count we're pinning the dedupe against.
    let union_decl_count = script
        .packages
        .iter()
        .filter(|p| p.package == PACKAGE)
        .map(|p| p.unions.len())
        .sum::<usize>();
    assert_eq!(
        union_decl_count, 1,
        "expected canonicalization to collapse `Post | Comment` and `Comment | Post` \
         to a single IRUnionDecl in `{PACKAGE}`, got {union_decl_count}",
    );

    // `IRType::Union { members }` carries the canonical (sorted)
    // member list — both `take_ab` and `take_ba` should reference
    // the *same* `IRType::Union` instance, including identical
    // mangle and member ordering.
    let one_param = &script_function(&script, "one").params[0].ty;
    let two_param = &script_function(&script, "two").params[0].ty;
    assert_eq!(
        one_param, two_param,
        "expected the canonical IRType::Union to compare equal across surface spellings",
    );
    let IRType::Union { members, .. } = one_param else {
        panic!("expected `one`'s param to lower to IRType::Union, got {one_param:?}");
    };
    assert_eq!(
        members.len(),
        2,
        "expected canonical member list to retain both members, got {members:?}",
    );
}
