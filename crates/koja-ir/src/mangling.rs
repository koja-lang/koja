//! Mangling for monomorphized generic instantiations.
//!
//! Three helpers, one shape. `Pair<Int, String>` mangles to
//! `TestApp.Pair_$Int.TestApp.String$`. A method on that struct
//! mangles to `TestApp.Pair_$Int.TestApp.String$.first`. If the
//! method itself takes type params (`fn map<U>`) the args attach
//! to the method segment as `…$.map_$<U-args>$`. Mirrors v1's
//! mangling so cross-tool linker output stays comparable. The
//! `_$..$` brackets delimit each type-args block, and nested generic
//! args bring their own `_$..$` so depth-counting parses
//! unambiguously.
//!
//! Call sites pass typed data ([`IRSymbol`], `[IRType]`, `&str`).
//! The helpers below own all string concatenation so `IRSymbol`
//! stays opaque outside this module.

use koja_ast::identifier::Identifier;

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
/// type-args. Same shape as [`mangled_type_name`]. The `_$..$`
/// suffix attaches directly to the function symbol so call sites
/// and monomorphization agree on the symbol form.
pub(crate) fn mangled_function_name(symbol: &IRSymbol, args: &[IRType]) -> IRSymbol {
    mangled_type_name(symbol, args)
}

/// Mangle a method on a generic struct or enum, optionally with the
/// method's own type-args. `struct_template` is the receiver type's
/// symbol root, `receiver_args` are the receiver's instantiation
/// (empty for non-generic receivers), `method_name` is the bare
/// method identifier, and `method_args` are the method's own type-args
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

/// Mint the `IRSymbol` rooted at `Global.<path>` for a stdlib type a
/// cross-crate caller knows by name: a primitive receiver like
/// `["Bool"]` / `["String"]`, or a `Global` nested type like
/// `["Process", "CallError"]`. Stamped to the same shape the lift
/// pass produces for the corresponding decl in the `Global` package,
/// so LLVM/eval intrinsic emitters can look symbols up without
/// reaching into [`IRSymbol`]'s private constructors.
pub fn global_primitive_symbol(path: &[&str]) -> IRSymbol {
    IRSymbol::from_identifier(&Identifier::new(
        "Global",
        path.iter().map(|segment| segment.to_string()).collect(),
    ))
}

/// Mint the [`IRSymbol`] of the `Debug.format` method on a struct
/// or enum receiver carrying the (possibly-mangled) `receiver`
/// symbol. Auto-print code paths drive off the live runtime value
/// (whose `IRSymbol` is already mangled to its concrete
/// monomorphization, e.g. `Global.Result_$Int64.String$`) so they
/// bypass receiver-template reconstruction. The resulting symbol
/// matches the same one [`super::lower::calls::lower_method_call`]
/// would emit for a user-side `value.format()` call.
pub fn debug_format_for_symbol(receiver: &IRSymbol) -> IRSymbol {
    receiver.derived(".format")
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
        IRType::Enum(symbol) => symbol.mangled().to_string(),
        IRType::Float32 => "Float32".to_string(),
        IRType::Float64 => "Float64".to_string(),
        IRType::Function { params, ret, .. } => {
            let rendered_params: Vec<String> = params.iter().map(mangle_type).collect();
            format!("Fn_${};{}$", rendered_params.join(","), mangle_type(ret))
        }
        IRType::Indirect(inner) => format!("Indirect_${}$", mangle_type(inner)),
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
        IRType::Struct(symbol) => symbol.mangled().to_string(),
        IRType::Tuple(elements) => {
            let rendered: Vec<String> = elements.iter().map(mangle_type).collect();
            format!("Tuple_${}$", rendered.join(","))
        }
        IRType::UInt8 => "UInt8".to_string(),
        IRType::UInt16 => "UInt16".to_string(),
        IRType::UInt32 => "UInt32".to_string(),
        IRType::UInt64 => "UInt64".to_string(),
        IRType::Union { mangled, .. } => mangled.mangled().to_string(),
        IRType::Unit => "Unit".to_string(),
    }
}

/// Build the canonical mangled [`IRSymbol`] for a union with the
/// given (already mangled) member set. The `members` slice is
/// expected in canonical (sorted) order (typecheck's
/// `canonical_union` guarantees that), so any two surface unions
/// with the same canonical member set yield the exact same
/// `IRSymbol`. Backends look up `IRUnionDecl` entries by this
/// symbol via [`crate::IRProgram::union_decl`].
///
/// Mirrors [`mangled_type_name`] / [`mangled_method_name`] in
/// returning an `IRSymbol` directly so call sites never have to
/// hand-wrap the underlying `String`.
pub(crate) fn union_mangle(members: &[IRType]) -> IRSymbol {
    let parts: Vec<String> = members.iter().map(mangle_type).collect();
    IRSymbol::synthetic(format!("Union_{}", parts.join("_or_")))
}

/// Symbol of the synthesized clone glue for `ty` (`<type>.$clone$`).
/// Hung off the type's own symbol exactly like [`mangled_method_name`]
/// hangs a method off its receiver. Glue is a synthesized method on
/// the type, so a struct in `TestApp` gets `TestApp.Point.$clone$` and
/// stays rooted in its own package. The `$`-fenced suffix can never
/// collide with a user-defined `fn clone` (which mangles to the bare
/// `<type>.clone`). Acquisition dispatches to this symbol by type,
/// never by method-name lookup. Both the [`crate::elaborate`] pass
/// (which registers the glue) and the LLVM backend (which emits the
/// `call`s) mint through this single helper so they agree by
/// construction.
pub fn clone_glue_symbol(ty: &IRType) -> IRSymbol {
    glue_base(ty).derived(".$clone$")
}

/// Symbol of the synthesized deep-copy glue for `ty`
/// (`<type>.$deep_copy$`). Process-boundary analog of
/// [`clone_glue_symbol`], with the same rooting and collision-free
/// guarantee.
pub fn deep_copy_glue_symbol(ty: &IRType) -> IRSymbol {
    glue_base(ty).derived(".$deep_copy$")
}

/// Symbol of the synthesized drop glue for `ty` (`<type>.$drop$`).
/// Drop analog of [`clone_glue_symbol`], with the same rooting and
/// collision-free guarantee.
pub fn drop_glue_symbol(ty: &IRType) -> IRSymbol {
    glue_base(ty).derived(".$drop$")
}

/// Symbol of the synthesized *by-pointer* envelope-payload drop shim
/// for `ty` (`<type>.$envdrop$`). The runtime's type-erased discard
/// path (`koja-runtime-posix/src/wire.rs`) frees an undelivered message by
/// calling a `void(ptr)` function over the payload bytes, an ABI the
/// by-value [`drop_glue_symbol`] can't satisfy. The LLVM backend
/// synthesizes this thin shim per sent message / reply type. It loads
/// the payload through the pointer and routes into the by-value
/// `drop_T`. Same `$`-fenced collision-free rooting as the other glue.
pub fn envelope_drop_glue_symbol(ty: &IRType) -> IRSymbol {
    glue_base(ty).derived(".$envdrop$")
}

/// Symbol of the synthesized capture-release glue for a closure body
/// (`<body>.$drop_env$`). Hung off the closure body's own symbol, so
/// it stays in the body's package and is collision-free against any
/// user method (the `$`-fenced suffix can't appear in a surface
/// name). Both `crate::lower::closures` (which mints the
/// `FunctionKind::DropClosureGlue` body) and the LLVM backend
/// (which takes its address at `MakeClosure`) derive through this
/// single helper so they agree by construction.
pub fn closure_drop_env_symbol(body: &IRSymbol) -> IRSymbol {
    body.derived(".$drop_env$")
}

/// Symbol of the synthesized env deep-copy glue for a closure body
/// (`<body>.$copy_env$`). Copy analog of [`closure_drop_env_symbol`],
/// with the same rooting and collision-free guarantee. Minted by
/// `crate::lower::closures` (which registers the
/// `FunctionKind::CopyClosureGlue` shell) and resolved by the LLVM
/// backend at `MakeClosure` (which stamps its address into the env
/// header's `copy_fn` word).
pub fn closure_copy_env_symbol(body: &IRSymbol) -> IRSymbol {
    body.derived(".$copy_env$")
}

/// The symbol the per-type glue hangs off. Named types (struct /
/// enum / union) carry their own package-qualified [`IRSymbol`], so
/// the glue stays in their package. The structural primitive
/// collections (`List` / `Map` / `Set` / `Indirect`) have no owning
/// decl, so they get a synthetic root mangled from their shape,
/// package-less, the same treatment [`union_mangle`] gives unions.
fn glue_base(ty: &IRType) -> IRSymbol {
    match ty {
        IRType::Enum(symbol) | IRType::Struct(symbol) => symbol.clone(),
        IRType::Union { mangled, .. } => mangled.clone(),
        other => IRSymbol::synthetic(mangle_type(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn symbol(name: &str) -> IRSymbol {
        IRSymbol::from_identifier(&Identifier::new("Test", vec![name.to_string()]))
    }

    #[test]
    fn glue_symbol_cannot_collide_with_surface_method() {
        let ty = IRType::Struct(symbol("Thing"));
        assert_eq!(clone_glue_symbol(&ty).mangled(), "Test.Thing.$clone$");
        assert_ne!(
            clone_glue_symbol(&ty),
            mangled_method_name(&symbol("Thing"), &[], "clone", &[]),
        );
    }

    #[test]
    fn method_mangling_keeps_receiver_and_method_arguments_distinct() {
        let mangled = mangled_method_name(
            &symbol("Box"),
            &[IRType::String],
            "map",
            &[IRType::List(Box::new(IRType::Int64))],
        );
        assert_eq!(mangled.mangled(), "Test.Box_$String$.map_$List_$Int64$$");
    }

    #[test]
    fn nested_type_arguments_preserve_boundaries() {
        let mangled = mangled_type_name(
            &symbol("Pair"),
            &[
                IRType::List(Box::new(IRType::Struct(symbol("Item")))),
                IRType::Map {
                    key: Box::new(IRType::String),
                    value: Box::new(IRType::Int64),
                },
            ],
        );
        assert_eq!(
            mangled.mangled(),
            "Test.Pair_$List_$Test.Item$.Map_$String.Int64$$"
        );
    }
}
