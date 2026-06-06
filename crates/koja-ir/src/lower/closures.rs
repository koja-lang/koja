//! Closure-expression lowering. Three responsibilities:
//!
//! 1. **Capture analysis** — walk the closure body, collect the
//!    outer-scope [`LocalId`]s referenced (deduped, in encounter
//!    order). Nested closures contribute their own captures up to
//!    this closure's scope.
//! 2. **Body synthesis** — mint a `<enclosing>__closure<N>` symbol
//!    and a [`FunctionKind::Closure`] [`IRFunction`] whose body
//!    lowers under a fresh ctx whose [`ClosureState::set_captures`]
//!    redirects outer-local idents through env-loads.
//! 3. **Fn-as-value adapter** — wrap a named function in a
//!    captureless [`FunctionKind::Closure`] so it can flow through
//!    closure-typed slots ([`synthesize_fn_as_closure_wrapper`]).
//!
//! [`MakeClosure`] is then emitted in the outer block, reading each
//! capture through the outer ctx's normal local-or-capture path.
//! Heap captures move into the env (the outer slot is marked Moved
//! so fn-exit drops skip it).

use std::collections::{BTreeMap, BTreeSet, HashSet};

use koja_ast::ast::{
    AssignTarget, BinarySegment, ClosureParam, EnumConstructionData, Expr, ExprKind, MatchArm,
    Pattern, Statement, StringPart,
};
use koja_ast::identifier::{AnonymousKind, FnParam, LocalId, Resolution, ResolvedType};
use koja_typecheck::{FunctionSignature, GlobalRegistry};

use crate::function::{
    FunctionKind, IRBlockId, IRFunction, IRFunctionParam, IRInstruction, IRSymbol, IRTerminator,
};
use crate::local::IRLocalId;
use crate::types::{IRType, ValueId};

use super::body::{finalize_open_flow, lower_body};
use super::ctx::{FnLowerCtx, LowerOutput};
use super::drops::emit_function_exit_drops;
use super::package::resolved_type_to_ir_type;

/// Lower a `fn (x: T) -> U ... end` closure expression.
pub(super) fn lower_block_closure(
    params: &[ClosureParam],
    body: &[Statement],
    closure_resolution: &ResolvedType,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    let fn_params = expect_function_params(closure_resolution);
    let fn_ret = expect_function_return(closure_resolution);
    let own_set = own_set_for(params, body);
    let captures = collect_captures(BodyShape::Block(body), &own_set);
    let captures_with_types = resolve_capture_types(&captures, registry, output);

    let symbol = ctx.closures_mut().mint_symbol();
    let synthesized = synthesize_body(
        &symbol,
        ClosureSig {
            body: BodyShape::Block(body),
            closure_params: params,
            fn_params,
            fn_ret,
        },
        &captures_with_types,
        registry,
        output,
    )?;
    output.synthesized_functions.push(synthesized);

    emit_make_closure(
        symbol,
        &captures_with_types,
        closure_resolution,
        ctx,
        block,
        registry,
        output,
    )
}

/// Lower a `x -> body_expr` short closure expression.
pub(super) fn lower_short_closure(
    params: &[ClosureParam],
    body: &Expr,
    closure_resolution: &ResolvedType,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    let fn_params = expect_function_params(closure_resolution);
    let fn_ret = expect_function_return(closure_resolution);
    let own_set = own_set_for(params, &[]);
    let captures = collect_captures(BodyShape::Short(body), &own_set);
    let captures_with_types = resolve_capture_types(&captures, registry, output);

    let symbol = ctx.closures_mut().mint_symbol();
    let synthesized = synthesize_body(
        &symbol,
        ClosureSig {
            body: BodyShape::Short(body),
            closure_params: params,
            fn_params,
            fn_ret,
        },
        &captures_with_types,
        registry,
        output,
    )?;
    output.synthesized_functions.push(synthesized);

    emit_make_closure(
        symbol,
        &captures_with_types,
        closure_resolution,
        ctx,
        block,
        registry,
        output,
    )
}

#[derive(Clone, Copy)]
enum BodyShape<'a> {
    Block(&'a [Statement]),
    Short(&'a Expr),
}

/// One capture, keyed by the outer [`LocalId`] it pulls from.
struct CaptureInfo {
    local_id: LocalId,
    ir_type: IRType,
}

fn expect_function_params(resolution: &ResolvedType) -> &[FnParam] {
    match resolution {
        ResolvedType::Anonymous(AnonymousKind::Function { params, .. }) => params,
        other => panic!(
            "IR lower: closure resolution must be Anonymous(Function), got {other:?} — \
             typecheck seal violation",
        ),
    }
}

fn expect_function_return(resolution: &ResolvedType) -> &ResolvedType {
    match resolution {
        ResolvedType::Anonymous(AnonymousKind::Function { ret, .. }) => ret,
        other => panic!(
            "IR lower: closure resolution must be Anonymous(Function), got {other:?} — \
             typecheck seal violation",
        ),
    }
}

/// Build the closure's "own" [`LocalId`] set: stamped param ids
/// plus any single-segment assignment targets in the body.
fn own_set_for(params: &[ClosureParam], body: &[Statement]) -> HashSet<LocalId> {
    let mut set = HashSet::new();
    for param in params {
        if let ClosureParam::Name {
            local_id: Some(id), ..
        } = param
        {
            set.insert(*id);
        }
    }
    for stmt in body {
        if let Statement::Assignment {
            target: AssignTarget::LValue(lvalue),
            ..
        } = stmt
            && lvalue.segments.len() == 1
            && let Some(id) = lvalue.local_id
        {
            set.insert(id);
        }
    }
    set
}

/// Walk `body` collecting every [`LocalId`] referenced through
/// `Resolution::Local` (or `Self_::local_id`) that isn't in
/// `own_set`. Returns dedup'd ids in encounter order. Nested
/// closures contribute via a per-scope frame; their own params /
/// body-locals shadow ours during their subwalk.
fn collect_captures(
    body: BodyShape<'_>,
    own_set: &HashSet<LocalId>,
) -> Vec<(LocalId, ResolvedType)> {
    let mut walker = CaptureWalker {
        scopes: vec![own_set.clone()],
        seen: BTreeSet::new(),
        order: Vec::new(),
        types: BTreeMap::new(),
    };
    walker.visit_body(body);
    walker
        .order
        .into_iter()
        .map(|id| (id, walker.types.remove(&id).unwrap()))
        .collect()
}

struct CaptureWalker {
    scopes: Vec<HashSet<LocalId>>,
    seen: BTreeSet<LocalId>,
    order: Vec<LocalId>,
    types: BTreeMap<LocalId, ResolvedType>,
}

impl CaptureWalker {
    fn visible(&self, id: LocalId) -> bool {
        self.scopes.iter().any(|frame| frame.contains(&id))
    }

    fn record(&mut self, id: LocalId, resolution: ResolvedType) {
        if self.seen.insert(id) {
            self.order.push(id);
            self.types.insert(id, resolution);
        }
    }

    fn visit_body(&mut self, body: BodyShape<'_>) {
        match body {
            BodyShape::Block(statements) => self.visit_statements(statements),
            BodyShape::Short(expr) => self.visit_expr(expr),
        }
    }

    fn visit_statements(&mut self, statements: &[Statement]) {
        for stmt in statements {
            self.visit_statement(stmt);
        }
    }

    fn visit_statement(&mut self, stmt: &Statement) {
        match stmt {
            Statement::Assignment { target, value, .. } => {
                self.visit_assign_target(target);
                self.visit_expr(value);
            }
            Statement::Break { .. } => {}
            Statement::CompoundAssign { value, .. } => self.visit_expr(value),
            Statement::Expr(expr) => self.visit_expr(expr),
            Statement::Return { value, .. } => {
                if let Some(expr) = value {
                    self.visit_expr(expr);
                }
            }
        }
    }

    fn visit_assign_target(&mut self, target: &AssignTarget) {
        match target {
            AssignTarget::LValue(_) => {}
            AssignTarget::Pattern(pattern) => self.visit_pattern(pattern),
        }
    }

    fn visit_expr(&mut self, expr: &Expr) {
        match &expr.kind {
            ExprKind::Binary { left, right, .. } => {
                self.visit_expr(left);
                self.visit_expr(right);
            }
            ExprKind::BinaryLiteral { segments } => {
                for segment in segments {
                    self.visit_binary_segment(segment);
                }
            }
            ExprKind::Call { callee, args, .. } => {
                self.visit_expr(callee);
                for arg in args {
                    self.visit_expr(&arg.value);
                }
            }
            ExprKind::Closure { params, body, .. } => {
                self.enter_closure(params, body);
            }
            ExprKind::Cond { arms, else_body } => {
                for arm in arms {
                    self.visit_expr(&arm.condition);
                    self.visit_statements(&arm.body);
                }
                if let Some(body) = else_body {
                    self.visit_statements(body);
                }
            }
            ExprKind::EnumConstruction { data, .. } => match data {
                EnumConstructionData::Unit => {}
                EnumConstructionData::Tuple(exprs) => {
                    for expr in exprs {
                        self.visit_expr(expr);
                    }
                }
                EnumConstructionData::Struct(field_inits) => {
                    for field in field_inits {
                        self.visit_expr(&field.value);
                    }
                }
            },
            ExprKind::FieldAccess { receiver, .. } => self.visit_expr(receiver),
            ExprKind::For { iterable, body, .. } => {
                self.visit_expr(iterable);
                self.visit_statements(body);
            }
            ExprKind::Group { expr: inner } => self.visit_expr(inner),
            ExprKind::Ident { resolution, .. } => {
                if let Resolution::Local(id) = resolution
                    && !self.visible(*id)
                {
                    self.record(*id, expr.resolution.clone());
                }
            }
            ExprKind::If {
                condition,
                then_body,
                else_body,
            } => {
                self.visit_expr(condition);
                self.visit_statements(then_body);
                if let Some(body) = else_body {
                    self.visit_statements(body);
                }
            }
            ExprKind::List { elements } => {
                for element in elements {
                    self.visit_expr(element);
                }
            }
            ExprKind::Literal { .. } => {}
            ExprKind::Loop { body } => self.visit_statements(body),
            ExprKind::Map { entries } => {
                for (key, value) in entries {
                    self.visit_expr(key);
                    self.visit_expr(value);
                }
            }
            ExprKind::Match { subject, arms } => {
                self.visit_expr(subject);
                for arm in arms {
                    self.visit_match_arm(arm);
                }
            }
            ExprKind::MethodCall { receiver, args, .. } => {
                self.visit_expr(receiver);
                for arg in args {
                    self.visit_expr(&arg.value);
                }
            }
            ExprKind::Receive {
                arms,
                after_timeout,
                after_body,
            } => {
                for arm in arms {
                    self.visit_match_arm(arm);
                }
                if let Some(timeout) = after_timeout {
                    self.visit_expr(timeout);
                }
                self.visit_statements(after_body);
            }
            ExprKind::Self_ { local_id } => {
                if let Some(id) = local_id
                    && !self.visible(*id)
                {
                    self.record(*id, expr.resolution.clone());
                }
            }
            ExprKind::ShortClosure { params, body } => {
                self.enter_short_closure(params, body);
            }
            ExprKind::Spawn { expr: inner } => self.visit_expr(inner),
            ExprKind::String { parts, .. } => {
                for part in parts {
                    if let StringPart::Interpolation { expr, .. } = part {
                        self.visit_expr(expr);
                    }
                }
            }
            ExprKind::StructConstruction { fields, .. } => {
                for field in fields {
                    self.visit_expr(&field.value);
                }
            }
            ExprKind::Ternary {
                condition,
                then_expr,
                else_expr,
            } => {
                self.visit_expr(condition);
                self.visit_expr(then_expr);
                self.visit_expr(else_expr);
            }
            ExprKind::Unary { operand, .. } => self.visit_expr(operand),
            ExprKind::Unless { condition, body } => {
                self.visit_expr(condition);
                self.visit_statements(body);
            }
            ExprKind::While { condition, body } => {
                self.visit_expr(condition);
                self.visit_statements(body);
            }
        }
    }

    fn enter_closure(&mut self, params: &[ClosureParam], body: &[Statement]) {
        let frame = own_set_for(params, body);
        self.scopes.push(frame);
        self.visit_statements(body);
        self.scopes.pop();
    }

    fn enter_short_closure(&mut self, params: &[ClosureParam], body: &Expr) {
        let frame = own_set_for(params, &[]);
        self.scopes.push(frame);
        self.visit_expr(body);
        self.scopes.pop();
    }

    fn visit_match_arm(&mut self, arm: &MatchArm) {
        self.visit_pattern(&arm.pattern);
        if let Some(guard) = arm.guard.as_ref() {
            self.visit_expr(guard);
        }
        self.visit_statements(&arm.body);
    }

    fn visit_pattern(&mut self, _pattern: &Pattern) {
        // Patterns don't reference outer locals through expressions
        // we lower today; nested expression slots (`Bind { default }`)
        // would extend this when they land.
    }

    fn visit_binary_segment(&mut self, segment: &BinarySegment) {
        self.visit_expr(&segment.value);
    }
}

/// Resolve each capture's [`IRType`] alongside its AST resolution.
fn resolve_capture_types(
    captures: &[(LocalId, ResolvedType)],
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Vec<CaptureInfo> {
    captures
        .iter()
        .map(|(local_id, resolution)| {
            let ir_type =
                resolved_type_to_ir_type(resolution, registry, &mut output.instantiations);
            CaptureInfo {
                local_id: *local_id,
                ir_type,
            }
        })
        .collect()
}

/// AST + lifted-signature view of one closure expression. Bundles
/// the four facets [`synthesize_body`] needs to materialize a body:
/// surface params (`ClosureParam` carries `local_id` stamps), the
/// statement / short-expr form, the resolver-stamped fn-type
/// `params` (with `mode`), and the fn-type return.
struct ClosureSig<'a> {
    body: BodyShape<'a>,
    closure_params: &'a [ClosureParam],
    fn_params: &'a [FnParam],
    fn_ret: &'a ResolvedType,
}

/// Build the closure body's [`IRFunction`] under a fresh ctx. The
/// ctx is seeded with the captures map so outer-local idents
/// inside the body lower to [`IRInstruction::LoadCapture`].
fn synthesize_body(
    symbol: &IRSymbol,
    sig: ClosureSig<'_>,
    captures: &[CaptureInfo],
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<IRFunction, ()> {
    let mut ctx = FnLowerCtx::new();
    ctx.closures_mut().set_enclosing_symbol(symbol.clone());
    let capture_ids: Vec<LocalId> = captures.iter().map(|c| c.local_id).collect();
    ctx.closures_mut().set_captures(&capture_ids);

    let entry = ctx.fresh_block("entry");
    let params = lower_closure_params(
        sig.closure_params,
        sig.fn_params,
        registry,
        output,
        &mut ctx,
    );
    let body_statements = match sig.body {
        BodyShape::Block(stmts) => stmts.to_vec(),
        BodyShape::Short(expr) => vec![Statement::Expr(expr.clone())],
    };
    let flow = lower_body(&body_statements, &mut ctx, entry, registry, output)?;
    finalize_open_flow(&mut ctx, flow);

    let return_type = resolved_type_to_ir_type(sig.fn_ret, registry, &mut output.instantiations);
    let env_layout: Vec<IRType> = captures.iter().map(|c| c.ir_type.clone()).collect();
    Ok(IRFunction {
        blocks: ctx.into_blocks(),
        kind: FunctionKind::Closure { env_layout },
        params,
        return_type,
        symbol: symbol.clone(),
    })
}

/// Mint and promote the closure body's user-visible parameters,
/// mirroring [`crate::lower::package::lower_params`] but driven by
/// a [`ClosureParam`] / [`FnParam`] pair instead of an AST
/// `Function`.
fn lower_closure_params(
    closure_params: &[ClosureParam],
    fn_params: &[FnParam],
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
    ctx: &mut FnLowerCtx,
) -> Vec<IRFunctionParam> {
    let mut params = Vec::with_capacity(closure_params.len());
    for (index, (closure_param, fn_param)) in
        closure_params.iter().zip(fn_params.iter()).enumerate()
    {
        let local_id = closure_param_local_id(closure_param, index);
        let ty = resolved_type_to_ir_type(&fn_param.ty, registry, &mut output.instantiations);
        let id = ctx.fresh_value(ty.clone());
        let ir_local = IRLocalId::from_local_id(local_id);
        let entry = ctx.entry_block();
        ctx.cfg.append(
            entry,
            IRInstruction::LocalDecl {
                local: ir_local,
                ty: ty.clone(),
            },
        );
        ctx.cfg.append(
            entry,
            IRInstruction::LocalWrite {
                local: ir_local,
                value: id,
            },
        );
        ctx.mark_local_declared(ir_local, ty.clone());
        params.push(IRFunctionParam {
            id,
            local_id: ir_local,
            ty,
        });
    }
    params
}

fn closure_param_local_id(param: &ClosureParam, index: usize) -> LocalId {
    match param {
        ClosureParam::Name {
            local_id: Some(id), ..
        }
        | ClosureParam::Wildcard {
            local_id: Some(id), ..
        } => *id,
        ClosureParam::Name { local_id: None, .. }
        | ClosureParam::Wildcard { local_id: None, .. } => panic!(
            "IR lower: closure param #{index} carries no LocalId — \
             typecheck resolve invariant violation",
        ),
        ClosureParam::Destructured { .. } => panic!(
            "IR lower: closure param #{index} ({:?}) is not yet supported in lowering",
            param,
        ),
    }
}

/// Read each capture in the outer ctx then emit
/// [`IRInstruction::MakeClosure`]. Captures route through the
/// outer ctx's local-or-capture path so a closure built inside
/// another closure correctly forwards captures via
/// [`IRInstruction::LoadCapture`].
fn emit_make_closure(
    symbol: IRSymbol,
    captures: &[CaptureInfo],
    closure_resolution: &ResolvedType,
    ctx: &mut FnLowerCtx,
    block: IRBlockId,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> Result<(ValueId, IRBlockId), ()> {
    let mut capture_values = Vec::with_capacity(captures.len());
    for capture in captures {
        capture_values.push(read_capture(capture, ctx, block));
    }
    let ty = resolved_type_to_ir_type(closure_resolution, registry, &mut output.instantiations);
    let dest = ctx.fresh_value(ty.clone());
    ctx.cfg.append(
        block,
        IRInstruction::MakeClosure {
            body: symbol,
            captures: capture_values,
            dest,
            ty,
        },
    );
    Ok((dest, block))
}

/// Read a single capture's current value in the outer ctx. Under
/// value semantics every capture copies into the env: a `LoadCapture`
/// when the outer ctx is itself a closure body, otherwise a
/// `LocalRead` of the outer slot. The outer binding stays live.
fn read_capture(capture: &CaptureInfo, ctx: &mut FnLowerCtx, block: IRBlockId) -> ValueId {
    if let Some(capture_index) = ctx.closures().capture_index(capture.local_id) {
        let dest = ctx.fresh_value(capture.ir_type.clone());
        ctx.cfg.append(
            block,
            IRInstruction::LoadCapture {
                capture_index,
                dest,
                ty: capture.ir_type.clone(),
            },
        );
        return dest;
    }
    let ir_local = IRLocalId::from_local_id(capture.local_id);
    let dest = ctx.fresh_value(capture.ir_type.clone());
    ctx.cfg.append(
        block,
        IRInstruction::LocalRead {
            dest,
            local: ir_local,
            ty: capture.ir_type.clone(),
        },
    );
    dest
}

/// Look up (or mint and cache) the captureless wrapper closure that
/// adapts a named function to a closure value. The wrapper has the
/// same user-visible signature as `target` and a body that simply
/// forwards every param into a direct [`IRInstruction::Call`] of
/// `target`. Cached on `output.fn_as_closure_wrappers` so repeated
/// references to the same fn share a single synthesized body.
pub(super) fn synthesize_fn_as_closure_wrapper(
    target_symbol: &IRSymbol,
    sig: &FunctionSignature,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> IRSymbol {
    if let Some(cached) = output.fn_as_closure_wrappers.get(target_symbol) {
        return cached.clone();
    }
    let wrapper_symbol = target_symbol.derived("__as_closure");
    let function = build_fn_as_closure_wrapper(
        target_symbol.clone(),
        wrapper_symbol.clone(),
        sig,
        registry,
        output,
    );
    output.synthesized_functions.push(function);
    output
        .fn_as_closure_wrappers
        .insert(target_symbol.clone(), wrapper_symbol.clone());
    wrapper_symbol
}

/// Hand-build the wrapper [`IRFunction`]. Mirrors
/// [`super::package::lower_params`] for parameter promotion, then
/// reads each slot back, calls `target_symbol`, and returns the
/// result. Owned heap params drop at fn exit through the standard
/// [`emit_function_exit_drops`] helper.
fn build_fn_as_closure_wrapper(
    target_symbol: IRSymbol,
    wrapper_symbol: IRSymbol,
    sig: &FunctionSignature,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
) -> IRFunction {
    let mut ctx = FnLowerCtx::new();
    ctx.closures_mut()
        .set_enclosing_symbol(wrapper_symbol.clone());
    let entry = ctx.fresh_block("entry");

    let params = mint_wrapper_params(sig, &mut ctx, registry, output, entry);
    let arg_values = read_wrapper_args(&params, &mut ctx, entry);

    let return_ty =
        resolved_type_to_ir_type(&sig.return_type, registry, &mut output.instantiations);
    let call_dest = ctx.fresh_value(return_ty.clone());
    ctx.cfg.append(
        entry,
        IRInstruction::Call {
            args: arg_values,
            callee: target_symbol,
            dest: call_dest,
        },
    );
    emit_function_exit_drops(&mut ctx, entry);
    ctx.cfg.set_terminator(
        entry,
        IRTerminator::Return {
            value: Some(call_dest),
        },
    );

    IRFunction {
        blocks: ctx.into_blocks(),
        kind: FunctionKind::Closure {
            env_layout: Vec::new(),
        },
        params,
        return_type: return_ty,
        symbol: wrapper_symbol,
    }
}

/// Mint a fresh slot per wrapped fn parameter. Each slot mirrors a
/// regular fn-param promotion: `LocalDecl` + `LocalWrite` in the
/// entry block.
fn mint_wrapper_params(
    sig: &FunctionSignature,
    ctx: &mut FnLowerCtx,
    registry: &GlobalRegistry,
    output: &mut LowerOutput,
    entry: IRBlockId,
) -> Vec<IRFunctionParam> {
    let mut params = Vec::with_capacity(sig.params.len());
    for (index, param) in sig.params.iter().enumerate() {
        let ty = resolved_type_to_ir_type(&param.ty, registry, &mut output.instantiations);
        let id = ctx.fresh_value(ty.clone());
        let local_id = LocalId::new(index as u32);
        let ir_local = IRLocalId::from_local_id(local_id);
        ctx.cfg.append(
            entry,
            IRInstruction::LocalDecl {
                local: ir_local,
                ty: ty.clone(),
            },
        );
        ctx.cfg.append(
            entry,
            IRInstruction::LocalWrite {
                local: ir_local,
                value: id,
            },
        );
        ctx.mark_local_declared(ir_local, ty.clone());
        params.push(IRFunctionParam {
            id,
            local_id: ir_local,
            ty,
        });
    }
    params
}

/// Read each promoted slot back into a fresh `ValueId` for the
/// inner [`IRInstruction::Call`]. Mirrors [`super::calls::emit_call`]'s
/// arg-lowering shape (`LocalRead` per arg) so callee semantics
/// match a hand-written `fn (x) -> target(x) end` shim.
fn read_wrapper_args(
    params: &[IRFunctionParam],
    ctx: &mut FnLowerCtx,
    entry: IRBlockId,
) -> Vec<ValueId> {
    let mut values = Vec::with_capacity(params.len());
    for param in params {
        let dest = ctx.fresh_value(param.ty.clone());
        ctx.cfg.append(
            entry,
            IRInstruction::LocalRead {
                dest,
                local: param.local_id,
                ty: param.ty.clone(),
            },
        );
        values.push(dest);
    }
    values
}
