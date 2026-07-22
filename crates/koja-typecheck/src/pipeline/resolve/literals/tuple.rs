//! Tuple-literal resolution. Tuples are structural, so there is no
//! carrier protocol. The literal resolves element-wise and stamps
//! `Anonymous(Tuple)` with the element types.

use koja_ast::ast::{Diagnostic, Expr};
use koja_ast::identifier::{AnonymousKind, ResolvedType};

use super::super::ctx::Resolver;
use super::super::expr::resolve_expr_with_expected;
use super::super::types::peel_alias;

/// Resolve a `(a, b)` literal. An expected tuple type of matching
/// arity propagates into the elements so contextual shapes like
/// generic enum unit variants infer inside tuple literals.
pub(in super::super) fn resolve_tuple_literal(
    elements: &mut [Expr],
    expected: Option<&ResolvedType>,
    resolver: &mut Resolver<'_>,
    diagnostics: &mut Vec<Diagnostic>,
) -> ResolvedType {
    let expected_elements = match expected.map(|hint| peel_alias(hint, resolver.registry)) {
        Some(ResolvedType::Anonymous(AnonymousKind::Tuple {
            elements: element_types,
        })) if element_types.len() == elements.len() => Some(element_types),
        _ => None,
    };
    let mut element_types = Vec::with_capacity(elements.len());
    for (index, element) in elements.iter_mut().enumerate() {
        let hint = expected_elements.as_ref().map(|types| &types[index]);
        resolve_expr_with_expected(element, hint, resolver, diagnostics);
        element_types.push(element.resolution.clone());
    }
    if elements.len() < 2 {
        // Only reachable after a parser arity error, which already
        // fails the compile. Leave the hole unresolved.
        return ResolvedType::unresolved();
    }
    ResolvedType::Anonymous(AnonymousKind::Tuple {
        elements: element_types,
    })
}
