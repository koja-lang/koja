//! Function signatures laid out by `koja-fmt`, for hover.
//!
//! The registry holds resolved types, not source syntax, so this
//! module builds a [`Function`] header from the entry and hands it to
//! [`koja_fmt::format_signature`]. The hover then shows the signature
//! the way `koja format` would write it. Every span on the built node
//! is synthetic and only exists to satisfy the AST shape.

use koja_ast::ast::{Function, FunctionOrigin, Name, Param, TypeExpr, TypeParam, Visibility};
use koja_ast::identifier::{
    AnonymousKind, GlobalRegistryId, Identifier, Resolution, ResolvedType,
};
use koja_ast::span::Span;
use koja_typecheck::{FunctionSignature, GlobalKind, GlobalRegistry, RegistryEntry, VisibilityScope};

use crate::Analysis;

/// Line width the hover signature wraps at. Matches the formatter's
/// default so the hover and the file agree.
pub const WIDTH: u32 = 80;

/// The formatted header for the function `id`, wrapped at [`WIDTH`].
/// `None` when `id` is not a function or its signature is not lifted.
pub fn function_signature(analysis: &Analysis<'_>, id: GlobalRegistryId) -> Option<String> {
    let registry = analysis.registry;
    let entry = registry.get(id)?;
    let GlobalKind::Function(definition) = &entry.kind else {
        return None;
    };
    let signature = definition.signature.as_ref()?;
    let function = build_function(registry, id, entry, signature);
    let display_name = entry.identifier.path().join(".");
    Some(koja_fmt::format_signature(
        &function,
        &display_name,
        WIDTH,
    ))
}

fn build_function(
    registry: &GlobalRegistry,
    id: GlobalRegistryId,
    entry: &RegistryEntry,
    signature: &FunctionSignature,
) -> Function {
    let span = Span::zero().as_synthetic();
    let bounds = registry.type_param_bounds(id).unwrap_or(&[]);
    let type_params = entry
        .type_params
        .iter()
        .enumerate()
        .map(|(index, name)| TypeParam {
            name: name_of(name),
            bounds: bounds
                .get(index)
                .map(|bounds| {
                    bounds
                        .iter()
                        .map(|bound| {
                            named(registry, bound.protocol_id, &bound.args)
                        })
                        .collect()
                })
                .unwrap_or_default(),
            span,
        })
        .collect();

    let params = signature
        .params
        .iter()
        .map(|param| {
            if param.name == "self" {
                Param::Self_ {
                    local_id: None,
                    span,
                }
            } else {
                Param::Regular {
                    name: name_of(&param.name),
                    type_expr: type_expr_of(&param.ty, registry),
                    default: None,
                    local_id: None,
                    span,
                }
            }
        })
        .collect();

    let (return_type, error_type) = return_tail(signature, registry);

    Function {
        annotations: Vec::new(),
        origin: FunctionOrigin::Explicit,
        visibility: match entry.visibility {
            VisibilityScope::Public => Visibility::Public,
            VisibilityScope::PackagePrivate | VisibilityScope::TypePrivate(_) => {
                Visibility::Private
            }
        },
        name: name_of(entry.identifier.last()),
        type_params,
        params,
        return_type,
        error_type,
        body: None,
        span,
    }
}

/// Split a `-> T ! E` return back into its two clauses. Lift stores
/// it as `Result<T, E>` with `declared_fallible` set, so the hover
/// spells it the way the author did. A unit return is dropped, which
/// is how the formatter prints `fn f()`.
fn return_tail(
    signature: &FunctionSignature,
    registry: &GlobalRegistry,
) -> (Option<TypeExpr>, Option<TypeExpr>) {
    if signature.declared_fallible
        && let ResolvedType::Named {
            resolution: Resolution::Global(id),
            type_args,
        } = &signature.return_type
        && let [ok, err] = type_args.as_slice()
        && is_global(registry, *id, "Result")
    {
        return (
            Some(type_expr_of(ok, registry)),
            Some(type_expr_of(err, registry)),
        );
    }
    (Some(type_expr_of(&signature.return_type, registry)), None)
}

/// Source syntax for a resolved type. Names come from the registry
/// entry's short name, so an alias shows its own name and a type from
/// another package shows without its qualifier. Anything unresolved
/// prints as `_`.
pub fn type_expr_of(ty: &ResolvedType, registry: &GlobalRegistry) -> TypeExpr {
    let span = Span::zero().as_synthetic();
    match ty {
        ResolvedType::Anonymous(AnonymousKind::Function { params, ret }) => TypeExpr::Function {
            params: params.iter().map(|p| type_expr_of(p, registry)).collect(),
            return_type: Box::new(type_expr_of(ret, registry)),
            span,
        },
        ResolvedType::Anonymous(AnonymousKind::Tuple { elements }) => TypeExpr::Tuple {
            elements: elements.iter().map(|e| type_expr_of(e, registry)).collect(),
            span,
        },
        ResolvedType::Named {
            resolution: Resolution::Global(id),
            type_args,
        } => {
            if type_args.is_empty() && is_global(registry, *id, "Unit") {
                return TypeExpr::Unit { span };
            }
            named(registry, *id, type_args)
        }
        ResolvedType::Named {
            resolution: Resolution::TypeParam { owner, index },
            ..
        } => TypeExpr::named(
            vec![name_of(
                registry.type_param_name(*owner, *index).unwrap_or("_"),
            )],
            span,
        ),
        ResolvedType::Named {
            resolution: Resolution::Local(_) | Resolution::Unresolved,
            ..
        }
        | ResolvedType::Unresolved => TypeExpr::named(vec![name_of("_")], span),
        ResolvedType::Union(members) => TypeExpr::Union {
            types: members.iter().map(|m| type_expr_of(m, registry)).collect(),
            span,
        },
    }
}

/// `Name<Args>` for the registry entry `id`, or `_` when the id is
/// not in the registry.
fn named(registry: &GlobalRegistry, id: GlobalRegistryId, args: &[ResolvedType]) -> TypeExpr {
    let span = Span::zero().as_synthetic();
    let head = registry
        .get(id)
        .map(|entry| entry.identifier.last())
        .unwrap_or("_");
    let path = vec![name_of(head)];
    if args.is_empty() {
        TypeExpr::named(path, span)
    } else {
        TypeExpr::generic(
            path,
            args.iter().map(|a| type_expr_of(a, registry)).collect(),
            span,
        )
    }
}

fn is_global(registry: &GlobalRegistry, id: GlobalRegistryId, name: &str) -> bool {
    registry
        .get(id)
        .is_some_and(|entry| entry.identifier == Identifier::single("Global", name))
}

fn name_of(text: &str) -> Name {
    Name {
        text: text.to_string(),
        span: Span::zero().as_synthetic(),
    }
}
