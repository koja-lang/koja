//! Mangling for monomorphized generic instantiations.
//!
//! Single helper, single shape. `Pair<Int, String>` mangles to
//! `TestApp.Pair_$Int.TestApp.String$`. Mirrors v1's mangling so
//! cross-tool linker output stays comparable. The `_$..$` brackets
//! delimit the type-args block; nested generic args bring their own
//! `_$..$` so depth-counting parses unambiguously.

use crate::function::IRSymbol;
use crate::types::IRType;

/// Mangle `(symbol, args)` into a fresh `IRSymbol` rooted at the
/// same identifier as `symbol`. Empty `args` returns `symbol`
/// unchanged so non-generic callers can route through this helper
/// without branching.
pub(crate) fn mangled_type_name(symbol: &IRSymbol, args: &[IRType]) -> IRSymbol {
    if args.is_empty() {
        return symbol.clone();
    }
    let rendered: Vec<String> = args.iter().map(mangle_type).collect();
    symbol.derived(&format!("_${}$", rendered.join(".")))
}

/// Mangle a generic function's identifier with its inferred
/// type-args. Same shape as [`mangled_type_name`] — the `_$..$`
/// suffix attaches directly to the function symbol so call sites
/// and monomorphization agree on the symbol form.
pub(crate) fn mangled_function_name(symbol: &IRSymbol, args: &[IRType]) -> IRSymbol {
    mangled_type_name(symbol, args)
}

fn mangle_type(ty: &IRType) -> String {
    match ty {
        IRType::Binary => "Binary".to_string(),
        IRType::Bits => "Bits".to_string(),
        IRType::Bool => "Bool".to_string(),
        IRType::CPtr(inner) => format!("CPtr_${}$", mangle_type(inner)),
        IRType::Enum(symbol) | IRType::Struct(symbol) => symbol.mangled().to_string(),
        IRType::Float32 => "Float32".to_string(),
        IRType::Float64 => "Float64".to_string(),
        IRType::Function { params, ret } => {
            let rendered_params: Vec<String> = params.iter().map(mangle_type).collect();
            format!("Fn_${};{}$", rendered_params.join(","), mangle_type(ret))
        }
        IRType::Int8 => "Int8".to_string(),
        IRType::Int16 => "Int16".to_string(),
        IRType::Int32 => "Int32".to_string(),
        IRType::Int64 => "Int64".to_string(),
        IRType::String => "String".to_string(),
        IRType::UInt8 => "UInt8".to_string(),
        IRType::UInt16 => "UInt16".to_string(),
        IRType::UInt32 => "UInt32".to_string(),
        IRType::UInt64 => "UInt64".to_string(),
        IRType::Unit => "Unit".to_string(),
    }
}
