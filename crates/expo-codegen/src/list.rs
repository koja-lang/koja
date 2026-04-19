//! Codegen for `List<T>` intrinsic methods.
//!
//! List is a heap-backed growable array using `malloc`/`realloc`/`free`.
//! Layout: `{ ptr: i8*, length: i64, capacity: i64 }`

use expo_ast::identifier::TypeIdentifier;
use expo_ast::types::named_generic_std;
use expo_typecheck::types::{Primitive, Type, mangle_name};

use crate::compiler::{Compiler, EmitResult};
use crate::generics::ensure_types_exist;
use crate::types::to_llvm_type;

pub fn monomorphize_list_struct<'ctx>(c: &mut Compiler<'ctx>, mangled: &str) -> Result<(), String> {
    let st = c.context.opaque_struct_type(mangled);
    let ptr_type = c.context.ptr_type(inkwell::AddressSpace::default());
    let i64_type = c.context.i64_type();
    st.set_body(&[ptr_type.into(), i64_type.into(), i64_type.into()], false);
    c.types.register_monomorphized(mangled.to_string(), st);
    c.layouts.register_struct_layout(
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
    let list_struct = c
        .types
        .get_monomorphized(mangled_type)
        .ok_or_else(|| format!("no LLVM type for `{mangled_type}`"))?;

    let elem_ty = &type_args[0];
    let elem_llvm = to_llvm_type(elem_ty, c.context, &c.types)
        .ok_or_else(|| format!("cannot map element type `{}` to LLVM", elem_ty.display()))?;
    let elem_size = crate::compiler::llvm_field_byte_size(elem_llvm) as u64;

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
            let raw_ptr = c.call(malloc, &[alloc_size.into()], "buf").unwrap();

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
                .call(realloc, &[buf_ptr.into(), new_size.into()], "new_buf")
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
            let option_mangled = mangle_name(&TypeIdentifier::std("Option"), &option_type_args);
            ensure_types_exist(c, &named_generic_std("Option", option_type_args))?;
            let option_struct = c
                .types
                .get_monomorphized(&option_mangled)
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

        "pop" => {
            let option_type_args = vec![elem_ty.clone()];
            ensure_types_exist(c, &named_generic_std("Option", option_type_args.clone()))?;
            let option_mangled = mangle_name(&TypeIdentifier::std("Option"), &option_type_args);
            let option_struct = c
                .types
                .get_monomorphized(&option_mangled)
                .ok_or_else(|| format!("no LLVM type for {option_mangled}"))?;

            let list_type = named_generic_std("List", vec![elem_ty.clone()]);
            let option_type = named_generic_std("Option", option_type_args);
            let pair_type_args = vec![option_type, list_type];
            ensure_types_exist(c, &named_generic_std("Pair", pair_type_args.clone()))?;
            let pair_mangled = mangle_name(&TypeIdentifier::std("Pair"), &pair_type_args);
            let pair_struct = c
                .types
                .get_monomorphized(&pair_mangled)
                .ok_or_else(|| format!("no LLVM type for {pair_mangled}"))?;

            let i8_ty = c.context.i8_type();
            let fn_type = pair_struct.fn_type(&[list_struct.into()], false);
            let fn_val = c.module.add_function(mangled_fn, fn_type, None);
            c.functions.insert(mangled_fn.to_string(), fn_val);

            let entry = c.context.append_basic_block(fn_val, "entry");
            let empty_bb = c.context.append_basic_block(fn_val, "empty");
            let nonempty_bb = c.context.append_basic_block(fn_val, "nonempty");

            let saved_block = c.builder.get_insert_block();
            c.builder.position_at_end(entry);

            let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
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
            let cap = c.builder.build_extract_value(self_val, 2, "cap").unwrap();

            let is_empty = c
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::EQ,
                    len,
                    i64_ty.const_int(0, false),
                    "is_empty",
                )
                .unwrap();
            c.builder
                .build_conditional_branch(is_empty, empty_bb, nonempty_bb)
                .unwrap();

            // Empty: Pair{first: None, second: self}
            c.builder.position_at_end(empty_bb);
            let none_alloca = c
                .builder
                .build_alloca(option_struct, "none_alloca")
                .unwrap();
            let tag_ptr = c
                .builder
                .build_struct_gep(option_struct, none_alloca, 0, "tag_ptr")
                .unwrap();
            c.builder
                .build_store(tag_ptr, i8_ty.const_int(1, false))
                .unwrap();
            let none_val = c
                .builder
                .build_load(option_struct, none_alloca, "none_val")
                .unwrap();

            let pair_alloca_e = c.builder.build_alloca(pair_struct, "pair_empty").unwrap();
            let first_ptr = c
                .builder
                .build_struct_gep(pair_struct, pair_alloca_e, 0, "first_ptr")
                .unwrap();
            c.builder.build_store(first_ptr, none_val).unwrap();
            let second_ptr = c
                .builder
                .build_struct_gep(pair_struct, pair_alloca_e, 1, "second_ptr")
                .unwrap();
            c.builder.build_store(second_ptr, self_val).unwrap();
            let result_empty = c
                .builder
                .build_load(pair_struct, pair_alloca_e, "pair_empty_val")
                .unwrap();
            c.builder.build_return(Some(&result_empty)).unwrap();

            // Non-empty: read last element, decrement length
            c.builder.position_at_end(nonempty_bb);
            let new_len = c
                .builder
                .build_int_sub(len, i64_ty.const_int(1, false), "new_len")
                .unwrap();
            let byte_offset = c
                .builder
                .build_int_mul(new_len, i64_ty.const_int(elem_size, false), "byte_off")
                .unwrap();
            let elem_ptr = unsafe {
                c.builder
                    .build_gep(c.context.i8_type(), buf_ptr, &[byte_offset], "elem_ptr")
                    .unwrap()
            };
            let elem_val = c
                .builder
                .build_load(elem_llvm, elem_ptr, "elem_val")
                .unwrap();

            let some_alloca = c
                .builder
                .build_alloca(option_struct, "some_alloca")
                .unwrap();
            let tag_ptr = c
                .builder
                .build_struct_gep(option_struct, some_alloca, 0, "tag_ptr")
                .unwrap();
            c.builder
                .build_store(tag_ptr, i8_ty.const_int(0, false))
                .unwrap();
            let payload_ptr = c
                .builder
                .build_struct_gep(option_struct, some_alloca, 1, "payload_ptr")
                .unwrap();
            c.builder.build_store(payload_ptr, elem_val).unwrap();
            let some_val = c
                .builder
                .build_load(option_struct, some_alloca, "some_val")
                .unwrap();

            let shortened = list_struct.get_undef();
            let shortened = c
                .builder
                .build_insert_value(shortened, buf_ptr, 0, "s_ptr")
                .unwrap()
                .into_struct_value();
            let shortened = c
                .builder
                .build_insert_value(shortened, new_len, 1, "s_len")
                .unwrap()
                .into_struct_value();
            let shortened = c
                .builder
                .build_insert_value(shortened, cap, 2, "s_cap")
                .unwrap()
                .into_struct_value();

            let pair_alloca_n = c
                .builder
                .build_alloca(pair_struct, "pair_nonempty")
                .unwrap();
            let first_ptr = c
                .builder
                .build_struct_gep(pair_struct, pair_alloca_n, 0, "first_ptr")
                .unwrap();
            c.builder.build_store(first_ptr, some_val).unwrap();
            let second_ptr = c
                .builder
                .build_struct_gep(pair_struct, pair_alloca_n, 1, "second_ptr")
                .unwrap();
            c.builder.build_store(second_ptr, shortened).unwrap();
            let result_nonempty = c
                .builder
                .build_load(pair_struct, pair_alloca_n, "pair_nonempty_val")
                .unwrap();
            c.builder.build_return(Some(&result_nonempty)).unwrap();

            if let Some(bb) = saved_block {
                c.builder.position_at_end(bb);
            }
        }

        "replace_at" => {
            let fn_type = list_struct.fn_type(
                &[list_struct.into(), i64_ty.into(), elem_llvm.into()],
                false,
            );
            let fn_val = c.module.add_function(mangled_fn, fn_type, None);
            c.functions.insert(mangled_fn.to_string(), fn_val);

            let entry = c.context.append_basic_block(fn_val, "entry");
            let in_bounds_bb = c.context.append_basic_block(fn_val, "in_bounds");
            let done_bb = c.context.append_basic_block(fn_val, "done");

            let saved_block = c.builder.get_insert_block();
            c.builder.position_at_end(entry);

            let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
            let index = fn_val.get_nth_param(1).unwrap().into_int_value();
            let value = fn_val.get_nth_param(2).unwrap();

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

            let ok = c
                .builder
                .build_int_compare(inkwell::IntPredicate::ULT, index, len, "in_bounds")
                .unwrap();
            c.builder
                .build_conditional_branch(ok, in_bounds_bb, done_bb)
                .unwrap();

            c.builder.position_at_end(in_bounds_bb);
            let byte_offset = c
                .builder
                .build_int_mul(index, i64_ty.const_int(elem_size, false), "byte_off")
                .unwrap();
            let elem_ptr = unsafe {
                c.builder
                    .build_gep(c.context.i8_type(), buf_ptr, &[byte_offset], "elem_ptr")
                    .unwrap()
            };
            c.builder.build_store(elem_ptr, value).unwrap();
            c.builder.build_unconditional_branch(done_bb).unwrap();

            c.builder.position_at_end(done_bb);
            c.builder.build_return(Some(&self_val)).unwrap();

            if let Some(bb) = saved_block {
                c.builder.position_at_end(bb);
            }
        }

        "slice" => {
            let fn_type =
                list_struct.fn_type(&[list_struct.into(), i64_ty.into(), i64_ty.into()], false);
            let fn_val = c.module.add_function(mangled_fn, fn_type, None);
            c.functions.insert(mangled_fn.to_string(), fn_val);

            let entry = c.context.append_basic_block(fn_val, "entry");
            let nonempty_bb = c.context.append_basic_block(fn_val, "nonempty");
            let empty_bb = c.context.append_basic_block(fn_val, "empty");

            let saved_block = c.builder.get_insert_block();
            c.builder.position_at_end(entry);

            let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
            let start = fn_val.get_nth_param(1).unwrap().into_int_value();
            let count = fn_val.get_nth_param(2).unwrap().into_int_value();

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

            // Clamp start: if start >= len, clamped_start = len
            let start_ok = c
                .builder
                .build_int_compare(inkwell::IntPredicate::ULT, start, len, "start_ok")
                .unwrap();
            let clamped_start = c
                .builder
                .build_select(start_ok, start, len, "clamped_start")
                .unwrap()
                .into_int_value();

            // remaining = len - clamped_start
            let remaining = c
                .builder
                .build_int_sub(len, clamped_start, "remaining")
                .unwrap();

            // Clamp count: if count > remaining, clamped_count = remaining
            let count_ok = c
                .builder
                .build_int_compare(inkwell::IntPredicate::ULE, count, remaining, "count_ok")
                .unwrap();
            let clamped_count = c
                .builder
                .build_select(count_ok, count, remaining, "clamped_count")
                .unwrap()
                .into_int_value();

            let has_elems = c
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::UGT,
                    clamped_count,
                    i64_ty.const_int(0, false),
                    "has_elems",
                )
                .unwrap();
            c.builder
                .build_conditional_branch(has_elems, nonempty_bb, empty_bb)
                .unwrap();

            // Non-empty: malloc + memcpy
            c.builder.position_at_end(nonempty_bb);
            let alloc_bytes = c
                .builder
                .build_int_mul(
                    clamped_count,
                    i64_ty.const_int(elem_size, false),
                    "alloc_bytes",
                )
                .unwrap();
            let malloc = *c.functions.get("malloc").expect("malloc not declared");
            let new_buf = c.call(malloc, &[alloc_bytes.into()], "new_buf").unwrap();

            let src_offset = c
                .builder
                .build_int_mul(clamped_start, i64_ty.const_int(elem_size, false), "src_off")
                .unwrap();
            let src_ptr = unsafe {
                c.builder
                    .build_gep(c.context.i8_type(), buf_ptr, &[src_offset], "src_ptr")
                    .unwrap()
            };
            let memcpy = *c.functions.get("memcpy").expect("memcpy not declared");
            c.call_void(
                memcpy,
                &[new_buf.into(), src_ptr.into(), alloc_bytes.into()],
                "cpy",
            );

            let result = list_struct.get_undef();
            let result = c
                .builder
                .build_insert_value(result, new_buf, 0, "r_ptr")
                .unwrap()
                .into_struct_value();
            let result = c
                .builder
                .build_insert_value(result, clamped_count, 1, "r_len")
                .unwrap()
                .into_struct_value();
            let result = c
                .builder
                .build_insert_value(result, clamped_count, 2, "r_cap")
                .unwrap()
                .into_struct_value();
            c.builder.build_return(Some(&result)).unwrap();

            // Empty slice: return a fresh empty list
            c.builder.position_at_end(empty_bb);
            let empty_alloc = c
                .call(malloc, &[i64_ty.const_int(0, false).into()], "empty_buf")
                .unwrap();
            let empty_result = list_struct.get_undef();
            let empty_result = c
                .builder
                .build_insert_value(empty_result, empty_alloc, 0, "e_ptr")
                .unwrap()
                .into_struct_value();
            let empty_result = c
                .builder
                .build_insert_value(empty_result, i64_ty.const_int(0, false), 1, "e_len")
                .unwrap()
                .into_struct_value();
            let empty_result = c
                .builder
                .build_insert_value(empty_result, i64_ty.const_int(0, false), 2, "e_cap")
                .unwrap()
                .into_struct_value();
            c.builder.build_return(Some(&empty_result)).unwrap();

            if let Some(bb) = saved_block {
                c.builder.position_at_end(bb);
            }
        }

        "concat" => {
            let fn_type = list_struct.fn_type(&[list_struct.into(), list_struct.into()], false);
            let fn_val = c.module.add_function(mangled_fn, fn_type, None);
            c.functions.insert(mangled_fn.to_string(), fn_val);

            let entry = c.context.append_basic_block(fn_val, "entry");
            let grow_bb = c.context.append_basic_block(fn_val, "grow");
            let copy_bb = c.context.append_basic_block(fn_val, "copy");

            let saved_block = c.builder.get_insert_block();
            c.builder.position_at_end(entry);

            let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
            let other_val = fn_val.get_nth_param(1).unwrap().into_struct_value();

            let self_ptr = c
                .builder
                .build_extract_value(self_val, 0, "self_ptr")
                .unwrap();
            let self_len = c
                .builder
                .build_extract_value(self_val, 1, "self_len")
                .unwrap()
                .into_int_value();
            let self_cap = c
                .builder
                .build_extract_value(self_val, 2, "self_cap")
                .unwrap()
                .into_int_value();

            let other_ptr = c
                .builder
                .build_extract_value(other_val, 0, "other_ptr")
                .unwrap()
                .into_pointer_value();
            let other_len = c
                .builder
                .build_extract_value(other_val, 1, "other_len")
                .unwrap()
                .into_int_value();

            let total_len = c
                .builder
                .build_int_add(self_len, other_len, "total_len")
                .unwrap();

            let needs_grow = c
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::UGT,
                    total_len,
                    self_cap,
                    "needs_grow",
                )
                .unwrap();
            c.builder
                .build_conditional_branch(needs_grow, grow_bb, copy_bb)
                .unwrap();

            // Grow: realloc to fit total_len
            c.builder.position_at_end(grow_bb);
            let new_cap = total_len;
            let new_size = c
                .builder
                .build_int_mul(new_cap, i64_ty.const_int(elem_size, false), "new_size")
                .unwrap();
            let realloc = *c.functions.get("realloc").expect("realloc not declared");
            let new_ptr = c
                .call(realloc, &[self_ptr.into(), new_size.into()], "new_buf")
                .unwrap();
            c.builder.build_unconditional_branch(copy_bb).unwrap();

            // Copy other's data after self's data
            c.builder.position_at_end(copy_bb);
            let phi_ptr = c.builder.build_phi(ptr_ty, "ptr_phi").unwrap();
            phi_ptr.add_incoming(&[(&self_ptr, entry), (&new_ptr, grow_bb)]);
            let phi_cap = c.builder.build_phi(i64_ty, "cap_phi").unwrap();
            phi_cap.add_incoming(&[(&self_cap, entry), (&new_cap, grow_bb)]);

            let final_ptr = phi_ptr.as_basic_value().into_pointer_value();
            let final_cap = phi_cap.as_basic_value().into_int_value();

            let dst_offset = c
                .builder
                .build_int_mul(self_len, i64_ty.const_int(elem_size, false), "dst_off")
                .unwrap();
            let dst_ptr = unsafe {
                c.builder
                    .build_gep(c.context.i8_type(), final_ptr, &[dst_offset], "dst_ptr")
                    .unwrap()
            };
            let copy_bytes = c
                .builder
                .build_int_mul(other_len, i64_ty.const_int(elem_size, false), "copy_bytes")
                .unwrap();
            let memcpy = *c.functions.get("memcpy").expect("memcpy not declared");
            c.call_void(
                memcpy,
                &[dst_ptr.into(), other_ptr.into(), copy_bytes.into()],
                "cpy",
            );

            let result = list_struct.get_undef();
            let result = c
                .builder
                .build_insert_value(result, final_ptr, 0, "r_ptr")
                .unwrap()
                .into_struct_value();
            let result = c
                .builder
                .build_insert_value(result, total_len, 1, "r_len")
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

        _ => return Ok(EmitResult::NotIntrinsic),
    }

    Ok(EmitResult::Emitted)
}
