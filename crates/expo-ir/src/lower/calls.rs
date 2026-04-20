//! Lowering for bare-name function calls.
//!
//! Decides which `ResolvedCall` variant a call expression resolves to:
//! struct constructor, builtin (`panic`/`print`), direct call to a
//! defined symbol, indirect call through a closure-typed variable, or
//! generic that needs monomorphization. Mangled-symbol selection
//! (package-qualifying user methods, leaving stdlib symbols bare) and
//! signature lookup happen here. The four `impl Fn(...)` parameters
//! are the seam where the LLVM-bound caches in `expo-codegen`
//! (`functions`, `fn_state.variables`, `llvm_types`, `generic_fn_asts`)
//! are queried without coupling `expo-ir` to a backend; emission uses
//! the chosen mangled name (and the variable name from the call site)
//! to fetch the actual `FunctionValue`/`PointerValue` post-dispatch.

use expo_ast::identifier::TypeIdentifier;
use expo_typecheck::types::{Type, unwrap_indirect};

use crate::identity::FunctionIdentifier;
use crate::lower::ctx::LowerCtx;
use crate::lower::naming::current_method_symbol_prefix;
use crate::lower::types::resolve_name_current;
use crate::resolved::calls::{BuiltinCall, ResolvedCall};

/// Resolves a bare-name function call to a [`ResolvedCall`]. The four
/// closures bridge to the LLVM-bound caches that live on the codegen
/// `Compiler` (function symbols, local variables, type cache, generic
/// AST cache); each is consulted at most twice.
pub fn resolve_call(
    ctx: &LowerCtx<'_>,
    name: &str,
    is_struct_constructor: impl Fn(Option<&TypeIdentifier>, &str) -> bool,
    function_exists: impl Fn(&FunctionIdentifier) -> bool,
    variable_type: impl Fn(&str) -> Option<Type>,
    is_generic_function: impl Fn(&str) -> bool,
) -> Result<ResolvedCall, String> {
    let resolved_id = resolve_name_current(ctx, name).cloned();

    if is_struct_constructor(resolved_id.as_ref(), name) {
        return Ok(ResolvedCall::StructConstructor {
            identifier: resolved_id,
        });
    }

    match name {
        "panic" => return Ok(ResolvedCall::Builtin(BuiltinCall::Panic)),
        "print" | "print_Bool" | "print_Float" | "print_Int" | "print_Int32" | "print_String" => {
            return Ok(ResolvedCall::Builtin(BuiltinCall::Print));
        }
        _ => {}
    }

    // When we're inside a method body, the unqualified call `foo(..)` can also
    // refer to another method on the same type. Build the candidate LLVM symbol
    // using the same package-qualifying rule as definition-site mangling so the
    // lookup succeeds for user packages (e.g. `crypto.HMAC_hmac_raw`) without
    // breaking stdlib symbols (e.g. `Int_hash`).
    let mangled_candidate = ctx.fn_lower.self_type_name.as_ref().map(|type_name| {
        let prefix = current_method_symbol_prefix(ctx, type_name);
        format!("{prefix}_{name}")
    });

    let chosen_mangled: Option<FunctionIdentifier> =
        if function_exists(&FunctionIdentifier::new(name)) {
            Some(FunctionIdentifier::new(name))
        } else {
            mangled_candidate
                .as_ref()
                .map(FunctionIdentifier::new)
                .filter(|candidate| function_exists(candidate))
        };

    if let Some(mangled_name) = chosen_mangled {
        let signature = ctx.type_ctx.function_sig(name).or_else(|| {
            ctx.fn_lower
                .self_type_name
                .as_ref()
                .and_then(|type_name| resolve_name_current(ctx, type_name))
                .and_then(|id| ctx.type_ctx.get_type(id))
                .and_then(|type_info| type_info.functions.get(name))
        });
        let param_types: Vec<Type> = signature
            .map(|sig| sig.params.iter().map(|param| param.ty.clone()).collect())
            .unwrap_or_default();
        let return_type = signature
            .map(|sig| sig.return_type.clone())
            .unwrap_or(Type::Unknown);
        return Ok(ResolvedCall::Direct {
            mangled_name,
            param_types,
            return_type,
        });
    }

    if let Some(raw_type) = variable_type(name) {
        let inner = unwrap_indirect(&raw_type);
        let Type::Function {
            params,
            return_type,
        } = inner.clone()
        else {
            return Err(format!("undefined function: {name}"));
        };
        return Ok(ResolvedCall::ClosureVariable {
            params,
            return_type: *return_type,
        });
    }

    if is_generic_function(name) {
        return Ok(ResolvedCall::Generic);
    }

    Err(format!("undefined function: {name}"))
}
