//! Resolved closure metadata, determined before any backend emission.

use expo_ast::types::Type;

/// A closure's resolved metadata: captures, name, parameter types, and return
/// type. Produced by analyzing the closure AST and type-checker info.
pub struct ResolvedClosure {
    /// Names of variables captured from the enclosing scope.
    pub capture_names: Vec<String>,
    /// The generated internal name for this closure (e.g. `"__closure_0"`).
    pub closure_name: String,
    /// The resolved types of each closure parameter, in order.
    pub parameter_types: Vec<Type>,
    /// The closure's return type.
    pub return_type: Type,
}
