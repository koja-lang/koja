//! Lower `ExprKind::Map` into a `Map.new().put(...)` IR call chain.
//!
//! Typecheck stamps `expr.resolution = Map<K, V>` on the literal but
//! leaves `ExprKind::Map` on the sealed AST. We synthesize the
//! equivalent `MethodCall` tree here so the emitted IR is identical to
//! a hand-written `Map.new().put(k1, v1).put(k2, v2)`.

use expo_ast::ast::{Arg, Expr, ExprKind};
use expo_ast::identifier::{Identifier, Resolution, ResolvedType};
use expo_ast::span::Span;
use expo_typecheck::GlobalRegistry;

use super::calls::{MethodCallShape, lower_method_call};
use super::ctx::{FnLowerCtx, LowerOutput};
use crate::function::IRBlockId;
use crate::types::ValueId;

pub(super) fn lower_map_literal(
    entries: &[(Expr, Expr)],
    expr_resolution: &ResolvedType,
    span: Span,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    let map_id = registry
        .lookup(&Identifier::new("Global", vec!["Map".to_string()]))
        .map(|(id, _)| id)
        .unwrap_or_else(|| {
            panic!(
                "IR lower: map literal reaches lower without `Global.Map` in registry — \
                 seal violation",
            )
        });
    let new_receiver = stamped_expr(
        ExprKind::Ident {
            name: "Map".to_string(),
            resolution: Resolution::Global(map_id),
        },
        expr_resolution.clone(),
        span,
    );
    let new_call = stamped_expr(
        ExprKind::MethodCall {
            receiver: Box::new(new_receiver),
            method: "new".to_string(),
            args: Vec::new(),
            type_args: Vec::new(),
        },
        expr_resolution.clone(),
        span,
    );
    let chain = entries.iter().fold(new_call, |receiver, (key, value)| {
        let key_arg = Arg {
            name: None,
            span: key.span,
            value: key.clone(),
        };
        let value_arg = Arg {
            name: None,
            span: value.span,
            value: value.clone(),
        };
        stamped_expr(
            ExprKind::MethodCall {
                receiver: Box::new(receiver),
                method: "put".to_string(),
                args: vec![key_arg, value_arg],
                type_args: Vec::new(),
            },
            expr_resolution.clone(),
            span,
        )
    });
    let ExprKind::MethodCall {
        receiver,
        method,
        args,
        type_args,
    } = &chain.kind
    else {
        unreachable!("synthesized map-literal chain always produces MethodCall");
    };
    lower_method_call(
        receiver,
        MethodCallShape {
            method,
            args,
            method_type_args: type_args,
        },
        ctx,
        block,
        registry,
        output,
    )
}

fn stamped_expr(kind: ExprKind, resolution: ResolvedType, span: Span) -> Expr {
    let mut expr = Expr::new(kind, span);
    expr.resolution = resolution;
    expr
}
