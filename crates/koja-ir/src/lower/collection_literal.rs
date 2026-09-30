//! Lower `ExprKind::List` and `ExprKind::Map` into a `new()` plus
//! insert IR call chain.
//!
//! Typecheck stamps `expr.resolution` (`List<T>` or `Map<K, V>`) on the
//! literal but leaves the literal node on the sealed AST. We synthesize
//! the equivalent `MethodCall` tree here so the emitted IR is identical
//! to a hand-written `List.new().append(a).append(b)` or
//! `Map.new().put(k1, v1).put(k2, v2)`. The two literal kinds differ
//! only in the type name, the insert method, and how many arguments
//! each item contributes, so [`synthesize_collection_chain`] takes
//! those three and the adapters below supply them.

use koja_ast::ast::{Arg, Expr, ExprKind, Name};
use koja_ast::identifier::{Identifier, Resolution, ResolvedType};
use koja_ast::span::Span;
use koja_typecheck::GlobalRegistry;

use super::calls::{lower_method_call, synthesized_method_target};
use super::ctx::FnLowerCtx;
use crate::function::IRBlockId;
use crate::types::ValueId;

/// Build one positional [`Arg`] from a literal element.
fn arg_of(value: &Expr) -> Arg {
    Arg {
        name: None,
        span: value.span,
        value: value.clone(),
    }
}

/// Lower the synthesized `MethodCall` chain like any other method call.
fn lower_chain(
    chain: &Expr,
    ctx: &mut FnLowerCtx<'_>,
    block: IRBlockId,
) -> Result<(ValueId, IRBlockId), ()> {
    let ExprKind::MethodCall {
        receiver,
        method,
        args,
        target,
        type_args,
    } = &chain.kind
    else {
        unreachable!("synthesized collection-literal chain always produces MethodCall");
    };
    lower_method_call(receiver, method, args, type_args, *target, ctx, block)
}

pub(super) fn lower_list_literal(
    elements: &[Expr],
    expr_resolution: &ResolvedType,
    span: Span,
    ctx: &mut FnLowerCtx<'_>,
    block: IRBlockId,
) -> Result<(ValueId, IRBlockId), ()> {
    let items = elements.iter().map(|element| vec![arg_of(element)]);
    let chain =
        synthesize_collection_chain("List", "append", items, expr_resolution, span, ctx.registry);
    lower_chain(&chain, ctx, block)
}

pub(super) fn lower_map_literal(
    entries: &[(Expr, Expr)],
    expr_resolution: &ResolvedType,
    span: Span,
    ctx: &mut FnLowerCtx<'_>,
    block: IRBlockId,
) -> Result<(ValueId, IRBlockId), ()> {
    let items = entries
        .iter()
        .map(|(key, value)| vec![arg_of(key), arg_of(value)]);
    let chain =
        synthesize_collection_chain("Map", "put", items, expr_resolution, span, ctx.registry);
    lower_chain(&chain, ctx, block)
}

fn stamped_expr(kind: ExprKind, resolution: ResolvedType, span: Span) -> Expr {
    let mut expr = Expr::new(kind, span);
    expr.resolution = resolution;
    expr
}

/// Synthesize `Type.new().method(..).method(..)` with every node
/// stamped `resolution`. Each item holds the arguments of one insert
/// call: one element for a list, a key and a value for a map. The
/// insert arity counts the receiver, so it is the item length plus one.
fn synthesize_collection_chain(
    type_name: &str,
    method: &str,
    items: impl Iterator<Item = Vec<Arg>>,
    resolution: &ResolvedType,
    span: Span,
    registry: &GlobalRegistry,
) -> Expr {
    let type_id = registry
        .lookup(&Identifier::single("Global", type_name))
        .map(|(id, _)| id)
        .unwrap_or_else(|| {
            panic!(
                "IR lower: `{type_name}` literal reaches lower without `Global.{type_name}` \
                 in registry (seal violation)",
            )
        });
    let new_receiver = stamped_expr(
        ExprKind::Ident {
            name: type_name.to_string(),
            resolution: Resolution::Global(type_id),
        },
        resolution.clone(),
        span,
    );
    let new_call = stamped_expr(
        ExprKind::MethodCall {
            receiver: Box::new(new_receiver),
            method: Name::new("new", span),
            args: Vec::new(),
            target: synthesized_method_target(registry, type_id, "new", 0),
            type_args: Vec::new(),
        },
        resolution.clone(),
        span,
    );
    items.fold(new_call, |receiver, args| {
        let target = synthesized_method_target(registry, type_id, method, args.len() + 1);
        stamped_expr(
            ExprKind::MethodCall {
                receiver: Box::new(receiver),
                method: Name::new(method, span),
                args,
                target,
                type_args: Vec::new(),
            },
            resolution.clone(),
            span,
        )
    })
}
