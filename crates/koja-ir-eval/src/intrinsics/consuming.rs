//! Consuming twins of the collection mutators (`List.append`,
//! `Map.put`, `Set.insert`), minted by the consume-fusion pass in
//! `koja_ir::elaborate::consume` when the receiver value is dead at
//! the call site.
//!
//! Eval values share `Rc`s freely (clone glue is an identity here),
//! so buffer reuse is gated on true runtime uniqueness rather than
//! the IR proof alone. The interpreter moves the dead receiver's
//! register into the call args, and when that leaves the backing
//! storage with a single strong reference the twin mutates it in
//! place. Any other holder (a slot awaiting overwrite in a rebind
//! loop, an earlier alias, a closure capture) forces the copying
//! original, which produces the same result. Rebind loops therefore
//! stay copying under eval, and the linear-time guarantee is pinned
//! on the LLVM backend only.

use std::rc::Rc;

use koja_ir::ConsumingMethod;

use crate::error::RuntimeError;
use crate::intrinsics::{list, map, set};
use crate::value::Value;

pub(super) fn dispatch(method: ConsumingMethod, args: &[Value]) -> Result<Value, RuntimeError> {
    match method {
        ConsumingMethod::ListAppend => append(args),
        ConsumingMethod::MapPut => put(args),
        ConsumingMethod::SetInsert => insert(args),
    }
}

fn append(args: &[Value]) -> Result<Value, RuntimeError> {
    if let Some(Value::List(items)) = args.first()
        && Rc::strong_count(items) == 1
        && let Some(item) = args.get(1)
    {
        items.borrow_mut().push(item.clone());
        return Ok(Value::List(items.clone()));
    }
    list::append(args)
}

fn put(args: &[Value]) -> Result<Value, RuntimeError> {
    if let Some(Value::Map(entries)) = args.first()
        && Rc::strong_count(entries) == 1
        && let (Some(key), Some(value)) = (args.get(1), args.get(2))
    {
        let mut entries_mut = entries.borrow_mut();
        if let Some(slot) = entries_mut.iter_mut().find(|(k, _)| k == key) {
            slot.1 = value.clone();
        } else {
            entries_mut.push((key.clone(), value.clone()));
        }
        drop(entries_mut);
        return Ok(Value::Map(entries.clone()));
    }
    map::put(args)
}

fn insert(args: &[Value]) -> Result<Value, RuntimeError> {
    if let Some(Value::Set(items)) = args.first()
        && Rc::strong_count(items) == 1
        && let Some(item) = args.get(1)
    {
        let mut items_mut = items.borrow_mut();
        if !items_mut.iter().any(|existing| existing == item) {
            items_mut.push(item.clone());
        }
        drop(items_mut);
        return Ok(Value::Set(items.clone()));
    }
    set::insert(args)
}
