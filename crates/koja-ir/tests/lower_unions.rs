//! Coverage for union lowering across the IR pipeline:
//!
//! - An `AnonymousKind::Union` lowers to an `IRType::Union` that holds
//!   the canonical mangle, which is stable under member reordering
//!   and aliasing. The lifter already canonicalizes the source side,
//!   so two surface spellings of the same set yield the same
//!   `IRType`.
//! - A member-typed source flowing into a union slot stamps
//!   `Coercion::UnionWiden` at typecheck and lowers to a single
//!   [`IRInstruction::UnionWrap`] with the member's index in the
//!   canonical member list.
//! - A `p: Member -> body` arm in a `match` over a union subject
//!   lowers to a tag check ([`IRInstruction::UnionTagGet`] plus a
//!   const-`==` predicate) and a payload extract
//!   ([`IRInstruction::UnionPayloadGet`]) that feeds the bound
//!   local at the head of the body block.
//! - A call through a protocol bound with a union receiver lowers to
//!   one tag switch with a call to the member's own impl in each arm.
//! - Each package that lowers a union holds exactly one
//!   [`IRUnionDecl`] for it, keyed by the same mangled symbol the
//!   type carries. The members live on the decl. Two packages that
//!   lower one anonymous union hold equal decls.
//! - The union of a recursive `type` alias lowers to an
//!   `IRType::Union` with the alias name, and the package of the
//!   alias holds its decl.
//! - A struct that holds its own union inline gets a boxed field.

use std::collections::BTreeMap;

use koja_ir::{ConstValue, IRInstruction, IRScript, IRSymbol, IRType, IRUnionDecl};

mod common;

use common::{PACKAGE, all_instructions, lower_script_source as lower, script_function};

/// The union decls that `package` holds in `script`.
fn union_decls<'a>(script: &'a IRScript, package: &str) -> &'a BTreeMap<IRSymbol, IRUnionDecl> {
    let package = script
        .packages
        .iter()
        .find(|p| p.package == package)
        .unwrap_or_else(|| panic!("expected a `{package}` package"));
    &package.unions
}

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

    let wrap_count = all_instructions(&script.blocks)
        .filter(|i| matches!(i, IRInstruction::UnionWrap { .. }))
        .count();
    assert_eq!(
        wrap_count, 1,
        "expected exactly one UnionWrap for the bare-Post -> union arg site, got {wrap_count}",
    );

    // Scope to the test package. The autoimported stdlib carries its
    // own unions (e.g. `IO.Descriptor.write`'s `Binary | String`), so summing
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
fn generic_variant_payload_lowers_to_union_wrap() {
    // The return type sets `T` to `Post | Comment`, so the payload is
    // a member. It lowers to one `UnionWrap`, and the `Option.Some`
    // is an `Option` over that union.
    let source = "
        struct Post
          title: String
        end

        struct Comment
          body: String
        end

        fn wrap(post: Post) -> Option<Post | Comment>
          Option.Some(post)
        end

        wrap(Post{title: \"hi\"})
        ";
    let script = lower(source);
    let wrap_fn = script_function(&script, "wrap");

    let wrap_types: Vec<&IRType> = all_instructions(&wrap_fn.blocks)
        .filter_map(|i| match i {
            IRInstruction::UnionWrap { ty, .. } => Some(ty),
            _ => None,
        })
        .collect();
    let [IRType::Union(union)] = wrap_types.as_slice() else {
        panic!("expected exactly one UnionWrap for the `Option.Some` payload, got {wrap_types:?}");
    };

    let constructed: Vec<String> = all_instructions(&wrap_fn.blocks)
        .filter_map(|i| match i {
            IRInstruction::EnumConstruct { ty, .. } => Some(ty.to_string()),
            _ => None,
        })
        .collect();
    assert!(
        matches!(constructed.as_slice(), [option] if option.contains(&union.to_string())),
        "expected one `Option.Some` over `{union}`, got {constructed:?}",
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

    let tag_count = all_instructions(&describe_fn.blocks)
        .filter(|i| matches!(i, IRInstruction::UnionTagGet { .. }))
        .count();
    assert!(
        tag_count >= 1,
        "expected at least one UnionTagGet for the typed-binding arms, got {tag_count}",
    );

    let payload_count = all_instructions(&describe_fn.blocks)
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
    let payload_indices: Vec<u8> = all_instructions(&describe_fn.blocks)
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
fn bounded_call_on_union_lowers_to_one_member_call_per_arm() {
    // `show<Post | Comment>` cannot name one `label`, so the call
    // switches on the tag and each arm calls the member's own impl
    // with the shared argument.
    let source = "
        protocol Label
          fn label(self, prefix: String) -> String
        end

        struct Post: Label
          title: String

          fn label(self, prefix: String) -> String
            prefix <> self.title
          end
        end

        struct Comment: Label
          body: String

          fn label(self, prefix: String) -> String
            prefix <> self.body
          end
        end

        fn show<T: Label>(value: T) -> String
          value.label(\"> \")
        end

        item: Post | Comment = Post{title: \"hi\"}
        show(item)
        ";
    let script = lower(source);
    let show_fn = script
        .packages
        .iter()
        .filter(|p| p.package == PACKAGE)
        .flat_map(|p| p.functions.values())
        .find(|f| f.symbol.mangled().starts_with(&format!("{PACKAGE}.show")))
        .expect("expected one monomorphized `show`");

    let tag_count = all_instructions(&show_fn.blocks)
        .filter(|i| matches!(i, IRInstruction::UnionTagGet { .. }))
        .count();
    assert_eq!(
        tag_count, 1,
        "expected one tag read for the dispatch, got {tag_count}"
    );

    let mut label_calls: Vec<(&str, usize)> = all_instructions(&show_fn.blocks)
        .filter_map(|i| match i {
            IRInstruction::Call { callee, args, .. } if callee.mangled().contains(".label") => {
                Some((callee.mangled(), args.len()))
            }
            _ => None,
        })
        .collect();
    label_calls.sort_unstable();
    assert_eq!(
        label_calls,
        vec![("TestApp.Comment.label/2", 2), ("TestApp.Post.label/2", 2)],
        "expected one `label` call per member, each with the payload and the argument",
    );

    let literal_count = all_instructions(&show_fn.blocks)
        .filter(|i| {
            matches!(
                i,
                IRInstruction::Const {
                    value: ConstValue::String(_),
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        literal_count, 1,
        "expected the argument to lower one time ahead of the switch, got {literal_count}",
    );
}

#[test]
fn union_member_canonicalization_yields_stable_mangle() {
    // Two surface spellings of the same member set (`Post |
    // Comment` vs `Comment | Post`) lower to the same `IRType::Union`
    // mangle. Pin against the program-level `unions` map. The
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
    // packages (e.g. `IO.Descriptor.write`'s `Binary | String`) don't inflate
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

    // Both `one` and `two` name the same `IRType::Union`, and its
    // decl carries the canonical (sorted) member list.
    let one_param = &script_function(&script, "one").params[0].ty;
    let two_param = &script_function(&script, "two").params[0].ty;
    assert_eq!(
        one_param, two_param,
        "expected the canonical IRType::Union to compare equal across surface spellings",
    );
    let IRType::Union(symbol) = one_param else {
        panic!("expected `one`'s param to lower to IRType::Union, got {one_param:?}");
    };
    let members = &union_decls(&script, PACKAGE)[symbol.mangled()].members;
    assert_eq!(
        members.len(),
        2,
        "expected canonical member list to retain both members, got {members:?}",
    );
}

#[test]
fn each_package_that_lowers_an_anonymous_union_holds_an_equal_decl() {
    // The stdlib names `Binary | String` in `IO.Descriptor.write`,
    // and the script names it again. Each package declares the
    // union, and the decls are equal.
    let source = "
        fn size(data: Binary | String) -> Int
          match data
            _ -> 0
          end
        end

        size(\"hi\")
        ";
    let script = lower(source);

    let holders: Vec<(&str, &IRUnionDecl)> = script
        .packages
        .iter()
        .filter_map(|p| {
            let decl = p.unions.get("Union_Binary_or_String")?;
            Some((p.package.as_str(), decl))
        })
        .collect();
    let names: Vec<&str> = holders.iter().map(|(name, _)| *name).collect();
    assert!(
        names.len() >= 2 && names.contains(&PACKAGE),
        "expected `{PACKAGE}` and a stdlib package to declare the union, got {names:?}",
    );
    let (_, first) = holders[0];
    assert_eq!(first.members, vec![IRType::Binary, IRType::String]);
    for (name, decl) in &holders {
        assert_eq!(*decl, first, "expected `{name}` to hold an equal decl");
    }
}

#[test]
fn anonymous_union_inside_a_recursive_alias_lands_in_the_alias_package() {
    // Only the members of `Value` name `Bool | String`. No function
    // lowers it, so the package of the alias must declare it.
    let source = "
        type Value = Int | List<Value> | Map<String, Bool | String>

        fn depth(value: Value) -> Int
          match value
            _ -> 0
          end
        end

        leaf: Value = 1
        depth(leaf)
        ";
    let script = lower(source);

    let decls = union_decls(&script, PACKAGE);
    let alias = &decls[format!("{PACKAGE}.Value").as_str()];
    let inner = &decls["Union_Bool_or_String"];
    assert_eq!(inner.members, vec![IRType::Bool, IRType::String]);
    assert_eq!(
        alias.members[2],
        IRType::Map {
            key: Box::new(IRType::String),
            value: Box::new(IRType::Union(inner.symbol.clone())),
        },
        "expected the map member to name the anonymous union by symbol",
    );
}

#[test]
fn recursive_alias_lowers_to_one_named_union() {
    // `by_name` names the alias and `spelled` writes its union out.
    // Both lower to the same `IRType::Union`, and one decl keyed by
    // the alias name holds the members.
    let source = "
        type Value = Int | List<Value>

        fn by_name(value: Value) -> Int
          match value
            _ -> 0
          end
        end

        fn spelled(value: Int | List<Value>) -> Int
          match value
            _ -> 0
          end
        end

        nested: Value = [1, [2]]
        by_name(nested) + spelled(nested)
        ";
    let script = lower(source);

    let by_name = &script_function(&script, "by_name").params[0].ty;
    let spelled = &script_function(&script, "spelled").params[0].ty;
    let IRType::Union(symbol) = by_name else {
        panic!("expected `by_name`'s param to lower to IRType::Union, got {by_name:?}");
    };
    assert_eq!(symbol.mangled(), format!("{PACKAGE}.Value"));
    assert_eq!(
        by_name, spelled,
        "expected the union written out to lower to the alias's named union",
    );

    let decls: Vec<_> = script
        .packages
        .iter()
        .filter(|p| p.package == PACKAGE)
        .flat_map(|p| p.unions.values())
        .collect();
    assert_eq!(
        decls.len(),
        1,
        "expected exactly one IRUnionDecl for `Value` in `{PACKAGE}`, got {decls:?}",
    );
    let decl = decls[0];
    assert_eq!(&decl.symbol, symbol);
    assert_eq!(
        decl.members,
        vec![IRType::Int64, IRType::List(Box::new(by_name.clone()))],
        "expected the list member to name the union again",
    );
    // The widest member is the 24-byte list `{ buf_ptr, len, cap }`.
    assert_eq!(decl.max_payload_size, 24);
}

#[test]
fn struct_that_holds_its_own_union_gets_a_boxed_field() {
    // `Node` holds `Int | Node` inline, so the struct would hold
    // itself. The cycle breaker boxes the field, and the union is
    // sized after that.
    let source = "
        struct Node
          next: Link
        end

        type Link = Int | Node

        fn depth(link: Link) -> Int
          match link
            _n: Int -> 0
            node: Node -> 1 + depth(node.next)
          end
        end

        depth(Node{next: Node{next: 3}})
        ";
    let script = lower(source);

    let node = script
        .struct_decl(&format!("{PACKAGE}.Node"))
        .expect("`Node` struct decl");
    let IRType::Indirect(inner) = &node.fields[0].ir_type else {
        panic!(
            "expected `Node.next` to be boxed, got {:?}",
            node.fields[0].ir_type
        );
    };
    let IRType::Union(symbol) = inner.as_ref() else {
        panic!("expected a union behind the box, got {inner:?}");
    };
    assert_eq!(
        symbol.mangled(),
        format!("Union_Int64_or_{PACKAGE}.Node"),
        "expected the anonymous union, since `Link` is not recursive",
    );
}
