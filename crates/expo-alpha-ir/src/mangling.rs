//! Mangling for monomorphized generic instantiations.
//!
//! Three helpers, one shape. `Pair<Int, String>` mangles to
//! `TestApp.Pair_$Int.TestApp.String$`. A method on that struct
//! mangles to `TestApp.Pair_$Int.TestApp.String$.first`; if the
//! method itself takes type params (`fn map<U>`) the args attach
//! to the method segment as `…$.map_$<U-args>$`. Mirrors v1's
//! mangling so cross-tool linker output stays comparable. The
//! `_$..$` brackets delimit each type-args block; nested generic
//! args bring their own `_$..$` so depth-counting parses
//! unambiguously.
//!
//! Call sites pass typed data ([`IRSymbol`], `[IRType]`, `&str`) —
//! the helpers below own all string concatenation so `IRSymbol`
//! stays opaque outside this module.

use expo_ast::identifier::Identifier;

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
    symbol.derived(&render_type_args(args))
}

/// Mangle a generic function's identifier with its inferred
/// type-args. Same shape as [`mangled_type_name`] — the `_$..$`
/// suffix attaches directly to the function symbol so call sites
/// and monomorphization agree on the symbol form.
pub(crate) fn mangled_function_name(symbol: &IRSymbol, args: &[IRType]) -> IRSymbol {
    mangled_type_name(symbol, args)
}

/// Mangle a method on a generic struct or enum, optionally with the
/// method's own type-args. `struct_template` is the receiver type's
/// symbol root; `receiver_args` are the receiver's instantiation
/// (empty for non-generic receivers); `method_name` is the bare
/// method identifier; `method_args` are the method's own type-args
/// (empty for struct-level-only methods).
///
/// Both call-site lowering and monomorphization route through this
/// single helper so the symbols agree by construction. With empty
/// `receiver_args` *and* empty `method_args` the result is just
/// `struct_template.derived(".{method_name}")`.
pub fn mangled_method_name(
    struct_template: &IRSymbol,
    receiver_args: &[IRType],
    method_name: &str,
    method_args: &[IRType],
) -> IRSymbol {
    let receiver = mangled_type_name(struct_template, receiver_args);
    let suffix = if method_args.is_empty() {
        format!(".{method_name}")
    } else {
        format!(".{method_name}{}", render_type_args(method_args))
    };
    receiver.derived(&suffix)
}

/// Mint the `IRSymbol` rooted at `Global.<receiver>` for a stdlib
/// primitive receiver like `Bool`, `Int`, `String`. Stamped to the
/// same shape the lift pass produces for the corresponding `impl
/// <Receiver> { @intrinsic fn <method> }` decl in the `Global`
/// package, so cross-crate callers (LLVM/eval intrinsic emitters)
/// can look up `<Type>.hash` / `<Type>.eq` by `IRSymbol` without
/// reaching into [`IRSymbol`]'s private constructors.
pub fn global_primitive_symbol(receiver: &str) -> IRSymbol {
    IRSymbol::from_identifier(&Identifier::new("Global", vec![receiver.to_string()]))
}

fn render_type_args(args: &[IRType]) -> String {
    let rendered: Vec<String> = args.iter().map(mangle_type).collect();
    format!("_${}$", rendered.join("."))
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
        IRType::List(inner) => format!("List_${}$", mangle_type(inner)),
        IRType::Map { key, value } => {
            format!("Map_${}.{}$", mangle_type(key), mangle_type(value))
        }
        IRType::Set(inner) => format!("Set_${}$", mangle_type(inner)),
        IRType::String => "String".to_string(),
        IRType::UInt8 => "UInt8".to_string(),
        IRType::UInt16 => "UInt16".to_string(),
        IRType::UInt32 => "UInt32".to_string(),
        IRType::UInt64 => "UInt64".to_string(),
        IRType::Unit => "Unit".to_string(),
    }
}
