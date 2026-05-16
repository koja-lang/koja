//! Phase 2 typecheck coverage for *generic protocols* — the
//! `protocol Eq<T>` shape: declaring user type-params on a protocol,
//! threading them through method signatures, recording per-impl
//! type-args on the target's `conformances` map, and verifying
//! impl heads match the protocol's arity.
//!
//! Single-bound enforcement and bounded dispatch live in `bounds.rs` /
//! `bounded_dispatch.rs`; struct-side concrete-impl shape coverage
//! lives in `structs.rs`. This file pins the protocol-side surface.

use expo_ast::identifier::{Identifier, Resolution, ResolvedType};
use expo_ast::util::dedent;
use expo_typecheck::{CheckedProgram, GlobalKind};

mod common;

use common::{
    PACKAGE, diagnostic_messages, typecheck_file as typecheck,
    typecheck_file_fail as typecheck_fail,
};

fn protocol_id(checked: &CheckedProgram, name: &str) -> expo_ast::identifier::GlobalRegistryId {
    checked
        .registry
        .lookup(&Identifier::new(PACKAGE, vec![name.to_string()]))
        .map(|(id, _)| id)
        .unwrap_or_else(|| panic!("`{name}` not registered"))
}

#[test]
fn single_param_generic_protocol_lifts_with_self_then_user_params() {
    // The registry's `type_params` for a protocol is `[Self, T, ...]` —
    // Self at slot 0 (synthetic), then user-declared params in
    // declaration order. `lift_protocol` resolves the method's
    // signature under a `TypeParamScope` rooted at the protocol id
    // so `Self` and `T` both resolve to `Resolution::TypeParam`
    // anchored on the protocol entry.
    let source = "
        protocol Match<T>
          fn matches(self, other: T) -> Bool
        end
        ";

    let checked = typecheck(&dedent(source));
    let id = protocol_id(&checked, "Match");
    let entry = checked.registry.get(id).expect("Match entry");
    assert_eq!(entry.type_params, vec!["Self".to_string(), "T".to_string()]);

    let GlobalKind::Protocol(Some(definition)) = &entry.kind else {
        panic!("expected lifted protocol, got {:?}", entry.kind);
    };
    let matches = definition
        .methods
        .iter()
        .find(|m| m.name == "matches")
        .expect("matches method lifted");
    let other_ty = &matches.non_self_params[0].ty;
    let ResolvedType::Named {
        resolution:
            Resolution::TypeParam {
                owner: param_owner,
                index,
            },
        ..
    } = other_ty
    else {
        panic!("expected `other: TypeParam`, got {:?}", other_ty);
    };
    let param_owner = *param_owner;
    let index = *index;
    assert_eq!(param_owner, id);
    assert_eq!(index.as_u32(), 1, "T is at slot 1 (Self occupies slot 0)");
}

#[test]
fn multi_param_generic_protocol_assigns_distinct_indices_in_order() {
    let source = "
        protocol Process<C, M, R>
          fn run(self, ctx: C, msg: M) -> R
        end
        ";

    let checked = typecheck(&dedent(source));
    let id = protocol_id(&checked, "Process");
    let entry = checked.registry.get(id).expect("Process entry");
    assert_eq!(
        entry.type_params,
        vec![
            "Self".to_string(),
            "C".to_string(),
            "M".to_string(),
            "R".to_string(),
        ],
    );
}

#[test]
fn protocol_self_param_name_is_reserved() {
    // `Self` is a synthetic slot-0 type param; the user may not
    // re-declare it.
    let source = "
        protocol Bad<Self>
          fn run(self) -> Int
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages.iter().any(|m| m.contains("`Self` is reserved")),
        "expected reserved-Self diagnostic, got {messages:?}",
    );
}

#[test]
fn impl_records_protocol_args_on_target_conformances() {
    // `impl Match<String> for User` — the conformance recorded on
    // `User`'s `StructDefinition` carries `[String]` as the protocol
    // type-args. `verify_bounds` consumes this directly via
    // `lookup_conformance`.
    let source = "
        protocol Match<T>
          fn matches(self, other: T) -> Bool
        end

        struct User
          id: Int
        end

        impl Match<String> for User
          fn matches(self, other: String) -> Bool
            true
          end
        end
        ";

    let checked = typecheck(&dedent(source));
    let user_id = protocol_id(&checked, "User");
    let match_id = protocol_id(&checked, "Match");
    let args = checked
        .registry
        .lookup_conformance(user_id, match_id)
        .expect("User conforms to Match");
    assert_eq!(args.len(), 1);
    let ResolvedType::Named {
        resolution: Resolution::Global(string_id),
        ..
    } = args[0]
    else {
        panic!("expected protocol-arg `String`, got {:?}", args[0]);
    };
    let (expected_string_id, _) = checked
        .registry
        .lookup(&Identifier::new("Global", vec!["String".to_string()]))
        .expect("String registered");
    assert_eq!(string_id, expected_string_id);
}

#[test]
fn impl_with_wrong_protocol_arity_diagnoses() {
    let source = "
        protocol Match<T>
          fn matches(self, other: T) -> Bool
        end

        struct User
          id: Int
        end

        impl Match for User
          fn matches(self, other: String) -> Bool
            true
          end
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("Match") && m.contains("type argument")),
        "expected protocol-arity diagnostic, got {messages:?}",
    );
}

#[test]
fn duplicate_impl_for_same_protocol_diagnoses() {
    // Two `impl Match<String> for User` blocks both record `User`
    // as conforming to `Match` — `record_conformance` rejects the
    // second.
    let source = "
        protocol Match<T>
          fn matches(self, other: T) -> Bool
        end

        struct User
          id: Int
        end

        impl Match<String> for User
          fn matches(self, other: String) -> Bool
            true
          end
        end

        impl Match<String> for User
          fn matches(self, other: String) -> Bool
            false
          end
        end
        ";

    let failure = typecheck_fail(&dedent(source));
    let messages = diagnostic_messages(&failure);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("duplicate") || m.contains("already defined")),
        "expected duplicate-impl diagnostic, got {messages:?}",
    );
}

#[test]
fn generic_target_impls_generic_protocol_with_matching_param() {
    // `impl Match<T> for User<T>` — the impl's free `T` aliases
    // `User`'s slot-0 type param (Cleanup #1 anchoring), so the
    // protocol method's `other: T` resolves under that anchor and
    // every `Bag{...}.matches(...)` call substitutes consistently.
    let source = "
        protocol Match<T>
          fn matches(self, other: T) -> Bool
        end

        struct Wrap<T>
          value: T
        end

        impl Match<T> for Wrap<T>
          fn matches(self, other: T) -> Bool
            true
          end
        end

        fn use_wrap() -> Bool
          Wrap{value: 1}.matches(2)
        end
        ";

    typecheck(&dedent(source));
}
