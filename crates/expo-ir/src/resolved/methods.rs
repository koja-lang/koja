//! Resolved method signature for generic impl methods.

use std::collections::HashMap;

use expo_ast::ast::Function;
use expo_ast::types::Type;

use crate::identity::{FunctionIdentifier, MonomorphizedTypeIdentifier};

/// Fully resolved method signature: AST, types, substitutions, and self-type.
/// Produced by `resolve_method_signature` without any backend emission.
pub struct ResolvedMethodSignature {
    /// The AST node for the method body (specialized or generic).
    pub func_ast: Function,
    /// Whether this is a static method (no `self` parameter).
    pub is_static: bool,
    /// The mangled function name (e.g. `"List_$Int32$_push"`).
    pub mangled_fn: FunctionIdentifier,
    /// The mangled type name (e.g. `"List_$Int32$"`).
    pub mangled_type: MonomorphizedTypeIdentifier,
    /// The resolved types of each parameter, in declaration order.
    pub param_types: Vec<Type>,
    /// The resolved return type.
    pub return_type: Type,
    /// The resolved self type, if this is an instance method.
    pub self_type: Option<Type>,
    /// The type parameter substitutions applied during monomorphization.
    pub subst: HashMap<String, Type>,
}
