//! Type registration: translates Expo type-checked structs, enums, and unions
//! into LLVM struct types using a multi-pass approach so cross-referencing
//! types resolve correctly.

use expo_typecheck::context::VariantData;
use expo_typecheck::types::{Type, mangle_type};
use inkwell::types::StructType;

use crate::compiler::{Compiler, llvm_field_byte_size};
use crate::types::to_llvm_type;

impl<'ctx> Compiler<'ctx> {
    /// Translates Expo type-checked structs and enums into LLVM struct types.
    /// Uses a multi-pass approach (opaque types first, then bodies) so
    /// cross-referencing types resolve correctly.
    pub(crate) fn register_types(&mut self) {
        // Pass 1: create opaque types so cross-references resolve
        for (name, info) in self.type_ctx.types.iter().filter(|(_, ti)| ti.is_struct()) {
            if !info.type_params.is_empty() {
                continue;
            }
            let st = self.context.opaque_struct_type(name);
            self.types.structs.insert(name.clone(), st);
        }
        for (name, info) in self.type_ctx.types.iter().filter(|(_, ti)| ti.is_enum()) {
            if !info.type_params.is_empty() {
                continue;
            }
            let et = self.context.opaque_struct_type(name);
            self.types.structs.insert(name.clone(), et);
        }

        // Pass 1b: ensure all field/variant types exist (triggers monomorphization
        // of generic instances like List<Token> before struct bodies are set).
        // Indirect-wrapped types are skipped: they compile to pointers, so their
        // inner generic instances can be monomorphized lazily (after struct bodies
        // are set and sizes are known).
        let field_types: Vec<Type> = self
            .type_ctx
            .types
            .iter()
            .filter(|(_, ti)| ti.is_struct() && ti.type_params.is_empty())
            .flat_map(|(_, info)| info.fields().unwrap().iter().map(|(_, ty)| ty.clone()))
            .filter(|ty| !matches!(ty, Type::Indirect(_)))
            .collect();
        for ty in &field_types {
            let _ = self.ensure_types_exist(ty);
        }

        let variant_types: Vec<Type> = self
            .type_ctx
            .types
            .iter()
            .filter(|(_, ti)| ti.is_enum() && ti.type_params.is_empty())
            .flat_map(|(_, info)| {
                info.variants().unwrap().iter().flat_map(|v| match &v.data {
                    VariantData::Tuple(types) => types.clone(),
                    VariantData::Struct(fields) => {
                        fields.iter().map(|(_, ty)| ty.clone()).collect()
                    }
                    VariantData::Unit => Vec::new(),
                })
            })
            .filter(|ty| !matches!(ty, Type::Indirect(_)))
            .collect();
        for ty in &variant_types {
            let _ = self.ensure_types_exist(ty);
        }

        // Pass 2: set struct bodies (skip generic templates)
        for (name, info) in self.type_ctx.types.iter().filter(|(_, ti)| ti.is_struct()) {
            if !info.type_params.is_empty() {
                continue;
            }
            let struct_type = *self.types.structs.get(name).unwrap();
            let field_types: Vec<_> = info
                .fields()
                .unwrap()
                .iter()
                .filter_map(|(_, ty)| to_llvm_type(ty, self.context, &self.types.structs))
                .collect();
            struct_type.set_body(&field_types, false);
        }

        // Pass 3: set enum bodies (skip generic templates)
        for (name, info) in self.type_ctx.types.iter().filter(|(_, ti)| ti.is_enum()) {
            if !info.type_params.is_empty() {
                continue;
            }
            let enum_type = *self.types.structs.get(name).unwrap();
            let variants: Vec<_> = info
                .variants()
                .unwrap()
                .iter()
                .map(|v| (v.name.clone(), v.data.clone()))
                .collect();
            self.build_enum_layout(name, enum_type, &variants);
        }

        // Pass 4: register union types (tagged-union layout reusing enum infrastructure)
        let mut union_types: Vec<Type> = Vec::new();
        for ty in self.type_ctx.type_aliases.values() {
            collect_union_types(ty, &mut union_types);
        }
        for sig in self.type_ctx.functions.values() {
            collect_union_types(&sig.return_type, &mut union_types);
            for p in &sig.params {
                collect_union_types(&p.ty, &mut union_types);
            }
        }
        for info in self.type_ctx.types.values().filter(|ti| ti.is_struct()) {
            for (_, ty) in info.fields().unwrap() {
                collect_union_types(ty, &mut union_types);
            }
        }

        for union_ty in &union_types {
            let Type::Union(members) = union_ty else {
                continue;
            };
            let mangled = mangle_type(union_ty);
            if self.types.structs.contains_key(&mangled) {
                continue;
            }

            let opaque = self.context.opaque_struct_type(&mangled);
            self.types.structs.insert(mangled.clone(), opaque);

            self.build_union_layout(&mangled, opaque, members);
        }
    }

    /// Builds the LLVM tagged-union layout for an enum: creates variant payload
    /// structs, sets the body on the (already-registered) opaque struct, populates
    /// `enum_variant_payloads`, and builds the variant name table.
    pub(crate) fn build_enum_layout(
        &mut self,
        name: &str,
        enum_type: StructType<'ctx>,
        variants: &[(String, VariantData)],
    ) {
        let mut variant_payloads = Vec::new();
        let mut max_payload_size: u32 = 0;

        for (vname, vdata) in variants {
            match vdata {
                VariantData::Unit => {
                    variant_payloads.push((vname.clone(), None));
                }
                VariantData::Tuple(types) => {
                    let mut field_llvm: Vec<_> = types
                        .iter()
                        .filter_map(|ty| to_llvm_type(ty, self.context, &self.types.structs))
                        .collect();
                    if field_llvm.is_empty() && !types.is_empty() {
                        field_llvm.push(self.context.i8_type().into());
                    }
                    let payload = self.context.struct_type(&field_llvm, true);
                    let size: u32 = field_llvm.iter().map(|t| llvm_field_byte_size(*t)).sum();
                    max_payload_size = max_payload_size.max(size);
                    variant_payloads.push((vname.clone(), Some(payload)));
                }
                VariantData::Struct(fields) => {
                    let mut field_llvm: Vec<_> = fields
                        .iter()
                        .filter_map(|(_, ty)| to_llvm_type(ty, self.context, &self.types.structs))
                        .collect();
                    if field_llvm.is_empty() && !fields.is_empty() {
                        field_llvm.push(self.context.i8_type().into());
                    }
                    let payload = self.context.struct_type(&field_llvm, true);
                    let size: u32 = field_llvm.iter().map(|t| llvm_field_byte_size(*t)).sum();
                    max_payload_size = max_payload_size.max(size);
                    variant_payloads.push((vname.clone(), Some(payload)));
                }
            }
        }

        let i8_type = self.context.i8_type();
        if max_payload_size > 0 {
            let payload_array = i8_type.array_type(max_payload_size);
            enum_type.set_body(&[i8_type.into(), payload_array.into()], false);
        } else {
            enum_type.set_body(&[i8_type.into()], false);
        }

        self.types
            .enum_variant_payloads
            .insert(name.to_string(), variant_payloads);

        let ptr_type = self.context.ptr_type(inkwell::AddressSpace::default());
        let name_ptrs: Vec<_> = variants
            .iter()
            .map(|(vname, _)| {
                let bytes = self.context.const_string(vname.as_bytes(), true);
                let g =
                    self.module
                        .add_global(bytes.get_type(), None, &format!("{name}_{vname}_name"));
                g.set_initializer(&bytes);
                g.set_constant(true);
                g.as_pointer_value()
            })
            .collect();
        let table_init = ptr_type.const_array(&name_ptrs);
        let table_global = self.module.add_global(
            table_init.get_type(),
            None,
            &format!("{name}_variant_names"),
        );
        table_global.set_initializer(&table_init);
        table_global.set_constant(true);
        self.types
            .enum_name_tables
            .insert(name.to_string(), table_global.as_pointer_value());
    }

    /// Builds the LLVM tagged-union layout for a union type: creates variant
    /// payload structs from member types and sets the body. Unlike enums, unions
    /// do not have a variant name table.
    pub(crate) fn build_union_layout(
        &mut self,
        name: &str,
        opaque: StructType<'ctx>,
        members: &[Type],
    ) {
        let i8_type = self.context.i8_type();
        let mut variant_payloads = Vec::new();
        let mut max_payload_size: u32 = 0;

        for member in members {
            let member_name = mangle_type(member);
            if let Some(llvm_ty) = to_llvm_type(member, self.context, &self.types.structs) {
                let payload = self.context.struct_type(&[llvm_ty], true);
                let size = llvm_field_byte_size(llvm_ty);
                max_payload_size = max_payload_size.max(size);
                variant_payloads.push((member_name, Some(payload)));
            } else {
                variant_payloads.push((member_name, None));
            }
        }

        if max_payload_size > 0 {
            let payload_array = i8_type.array_type(max_payload_size);
            opaque.set_body(&[i8_type.into(), payload_array.into()], false);
        } else {
            opaque.set_body(&[i8_type.into()], false);
        }

        self.types
            .enum_variant_payloads
            .insert(name.to_string(), variant_payloads);
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
        Type::GenericInstance { type_args, .. } => {
            for ta in type_args {
                collect_union_types(ta, out);
            }
        }
        Type::Indirect(inner) => collect_union_types(inner, out),
        _ => {}
    }
}
