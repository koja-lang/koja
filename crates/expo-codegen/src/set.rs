//! Codegen for `Set<T>` intrinsic methods.
//!
//! Set is a hash-table-backed unique collection using open addressing
//! with linear probing. Layout matches the shared hashtable struct:
//! `{ entries_ptr: i8*, states_ptr: i8*, length: i64, capacity: i64 }`

use expo_ast::identifier::TypeIdentifier;
use expo_typecheck::types::{Type, mangle_name};

use crate::compiler::{Compiler, EmitResult};
use crate::generics::monomorphize_struct;
use crate::hashtable;
use crate::types::to_llvm_type;

pub fn emit_set_method<'ctx>(
    c: &mut Compiler<'ctx>,
    mangled_type: &str,
    mangled_fn: &str,
    method_name: &str,
    type_args: &[Type],
) -> Result<EmitResult, String> {
    let set_struct = c
        .llvm_types
        .get_monomorphized(mangled_type)
        .ok_or_else(|| format!("no LLVM type for `{mangled_type}`"))?;

    if type_args.is_empty() {
        return Err("Set requires one type argument <T>".to_string());
    }
    let elem_type = &type_args[0];

    let elem_llvm = to_llvm_type(elem_type, c.context, &c.llvm_types)
        .ok_or_else(|| format!("no LLVM type for Set element `{elem_type:?}`"))?;

    let elem_size = crate::compiler::llvm_field_byte_size(elem_llvm) as u64;

    let hash_fn = hashtable::ensure_hash_fn(c, elem_type)?;
    let eq_fn = hashtable::ensure_eq_fn(c, elem_type)?;

    let i64_ty = c.context.i64_type();
    let i32_ty = c.context.i32_type();
    let i8_ty = c.context.i8_type();
    let i1_ty = c.context.bool_type();
    let ptr_ty = c.context.ptr_type(inkwell::AddressSpace::default());

    match method_name {
        "new" => {
            hashtable::emit_hashtable_new(c, mangled_fn, set_struct, elem_size)?;
            return Ok(EmitResult::Emitted);
        }

        "insert" => {
            let fn_type = set_struct.fn_type(&[set_struct.into(), elem_llvm.into()], false);
            let fn_val = c.module.add_function(mangled_fn, fn_type, None);
            c.functions.insert(mangled_fn.to_string(), fn_val);

            let entry_bb = c.context.append_basic_block(fn_val, "entry");
            let saved_block = c.builder.get_insert_block();
            c.builder.position_at_end(entry_bb);

            let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
            let item_val = fn_val.get_nth_param(1).unwrap();

            let entries_ptr = c
                .builder
                .build_extract_value(self_val, 0, "entries")
                .unwrap()
                .into_pointer_value();
            let states_ptr = c
                .builder
                .build_extract_value(self_val, 1, "states")
                .unwrap()
                .into_pointer_value();
            let length = c
                .builder
                .build_extract_value(self_val, 2, "len")
                .unwrap()
                .into_int_value();
            let capacity = c
                .builder
                .build_extract_value(self_val, 3, "cap")
                .unwrap()
                .into_int_value();

            let need_resize_bb = c.context.append_basic_block(fn_val, "need_resize");
            let no_resize_bb = c.context.append_basic_block(fn_val, "no_resize");
            let probe_bb = c.context.append_basic_block(fn_val, "probe");

            let len_plus_1 = c
                .builder
                .build_int_add(length, i64_ty.const_int(1, false), "lp1")
                .unwrap();
            let lhs = c
                .builder
                .build_int_mul(len_plus_1, i64_ty.const_int(4, false), "lhs")
                .unwrap();
            let rhs = c
                .builder
                .build_int_mul(capacity, i64_ty.const_int(3, false), "rhs")
                .unwrap();
            let should_resize = c
                .builder
                .build_int_compare(inkwell::IntPredicate::UGT, lhs, rhs, "should_resize")
                .unwrap();
            c.builder
                .build_conditional_branch(should_resize, need_resize_bb, no_resize_bb)
                .unwrap();

            // Resize
            c.builder.position_at_end(need_resize_bb);
            let new_cap = c
                .builder
                .build_int_mul(capacity, i64_ty.const_int(2, false), "new_cap")
                .unwrap();
            let new_entries_bytes = c
                .builder
                .build_int_mul(new_cap, i64_ty.const_int(elem_size, false), "new_e_bytes")
                .unwrap();
            let malloc = *c.functions.get("malloc").unwrap();
            let new_entries_ptr = c
                .call(malloc, &[new_entries_bytes.into()], "new_entries")
                .unwrap()
                .into_pointer_value();
            let new_states_ptr = c
                .call(malloc, &[new_cap.into()], "new_states")
                .unwrap()
                .into_pointer_value();
            let memset = *c.functions.get("memset").unwrap();
            c.call_void(
                memset,
                &[
                    new_states_ptr.into(),
                    i32_ty.const_int(0, false).into(),
                    new_cap.into(),
                ],
                "clear_new_states",
            );

            let rehash_bb = c.context.append_basic_block(fn_val, "rehash");
            let rehash_body = c.context.append_basic_block(fn_val, "rehash_body");
            let rehash_probe = c.context.append_basic_block(fn_val, "rehash_probe");
            let rehash_store = c.context.append_basic_block(fn_val, "rehash_store");
            let rehash_next = c.context.append_basic_block(fn_val, "rehash_next");
            let rehash_done = c.context.append_basic_block(fn_val, "rehash_done");

            c.builder.build_unconditional_branch(rehash_bb).unwrap();
            c.builder.position_at_end(rehash_bb);

            let phi_i = c.builder.build_phi(i64_ty, "ri").unwrap();
            phi_i.add_incoming(&[(&i64_ty.const_int(0, false), need_resize_bb)]);
            let ri = phi_i.as_basic_value().into_int_value();
            let ri_done = c
                .builder
                .build_int_compare(inkwell::IntPredicate::UGE, ri, capacity, "ri_done")
                .unwrap();
            c.builder
                .build_conditional_branch(ri_done, rehash_done, rehash_body)
                .unwrap();

            c.builder.position_at_end(rehash_body);
            let state_ptr_at_ri = unsafe {
                c.builder
                    .build_gep(i8_ty, states_ptr, &[ri], "old_state_ptr")
                    .unwrap()
            };
            let state_at_ri = c
                .builder
                .build_load(i8_ty, state_ptr_at_ri, "old_state")
                .unwrap()
                .into_int_value();
            let is_occupied = c
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::EQ,
                    state_at_ri,
                    i8_ty.const_int(1, false),
                    "is_occ",
                )
                .unwrap();
            c.builder
                .build_conditional_branch(is_occupied, rehash_probe, rehash_next)
                .unwrap();

            c.builder.position_at_end(rehash_probe);
            let old_entry_offset = c
                .builder
                .build_int_mul(ri, i64_ty.const_int(elem_size, false), "old_off")
                .unwrap();
            let old_entry_ptr = unsafe {
                c.builder
                    .build_gep(i8_ty, entries_ptr, &[old_entry_offset], "old_entry")
                    .unwrap()
            };
            let old_key = c
                .builder
                .build_load(elem_llvm, old_entry_ptr, "old_key")
                .unwrap();
            let old_hash = c
                .call(hash_fn, &[old_key.into()], "old_hash")
                .unwrap()
                .into_int_value();
            let new_mask = c
                .builder
                .build_int_sub(new_cap, i64_ty.const_int(1, false), "new_mask")
                .unwrap();
            let new_slot_init = c
                .builder
                .build_and(old_hash, new_mask, "new_slot_init")
                .unwrap();

            let rehash_probe_loop = c.context.append_basic_block(fn_val, "rehash_probe_loop");
            let rehash_advance = c.context.append_basic_block(fn_val, "rehash_advance");
            c.builder
                .build_unconditional_branch(rehash_probe_loop)
                .unwrap();
            c.builder.position_at_end(rehash_probe_loop);

            let phi_slot = c.builder.build_phi(i64_ty, "rp_slot").unwrap();
            phi_slot.add_incoming(&[(&new_slot_init, rehash_probe)]);
            let rp_slot = phi_slot.as_basic_value().into_int_value();
            let new_state_at = unsafe {
                c.builder
                    .build_gep(i8_ty, new_states_ptr, &[rp_slot], "ns_ptr")
                    .unwrap()
            };
            let ns_val = c
                .builder
                .build_load(i8_ty, new_state_at, "ns_val")
                .unwrap()
                .into_int_value();
            let ns_empty = c
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::EQ,
                    ns_val,
                    i8_ty.const_int(0, false),
                    "ns_empty",
                )
                .unwrap();
            c.builder
                .build_conditional_branch(ns_empty, rehash_store, rehash_advance)
                .unwrap();

            c.builder.position_at_end(rehash_advance);
            let rp_next = c
                .builder
                .build_int_add(rp_slot, i64_ty.const_int(1, false), "rp_next_raw")
                .unwrap();
            let rp_wrapped = c.builder.build_and(rp_next, new_mask, "rp_next").unwrap();
            phi_slot.add_incoming(&[(&rp_wrapped, rehash_advance)]);
            c.builder
                .build_unconditional_branch(rehash_probe_loop)
                .unwrap();

            c.builder.position_at_end(rehash_store);
            let new_entry_offset = c
                .builder
                .build_int_mul(rp_slot, i64_ty.const_int(elem_size, false), "new_off")
                .unwrap();
            let new_entry_ptr = unsafe {
                c.builder
                    .build_gep(i8_ty, new_entries_ptr, &[new_entry_offset], "new_entry")
                    .unwrap()
            };
            c.builder.build_store(new_entry_ptr, old_key).unwrap();
            c.builder
                .build_store(new_state_at, i8_ty.const_int(1, false))
                .unwrap();
            c.builder.build_unconditional_branch(rehash_next).unwrap();

            c.builder.position_at_end(rehash_next);
            let ri_next = c
                .builder
                .build_int_add(ri, i64_ty.const_int(1, false), "ri_next")
                .unwrap();
            phi_i.add_incoming(&[(&ri_next, rehash_next)]);
            c.builder.build_unconditional_branch(rehash_bb).unwrap();

            c.builder.position_at_end(rehash_done);
            let free = *c.functions.get("free").unwrap();
            c.call_void(free, &[entries_ptr.into()], "free_old_entries");
            c.call_void(free, &[states_ptr.into()], "free_old_states");
            c.builder.build_unconditional_branch(probe_bb).unwrap();

            c.builder.position_at_end(no_resize_bb);
            c.builder.build_unconditional_branch(probe_bb).unwrap();

            // Probe for slot
            c.builder.position_at_end(probe_bb);
            let phi_eptr = c.builder.build_phi(ptr_ty, "eptr").unwrap();
            phi_eptr.add_incoming(&[
                (&new_entries_ptr, rehash_done),
                (&entries_ptr, no_resize_bb),
            ]);
            let phi_sptr = c.builder.build_phi(ptr_ty, "sptr").unwrap();
            phi_sptr.add_incoming(&[(&new_states_ptr, rehash_done), (&states_ptr, no_resize_bb)]);
            let phi_cap = c.builder.build_phi(i64_ty, "cap_phi").unwrap();
            phi_cap.add_incoming(&[(&new_cap, rehash_done), (&capacity, no_resize_bb)]);

            let final_entries = phi_eptr.as_basic_value().into_pointer_value();
            let final_states = phi_sptr.as_basic_value().into_pointer_value();
            let final_cap = phi_cap.as_basic_value().into_int_value();

            let hash_val = c
                .call(hash_fn, &[item_val.into()], "item_hash")
                .unwrap()
                .into_int_value();
            let mask = c
                .builder
                .build_int_sub(final_cap, i64_ty.const_int(1, false), "mask")
                .unwrap();
            let start_slot = c.builder.build_and(hash_val, mask, "start_slot").unwrap();

            let probe_loop_bb = c.context.append_basic_block(fn_val, "probe_loop");
            let check_occ_bb = c.context.append_basic_block(fn_val, "check_occ");
            let compare_key_bb = c.context.append_basic_block(fn_val, "compare_key");
            let already_bb = c.context.append_basic_block(fn_val, "already");
            let insert_bb = c.context.append_basic_block(fn_val, "insert");
            let advance_bb = c.context.append_basic_block(fn_val, "advance");

            c.builder.build_unconditional_branch(probe_loop_bb).unwrap();
            c.builder.position_at_end(probe_loop_bb);

            let phi_idx = c.builder.build_phi(i64_ty, "pidx").unwrap();
            phi_idx.add_incoming(&[(&start_slot, probe_bb)]);
            let pidx = phi_idx.as_basic_value().into_int_value();

            let s_ptr = unsafe {
                c.builder
                    .build_gep(i8_ty, final_states, &[pidx], "s_ptr")
                    .unwrap()
            };
            let s_val = c
                .builder
                .build_load(i8_ty, s_ptr, "s_val")
                .unwrap()
                .into_int_value();
            let is_empty = c
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::EQ,
                    s_val,
                    i8_ty.const_int(0, false),
                    "is_empty",
                )
                .unwrap();
            c.builder
                .build_conditional_branch(is_empty, insert_bb, check_occ_bb)
                .unwrap();

            c.builder.position_at_end(check_occ_bb);
            let is_occ = c
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::EQ,
                    s_val,
                    i8_ty.const_int(1, false),
                    "is_occ",
                )
                .unwrap();
            c.builder
                .build_conditional_branch(is_occ, compare_key_bb, insert_bb)
                .unwrap();

            c.builder.position_at_end(compare_key_bb);
            let e_off = c
                .builder
                .build_int_mul(pidx, i64_ty.const_int(elem_size, false), "e_off")
                .unwrap();
            let e_ptr = unsafe {
                c.builder
                    .build_gep(i8_ty, final_entries, &[e_off], "e_ptr")
                    .unwrap()
            };
            let existing = c.builder.build_load(elem_llvm, e_ptr, "existing").unwrap();
            let keys_equal = c
                .call(eq_fn, &[item_val.into(), existing.into()], "eq")
                .unwrap()
                .into_int_value();
            c.builder
                .build_conditional_branch(keys_equal, already_bb, advance_bb)
                .unwrap();

            // Already present: return unchanged
            c.builder.position_at_end(already_bb);
            let result = set_struct.get_undef();
            let result = c
                .builder
                .build_insert_value(result, final_entries, 0, "a_e")
                .unwrap()
                .into_struct_value();
            let result = c
                .builder
                .build_insert_value(result, final_states, 1, "a_s")
                .unwrap()
                .into_struct_value();
            let result = c
                .builder
                .build_insert_value(result, length, 2, "a_l")
                .unwrap()
                .into_struct_value();
            let result = c
                .builder
                .build_insert_value(result, final_cap, 3, "a_c")
                .unwrap()
                .into_struct_value();
            c.builder.build_return(Some(&result)).unwrap();

            // Insert new element
            c.builder.position_at_end(insert_bb);
            let ins_off = c
                .builder
                .build_int_mul(pidx, i64_ty.const_int(elem_size, false), "ins_off")
                .unwrap();
            let ins_ptr = unsafe {
                c.builder
                    .build_gep(i8_ty, final_entries, &[ins_off], "ins_ptr")
                    .unwrap()
            };
            c.builder.build_store(ins_ptr, item_val).unwrap();
            c.builder
                .build_store(s_ptr, i8_ty.const_int(1, false))
                .unwrap();
            let new_len = c
                .builder
                .build_int_add(length, i64_ty.const_int(1, false), "new_len")
                .unwrap();
            let result = set_struct.get_undef();
            let result = c
                .builder
                .build_insert_value(result, final_entries, 0, "i_e")
                .unwrap()
                .into_struct_value();
            let result = c
                .builder
                .build_insert_value(result, final_states, 1, "i_s")
                .unwrap()
                .into_struct_value();
            let result = c
                .builder
                .build_insert_value(result, new_len, 2, "i_l")
                .unwrap()
                .into_struct_value();
            let result = c
                .builder
                .build_insert_value(result, final_cap, 3, "i_c")
                .unwrap()
                .into_struct_value();
            c.builder.build_return(Some(&result)).unwrap();

            c.builder.position_at_end(advance_bb);
            let next_idx = c
                .builder
                .build_int_add(pidx, i64_ty.const_int(1, false), "next_idx")
                .unwrap();
            let wrapped = c.builder.build_and(next_idx, mask, "wrapped").unwrap();
            phi_idx.add_incoming(&[(&wrapped, advance_bb)]);
            c.builder.build_unconditional_branch(probe_loop_bb).unwrap();

            if let Some(bb) = saved_block {
                c.builder.position_at_end(bb);
            }
        }

        "has?" => {
            let fn_type = i1_ty.fn_type(&[set_struct.into(), elem_llvm.into()], false);
            let fn_val = c.module.add_function(mangled_fn, fn_type, None);
            c.functions.insert(mangled_fn.to_string(), fn_val);

            let entry_bb = c.context.append_basic_block(fn_val, "entry");
            let saved_block = c.builder.get_insert_block();
            c.builder.position_at_end(entry_bb);

            let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
            let item_val = fn_val.get_nth_param(1).unwrap();

            let entries_ptr = c
                .builder
                .build_extract_value(self_val, 0, "entries")
                .unwrap()
                .into_pointer_value();
            let states_ptr = c
                .builder
                .build_extract_value(self_val, 1, "states")
                .unwrap()
                .into_pointer_value();
            let capacity = c
                .builder
                .build_extract_value(self_val, 3, "cap")
                .unwrap()
                .into_int_value();

            let hash_val = c
                .call(hash_fn, &[item_val.into()], "item_hash")
                .unwrap()
                .into_int_value();
            let mask = c
                .builder
                .build_int_sub(capacity, i64_ty.const_int(1, false), "mask")
                .unwrap();
            let start_slot = c.builder.build_and(hash_val, mask, "start").unwrap();

            let probe_bb = c.context.append_basic_block(fn_val, "probe");
            let check_bb = c.context.append_basic_block(fn_val, "check");
            let cmp_bb = c.context.append_basic_block(fn_val, "cmp");
            let found_bb = c.context.append_basic_block(fn_val, "found");
            let not_found_bb = c.context.append_basic_block(fn_val, "not_found");
            let advance_bb = c.context.append_basic_block(fn_val, "advance");

            c.builder.build_unconditional_branch(probe_bb).unwrap();
            c.builder.position_at_end(probe_bb);

            let phi_idx = c.builder.build_phi(i64_ty, "pidx").unwrap();
            phi_idx.add_incoming(&[(&start_slot, entry_bb)]);
            let pidx = phi_idx.as_basic_value().into_int_value();

            let s_ptr = unsafe {
                c.builder
                    .build_gep(i8_ty, states_ptr, &[pidx], "s_ptr")
                    .unwrap()
            };
            let s_val = c
                .builder
                .build_load(i8_ty, s_ptr, "s_val")
                .unwrap()
                .into_int_value();
            let is_empty = c
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::EQ,
                    s_val,
                    i8_ty.const_int(0, false),
                    "is_empty",
                )
                .unwrap();
            c.builder
                .build_conditional_branch(is_empty, not_found_bb, check_bb)
                .unwrap();

            c.builder.position_at_end(check_bb);
            let is_occ = c
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::EQ,
                    s_val,
                    i8_ty.const_int(1, false),
                    "is_occ",
                )
                .unwrap();
            c.builder
                .build_conditional_branch(is_occ, cmp_bb, advance_bb)
                .unwrap();

            c.builder.position_at_end(cmp_bb);
            let e_off = c
                .builder
                .build_int_mul(pidx, i64_ty.const_int(elem_size, false), "e_off")
                .unwrap();
            let e_ptr = unsafe {
                c.builder
                    .build_gep(i8_ty, entries_ptr, &[e_off], "e_ptr")
                    .unwrap()
            };
            let existing = c.builder.build_load(elem_llvm, e_ptr, "existing").unwrap();
            let keys_equal = c
                .call(eq_fn, &[item_val.into(), existing.into()], "eq")
                .unwrap()
                .into_int_value();
            c.builder
                .build_conditional_branch(keys_equal, found_bb, advance_bb)
                .unwrap();

            c.builder.position_at_end(found_bb);
            c.builder
                .build_return(Some(&i1_ty.const_int(1, false)))
                .unwrap();

            c.builder.position_at_end(not_found_bb);
            c.builder
                .build_return(Some(&i1_ty.const_int(0, false)))
                .unwrap();

            c.builder.position_at_end(advance_bb);
            let next_idx = c
                .builder
                .build_int_add(pidx, i64_ty.const_int(1, false), "next")
                .unwrap();
            let wrapped = c.builder.build_and(next_idx, mask, "wrapped").unwrap();
            phi_idx.add_incoming(&[(&wrapped, advance_bb)]);
            c.builder.build_unconditional_branch(probe_bb).unwrap();

            if let Some(bb) = saved_block {
                c.builder.position_at_end(bb);
            }
        }

        "remove" => {
            let fn_type = set_struct.fn_type(&[set_struct.into(), elem_llvm.into()], false);
            let fn_val = c.module.add_function(mangled_fn, fn_type, None);
            c.functions.insert(mangled_fn.to_string(), fn_val);

            let entry_bb = c.context.append_basic_block(fn_val, "entry");
            let saved_block = c.builder.get_insert_block();
            c.builder.position_at_end(entry_bb);

            let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
            let item_val = fn_val.get_nth_param(1).unwrap();

            let entries_ptr = c
                .builder
                .build_extract_value(self_val, 0, "entries")
                .unwrap()
                .into_pointer_value();
            let states_ptr = c
                .builder
                .build_extract_value(self_val, 1, "states")
                .unwrap()
                .into_pointer_value();
            let length = c
                .builder
                .build_extract_value(self_val, 2, "len")
                .unwrap()
                .into_int_value();
            let capacity = c
                .builder
                .build_extract_value(self_val, 3, "cap")
                .unwrap()
                .into_int_value();

            let hash_val = c
                .call(hash_fn, &[item_val.into()], "item_hash")
                .unwrap()
                .into_int_value();
            let mask = c
                .builder
                .build_int_sub(capacity, i64_ty.const_int(1, false), "mask")
                .unwrap();
            let start_slot = c.builder.build_and(hash_val, mask, "start").unwrap();

            let probe_bb = c.context.append_basic_block(fn_val, "probe");
            let check_bb = c.context.append_basic_block(fn_val, "check");
            let cmp_bb = c.context.append_basic_block(fn_val, "cmp");
            let found_bb = c.context.append_basic_block(fn_val, "found");
            let not_found_bb = c.context.append_basic_block(fn_val, "not_found");
            let advance_bb = c.context.append_basic_block(fn_val, "advance");

            c.builder.build_unconditional_branch(probe_bb).unwrap();
            c.builder.position_at_end(probe_bb);

            let phi_idx = c.builder.build_phi(i64_ty, "pidx").unwrap();
            phi_idx.add_incoming(&[(&start_slot, entry_bb)]);
            let pidx = phi_idx.as_basic_value().into_int_value();

            let s_ptr = unsafe {
                c.builder
                    .build_gep(i8_ty, states_ptr, &[pidx], "s_ptr")
                    .unwrap()
            };
            let s_val = c
                .builder
                .build_load(i8_ty, s_ptr, "s_val")
                .unwrap()
                .into_int_value();
            let is_empty = c
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::EQ,
                    s_val,
                    i8_ty.const_int(0, false),
                    "is_empty",
                )
                .unwrap();
            c.builder
                .build_conditional_branch(is_empty, not_found_bb, check_bb)
                .unwrap();

            c.builder.position_at_end(check_bb);
            let is_occ = c
                .builder
                .build_int_compare(
                    inkwell::IntPredicate::EQ,
                    s_val,
                    i8_ty.const_int(1, false),
                    "is_occ",
                )
                .unwrap();
            c.builder
                .build_conditional_branch(is_occ, cmp_bb, advance_bb)
                .unwrap();

            c.builder.position_at_end(cmp_bb);
            let e_off = c
                .builder
                .build_int_mul(pidx, i64_ty.const_int(elem_size, false), "e_off")
                .unwrap();
            let e_ptr = unsafe {
                c.builder
                    .build_gep(i8_ty, entries_ptr, &[e_off], "e_ptr")
                    .unwrap()
            };
            let existing = c.builder.build_load(elem_llvm, e_ptr, "existing").unwrap();
            let keys_equal = c
                .call(eq_fn, &[item_val.into(), existing.into()], "eq")
                .unwrap()
                .into_int_value();
            c.builder
                .build_conditional_branch(keys_equal, found_bb, advance_bb)
                .unwrap();

            c.builder.position_at_end(found_bb);
            c.builder
                .build_store(s_ptr, i8_ty.const_int(2, false))
                .unwrap();
            let new_len = c
                .builder
                .build_int_sub(length, i64_ty.const_int(1, false), "new_len")
                .unwrap();
            let result = set_struct.get_undef();
            let result = c
                .builder
                .build_insert_value(result, entries_ptr, 0, "r_e")
                .unwrap()
                .into_struct_value();
            let result = c
                .builder
                .build_insert_value(result, states_ptr, 1, "r_s")
                .unwrap()
                .into_struct_value();
            let result = c
                .builder
                .build_insert_value(result, new_len, 2, "r_l")
                .unwrap()
                .into_struct_value();
            let result = c
                .builder
                .build_insert_value(result, capacity, 3, "r_c")
                .unwrap()
                .into_struct_value();
            c.builder.build_return(Some(&result)).unwrap();

            c.builder.position_at_end(not_found_bb);
            c.builder.build_return(Some(&self_val)).unwrap();

            c.builder.position_at_end(advance_bb);
            let next_idx = c
                .builder
                .build_int_add(pidx, i64_ty.const_int(1, false), "next")
                .unwrap();
            let wrapped = c.builder.build_and(next_idx, mask, "wrapped").unwrap();
            phi_idx.add_incoming(&[(&wrapped, advance_bb)]);
            c.builder.build_unconditional_branch(probe_bb).unwrap();

            if let Some(bb) = saved_block {
                c.builder.position_at_end(bb);
            }
        }

        "length" => {
            hashtable::emit_hashtable_length(c, mangled_fn, set_struct)?;
            return Ok(EmitResult::Emitted);
        }

        "empty?" => {
            hashtable::emit_hashtable_empty(c, mangled_fn, set_struct)?;
            return Ok(EmitResult::Emitted);
        }

        "from_list" => {
            let list_id = TypeIdentifier::std("List");
            let list_mangled = mangle_name(&list_id, std::slice::from_ref(elem_type));
            monomorphize_struct(c, &list_id, std::slice::from_ref(elem_type))?;
            let list_struct = c
                .llvm_types
                .get_monomorphized(&list_mangled)
                .ok_or_else(|| format!("no LLVM type for {list_mangled}"))?;

            let fn_type = set_struct.fn_type(&[list_struct.into()], false);
            let fn_val = c.module.add_function(mangled_fn, fn_type, None);
            c.functions.insert(mangled_fn.to_string(), fn_val);

            let entry_bb = c.context.append_basic_block(fn_val, "entry");
            let loop_bb = c.context.append_basic_block(fn_val, "loop");
            let body_bb = c.context.append_basic_block(fn_val, "body");
            let done_bb = c.context.append_basic_block(fn_val, "done");
            let saved_block = c.builder.get_insert_block();
            c.builder.position_at_end(entry_bb);

            let list_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
            let list_ptr = c
                .builder
                .build_extract_value(list_val, 0, "list_ptr")
                .unwrap()
                .into_pointer_value();
            let list_len = c
                .builder
                .build_extract_value(list_val, 1, "list_len")
                .unwrap()
                .into_int_value();

            let new_fn_name = format!("{mangled_type}_new");
            let _ = emit_set_method(c, mangled_type, &new_fn_name, "new", type_args)?;
            let new_fn = *c.functions.get(&new_fn_name).unwrap();
            let init_set = c.call(new_fn, &[], "init_set").unwrap().into_struct_value();

            let set_alloca = c.builder.build_alloca(set_struct, "set_acc").unwrap();
            c.builder.build_store(set_alloca, init_set).unwrap();

            c.builder.build_unconditional_branch(loop_bb).unwrap();
            c.builder.position_at_end(loop_bb);

            let phi_i = c.builder.build_phi(i64_ty, "i").unwrap();
            phi_i.add_incoming(&[(&i64_ty.const_int(0, false), entry_bb)]);
            let i_val = phi_i.as_basic_value().into_int_value();

            let done_cond = c
                .builder
                .build_int_compare(inkwell::IntPredicate::UGE, i_val, list_len, "done")
                .unwrap();
            c.builder
                .build_conditional_branch(done_cond, done_bb, body_bb)
                .unwrap();

            c.builder.position_at_end(body_bb);
            let byte_offset = c
                .builder
                .build_int_mul(i_val, i64_ty.const_int(elem_size, false), "byte_off")
                .unwrap();
            let elem_ptr = unsafe {
                c.builder
                    .build_gep(i8_ty, list_ptr, &[byte_offset], "elem_ptr")
                    .unwrap()
            };
            let elem_val = c.builder.build_load(elem_llvm, elem_ptr, "elem").unwrap();

            let insert_fn_name = format!("{mangled_type}_insert");
            let _ = emit_set_method(c, mangled_type, &insert_fn_name, "insert", type_args)?;
            let insert_fn = *c.functions.get(&insert_fn_name).unwrap();

            let current_set = c
                .builder
                .build_load(set_struct, set_alloca, "cur_set")
                .unwrap();
            let new_set = c
                .call(insert_fn, &[current_set.into(), elem_val.into()], "new_set")
                .unwrap();
            c.builder.build_store(set_alloca, new_set).unwrap();

            let next_i = c
                .builder
                .build_int_add(i_val, i64_ty.const_int(1, false), "next_i")
                .unwrap();
            phi_i.add_incoming(&[(&next_i, body_bb)]);
            c.builder.build_unconditional_branch(loop_bb).unwrap();

            c.builder.position_at_end(done_bb);
            let final_set = c
                .builder
                .build_load(set_struct, set_alloca, "final_set")
                .unwrap();
            c.builder.build_return(Some(&final_set)).unwrap();

            if let Some(bb) = saved_block {
                c.builder.position_at_end(bb);
            }
        }

        _ => return Ok(EmitResult::NotIntrinsic),
    }

    Ok(EmitResult::Emitted)
}
