//! Codegen for `Map<K,V>` intrinsic methods.
//!
//! Map is a hash-table-backed associative container using open addressing
//! with linear probing. Layout matches the shared hashtable struct:
//! `{ entries_ptr: i8*, states_ptr: i8*, length: i64, capacity: i64 }`

use expo_typecheck::types::{GenericKind, Type};

use crate::compiler::{Compiler, EmitResult, type_byte_size};
use crate::hashtable;
use crate::types::to_llvm_type;

pub fn emit_map_method<'ctx>(
    c: &mut Compiler<'ctx>,
    mangled_type: &str,
    mangled_fn: &str,
    method_name: &str,
    type_args: &[Type],
) -> Result<EmitResult, String> {
    let map_struct = *c
        .struct_types
        .get(mangled_type)
        .ok_or_else(|| format!("no LLVM type for `{mangled_type}`"))?;

    if type_args.len() < 2 {
        return Err("Map requires two type arguments <K, V>".to_string());
    }
    let key_type = &type_args[0];
    let val_type = &type_args[1];

    let key_llvm = to_llvm_type(key_type, c.context, &c.struct_types)
        .ok_or_else(|| format!("no LLVM type for Map key `{key_type:?}`"))?;
    let val_llvm = to_llvm_type(val_type, c.context, &c.struct_types)
        .ok_or_else(|| format!("no LLVM type for Map value `{val_type:?}`"))?;

    let key_size = type_byte_size(key_type) as u64;
    let val_size = type_byte_size(val_type) as u64;
    let entry_size = key_size + val_size;

    let hash_fn = hashtable::ensure_hash_fn(c, key_type)?;
    let eq_fn = hashtable::ensure_eq_fn(c, key_type)?;

    let i64_ty = c.context.i64_type();
    let i32_ty = c.context.i32_type();
    let i8_ty = c.context.i8_type();
    let i1_ty = c.context.bool_type();
    let ptr_ty = c.context.ptr_type(inkwell::AddressSpace::default());

    match method_name {
        "new" => {
            hashtable::emit_hashtable_new(c, mangled_fn, map_struct, entry_size)?;
            return Ok(EmitResult::Emitted);
        }

        "put" => {
            let fn_type = map_struct.fn_type(
                &[map_struct.into(), key_llvm.into(), val_llvm.into()],
                false,
            );
            let fn_val = c.module.add_function(mangled_fn, fn_type, None);
            c.functions.insert(mangled_fn.to_string(), fn_val);

            let entry_bb = c.context.append_basic_block(fn_val, "entry");
            let saved_block = c.builder.get_insert_block();
            c.builder.position_at_end(entry_bb);

            let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
            let key_val = fn_val.get_nth_param(1).unwrap();
            let value_val = fn_val.get_nth_param(2).unwrap();

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

            // Check load factor: if (length + 1) * 4 > capacity * 3, resize
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

            // Resize block
            c.builder.position_at_end(need_resize_bb);
            let new_cap = c
                .builder
                .build_int_mul(capacity, i64_ty.const_int(2, false), "new_cap")
                .unwrap();
            let new_entries_bytes = c
                .builder
                .build_int_mul(
                    new_cap,
                    i64_ty.const_int(entry_size, false),
                    "new_entries_bytes",
                )
                .unwrap();
            let malloc = *c.functions.get("malloc").unwrap();
            let new_entries_ptr = c
                .builder
                .build_call(malloc, &[new_entries_bytes.into()], "new_entries")
                .unwrap()
                .try_as_basic_value()
                .left()
                .unwrap()
                .into_pointer_value();
            let new_states_ptr = c
                .builder
                .build_call(malloc, &[new_cap.into()], "new_states")
                .unwrap()
                .try_as_basic_value()
                .left()
                .unwrap()
                .into_pointer_value();
            let memset = *c.functions.get("memset").unwrap();
            c.builder
                .build_call(
                    memset,
                    &[
                        new_states_ptr.into(),
                        i32_ty.const_int(0, false).into(),
                        new_cap.into(),
                    ],
                    "clear_new_states",
                )
                .unwrap();

            // Rehash loop
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
                .build_int_mul(ri, i64_ty.const_int(entry_size, false), "old_off")
                .unwrap();
            let old_entry_ptr = unsafe {
                c.builder
                    .build_gep(i8_ty, entries_ptr, &[old_entry_offset], "old_entry")
                    .unwrap()
            };
            let old_key = c
                .builder
                .build_load(key_llvm, old_entry_ptr, "old_key")
                .unwrap();

            let old_hash = c
                .builder
                .build_call(hash_fn, &[old_key.into()], "old_hash")
                .unwrap()
                .try_as_basic_value()
                .left()
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
                .build_int_mul(rp_slot, i64_ty.const_int(entry_size, false), "new_off")
                .unwrap();
            let new_entry_ptr = unsafe {
                c.builder
                    .build_gep(i8_ty, new_entries_ptr, &[new_entry_offset], "new_entry")
                    .unwrap()
            };

            let memcpy = *c.functions.get("memcpy").unwrap();
            c.builder
                .build_call(
                    memcpy,
                    &[
                        new_entry_ptr.into(),
                        old_entry_ptr.into(),
                        i64_ty.const_int(entry_size, false).into(),
                    ],
                    "rehash_copy",
                )
                .unwrap();
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

            // Free old buffers
            c.builder.position_at_end(rehash_done);
            let free = *c.functions.get("free").unwrap();
            c.builder
                .build_call(free, &[entries_ptr.into()], "free_old_entries")
                .unwrap();
            c.builder
                .build_call(free, &[states_ptr.into()], "free_old_states")
                .unwrap();
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
                .builder
                .build_call(hash_fn, &[key_val.into()], "key_hash")
                .unwrap()
                .try_as_basic_value()
                .left()
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
            let update_bb = c.context.append_basic_block(fn_val, "update");
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
                .build_int_mul(pidx, i64_ty.const_int(entry_size, false), "e_off")
                .unwrap();
            let e_ptr = unsafe {
                c.builder
                    .build_gep(i8_ty, final_entries, &[e_off], "e_ptr")
                    .unwrap()
            };
            let existing_key = c
                .builder
                .build_load(key_llvm, e_ptr, "existing_key")
                .unwrap();
            let keys_equal = c
                .builder
                .build_call(eq_fn, &[key_val.into(), existing_key.into()], "keys_eq")
                .unwrap()
                .try_as_basic_value()
                .left()
                .unwrap()
                .into_int_value();
            c.builder
                .build_conditional_branch(keys_equal, update_bb, advance_bb)
                .unwrap();

            // Update existing value
            c.builder.position_at_end(update_bb);
            let e_off2 = c
                .builder
                .build_int_mul(pidx, i64_ty.const_int(entry_size, false), "e_off2")
                .unwrap();
            let e_ptr2 = unsafe {
                c.builder
                    .build_gep(i8_ty, final_entries, &[e_off2], "e_ptr2")
                    .unwrap()
            };
            let val_ptr = unsafe {
                c.builder
                    .build_gep(
                        i8_ty,
                        e_ptr2,
                        &[i64_ty.const_int(key_size, false)],
                        "val_ptr",
                    )
                    .unwrap()
            };
            c.builder.build_store(val_ptr, value_val).unwrap();

            let result = map_struct.get_undef();
            let result = c
                .builder
                .build_insert_value(result, final_entries, 0, "u_e")
                .unwrap()
                .into_struct_value();
            let result = c
                .builder
                .build_insert_value(result, final_states, 1, "u_s")
                .unwrap()
                .into_struct_value();
            let result = c
                .builder
                .build_insert_value(result, length, 2, "u_l")
                .unwrap()
                .into_struct_value();
            let result = c
                .builder
                .build_insert_value(result, final_cap, 3, "u_c")
                .unwrap()
                .into_struct_value();
            c.builder.build_return(Some(&result)).unwrap();

            // Insert new entry
            c.builder.position_at_end(insert_bb);
            let ins_off = c
                .builder
                .build_int_mul(pidx, i64_ty.const_int(entry_size, false), "ins_off")
                .unwrap();
            let ins_ptr = unsafe {
                c.builder
                    .build_gep(i8_ty, final_entries, &[ins_off], "ins_ptr")
                    .unwrap()
            };
            c.builder.build_store(ins_ptr, key_val).unwrap();
            let ins_val_ptr = unsafe {
                c.builder
                    .build_gep(
                        i8_ty,
                        ins_ptr,
                        &[i64_ty.const_int(key_size, false)],
                        "ins_val_ptr",
                    )
                    .unwrap()
            };
            c.builder.build_store(ins_val_ptr, value_val).unwrap();
            c.builder
                .build_store(s_ptr, i8_ty.const_int(1, false))
                .unwrap();

            let new_len = c
                .builder
                .build_int_add(length, i64_ty.const_int(1, false), "new_len")
                .unwrap();
            let result = map_struct.get_undef();
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

            // Advance probe
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

        "get" => {
            let option_type_args = vec![val_type.clone()];
            let option_mangled = expo_typecheck::types::mangle_name("Option", &option_type_args);
            c.ensure_types_exist(&Type::GenericInstance {
                base: "Option".to_string(),
                type_args: option_type_args.clone(),
                kind: GenericKind::Enum,
            })?;
            let option_struct = *c
                .struct_types
                .get(&option_mangled)
                .ok_or_else(|| format!("no LLVM type for {option_mangled}"))?;

            let fn_type = option_struct.fn_type(&[map_struct.into(), key_llvm.into()], false);
            let fn_val = c.module.add_function(mangled_fn, fn_type, None);
            c.functions.insert(mangled_fn.to_string(), fn_val);

            let entry_bb = c.context.append_basic_block(fn_val, "entry");
            let saved_block = c.builder.get_insert_block();
            c.builder.position_at_end(entry_bb);

            let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
            let key_val = fn_val.get_nth_param(1).unwrap();

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
                .builder
                .build_call(hash_fn, &[key_val.into()], "key_hash")
                .unwrap()
                .try_as_basic_value()
                .left()
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
                .build_int_mul(pidx, i64_ty.const_int(entry_size, false), "e_off")
                .unwrap();
            let e_ptr = unsafe {
                c.builder
                    .build_gep(i8_ty, entries_ptr, &[e_off], "e_ptr")
                    .unwrap()
            };
            let existing_key = c
                .builder
                .build_load(key_llvm, e_ptr, "existing_key")
                .unwrap();
            let keys_equal = c
                .builder
                .build_call(eq_fn, &[key_val.into(), existing_key.into()], "keys_eq")
                .unwrap()
                .try_as_basic_value()
                .left()
                .unwrap()
                .into_int_value();
            c.builder
                .build_conditional_branch(keys_equal, found_bb, advance_bb)
                .unwrap();

            // Found: return Some(value)
            c.builder.position_at_end(found_bb);
            let val_ptr = unsafe {
                c.builder
                    .build_gep(
                        i8_ty,
                        e_ptr,
                        &[i64_ty.const_int(key_size, false)],
                        "val_ptr",
                    )
                    .unwrap()
            };
            let val = c.builder.build_load(val_llvm, val_ptr, "val").unwrap();

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

            // Not found: return None
            c.builder.position_at_end(not_found_bb);
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

            // Advance probe
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

        "has?" => {
            let fn_type = i1_ty.fn_type(&[map_struct.into(), key_llvm.into()], false);
            let fn_val = c.module.add_function(mangled_fn, fn_type, None);
            c.functions.insert(mangled_fn.to_string(), fn_val);

            let entry_bb = c.context.append_basic_block(fn_val, "entry");
            let saved_block = c.builder.get_insert_block();
            c.builder.position_at_end(entry_bb);

            let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
            let key_val = fn_val.get_nth_param(1).unwrap();

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
                .builder
                .build_call(hash_fn, &[key_val.into()], "key_hash")
                .unwrap()
                .try_as_basic_value()
                .left()
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
                .build_int_mul(pidx, i64_ty.const_int(entry_size, false), "e_off")
                .unwrap();
            let e_ptr = unsafe {
                c.builder
                    .build_gep(i8_ty, entries_ptr, &[e_off], "e_ptr")
                    .unwrap()
            };
            let existing_key = c
                .builder
                .build_load(key_llvm, e_ptr, "existing_key")
                .unwrap();
            let keys_equal = c
                .builder
                .build_call(eq_fn, &[key_val.into(), existing_key.into()], "keys_eq")
                .unwrap()
                .try_as_basic_value()
                .left()
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
            let fn_type = map_struct.fn_type(&[map_struct.into(), key_llvm.into()], false);
            let fn_val = c.module.add_function(mangled_fn, fn_type, None);
            c.functions.insert(mangled_fn.to_string(), fn_val);

            let entry_bb = c.context.append_basic_block(fn_val, "entry");
            let saved_block = c.builder.get_insert_block();
            c.builder.position_at_end(entry_bb);

            let self_val = fn_val.get_nth_param(0).unwrap().into_struct_value();
            let key_val = fn_val.get_nth_param(1).unwrap();

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
                .builder
                .build_call(hash_fn, &[key_val.into()], "key_hash")
                .unwrap()
                .try_as_basic_value()
                .left()
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
                .build_int_mul(pidx, i64_ty.const_int(entry_size, false), "e_off")
                .unwrap();
            let e_ptr = unsafe {
                c.builder
                    .build_gep(i8_ty, entries_ptr, &[e_off], "e_ptr")
                    .unwrap()
            };
            let existing_key = c
                .builder
                .build_load(key_llvm, e_ptr, "existing_key")
                .unwrap();
            let keys_equal = c
                .builder
                .build_call(eq_fn, &[key_val.into(), existing_key.into()], "keys_eq")
                .unwrap()
                .try_as_basic_value()
                .left()
                .unwrap()
                .into_int_value();
            c.builder
                .build_conditional_branch(keys_equal, found_bb, advance_bb)
                .unwrap();

            // Tombstone the slot
            c.builder.position_at_end(found_bb);
            c.builder
                .build_store(s_ptr, i8_ty.const_int(2, false))
                .unwrap();
            let new_len = c
                .builder
                .build_int_sub(length, i64_ty.const_int(1, false), "new_len")
                .unwrap();
            let result = map_struct.get_undef();
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
            hashtable::emit_hashtable_length(c, mangled_fn, map_struct)?;
            return Ok(EmitResult::Emitted);
        }

        "empty?" => {
            hashtable::emit_hashtable_empty(c, mangled_fn, map_struct)?;
            return Ok(EmitResult::Emitted);
        }

        "from_map" => {
            let fn_type = map_struct.fn_type(&[map_struct.into()], false);
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

        _ => return Ok(EmitResult::NotIntrinsic),
    }

    Ok(EmitResult::Emitted)
}
