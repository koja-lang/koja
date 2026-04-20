//! Resolved metadata for the call-expression lowering path.
//!
//! Lowering (in [`crate::lower::calls`]) decides what kind of call a
//! bare-name invocation actually is: a builtin, a struct constructor, a
//! direct call to a defined function symbol, an indirect call through
//! a closure-typed variable, or a generic that needs monomorphization.
//! The decision shape is LLVM-free: `Direct` carries the chosen mangled
//! symbol so the caller does exactly one `FunctionValue` lookup post-
//! dispatch; `ClosureVariable` carries only the resolved signature so
//! the caller fetches its own `PointerValue` from the LLVM-bound
//! variables map.

use expo_ast::identifier::TypeIdentifier;
use expo_typecheck::context::FnParam;
use expo_typecheck::types::Type;

use crate::identity::FunctionIdentifier;

/// Builtin call kinds that lowering distinguishes by name (`panic`,
/// `print`, `print_<Primitive>`).
pub enum BuiltinCall {
    Panic,
    Print,
}

/// Outcome of resolving a bare-name function call. Each variant carries
/// only pure-semantic data; LLVM handles (`FunctionValue<'ctx>`,
/// `PointerValue<'ctx>`) are looked up by the caller after dispatch.
pub enum ResolvedCall {
    /// Compiler builtin (`panic`, `print`).
    Builtin(BuiltinCall),
    /// Indirect call through a closure-typed local variable. The caller
    /// re-fetches the variable's `PointerValue` from its own variables
    /// map after dispatch.
    ClosureVariable {
        /// Closure parameter list (the signature lowering inferred from
        /// the variable's `Type::Function`).
        params: Vec<FnParam>,
        /// Closure return type.
        return_type: Type,
    },
    /// Direct call to a defined function symbol. The caller looks up
    /// the `FunctionValue` once via `mangled_name`.
    Direct {
        /// LLVM symbol name lowering chose for the callee (either the
        /// bare `name` or a method-prefix-qualified candidate).
        mangled_name: FunctionIdentifier,
        /// Parameter types from the function signature, in declaration
        /// order.
        param_types: Vec<Type>,
        /// Return type from the function signature.
        return_type: Type,
    },
    /// Generic function -- monomorphization happens later in
    /// `compile_generic_call` after argument types are known.
    Generic,
    /// The bare name refers to a struct/type constructor; the caller
    /// dispatches to `compile_struct_construction`.
    StructConstructor {
        /// Resolved type identifier when name resolution found one.
        identifier: Option<TypeIdentifier>,
    },
}
