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

fn mangle_type(ty: &IRType) -> String {
    match ty {
        IRType::Bool => "Bool".to_string(),
        IRType::Enum(symbol) | IRType::Struct(symbol) => symbol.mangled().to_string(),
        IRType::Float32 => "Float32".to_string(),
        IRType::Float64 => "Float64".to_string(),
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
