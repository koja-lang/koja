//! Per-function type-checking environment with ownership tracking.
//!
//! Defines [`CheckEnv`], the environment carried through statement and expression
//! checking, and the [`VarState`]/[`VarInfo`] types that track variable liveness
//! and move state for ownership analysis.

use std::collections::{HashMap, HashSet};

use expo_ast::ast::TypeParam;
use expo_ast::span::Span;

use crate::context::{FunctionKind, TypeContext};
use crate::types::{Type, TypeIdentifier};

/// Ownership state of a local variable during type checking.
#[derive(Debug, Clone)]
pub(crate) enum VarState {
    Live,
    Moved(Span),
    MaybeMoved(Span),
}

/// Type and ownership state for a local variable.
#[derive(Debug, Clone)]
pub(crate) struct VarInfo {
    pub ty: Type,
    pub state: VarState,
}

/// Per-function environment used during type checking, tracking local variable
/// types, ownership states, the expected return type, and loop nesting depth.
pub(crate) struct CheckEnv<'a> {
    pub env: HashMap<String, VarInfo>,
    pub used_vars: HashSet<String>,
    pub loop_depth: usize,
    pub return_type: Type,
    pub kind: FunctionKind,
    pub struct_names: &'a [&'a str],
    pub enum_names: &'a [&'a str],
    /// Expected type from a variable's type annotation, used to resolve
    /// unresolved type parameters in generic static calls like `List.new()`.
    pub type_hint: Option<Type>,
    /// The message type `M` when the current function is a process function
    /// (spawned as `Process<M>`). Used by `receive` to infer its return type.
    pub process_msg_type: Option<Type>,
    /// Type parameters with bounds from the current function, used to resolve
    /// protocol method calls on bounded type variables.
    pub fn_type_params: Vec<TypeParam>,
    /// The enclosing type when checking a function inside a struct/enum.
    /// Used by `infer_call` to resolve bare calls to same-type methods.
    pub enclosing_type: Option<TypeIdentifier>,
    /// Concrete type arguments when checking a method body inside a
    /// specialized impl (e.g. `[Int]` for `impl Foo<Int>`). `None` for
    /// non-impl bodies, plain impls, and generic impls. Used by
    /// `infer_call` and the sig lookup helpers to find sibling functions
    /// stored in `ctx.specialized_methods`.
    pub enclosing_specialization: Option<Vec<Type>>,
}

impl<'a> CheckEnv<'a> {
    /// Creates a child environment inheriting all variables and settings,
    /// but with a fresh `used_vars` set and the given return type.
    pub fn child(&self, return_type: Type) -> CheckEnv<'a> {
        CheckEnv {
            env: self.env.clone(),
            used_vars: HashSet::new(),
            loop_depth: self.loop_depth,
            return_type,
            kind: self.kind,
            struct_names: self.struct_names,
            enum_names: self.enum_names,
            type_hint: None,
            process_msg_type: self.process_msg_type.clone(),
            fn_type_params: self.fn_type_params.clone(),
            enclosing_type: self.enclosing_type.clone(),
            enclosing_specialization: self.enclosing_specialization.clone(),
        }
    }

    /// Inserts a new variable with `Live` ownership state.
    pub fn insert_var(&mut self, name: String, ty: Type) {
        self.env.insert(
            name,
            VarInfo {
                ty,
                state: VarState::Live,
            },
        );
    }

    /// Returns the type of a variable, if it exists.
    pub fn get_type(&self, name: &str) -> Option<&Type> {
        self.env.get(name).map(|v| &v.ty)
    }

    /// Marks a variable as moved at the given span.
    pub fn mark_moved(&mut self, name: &str, span: Span) {
        if let Some(info) = self.env.get_mut(name) {
            info.state = VarState::Moved(span);
        }
    }

    /// Checks that a variable has not been moved or maybe-moved, emitting
    /// a diagnostic if it has. Returns `true` if the variable is still live.
    pub fn check_not_moved(&self, name: &str, use_span: Span, ctx: &mut TypeContext) -> bool {
        if let Some(info) = self.env.get(name) {
            match &info.state {
                VarState::Moved(_) => {
                    ctx.error_with_hint(
                        format!("use of moved value `{}`", name),
                        "value was moved earlier in this scope; consider using clone()".into(),
                        use_span,
                    );
                    return false;
                }
                VarState::MaybeMoved(_) => {
                    ctx.error_with_hint(
                        format!("value `{}` may have been moved", name),
                        "value was moved in one branch but not another; ensure consistent ownership across branches".into(),
                        use_span,
                    );
                    return false;
                }
                VarState::Live => {}
            }
        }
        true
    }

    /// Merges variable ownership states from multiple branches (e.g. if/else).
    ///
    /// If all branches move a variable, it becomes `Moved`. If only some do,
    /// it becomes `MaybeMoved`.
    pub fn merge_branches(&mut self, branches: &[HashMap<String, VarInfo>]) {
        if branches.is_empty() {
            return;
        }
        for (name, info) in &mut self.env {
            let branch_states: Vec<Option<&VarState>> = branches
                .iter()
                .map(|b| b.get(name).map(|v| &v.state))
                .collect();

            if branch_states.iter().all(|s| s.is_none()) {
                continue;
            }

            let any_moved = branch_states
                .iter()
                .any(|s| matches!(s, Some(VarState::Moved(_)) | Some(VarState::MaybeMoved(_))));
            let all_moved = branch_states
                .iter()
                .all(|s| matches!(s, Some(VarState::Moved(_)) | Some(VarState::MaybeMoved(_))));

            let moved_span = branch_states.iter().find_map(|s| match s {
                Some(VarState::Moved(span) | VarState::MaybeMoved(span)) => Some(*span),
                _ => None,
            });

            if let Some(span) = moved_span {
                if all_moved {
                    info.state = VarState::Moved(span);
                } else if any_moved {
                    info.state = VarState::MaybeMoved(span);
                }
            }
        }
    }
}
