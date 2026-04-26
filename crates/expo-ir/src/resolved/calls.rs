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

/// Outcome of resolving a method call (`receiver.method(args)`). Carries
/// only LLVM-free metadata; the caller (`expo-codegen`) drains
/// `pending_mono` against the existing monomorphization shim and looks
/// up the LLVM `FunctionValue` post-emit.
pub struct ResolvedMethodCall {
    /// Mangled callee symbol the caller will look up in its own
    /// `Compiler.functions` map.
    pub mangled_name: FunctionIdentifier,
    /// Resolved parameter types (excluding the implicit `self`
    /// receiver, which the caller passes in directly).
    pub param_types: Vec<Type>,
    /// Resolved return type.
    pub return_type: Type,
    /// `true` if the method takes its receiver by move (the caller
    /// adjusts the receiver's ownership tracking accordingly).
    pub is_move: bool,
    /// `Some(_)` when the call is on a generic receiver and may need
    /// monomorphization. The caller checks the LLVM cache and, if
    /// missing, calls `monomorphize_impl_method` (which handles stdlib
    /// intrinsic dispatch and IR-program planning + LLVM emission).
    pub pending_mono: Option<PendingMethodMono>,
}

/// Outcome of resolving a static method call (`Type.method(args)`,
/// e.g. `List.new()` or `Task.async(f)`). Like [`ResolvedMethodCall`]
/// but additionally carries any pending type monomorphization for
/// generic static calls (e.g. `List<Int>.new()` requires
/// `List<Int>` to be monomorphized first).
pub struct ResolvedStaticCall {
    /// Mangled callee symbol the caller will look up in its own
    /// `Compiler.functions` map.
    pub mangled_name: FunctionIdentifier,
    /// Resolved parameter types.
    pub param_types: Vec<Type>,
    /// Resolved return type.
    pub return_type: Type,
    /// `Some(_)` when the receiver type is generic and not yet
    /// monomorphized. The caller dispatches to `monomorphize_struct` or
    /// `monomorphize_enum` based on `is_enum`.
    pub pending_type_mono: Option<PendingTypeMono>,
    /// `Some(_)` when the static method itself needs monomorphization
    /// (the function symbol is missing from the LLVM cache).
    pub pending_mono: Option<PendingMethodMono>,
}

/// Pending monomorphization request for an impl method (instance or
/// static). Mirrors the parameter list of `monomorphize_impl_method`.
pub struct PendingMethodMono {
    pub base_type: String,
    pub method: String,
    pub type_args: Vec<Type>,
    pub method_type_args: Vec<Type>,
}

/// Pending monomorphization request for a generic struct or enum.
pub struct PendingTypeMono {
    pub identifier: TypeIdentifier,
    pub type_args: Vec<Type>,
    /// `true` if the type is an enum (the caller uses
    /// `monomorphize_enum`); `false` for structs (`monomorphize_struct`).
    pub is_enum: bool,
}
