//! Codegen for `List<T>` intrinsic methods.
//!
//! List is a heap-backed growable array using `malloc`/`realloc`/`free`.
//! Layout: `{ ptr: i8*, length: i64, capacity: i64 }`

use expo_typecheck::types::{GenericKind, Primitive, Type};

use crate::compiler::{Compiler, EmitResult, type_byte_size};
use crate::types::to_llvm_type;

pub fn monomorphize_list_struct<'ctx>(c: &mut Compiler<'ctx>, mangled: &str) -> Result<(), String> {
    let st = c.context.opaque_struct_type(mangled);
    let ptr_type = c.context.ptr_type(inkwell::AddressSpace::default());
    let i64_type = c.context.i64_type();
    st.set_body(&[ptr_type.into(), i64_type.into(), i64_type.into()], false);
    c.struct_types.insert(mangled.to_string(), st);
    c.mono_struct_info.insert(
        mangled.to_string(),
        vec![
            ("ptr".to_string(), Type::Primitive(Primitive::String)),
            ("length".to_string(), Type::Primitive(Primitive::I64)),
            ("capacity".to_string(), Type::Primitive(Primitive::I64)),
        ],
    );
    Ok(())
}

pub fn emit_list_method<'ctx>(
    c: &mut Compiler<'ctx>,
    mangled_type: &str,
    mangled_fn: &str,
    method_name: &str,
    type_args: &[Type],
) -> Result<EmitResult, String> {
    let list_struct = *c
        .struct_types
        .get(mangled_type)
        .ok_or_else(|| format!("no LLVM type for `{mangled_type}`"))?;

    let elem_ty = &type_args[0];
    let elem_llvm = to_llvm_type(elem_ty, c.context, &c.struct_types)
        .ok_or_else(|| format!("cannot map element type `{}` to LLVM", elem_ty.display()))?;
    let elem_size = type_byte_size(elem_ty) as u64;

    let ptr_ty = c.context.ptr_type(inkwell::AddressSpace::default());
    let i64_ty = c.context.i64_type();
    let i1_ty = c.context.bool_type();

    match method_name {
        "new" => {
            let fn_type = list_struct.fn_type(&[], false);
            let fn_val = c.module.add_function(mangled_fn, fn_type, None);
            c.functions.insert(mangled_fn.to_string(), fn_val);

            let entry = c.context.append_basic_block(fn_val, "entry");
            let saved_block = c.builder.get_insert_block();
            c.builder.position_at_end(entry);

            let initial_cap = i64_ty.const_int(8, false);
            let alloc_size = c
                .builder
                .build_int_mul(initial_cap, i64_ty.const_int(elem_size, false), "alloc_sz")
                .unwrap();
            let malloc = *c.functions.get("malloc").expect("malloc not declared");
            let raw_ptr = c
                .builder
                .build_call(malloc, &[alloc_size.into()], "buf")
                .unwrap()
                .try_as_basic_value()
                .left()
                .unwrap();

            let result = list_struct.get_undef();
            let result = c
                .builder
                .build_insert_value(result, raw_ptr, 0, "with_ptr")
                .unwrap()
                .into_struct_value();
            let result = c
                .builder
                .build_insert_value(result, i64_ty.const_int(0, false), 1, "with_len")
                .unwrap()
                .into_struct_value();
            let result = c
                .builder
                .build_insert_value(result, initial_cap, 2, "with_cap")
                .unwrap()
                .into_struct_value();

            c.builder.build_return(Some(&result)).unwrap();
            if let Some(bb) = saved_block {
                c.builder.position_at_end(bb);
            }
        }

        "append" => {
            let fn_type = list_struct.fn_type(&[list_struct.into(), elem_llvm.into()], false);
            let fn_val = c.module.add_function(mangled_fn, fn_type, None);
            c.functions.insert(mangled_fn.to_string(), fn_val);

            let entry = c.context.append_basic_block(fn_val, "entry");
            let grow_bb = c.context.append_basic_block(fn_val, "grow");
            let store_bb = c.context.append_basic_block(fn_val, "store");

            let saved_block = c.builder.get_insert_block();
            c.builder.position_at_end(entry);

            let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
            let item_val = fn_val.get_nth_param(1).unwrap();

            let buf_ptr = c
                .builder
                .build_extract_value(self_val, 0, "buf_ptr")
                .unwrap();
            let len = c
                .builder
                .build_extract_value(self_val, 1, "len")
                .unwrap()
                .into_int_value();
            let cap = c
                .builder
                .build_extract_value(self_val, 2, "cap")
                .unwrap()
                .into_int_value();

            let needs_grow = c
                .builder
                .build_int_compare(inkwell::IntPredicate::EQ, len, cap, "needs_grow")
                .unwrap();
            c.builder
                .build_conditional_branch(needs_grow, grow_bb, store_bb)
                .unwrap();

            c.builder.position_at_end(grow_bb);
            let new_cap = c
                .builder
                .build_int_mul(cap, i64_ty.const_int(2, false), "new_cap")
                .unwrap();
            let new_size = c
                .builder
                .build_int_mul(new_cap, i64_ty.const_int(elem_size, false), "new_size")
                .unwrap();
            let realloc = *c.functions.get("realloc").expect("realloc not declared");
            let new_ptr = c
                .builder
                .build_call(realloc, &[buf_ptr.into(), new_size.into()], "new_buf")
                .unwrap()
                .try_as_basic_value()
                .left()
                .unwrap();
            c.builder.build_unconditional_branch(store_bb).unwrap();

            c.builder.position_at_end(store_bb);
            let phi_ptr = c.builder.build_phi(ptr_ty, "ptr_phi").unwrap();
            phi_ptr.add_incoming(&[(&buf_ptr, entry), (&new_ptr, grow_bb)]);
            let phi_cap = c.builder.build_phi(i64_ty, "cap_phi").unwrap();
            phi_cap.add_incoming(&[(&cap, entry), (&new_cap, grow_bb)]);

            let final_ptr = phi_ptr.as_basic_value().into_pointer_value();
            let final_cap = phi_cap.as_basic_value().into_int_value();

            let byte_offset = c
                .builder
                .build_int_mul(len, i64_ty.const_int(elem_size, false), "byte_off")
                .unwrap();
            let elem_ptr = unsafe {
                c.builder
                    .build_gep(c.context.i8_type(), final_ptr, &[byte_offset], "elem_ptr")
                    .unwrap()
            };
            c.builder.build_store(elem_ptr, item_val).unwrap();

            let new_len = c
                .builder
                .build_int_add(len, i64_ty.const_int(1, false), "new_len")
                .unwrap();

            let result = list_struct.get_undef();
            let result = c
                .builder
                .build_insert_value(result, final_ptr, 0, "r_ptr")
                .unwrap()
                .into_struct_value();
            let result = c
                .builder
                .build_insert_value(result, new_len, 1, "r_len")
                .unwrap()
                .into_struct_value();
            let result = c
                .builder
                .build_insert_value(result, final_cap, 2, "r_cap")
                .unwrap()
                .into_struct_value();

            c.builder.build_return(Some(&result)).unwrap();
            if let Some(bb) = saved_block {
                c.builder.position_at_end(bb);
            }
        }

        "get" => {
            let option_type_args = vec![elem_ty.clone()];
            let option_mangled = expo_typecheck::types::mangle_name("Option", &option_type_args);
            c.ensure_types_exist(&Type::GenericInstance {
                base: "Option".to_string(),
                type_args: option_type_args,
                kind: GenericKind::Enum,
            })?;
            let option_struct = *c
                .struct_types
                .get(&option_mangled)
                .ok_or_else(|| format!("no LLVM type for {option_mangled}"))?;

            let i8_ty = c.context.i8_type();
            let fn_type = option_struct.fn_type(&[list_struct.into(), i64_ty.into()], false);
            let fn_val = c.module.add_function(mangled_fn, fn_type, None);
            c.functions.insert(mangled_fn.to_string(), fn_val);

            let entry = c.context.append_basic_block(fn_val, "entry");
            let ok_bb = c.context.append_basic_block(fn_val, "ok");
            let oob_bb = c.context.append_basic_block(fn_val, "oob");

            let saved_block = c.builder.get_insert_block();
            c.builder.position_at_end(entry);

            let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
            let index = fn_val.get_nth_param(1).unwrap().into_int_value();

            let buf_ptr = c
                .builder
                .build_extract_value(self_val, 0, "buf_ptr")
                .unwrap()
                .into_pointer_value();
            let len = c
                .builder
                .build_extract_value(self_val, 1, "len")
                .unwrap()
                .into_int_value();

            let in_bounds = c
                .builder
                .build_int_compare(inkwell::IntPredicate::ULT, index, len, "in_bounds")
                .unwrap();
            c.builder
                .build_conditional_branch(in_bounds, ok_bb, oob_bb)
                .unwrap();

            // In-bounds: return Some(element)
            c.builder.position_at_end(ok_bb);
            let byte_offset = c
                .builder
                .build_int_mul(index, i64_ty.const_int(elem_size, false), "byte_off")
                .unwrap();
            let elem_ptr = unsafe {
                c.builder
                    .build_gep(c.context.i8_type(), buf_ptr, &[byte_offset], "elem_ptr")
                    .unwrap()
            };
            let val = c
                .builder
                .build_load(elem_llvm, elem_ptr, "elem_val")
                .unwrap();
            let alloca_some = c
                .builder
                .build_alloca(option_struct, "some_alloca")
                .unwrap();
            let tag_ptr = c
                .builder
                .build_struct_gep(option_struct, alloca_some, 0, "tag_ptr")
                .unwrap();
            c.builder
                .build_store(tag_ptr, i8_ty.const_int(0, false))
                .unwrap();
            let payload_ptr = c
                .builder
                .build_struct_gep(option_struct, alloca_some, 1, "payload_ptr")
                .unwrap();
            c.builder.build_store(payload_ptr, val).unwrap();
            let result = c
                .builder
                .build_load(option_struct, alloca_some, "some_val")
                .unwrap();
            c.builder.build_return(Some(&result)).unwrap();

            // Out-of-bounds: return None
            c.builder.position_at_end(oob_bb);
            let alloca_none = c
                .builder
                .build_alloca(option_struct, "none_alloca")
                .unwrap();
            let tag_ptr = c
                .builder
                .build_struct_gep(option_struct, alloca_none, 0, "tag_ptr")
                .unwrap();
            c.builder
                .build_store(tag_ptr, i8_ty.const_int(1, false))
                .unwrap();
            let result = c
                .builder
                .build_load(option_struct, alloca_none, "none_val")
                .unwrap();
            c.builder.build_return(Some(&result)).unwrap();

            if let Some(bb) = saved_block {
                c.builder.position_at_end(bb);
            }
        }

        "length" => {
            let fn_type = i64_ty.fn_type(&[list_struct.into()], false);
            let fn_val = c.module.add_function(mangled_fn, fn_type, None);
            c.functions.insert(mangled_fn.to_string(), fn_val);

            let entry = c.context.append_basic_block(fn_val, "entry");
            let saved_block = c.builder.get_insert_block();
            c.builder.position_at_end(entry);

            let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
            let len = c.builder.build_extract_value(self_val, 1, "len").unwrap();
            c.builder.build_return(Some(&len)).unwrap();

            if let Some(bb) = saved_block {
                c.builder.position_at_end(bb);
            }
        }

        "from_list" => {
            let fn_type = list_struct.fn_type(&[list_struct.into()], false);
            let fn_val = c.module.add_function(mangled_fn, fn_type, None);
            c.functions.insert(mangled_fn.to_string(), fn_val);

            let entry = c.context.append_basic_block(fn_val, "entry");
            let saved_block = c.builder.get_insert_block();
            c.builder.position_at_end(entry);

            let self_val = fn_val.get_nth_param(0).unwrap();
            c.builder.build_return(Some(&self_val)).unwrap();

            if let Some(bb) = saved_block {
                c.builder.position_at_end(bb);
            }
        }

        "empty?" => {
            let fn_type = i1_ty.fn_type(&[list_struct.into()], false);
            let fn_val = c.module.add_function(mangled_fn, fn_type, None);
            c.functions.insert(mangled_fn.to_string(), fn_val);

            let entry = c.context.append_basic_block(fn_val, "entry");
            let saved_block = c.builder.get_insert_block();
            c.builder.position_at_end(entry);

            let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
            let len = c
                .builder
                .build_extract_value(self_val, 1, "len")
                .unwrap()
                .into_int_value();
            let is_empty = c
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::EQ,
                    len,
                    i64_ty.const_int(0, false),
                    "is_empty",
                )
                .unwrap();
            c.builder.build_return(Some(&is_empty)).unwrap();

            if let Some(bb) = saved_block {
                c.builder.position_at_end(bb);
            }
        }

        _ => return Ok(EmitResult::NotIntrinsic),
    }

    Ok(EmitResult::Emitted)
}
