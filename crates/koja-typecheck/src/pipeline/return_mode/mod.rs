//! `infer_return_modes`: classify each function's result as
//! [`ReturnMode::Owned`] (fresh heap the caller may drop) or
//! [`ReturnMode::Borrowed`] (a view aliasing an input / a static, which
//! must never be dropped) and stamp it onto the function's
//! [`FunctionSignature`](crate::registry::FunctionSignature).
//!
//! Runs after `resolve` so every call site carries its resolution and
//! after `synthesize_program` so every desugared call exists. The pass
//! only *computes and stores* the mode — drop insertion that consumes
//! it lands in a later phase, so the tree stays green.
//!
//! A memoized DFS over the resolved call graph: a function is `Owned`
//! iff every value it returns is owned, where a returned call's
//! ownership is its callee's mode. Unresolved callees and cycles
//! (recursion / mutual recursion) resolve to `Borrowed`, biasing the
//! pass to leak-not-double-free.

use std::collections::HashMap;

use koja_ast::ast::{
    AssignTarget, BinOp, CondArm, Expr, ExprKind, Function, Item, MatchArm, Param, PassMode,
    ReturnMode, Statement, StringPart, is_intrinsic,
};
use koja_ast::identifier::{GlobalRegistryId, Identifier, LocalId, Resolution, ResolvedType};

use crate::pipeline::collect::extend_target_path;
use crate::pipeline::lift_signatures::impl_target_name;
use crate::program::CheckedPackage;
use crate::registry::{GlobalKind, GlobalRegistry};

mod catalog;

/// Classify every function's return mode and stamp it onto the
/// registry. Operates on the resolved AST bodies in `packages`,
/// writing through [`GlobalRegistry::set_function_return_mode`].
pub(crate) fn infer_return_modes(packages: &[CheckedPackage], registry: &mut GlobalRegistry) {
    let functions = collect_functions(packages, registry);
    let ids: Vec<GlobalRegistryId> = functions.keys().copied().collect();
    let mut inference = Inference {
        functions,
        memo: HashMap::new(),
        registry,
    };
    for id in &ids {
        inference.mode(*id, &mut Vec::new());
    }
    for (id, mode) in inference.memo {
        registry.set_function_return_mode(id, mode);
    }
}

/// A function the pass can analyze: its canonical identifier (for the
/// intrinsic catalog), whether it's `@intrinsic`, and borrows of its
/// params and body for the `owned` walk.
#[derive(Clone)]
struct FnNode<'a> {
    body: Option<&'a [Statement]>,
    identifier: Identifier,
    is_intrinsic: bool,
    params: &'a [Param],
}

/// Build the id → [`FnNode`] index by walking every function-bearing
/// item and reconstructing the identifier each one registered under
/// (mirroring `lift_signatures`). Functions whose identifier doesn't
/// resolve to a signed function entry are skipped — they keep the
/// leak-safe default.
fn collect_functions<'a>(
    packages: &'a [CheckedPackage],
    registry: &GlobalRegistry,
) -> HashMap<GlobalRegistryId, FnNode<'a>> {
    let mut functions = HashMap::new();
    let mut record = |identifier: Identifier, function: &'a Function| {
        let Some((id, entry)) = registry.lookup(&identifier) else {
            return;
        };
        if !matches!(entry.kind, GlobalKind::Function(Some(_))) {
            return;
        }
        functions.insert(
            id,
            FnNode {
                body: function.body.as_deref(),
                identifier,
                is_intrinsic: is_intrinsic(&function.annotations),
                params: &function.params,
            },
        );
    };
    for pkg in packages {
        for file in &pkg.files {
            for item in &file.items {
                match item {
                    Item::Function(function) => {
                        record(
                            Identifier::new(&pkg.package, vec![function.name.clone()]),
                            function,
                        );
                    }
                    Item::Struct(decl) => {
                        for function in &decl.functions {
                            let path = vec![decl.name.clone(), function.name.clone()];
                            record(Identifier::new(&pkg.package, path), function);
                        }
                    }
                    Item::Enum(decl) => {
                        for function in &decl.functions {
                            let path = vec![decl.name.clone(), function.name.clone()];
                            record(Identifier::new(&pkg.package, path), function);
                        }
                    }
                    Item::Impl(block) => {
                        let Some(target) = impl_target_name(&block.target) else {
                            continue;
                        };
                        for function in block.members.iter().filter_map(member_function) {
                            let path = vec![target.to_string(), function.name.clone()];
                            record(Identifier::new(&pkg.package, path), function);
                        }
                    }
                    Item::Extend(block) => {
                        let Some((package, target)) =
                            extend_target_path(&block.target, &pkg.package)
                        else {
                            continue;
                        };
                        for function in block.members.iter().filter_map(member_function) {
                            let path = vec![target.to_string(), function.name.clone()];
                            record(Identifier::new(package.as_str(), path), function);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
    functions
}

fn member_function(member: &koja_ast::ast::ImplMember) -> Option<&Function> {
    match member {
        koja_ast::ast::ImplMember::Function(function) => Some(function),
        koja_ast::ast::ImplMember::TypeAlias(_) => None,
    }
}

/// Whether `ty` is a scalar primitive (no heap payload, so never
/// dropped). Used to fold scalar payloads through as `Owned`-neutral.
fn is_scalar_type(ty: &ResolvedType, registry: &GlobalRegistry) -> bool {
    let ResolvedType::Named {
        resolution: Resolution::Global(id),
        ..
    } = ty
    else {
        return false;
    };
    let Some(entry) = registry.get(*id) else {
        return false;
    };
    entry.identifier.package() == "Global"
        && matches!(
            entry.identifier.last(),
            "Bool"
                | "CPtr"
                | "Float"
                | "Float32"
                | "Float64"
                | "Int"
                | "Int8"
                | "Int16"
                | "Int32"
                | "Int64"
                | "UInt8"
                | "UInt16"
                | "UInt32"
                | "UInt64"
                | "Unit"
        )
}

/// `Owned` iff both inputs are owned; the fold identity is `Owned`
/// ("a function with no value-bearing return is vacuously owned").
fn both(lhs: ReturnMode, rhs: ReturnMode) -> ReturnMode {
    match (lhs, rhs) {
        (ReturnMode::Owned, ReturnMode::Owned) => ReturnMode::Owned,
        _ => ReturnMode::Borrowed,
    }
}

struct Inference<'a> {
    functions: HashMap<GlobalRegistryId, FnNode<'a>>,
    memo: HashMap<GlobalRegistryId, ReturnMode>,
    registry: &'a GlobalRegistry,
}

impl<'a> Inference<'a> {
    /// Memoized mode of function `id`. `visiting` is the active DFS
    /// stack; a back-edge into it (recursion) short-circuits to
    /// `Borrowed`.
    fn mode(&mut self, id: GlobalRegistryId, visiting: &mut Vec<GlobalRegistryId>) -> ReturnMode {
        if let Some(&mode) = self.memo.get(&id) {
            return mode;
        }
        if visiting.contains(&id) {
            return ReturnMode::Borrowed;
        }
        let Some(node) = self.functions.get(&id).cloned() else {
            return ReturnMode::Borrowed;
        };
        let mode = if node.is_intrinsic {
            catalog::intrinsic_return_mode(&node.identifier)
        } else if let Some(body) = node.body {
            visiting.push(id);
            let ctx = BodyCtx::new(node.params, body);
            let mode = self.body_mode(&ctx, body, visiting);
            visiting.pop();
            mode
        } else {
            ReturnMode::Borrowed
        };
        self.memo.insert(id, mode);
        mode
    }

    /// `Owned` iff the implicit tail and every explicit `return` are
    /// owned.
    fn body_mode(
        &mut self,
        ctx: &BodyCtx<'a>,
        body: &'a [Statement],
        visiting: &mut Vec<GlobalRegistryId>,
    ) -> ReturnMode {
        let mut mode = self.block_mode(ctx, body, visiting);
        let mut returns = Vec::new();
        collect_returns(body, &mut returns);
        for expr in returns {
            mode = both(mode, self.owned(ctx, expr, visiting, &mut Vec::new()));
            if mode == ReturnMode::Borrowed {
                return mode;
            }
        }
        mode
    }

    /// Mode of a block's value: its tail expression, or vacuously
    /// `Owned` when the block has no value-bearing tail.
    fn block_mode(
        &mut self,
        ctx: &BodyCtx<'a>,
        stmts: &'a [Statement],
        visiting: &mut Vec<GlobalRegistryId>,
    ) -> ReturnMode {
        match stmts.last() {
            Some(Statement::Expr(expr)) => self.owned(ctx, expr, visiting, &mut Vec::new()),
            _ => ReturnMode::Owned,
        }
    }

    /// Whether `expr`'s value is freshly owned. `seen` guards cycles in
    /// the local-binding graph (a local whose RHS reads itself).
    fn owned(
        &mut self,
        ctx: &BodyCtx<'a>,
        expr: &'a Expr,
        visiting: &mut Vec<GlobalRegistryId>,
        seen: &mut Vec<LocalId>,
    ) -> ReturnMode {
        // A scalar value (`Int`, `Bool`, `CPtr`, …) is never heap, so
        // its ownership is moot — fold it through as the `Owned`
        // identity so a scalar payload never drags an aggregate down.
        if is_scalar_type(&expr.resolution, self.registry) {
            return ReturnMode::Owned;
        }
        match &expr.kind {
            // Fresh aggregates: owned iff every heap payload is owned.
            ExprKind::EnumConstruction { data, .. } => {
                self.enum_data_mode(ctx, data, visiting, seen)
            }
            ExprKind::List { elements } => self.all_owned(ctx, elements, visiting, seen),
            ExprKind::Map { entries } => entries.iter().fold(ReturnMode::Owned, |mode, (k, v)| {
                both(
                    mode,
                    both(
                        self.owned(ctx, k, visiting, seen),
                        self.owned(ctx, v, visiting, seen),
                    ),
                )
            }),
            ExprKind::StructConstruction { fields, .. } => {
                fields.iter().fold(ReturnMode::Owned, |mode, field| {
                    both(mode, self.owned(ctx, &field.value, visiting, seen))
                })
            }
            // Fresh heap regardless of inputs.
            ExprKind::Binary {
                op: BinOp::Concat, ..
            }
            | ExprKind::BinaryLiteral { .. } => ReturnMode::Owned,
            // Owned only when interpolated (a plain string literal is a
            // static, like any other literal).
            ExprKind::String { parts, .. } => {
                if parts
                    .iter()
                    .any(|p| matches!(p, StringPart::Interpolation { .. }))
                {
                    ReturnMode::Owned
                } else {
                    ReturnMode::Borrowed
                }
            }
            // The call result's ownership is its callee's mode.
            ExprKind::Call { callee, .. } => match self.call_callee(callee) {
                Some(id) => self.mode(id, visiting),
                None => ReturnMode::Borrowed,
            },
            ExprKind::MethodCall {
                receiver, method, ..
            } => match self.method_callee(receiver, method) {
                Some(id) => self.mode(id, visiting),
                None => ReturnMode::Borrowed,
            },
            // Control-flow joins: owned iff every arm is owned.
            ExprKind::If {
                then_body,
                else_body,
                ..
            } => {
                let then_mode = self.block_mode(ctx, then_body, visiting);
                both(
                    then_mode,
                    self.optional_block_mode(ctx, else_body.as_deref(), visiting),
                )
            }
            ExprKind::Unless { body, .. } => self.block_mode(ctx, body, visiting),
            ExprKind::Cond { arms, else_body } => {
                self.arms_mode(ctx, arms, else_body.as_deref(), visiting)
            }
            ExprKind::Match { arms, .. } => self.match_arms_mode(ctx, arms, visiting),
            ExprKind::Ternary {
                then_expr,
                else_expr,
                ..
            } => both(
                self.owned(ctx, then_expr, visiting, seen),
                self.owned(ctx, else_expr, visiting, seen),
            ),
            ExprKind::Group { expr } => self.owned(ctx, expr, visiting, seen),
            // Bindings: param move-through is owned; tracked locals
            // inherit their recorded mode; everything else borrows.
            ExprKind::Ident {
                resolution: Resolution::Local(local),
                ..
            } => self.local_or_param_mode(ctx, *local, visiting, seen),
            ExprKind::Self_ {
                local_id: Some(local),
            } => self.param_mode(ctx, *local).unwrap_or(ReturnMode::Borrowed),
            // Views, statics, scalars, and indirect / unknown producers.
            _ => ReturnMode::Borrowed,
        }
    }

    fn enum_data_mode(
        &mut self,
        ctx: &BodyCtx<'a>,
        data: &'a koja_ast::ast::EnumConstructionData,
        visiting: &mut Vec<GlobalRegistryId>,
        seen: &mut Vec<LocalId>,
    ) -> ReturnMode {
        use koja_ast::ast::EnumConstructionData::{Struct, Tuple, Unit};
        match data {
            Unit => ReturnMode::Owned,
            Tuple(values) => self.all_owned(ctx, values, visiting, seen),
            Struct(fields) => fields.iter().fold(ReturnMode::Owned, |mode, field| {
                both(mode, self.owned(ctx, &field.value, visiting, seen))
            }),
        }
    }

    fn all_owned(
        &mut self,
        ctx: &BodyCtx<'a>,
        exprs: &'a [Expr],
        visiting: &mut Vec<GlobalRegistryId>,
        seen: &mut Vec<LocalId>,
    ) -> ReturnMode {
        exprs.iter().fold(ReturnMode::Owned, |mode, expr| {
            both(mode, self.owned(ctx, expr, visiting, seen))
        })
    }

    fn optional_block_mode(
        &mut self,
        ctx: &BodyCtx<'a>,
        block: Option<&'a [Statement]>,
        visiting: &mut Vec<GlobalRegistryId>,
    ) -> ReturnMode {
        match block {
            Some(stmts) => self.block_mode(ctx, stmts, visiting),
            None => ReturnMode::Owned,
        }
    }

    fn arms_mode(
        &mut self,
        ctx: &BodyCtx<'a>,
        arms: &'a [CondArm],
        else_body: Option<&'a [Statement]>,
        visiting: &mut Vec<GlobalRegistryId>,
    ) -> ReturnMode {
        let mut mode = self.optional_block_mode(ctx, else_body, visiting);
        for arm in arms {
            mode = both(mode, self.block_mode(ctx, &arm.body, visiting));
        }
        mode
    }

    fn match_arms_mode(
        &mut self,
        ctx: &BodyCtx<'a>,
        arms: &'a [MatchArm],
        visiting: &mut Vec<GlobalRegistryId>,
    ) -> ReturnMode {
        arms.iter().fold(ReturnMode::Owned, |mode, arm| {
            both(mode, self.block_mode(ctx, &arm.body, visiting))
        })
    }

    /// A `move` parameter hands its value through (owned); a borrow
    /// parameter aliases its caller (borrowed); a tracked local inherits
    /// its recorded mode.
    fn local_or_param_mode(
        &mut self,
        ctx: &BodyCtx<'a>,
        local: LocalId,
        visiting: &mut Vec<GlobalRegistryId>,
        seen: &mut Vec<LocalId>,
    ) -> ReturnMode {
        if let Some(mode) = self.param_mode(ctx, local) {
            return mode;
        }
        if seen.contains(&local) {
            return ReturnMode::Borrowed;
        }
        let Some(rhs) = ctx.local_rhs.get(&local) else {
            return ReturnMode::Borrowed;
        };
        if rhs.is_empty() {
            return ReturnMode::Borrowed;
        }
        seen.push(local);
        let mut mode = ReturnMode::Owned;
        for expr in rhs {
            mode = both(mode, self.owned(ctx, expr, visiting, seen));
            if mode == ReturnMode::Borrowed {
                break;
            }
        }
        seen.pop();
        mode
    }

    fn param_mode(&self, ctx: &BodyCtx<'a>, local: LocalId) -> Option<ReturnMode> {
        // Under value semantics the `move` keyword is inert: every
        // parameter borrows, so a value that flows straight through
        // from a parameter aliases storage the caller still owns and
        // must be returned `Borrowed` (never re-freed at the call site).
        ctx.param_modes
            .get(&local)
            .map(|_mode| ReturnMode::Borrowed)
    }

    /// Resolve a bare `Call` callee to its function id, or `None` for
    /// indirect / non-function callees.
    fn call_callee(&self, callee: &Expr) -> Option<GlobalRegistryId> {
        let ExprKind::Ident {
            resolution: Resolution::Global(id),
            ..
        } = &callee.kind
        else {
            return None;
        };
        let entry = self.registry.get(*id)?;
        matches!(entry.kind, GlobalKind::Function(_)).then_some(*id)
    }

    /// Resolve a method call to its function id by rebuilding the
    /// `[receiver_type.., method]` identifier the receiver dispatches
    /// to (mirroring IR lowering). `None` for bounded / protocol /
    /// closure receivers with no concrete head.
    fn method_callee(&self, receiver: &Expr, method: &str) -> Option<GlobalRegistryId> {
        let struct_id = self.canonical_receiver_id(self.receiver_struct_id(receiver)?);
        let entry = self.registry.get(struct_id)?;
        let mut path = entry.identifier.path().to_vec();
        path.push(method.to_string());
        let method_identifier = Identifier::new(entry.identifier.package(), path);
        let (id, method_entry) = self.registry.lookup(&method_identifier)?;
        matches!(method_entry.kind, GlobalKind::Function(_)).then_some(id)
    }

    fn receiver_struct_id(&self, receiver: &Expr) -> Option<GlobalRegistryId> {
        if let ExprKind::Ident {
            resolution: Resolution::Global(id),
            ..
        } = &receiver.kind
            && let Some(entry) = self.registry.get(*id)
            && matches!(entry.kind, GlobalKind::Enum(_) | GlobalKind::Struct(_))
        {
            return Some(*id);
        }
        match &receiver.resolution {
            ResolvedType::Named {
                resolution: Resolution::Global(id),
                ..
            } => Some(*id),
            _ => None,
        }
    }

    /// Collapse `Global.Int64` / `Global.Float64` onto `Global.Int` /
    /// `Global.Float` for method lookup, matching the typecheck alias
    /// rule (and IR lowering's `canonical_receiver_id`).
    fn canonical_receiver_id(&self, id: GlobalRegistryId) -> GlobalRegistryId {
        let Some(entry) = self.registry.get(id) else {
            return id;
        };
        if entry.identifier.package() != "Global" || entry.identifier.path().len() != 1 {
            return id;
        }
        let canonical = match entry.identifier.last() {
            "Float64" => "Float",
            "Int64" => "Int",
            _ => return id,
        };
        let canonical_identifier = Identifier::new("Global", vec![canonical.to_string()]);
        self.registry
            .lookup(&canonical_identifier)
            .map(|(id, _)| id)
            .unwrap_or(id)
    }
}

/// Per-function lookup tables for the `owned` walk: each param's pass
/// mode and each single-binding local's right-hand sides.
struct BodyCtx<'a> {
    local_rhs: HashMap<LocalId, Vec<&'a Expr>>,
    param_modes: HashMap<LocalId, PassMode>,
}

impl<'a> BodyCtx<'a> {
    fn new(params: &'a [Param], body: &'a [Statement]) -> Self {
        let mut param_modes = HashMap::new();
        for param in params {
            match param {
                Param::Regular {
                    local_id: Some(id),
                    mode,
                    ..
                }
                | Param::Self_ {
                    local_id: Some(id),
                    mode,
                    ..
                } => {
                    param_modes.insert(*id, *mode);
                }
                _ => {}
            }
        }
        let mut local_rhs: HashMap<LocalId, Vec<&'a Expr>> = HashMap::new();
        collect_local_bindings(body, &mut local_rhs);
        Self {
            local_rhs,
            param_modes,
        }
    }
}

/// Record every single-segment `local = expr` binding in `stmts` (and
/// nested control-flow blocks, but not nested closures) into `out`.
fn collect_local_bindings<'a>(stmts: &'a [Statement], out: &mut HashMap<LocalId, Vec<&'a Expr>>) {
    for stmt in stmts {
        match stmt {
            Statement::Assignment {
                target: AssignTarget::LValue(lvalue),
                value,
                ..
            } => {
                if lvalue.segments.len() == 1
                    && let Some(local) = lvalue.local_id
                {
                    out.entry(local).or_default().push(value);
                }
                collect_local_bindings_in_expr(value, out);
            }
            Statement::Assignment { value, .. } | Statement::Expr(value) => {
                collect_local_bindings_in_expr(value, out);
            }
            Statement::CompoundAssign { value, .. } => {
                collect_local_bindings_in_expr(value, out);
            }
            Statement::Return {
                value: Some(value), ..
            } => {
                collect_local_bindings_in_expr(value, out);
            }
            Statement::Return { value: None, .. } | Statement::Break { .. } => {}
        }
    }
}

fn collect_local_bindings_in_expr<'a>(expr: &'a Expr, out: &mut HashMap<LocalId, Vec<&'a Expr>>) {
    for_each_nested_block(expr, &mut |stmts| collect_local_bindings(stmts, out));
}

/// Collect every explicit `return expr` reachable from `stmts` without
/// crossing into a nested closure (whose returns are its own).
fn collect_returns<'a>(stmts: &'a [Statement], out: &mut Vec<&'a Expr>) {
    for stmt in stmts {
        match stmt {
            Statement::Return {
                value: Some(value), ..
            } => {
                out.push(value);
                collect_returns_in_expr(value, out);
            }
            Statement::Assignment { value, .. }
            | Statement::CompoundAssign { value, .. }
            | Statement::Expr(value) => collect_returns_in_expr(value, out),
            Statement::Return { value: None, .. } | Statement::Break { .. } => {}
        }
    }
}

fn collect_returns_in_expr<'a>(expr: &'a Expr, out: &mut Vec<&'a Expr>) {
    for_each_nested_block(expr, &mut |stmts| collect_returns(stmts, out));
}

/// Invoke `visit` on every statement block nested directly inside
/// `expr`, recursing through sub-expressions but stopping at closure
/// boundaries.
fn for_each_nested_block<'a>(expr: &'a Expr, visit: &mut impl FnMut(&'a [Statement])) {
    match &expr.kind {
        ExprKind::If {
            condition,
            then_body,
            else_body,
        } => {
            for_each_nested_block(condition, visit);
            visit(then_body);
            if let Some(body) = else_body {
                visit(body);
            }
        }
        ExprKind::Unless { condition, body } | ExprKind::While { condition, body } => {
            for_each_nested_block(condition, visit);
            visit(body);
        }
        ExprKind::Cond { arms, else_body } => {
            for arm in arms {
                for_each_nested_block(&arm.condition, visit);
                visit(&arm.body);
            }
            if let Some(body) = else_body {
                visit(body);
            }
        }
        ExprKind::Match { subject, arms } => {
            for_each_nested_block(subject, visit);
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    for_each_nested_block(guard, visit);
                }
                visit(&arm.body);
            }
        }
        ExprKind::For { iterable, body, .. } => {
            for_each_nested_block(iterable, visit);
            visit(body);
        }
        ExprKind::Loop { body } => visit(body),
        ExprKind::Receive {
            arms,
            after_timeout,
            after_body,
        } => {
            for arm in arms {
                if let Some(guard) = &arm.guard {
                    for_each_nested_block(guard, visit);
                }
                visit(&arm.body);
            }
            if let Some(timeout) = after_timeout {
                for_each_nested_block(timeout, visit);
            }
            visit(after_body);
        }
        ExprKind::Binary { left, right, .. } => {
            for_each_nested_block(left, visit);
            for_each_nested_block(right, visit);
        }
        ExprKind::Call { callee, args, .. } => {
            for_each_nested_block(callee, visit);
            for arg in args {
                for_each_nested_block(&arg.value, visit);
            }
        }
        ExprKind::MethodCall { receiver, args, .. } => {
            for_each_nested_block(receiver, visit);
            for arg in args {
                for_each_nested_block(&arg.value, visit);
            }
        }
        ExprKind::FieldAccess { receiver, .. } => for_each_nested_block(receiver, visit),
        ExprKind::Group { expr }
        | ExprKind::Spawn { expr }
        | ExprKind::Unary { operand: expr, .. } => for_each_nested_block(expr, visit),
        ExprKind::Ternary {
            condition,
            then_expr,
            else_expr,
        } => {
            for_each_nested_block(condition, visit);
            for_each_nested_block(then_expr, visit);
            for_each_nested_block(else_expr, visit);
        }
        ExprKind::List { elements } => {
            for element in elements {
                for_each_nested_block(element, visit);
            }
        }
        ExprKind::Map { entries } => {
            for (key, value) in entries {
                for_each_nested_block(key, visit);
                for_each_nested_block(value, visit);
            }
        }
        ExprKind::StructConstruction { fields, .. } => {
            for field in fields {
                for_each_nested_block(&field.value, visit);
            }
        }
        ExprKind::EnumConstruction { data, .. } => {
            use koja_ast::ast::EnumConstructionData::{Struct, Tuple, Unit};
            match data {
                Unit => {}
                Tuple(values) => {
                    for value in values {
                        for_each_nested_block(value, visit);
                    }
                }
                Struct(fields) => {
                    for field in fields {
                        for_each_nested_block(&field.value, visit);
                    }
                }
            }
        }
        ExprKind::String { parts, .. } => {
            for part in parts {
                if let StringPart::Interpolation { expr, .. } = part {
                    for_each_nested_block(expr, visit);
                }
            }
        }
        // Closures own their returns; scalars / idents / literals carry
        // no nested statement block.
        ExprKind::BinaryLiteral { .. }
        | ExprKind::Closure { .. }
        | ExprKind::Ident { .. }
        | ExprKind::Literal { .. }
        | ExprKind::Self_ { .. }
        | ExprKind::ShortClosure { .. } => {}
    }
}
