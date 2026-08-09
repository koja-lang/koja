//! Post-resolve definite-assignment analysis. Reading a local is an
//! error unless every path from function entry assigns it first.
//! Branch arms join by intersection (an `if` where both arms assign
//! counts as assigned after the join), loop bodies are maybe-executed
//! and contribute nothing past the loop, and diverging arms
//! (`return`, `break`, `Never`-typed statements) never veto a join.
//! Only reads are gated. Reassignment after a zero-trip loop stays
//! legal, matching the IR-side `merge_slot_states` liveness rules.

use std::collections::HashSet;

use koja_ast::ast::{
    ClosureParam, Diagnostic, EnumConstructionData, Expr, ExprKind, File, Function, ImplMember,
    Item, LValue, MatchArm, Param, Pattern, Statement, StringPart,
};
use koja_ast::identifier::{LocalId, Resolution, ResolvedType};
use koja_ast::span::Span;

use super::resolve::types::is_primitive;
use crate::registry::GlobalRegistry;

pub(crate) fn check_file(
    file: &File,
    registry: &GlobalRegistry,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let mut checker = Checker {
        diagnostics,
        registry,
    };
    for item in &file.items {
        match item {
            Item::Builtin(decl) => checker.check_functions(&decl.functions),
            Item::Enum(decl) => checker.check_functions(&decl.functions),
            Item::Extend(block) => checker.check_members(&block.members),
            Item::Function(function) => checker.check_function(function),
            Item::Impl(block) => checker.check_members(&block.members),
            Item::Struct(decl) => checker.check_functions(&decl.functions),
            _ => {}
        }
    }
    if let Some(body) = file.body.as_ref() {
        let mut state = FlowState::default();
        checker.check_body(body, &mut state);
    }
}

/// The set of locals definitely assigned on every path reaching the
/// current program point.
#[derive(Clone, Default)]
struct FlowState {
    assigned: HashSet<LocalId>,
    /// Set once flow cannot fall through (`return`, `break`, or a
    /// `Never`-typed statement). Reads past this point are
    /// unreachable and vacuously fine, and a diverged arm is
    /// excluded from joins.
    diverged: bool,
}

impl FlowState {
    fn assign(&mut self, id: LocalId) {
        self.assigned.insert(id);
    }

    fn is_assigned(&self, id: LocalId) -> bool {
        self.diverged || self.assigned.contains(&id)
    }

    fn diverge(&mut self) {
        self.diverged = true;
    }

    /// Replace this state with the join of `arms`: the intersection
    /// of every live arm's assigned set. All arms diverging means
    /// the point after the join is unreachable.
    fn join_arms(&mut self, arms: Vec<FlowState>) {
        if self.diverged {
            return;
        }
        let mut live = arms.into_iter().filter(|arm| !arm.diverged);
        let Some(first) = live.next() else {
            self.diverged = true;
            return;
        };
        let mut merged = first.assigned;
        for arm in live {
            merged.retain(|id| arm.assigned.contains(id));
        }
        self.assigned = merged;
    }
}

struct Checker<'a, 'd> {
    diagnostics: &'d mut Vec<Diagnostic>,
    registry: &'a GlobalRegistry,
}

impl Checker<'_, '_> {
    fn check_functions(&mut self, functions: &[Function]) {
        for function in functions {
            self.check_function(function);
        }
    }

    fn check_members(&mut self, members: &[ImplMember]) {
        for member in members {
            if let ImplMember::Function(function) = member {
                self.check_function(function);
            }
        }
    }

    fn check_function(&mut self, function: &Function) {
        let Some(body) = function.body.as_ref() else {
            return;
        };
        let mut state = FlowState::default();
        for param in &function.params {
            let (Param::Regular { local_id, .. } | Param::Self_ { local_id, .. }) = param;
            if let Some(id) = local_id {
                state.assign(*id);
            }
        }
        for param in &function.params {
            if let Param::Regular {
                default: Some(default),
                ..
            } = param
            {
                self.check_expr(default, &mut state);
            }
        }
        self.check_body(body, &mut state);
    }

    fn check_body(&mut self, body: &[Statement], state: &mut FlowState) {
        for stmt in body {
            self.check_statement(stmt, state);
        }
    }

    fn check_statement(&mut self, stmt: &Statement, state: &mut FlowState) {
        match stmt {
            Statement::Assignment { target, value, .. } => {
                self.check_expr(value, state);
                if target.segments.len() == 1 {
                    if let Some(id) = target.local_id {
                        state.assign(id);
                    }
                } else {
                    // A field write reads the head local before
                    // storing into it.
                    self.check_lvalue_head(target, state);
                }
                self.diverge_on_never(&value.resolution, state);
            }
            Statement::Break { .. } => state.diverge(),
            Statement::CompoundAssign { target, value, .. } => {
                self.check_expr(value, state);
                // `x += rhs` reads `x` (or the head of `p.x`) first.
                self.check_lvalue_head(target, state);
            }
            Statement::Destructure { pattern, value, .. } => {
                self.check_expr(value, state);
                self.bind_pattern(pattern, state);
            }
            Statement::Expr(expr) => {
                self.check_expr(expr, state);
                self.diverge_on_never(&expr.resolution, state);
            }
            Statement::Return { value, .. } => {
                if let Some(value) = value.as_ref() {
                    self.check_expr(value, state);
                }
                state.diverge();
            }
        }
    }

    fn check_expr(&mut self, expr: &Expr, state: &mut FlowState) {
        match &expr.kind {
            ExprKind::Binary { left, right, .. } => {
                self.check_expr(left, state);
                self.check_expr(right, state);
            }
            ExprKind::BinaryLiteral { segments } => {
                for segment in segments {
                    self.check_expr(&segment.value, state);
                    if let Some(size) = segment.size.as_ref() {
                        self.check_expr(size, state);
                    }
                }
            }
            ExprKind::Call { callee, args, .. } => {
                self.check_expr(callee, state);
                for arg in args {
                    self.check_expr(&arg.value, state);
                }
            }
            ExprKind::Closure { params, body, .. } => {
                // The body runs later (maybe never), so its
                // assignments stay local to this clone. Captured
                // reads must be assigned at the creation site.
                let mut inner = self.closure_entry_state(params, state);
                self.check_body(body, &mut inner);
            }
            ExprKind::Cond { arms, else_body } => {
                let mut arm_states = Vec::with_capacity(arms.len() + 1);
                for (index, arm) in arms.iter().enumerate() {
                    if index == 0 {
                        // Only the first condition is guaranteed to run.
                        self.check_expr(&arm.condition, state);
                    } else {
                        let mut cond_state = state.clone();
                        self.check_expr(&arm.condition, &mut cond_state);
                    }
                    let mut arm_state = state.clone();
                    self.check_body(&arm.body, &mut arm_state);
                    arm_states.push(arm_state);
                }
                if let Some(else_body) = else_body {
                    let mut else_state = state.clone();
                    self.check_body(else_body, &mut else_state);
                    arm_states.push(else_state);
                    state.join_arms(arm_states);
                }
                // Without an else the fall-through path assigns
                // nothing, so the pre-state already is the join.
            }
            ExprKind::EnumConstruction { data, .. } => match data {
                EnumConstructionData::Struct(fields) => {
                    for field in fields {
                        self.check_expr(&field.value, state);
                    }
                }
                EnumConstructionData::Tuple(elements) => {
                    for element in elements {
                        self.check_expr(element, state);
                    }
                }
                EnumConstructionData::Unit => {}
            },
            ExprKind::Fail { value } => {
                self.check_expr(value, state);
                state.diverge();
            }
            ExprKind::FieldAccess { receiver, .. } => self.check_expr(receiver, state),
            // Post-resolve success paths never contain `For` (it
            // desugars before resolve). Walk defensively on the
            // diagnostic path with loop-body semantics.
            ExprKind::For {
                pattern,
                iterable,
                body,
            } => {
                self.check_expr(iterable, state);
                let mut body_state = state.clone();
                self.bind_pattern(pattern, &mut body_state);
                self.check_body(body, &mut body_state);
            }
            ExprKind::Group { expr: inner } => self.check_expr(inner, state),
            ExprKind::Ident { name, resolution } => {
                if let Resolution::Local(id) = resolution {
                    self.check_read(*id, name, expr.span, state);
                }
            }
            ExprKind::If {
                condition,
                then_body,
                else_body,
            } => {
                self.check_expr(condition, state);
                let mut then_state = state.clone();
                self.check_body(then_body, &mut then_state);
                if let Some(else_body) = else_body {
                    let mut else_state = state.clone();
                    self.check_body(else_body, &mut else_state);
                    state.join_arms(vec![then_state, else_state]);
                }
                // Without an else the fall-through path assigns
                // nothing, so the pre-state already is the join.
            }
            ExprKind::List { elements } | ExprKind::Tuple { elements } => {
                for element in elements {
                    self.check_expr(element, state);
                }
            }
            ExprKind::Literal { .. } | ExprKind::Self_ { .. } => {}
            ExprKind::Loop { body } => {
                // A `loop` body may be cut short by `break` at any
                // point, so nothing it assigns survives the loop.
                // A break-less loop types as `Never` and diverges
                // via the statement-level check.
                let mut body_state = state.clone();
                self.check_body(body, &mut body_state);
            }
            ExprKind::Map { entries } => {
                for (key, value) in entries {
                    self.check_expr(key, state);
                    self.check_expr(value, state);
                }
            }
            ExprKind::Match { subject, arms } => {
                self.check_expr(subject, state);
                let arm_states = self.check_match_arms(arms, state);
                state.join_arms(arm_states);
            }
            ExprKind::MethodCall { receiver, args, .. } => {
                self.check_expr(receiver, state);
                for arg in args {
                    self.check_expr(&arg.value, state);
                }
            }
            ExprKind::Receive {
                arms,
                after_timeout,
                after_body,
            } => {
                if let Some(timeout) = after_timeout.as_ref() {
                    self.check_expr(timeout, state);
                }
                let mut arm_states = self.check_match_arms(arms, state);
                // Without an `after` clause, receive blocks until an
                // arm matches, so the arms alone cover every path.
                if after_timeout.is_some() {
                    let mut after_state = state.clone();
                    self.check_body(after_body, &mut after_state);
                    arm_states.push(after_state);
                }
                state.join_arms(arm_states);
            }
            ExprKind::Rescue {
                subject, handler, ..
            } => {
                self.check_expr(subject, state);
                // The handler runs only on the error path, so its
                // assignments do not survive the expression.
                let mut handler_state = state.clone();
                self.check_expr(handler, &mut handler_state);
            }
            ExprKind::ShortClosure { params, body } => {
                let mut inner = self.closure_entry_state(params, state);
                self.check_expr(body, &mut inner);
            }
            ExprKind::Spawn { expr: inner } => self.check_expr(inner, state),
            ExprKind::String { parts, .. } => {
                for part in parts {
                    if let StringPart::Interpolation { expr: inner, .. } = part {
                        self.check_expr(inner, state);
                    }
                }
            }
            ExprKind::StructConstruction { fields, .. } => {
                for field in fields {
                    self.check_expr(&field.value, state);
                }
            }
            ExprKind::Ternary {
                condition,
                then_expr,
                else_expr,
            } => {
                self.check_expr(condition, state);
                let mut then_state = state.clone();
                self.check_expr(then_expr, &mut then_state);
                self.diverge_on_never(&then_expr.resolution, &mut then_state);
                let mut else_state = state.clone();
                self.check_expr(else_expr, &mut else_state);
                self.diverge_on_never(&else_expr.resolution, &mut else_state);
                state.join_arms(vec![then_state, else_state]);
            }
            ExprKind::Try { expr: inner } => self.check_expr(inner, state),
            ExprKind::Unary { operand, .. } => self.check_expr(operand, state),
            ExprKind::Unless { condition, body } | ExprKind::While { condition, body } => {
                // `unless` has no else arm and a `while` body may run
                // zero times: neither contributes past itself.
                self.check_expr(condition, state);
                let mut body_state = state.clone();
                self.check_body(body, &mut body_state);
            }
        }
    }

    /// Walk each arm under the pre-arm state extended with its
    /// pattern bindings. Shared by `match` and `receive`.
    fn check_match_arms(&mut self, arms: &[MatchArm], state: &FlowState) -> Vec<FlowState> {
        arms.iter()
            .map(|arm| {
                let mut arm_state = state.clone();
                self.bind_pattern(&arm.pattern, &mut arm_state);
                if let Some(guard) = arm.guard.as_ref() {
                    self.check_expr(guard, &mut arm_state);
                }
                self.check_body(&arm.body, &mut arm_state);
                arm_state
            })
            .collect()
    }

    /// Clone the creation-site state and seed the closure's own
    /// params as assigned.
    fn closure_entry_state(&mut self, params: &[ClosureParam], state: &FlowState) -> FlowState {
        let mut inner = state.clone();
        for param in params {
            let (ClosureParam::Name { local_id, .. } | ClosureParam::Wildcard { local_id, .. }) =
                param;
            if let Some(id) = local_id {
                inner.assign(*id);
            }
        }
        inner
    }

    /// Mark every binding the pattern introduces as assigned. Binary
    /// patterns also read their size expressions, in segment order,
    /// so `<<len::8, payload::size(len)>>` sees `len` bound first.
    fn bind_pattern(&mut self, pattern: &Pattern, state: &mut FlowState) {
        match pattern {
            Pattern::Binary { segments, .. } => {
                for segment in segments {
                    if let Some(size) = segment.size.as_ref() {
                        self.check_expr(size, state);
                    }
                    if let ExprKind::Ident {
                        resolution: Resolution::Local(id),
                        ..
                    } = &segment.value.kind
                    {
                        state.assign(*id);
                    }
                }
            }
            Pattern::Binding { local_id, .. } | Pattern::TypedBinding { local_id, .. } => {
                if let Some(id) = local_id {
                    state.assign(*id);
                }
            }
            Pattern::Constructor { elements, .. }
            | Pattern::EnumTuple { elements, .. }
            | Pattern::List { elements, .. }
            | Pattern::Tuple { elements, .. } => {
                for element in elements {
                    self.bind_pattern(element, state);
                }
            }
            Pattern::EnumStruct { fields, .. } | Pattern::Struct { fields, .. } => {
                for field in fields {
                    self.bind_pattern(&field.pattern, state);
                }
            }
            Pattern::EnumUnit { .. } | Pattern::Literal { .. } | Pattern::Wildcard { .. } => {}
            Pattern::Or { patterns, .. } => {
                for pattern in patterns {
                    self.bind_pattern(pattern, state);
                }
            }
        }
    }

    /// A field write or compound assignment reads the target's head
    /// local before storing through it.
    fn check_lvalue_head(&mut self, target: &LValue, state: &FlowState) {
        if let Some(id) = target.local_id {
            self.check_read(id, &target.segments[0], target.span, state);
        }
    }

    fn check_read(&mut self, id: LocalId, name: &str, span: Span, state: &FlowState) {
        if state.is_assigned(id) {
            return;
        }
        self.diagnostics.push(Diagnostic::error_with_hint(
            format!("`{name}` does not have a value on every path to this read"),
            format!(
                "assign `{name}` a value before the branch or loop, or restructure so the \
                 branch produces the value (`{name} = if ... else ... end`)",
            ),
            span,
        ));
    }

    fn diverge_on_never(&mut self, ty: &ResolvedType, state: &mut FlowState) {
        if is_primitive(ty, self.registry, "Never") {
            state.diverge();
        }
    }
}
