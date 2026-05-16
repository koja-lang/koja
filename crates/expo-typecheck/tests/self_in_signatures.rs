//! Phase 2 typecheck coverage for `Self` in protocol method
//! signatures and impl-block contexts.
//!
//! `Self` is a synthetic slot-0 type-param on every protocol — the
//! protocol's lifted method signatures resolve `Self` to
//! `Resolution::TypeParam { owner: protocol_id, index: 0 }`. Inside
//! struct/enum/impl contexts the same `Self` keyword resolves to
//! the enclosing concrete type instead. This file pins both
//! resolutions and the call-site substitutions that flow `Self`
//! through to a concrete impl.

use expo_ast::identifier::{Identifier, Resolution, ResolvedType};
use expo_ast::util::dedent;
use expo_typecheck::GlobalKind;

mod common;

use common::{PACKAGE, typecheck_file as typecheck};

fn lookup_id(
    checked: &expo_typecheck::CheckedProgram,
    name: &str,
) -> expo_ast::identifier::GlobalRegistryId {
    checked
        .registry
        .lookup(&Identifier::new(PACKAGE, vec![name.to_string()]))
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("`{name}` not registered"))
}

#[test]
fn self_in_protocol_return_resolves_to_protocol_slot_zero_typeparam() {
    // `protocol Builder { fn make() -> Self end }` — the lifted
    // method's `return_type` carries `TypeParam { owner: Builder,
    // index: 0 }`. Slot 0 is reserved for the synthetic `Self`.
    let source = "
        protocol Builder
          fn make() -> Self
        end
        ";

    let checked = typecheck(&dedent(source));
    let builder_id = lookup_id(&checked, "Builder");
    let entry = checked.registry.get(builder_id).expect("Builder entry");
    let GlobalKind::Protocol(Some(definition)) = &entry.kind else {
        panic!("expected lifted protocol, got {:?}", entry.kind);
    };
    let make = definition
        .methods
        .iter()
        .find(|m| m.name == "make")
        .expect("make method lifted");
    let ResolvedType::Named {
        resolution: Resolution::TypeParam { owner, index },
        ..
    } = make.return_type
    else {
        panic!(
            "expected `Self` to resolve to TypeParam, got {:?}",
            make.return_type
        );
    };
    assert_eq!(owner, builder_id);
    assert_eq!(index.as_u32(), 0);
}

#[test]
fn self_in_protocol_param_resolves_to_protocol_slot_zero_typeparam() {
    // `protocol Eq { fn equals(self, other: Self) -> Bool end }`.
    // `other: Self` lifts as `TypeParam { owner: Eq, index: 0 }`,
    // letting `verify_and_synthesize_trait_impl` substitute the
    // impl's target type at the call site.
    let source = "
        protocol Equal
          fn equals(self, other: Self) -> Bool
        end
        ";

    let checked = typecheck(&dedent(source));
    let equal_id = lookup_id(&checked, "Equal");
    let entry = checked.registry.get(equal_id).expect("Equal entry");
    let GlobalKind::Protocol(Some(definition)) = &entry.kind else {
        panic!("expected lifted protocol, got {:?}", entry.kind);
    };
    let equals = definition
        .methods
        .iter()
        .find(|m| m.name == "equals")
        .expect("equals method lifted");
    assert_eq!(equals.non_self_params.len(), 1);
    let other_ty = &equals.non_self_params[0].ty;
    let ResolvedType::Named {
        resolution: Resolution::TypeParam { owner, index },
        ..
    } = other_ty
    else {
        panic!("expected `other: Self` -> TypeParam, got {:?}", other_ty);
    };
    assert_eq!(*owner, equal_id);
    assert_eq!(index.as_u32(), 0);
}

#[test]
fn self_in_inherent_method_return_resolves_to_enclosing_struct() {
    // `Self` in an inherent method ('impl Point { fn origin -> Self end }')
    // resolves to the struct id, not a TypeParam.
    let source = "
        struct Point
          x: Int
        end

        impl Point
          fn origin -> Self
            Point{x: 0}
          end
        end
        ";

    let checked = typecheck(&dedent(source));
    let (point_id, _) = checked
        .registry
        .lookup(&Identifier::new(PACKAGE, vec!["Point".to_string()]))
        .expect("Point registered");
    let (_, origin_entry) = checked
        .registry
        .lookup(&Identifier::new(
            PACKAGE,
            vec!["Point".to_string(), "origin".to_string()],
        ))
        .expect("Point.origin registered");
    let GlobalKind::Function(Some(signature)) = &origin_entry.kind else {
        panic!("Point.origin should be a lifted function");
    };
    let ResolvedType::Named {
        resolution: Resolution::Global(resolved_id),
        ..
    } = signature.return_type
    else {
        panic!(
            "expected `Self` to resolve to Global(Point), got {:?}",
            signature.return_type
        );
    };
    assert_eq!(resolved_id, point_id);
}

#[test]
fn self_in_trait_impl_method_resolves_to_concrete_target() {
    // For `impl Equal for User { fn equals(self, other: Self) ... end }`,
    // the impl-method's `other: Self` lifts as `User` (not the
    // protocol's TypeParam) — `SelfContext::Receiver` carries the
    // concrete target as `self_override` so the impl-side method's
    // `Self` resolves to its concrete type domain.
    let source = "
        protocol Equal
          fn equals(self, other: Self) -> Bool
        end

        struct User
          id: Int
        end

        impl Equal for User
          fn equals(self, other: Self) -> Bool
            true
          end
        end
        ";

    let checked = typecheck(&dedent(source));
    let (user_id, _) = checked
        .registry
        .lookup(&Identifier::new(PACKAGE, vec!["User".to_string()]))
        .expect("User registered");
    let (_, entry) = checked
        .registry
        .lookup(&Identifier::new(
            PACKAGE,
            vec!["User".to_string(), "equals".to_string()],
        ))
        .expect("User.equals registered");
    let GlobalKind::Function(Some(signature)) = &entry.kind else {
        panic!("User.equals should be a lifted function");
    };
    let user_named = ResolvedType::leaf(Resolution::Global(user_id));
    // self
    assert_eq!(
        signature.params[0].ty, user_named,
        "trait-impl method's self type should resolve to User",
    );
    // other: Self
    assert_eq!(
        signature.params[1].ty, user_named,
        "trait-impl method's `other: Self` should resolve to User \
         (Self in the impl context, not the protocol's TypeParam)",
    );
}

#[test]
fn self_in_protocol_method_substitutes_through_call_site_to_concrete() {
    // End-to-end substitution: declaring `protocol Equal { fn equals(self,
    // other: Self) -> Bool end }`, implementing it for `User`, and calling
    // `u.equals(other)` must type both arguments as `User` post-resolve.
    // No `TypeParam(Equal, 0)` is allowed to leak into the call site.
    let source = "
        protocol Equal
          fn equals(self, other: Self) -> Bool
        end

        struct User
          id: Int
        end

        impl Equal for User
          fn equals(self, other: Self) -> Bool
            true
          end
        end

        fn check() -> Bool
          User{id: 1}.equals(User{id: 2})
        end
        ";

    typecheck(&dedent(source));
}

#[test]
fn self_in_generic_struct_method_carries_struct_type_args() {
    // For `struct Bag<T> { fn snapshot(self) -> Self end }`, `Self`
    // is `Bag<TypeParam(Bag, 0)>` — same shape as the receiver's
    // self type. Mono substitutes the type-arg through to a
    // concrete `Bag<Int>` etc.
    let source = "
        struct Bag<T>
          item: T

          fn snapshot(self) -> Self
            self
          end
        end
        ";

    let checked = typecheck(&dedent(source));
    let (bag_id, _) = checked
        .registry
        .lookup(&Identifier::new(PACKAGE, vec!["Bag".to_string()]))
        .expect("Bag registered");
    let (_, snapshot_entry) = checked
        .registry
        .lookup(&Identifier::new(
            PACKAGE,
            vec!["Bag".to_string(), "snapshot".to_string()],
        ))
        .expect("Bag.snapshot registered");
    let GlobalKind::Function(Some(signature)) = &snapshot_entry.kind else {
        panic!("Bag.snapshot should be a lifted function");
    };
    let return_ty = &signature.return_type;
    let ResolvedType::Named {
        resolution: Resolution::Global(head_id),
        type_args,
    } = return_ty
    else {
        panic!("expected `Self` to resolve to Named(Global(Bag)), got {return_ty:?}");
    };
    assert_eq!(*head_id, bag_id);
    assert_eq!(
        type_args.len(),
        1,
        "`Self` in `Bag<T>.snapshot` must carry one type-arg",
    );
    let ResolvedType::Named {
        resolution: Resolution::TypeParam { owner, index },
        ..
    } = &type_args[0]
    else {
        panic!(
            "expected the type-arg to be a TypeParam(Bag, 0), got {:?}",
            type_args[0]
        );
    };
    assert_eq!(*owner, bag_id);
    assert_eq!(index.as_u32(), 0);
}
