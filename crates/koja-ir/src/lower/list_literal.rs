//! Lower `ExprKind::List` into a `List.new().append(...)` IR call chain.
//!
//! Typecheck stamps `expr.resolution = List<T>` on the literal but leaves
//! `ExprKind::List` on the sealed AST. We synthesize the equivalent
//! `MethodCall` tree here so the emitted IR is byte-for-byte identical
//! to a hand-written `List.new().append(a).append(b)`.

use koja_ast::ast::{Arg, Expr, ExprKind};
use koja_ast::identifier::{Identifier, Resolution, ResolvedType};
use koja_ast::span::Span;
use koja_typecheck::GlobalRegistry;

use super::calls::{MethodCallShape, lower_method_call};
use super::ctx::{FnLowerCtx, LowerOutput};
use crate::function::IRBlockId;
use crate::types::ValueId;

pub(super) fn lower_list_literal(
    elements: &[Expr],
    expr_resolution: &ResolvedType,
    span: Span,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    let list_id = registry
        .lookup(&Identifier::new("Global", vec!["List".to_string()]))
        .map(|(id, _)| id)
        .unwrap_or_else(|| {
            panic!(
                "IR lower: list literal reaches lower without `Global.List` in registry — \
                 seal violation",
            )
        });
    let new_receiver = stamped_expr(
        ExprKind::Ident {
            name: "List".to_string(),
            resolution: Resolution::Global(list_id),
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
    let chain = elements.iter().fold(new_call, |receiver, element| {
        let arg = Arg {
            name: None,
            span: element.span,
            value: element.clone(),
        };
        stamped_expr(
            ExprKind::MethodCall {
                receiver: Box::new(receiver),
                method: "append".to_string(),
                args: vec![arg],
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
        unreachable!("synthesized list-literal chain always produces MethodCall");
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
