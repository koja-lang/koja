//! Type registration: translates Expo type-checked structs, enums, and unions
//! into LLVM struct types using a multi-pass approach so cross-referencing
//! types resolve correctly.

use expo_ir::identity::VariantId;
use expo_typecheck::context::VariantData;
use expo_typecheck::types::{Type, mangle_type};
use inkwell::types::StructType;

use crate::compiler::{Compiler, llvm_field_byte_size};
use crate::generics::ensure_types_exist;
use crate::types::to_llvm_type;

/// Translates Expo type-checked structs and enums into LLVM struct types.
/// Uses a multi-pass approach (opaque types first, then bodies) so
/// cross-referencing types resolve correctly.
pub(crate) fn register_types(c: &mut Compiler) {
    // Pass 1: create opaque types so cross-references resolve
    for (id, info) in c.type_ctx.types.iter().filter(|(_, ti)| ti.is_struct()) {
        if !info.type_params.is_empty() {
            continue;
        }
        let st = c.context.opaque_struct_type(&id.qualified_name());
        c.llvm_types.register_concrete(id, st);
    }
    for (id, info) in c.type_ctx.types.iter().filter(|(_, ti)| ti.is_enum()) {
        if !info.type_params.is_empty() {
            continue;
        }
        let et = c.context.opaque_struct_type(&id.qualified_name());
        c.llvm_types.register_concrete(id, et);
    }

    // Pass 1b: ensure all field/variant types exist (triggers monomorphization
    // of generic instances like List<Token> before struct bodies are set).
    // Indirect-wrapped types are skipped: they compile to pointers, so their
    // inner generic instances can be monomorphized lazily (after struct bodies
    // are set and sizes are known).
    let field_types: Vec<Type> = c
        .type_ctx
        .types
        .iter()
        .filter(|(_, ti)| ti.is_struct() && ti.type_params.is_empty())
        .flat_map(|(_, info)| info.fields().unwrap().iter().map(|(_, ty)| ty.clone()))
        .filter(|ty| !matches!(ty, Type::Indirect(_)))
        .collect();
    for ty in &field_types {
        let _ = ensure_types_exist(c, ty);
    }

    let variant_types: Vec<Type> = c
        .type_ctx
        .types
        .iter()
        .filter(|(_, ti)| ti.is_enum() && ti.type_params.is_empty())
        .flat_map(|(_, info)| {
            info.variants().unwrap().iter().flat_map(|v| match &v.data {
                VariantData::Tuple(types) => types.clone(),
                VariantData::Struct(fields) => fields.iter().map(|(_, ty)| ty.clone()).collect(),
                VariantData::Unit => Vec::new(),
            })
        })
        .filter(|ty| !matches!(ty, Type::Indirect(_)))
        .collect();
    for ty in &variant_types {
        let _ = ensure_types_exist(c, ty);
    }

    // Pass 2: set struct bodies (skip generic templates)
    for (id, info) in c.type_ctx.types.iter().filter(|(_, ti)| ti.is_struct()) {
        if !info.type_params.is_empty() {
            continue;
        }
        let struct_type = c.llvm_types.get_concrete(id).unwrap();
        let field_types: Vec<_> = info
            .fields()
            .unwrap()
            .iter()
            .filter_map(|(_, ty)| to_llvm_type(ty, c.context, &c.llvm_types))
            .collect();
        struct_type.set_body(&field_types, false);

        // Also publish the field layout into `TypeLayouts` under the
        // package-qualified name so `Compiler::get_field_index/type` lookups
        // can be package-scoped without falling back to the bare-name
        // `TypeContext::find_type` path.
        let fields_owned: Vec<(String, Type)> = info
            .fields()
            .unwrap()
            .iter()
            .map(|(n, t)| (n.clone(), t.clone()))
            .collect();
        c.layouts
            .register_struct_layout(id.qualified_name(), fields_owned);
    }

    // Pass 3: set enum bodies (skip generic templates)
    for (id, info) in c.type_ctx.types.iter().filter(|(_, ti)| ti.is_enum()) {
        if !info.type_params.is_empty() {
            continue;
        }
        let enum_type = c.llvm_types.get_concrete(id).unwrap();
        let variants: Vec<_> = info
            .variants()
            .unwrap()
            .iter()
            .map(|v| (v.name.clone(), v.data.clone()))
            .collect();
        build_enum_layout(c, &id.qualified_name(), enum_type, &variants);
        c.layouts
            .register_enum_variants(id.qualified_name(), variants);
    }

    // Pass 4: register union types (tagged-union layout reusing enum infrastructure)
    let mut union_types: Vec<Type> = Vec::new();
    for ty in c.type_ctx.type_aliases.values() {
        collect_union_types(ty, &mut union_types);
    }
    for sig in c.type_ctx.functions.values() {
        collect_union_types(&sig.return_type, &mut union_types);
        for p in &sig.params {
            collect_union_types(&p.ty, &mut union_types);
        }
    }
    for info in c.type_ctx.types.values() {
        if let Some(fields) = info.fields() {
            for (_, ty) in fields {
                collect_union_types(ty, &mut union_types);
            }
        }
        for sig in info.functions.values() {
            collect_union_types(&sig.return_type, &mut union_types);
            for p in &sig.params {
                collect_union_types(&p.ty, &mut union_types);
            }
        }
    }

    for union_ty in &union_types {
        let Type::Union(members) = union_ty else {
            continue;
        };
        let mangled = mangle_type(union_ty);
        if c.llvm_types.contains_monomorphized(&mangled) {
            continue;
        }

        let opaque = c.context.opaque_struct_type(&mangled);
        c.llvm_types.register_monomorphized(mangled.clone(), opaque);
        // Defer until member bodies are known. Pass 4 runs after struct/enum
        // bodies are set, so most members are sized; finalize handles any
        // unions-of-unions in dependency order.
        c.llvm_types
            .pending_union_layouts
            .push((opaque, members.clone()));
    }

    finalize_pending_unions(c);
}

/// Drains [`LLVMTypeCache::pending_union_layouts`] and lays out each union
/// body now that its members are sized. Loops until the queue is empty so
/// any unions-of-unions enqueued during finalize are handled in dependency
/// order.
pub(crate) fn finalize_pending_unions<'ctx>(c: &mut Compiler<'ctx>) {
    while !c.llvm_types.pending_union_layouts.is_empty() {
        let drained: Vec<(StructType<'ctx>, Vec<Type>)> =
            std::mem::take(&mut c.llvm_types.pending_union_layouts);
        for (opaque, members) in drained {
            build_union_layout(c, opaque, &members);
        }
    }
}

/// Builds the LLVM tagged-union layout for an enum: creates each variant's
/// payload struct, inserts it into `enum_variant_payloads` keyed by a
/// [`VariantId`], and sets the body on the already-registered opaque struct.
/// Variant order (the tag value) is owned by `TypeLayouts` — this function
/// only owns LLVM handles.
pub(crate) fn build_enum_layout<'ctx>(
    c: &mut Compiler<'ctx>,
    name: &str,
    enum_type: StructType<'ctx>,
    variants: &[(String, VariantData)],
) {
    let mut max_payload_size: u32 = 0;

    for (vname, vdata) in variants {
        let payload_option = match vdata {
            VariantData::Unit => None,
            VariantData::Tuple(types) => {
                let mut field_llvm: Vec<_> = types
                    .iter()
                    .filter_map(|ty| to_llvm_type(ty, c.context, &c.llvm_types))
                    .collect();
                if field_llvm.is_empty() && !types.is_empty() {
                    field_llvm.push(c.context.i8_type().into());
                }
                let payload = c.context.struct_type(&field_llvm, true);
                let size: u32 = field_llvm.iter().map(|t| llvm_field_byte_size(*t)).sum();
                max_payload_size = max_payload_size.max(size);
                Some(payload)
            }
            VariantData::Struct(fields) => {
                let mut field_llvm: Vec<_> = fields
                    .iter()
                    .filter_map(|(_, ty)| to_llvm_type(ty, c.context, &c.llvm_types))
                    .collect();
                if field_llvm.is_empty() && !fields.is_empty() {
                    field_llvm.push(c.context.i8_type().into());
                }
                let payload = c.context.struct_type(&field_llvm, true);
                let size: u32 = field_llvm.iter().map(|t| llvm_field_byte_size(*t)).sum();
                max_payload_size = max_payload_size.max(size);
                Some(payload)
            }
        };
        let id = VariantId::new(name, vname.clone());
        c.llvm_types
            .enum_variant_payloads
            .insert(id, payload_option);
    }

    let i8_type = c.context.i8_type();
    if max_payload_size > 0 {
        let payload_array = i8_type.array_type(max_payload_size);
        enum_type.set_body(&[i8_type.into(), payload_array.into()], false);
    } else {
        enum_type.set_body(&[i8_type.into()], false);
    }
}

/// Builds the LLVM tagged-union layout for a union type: sizes the body to
/// fit the largest member. Unions do not register variant payloads in
/// `LLVMTypeCache`; their tag and member type are derived directly from the
/// member list at the use site.
pub(crate) fn build_union_layout<'ctx>(
    c: &mut Compiler<'ctx>,
    opaque: StructType<'ctx>,
    members: &[Type],
) {
    let i8_type = c.context.i8_type();
    let mut max_payload_size: u32 = 0;

    for member in members {
        if let Some(llvm_ty) = to_llvm_type(member, c.context, &c.llvm_types) {
            let size = llvm_field_byte_size(llvm_ty);
            max_payload_size = max_payload_size.max(size);
        }
    }

    if max_payload_size > 0 {
        let payload_array = i8_type.array_type(max_payload_size);
        opaque.set_body(&[i8_type.into(), payload_array.into()], false);
    } else {
        opaque.set_body(&[i8_type.into()], false);
    }
}

/// Recursively collects all `Type::Union` variants reachable from `ty`.
fn collect_union_types(ty: &Type, out: &mut Vec<Type>) {
    match ty {
        Type::Union(members) => {
            out.push(ty.clone());
            for m in members {
                collect_union_types(m, out);
            }
        }
        Type::Function {
            params,
            return_type,
        } => {
            for fp in params {
                collect_union_types(&fp.ty, out);
            }
            collect_union_types(return_type, out);
        }
        Type::Named { type_args, .. } => {
            for ta in type_args {
                collect_union_types(ta, out);
            }
        }
        Type::Indirect(inner) | Type::Pointer(inner) => collect_union_types(inner, out),
        _ => {}
    }
}
