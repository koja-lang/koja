//! `Map<K, V>` family. Every mutator here builds a fresh `Rc`. The
//! in-place twins live in [`super::consuming`]. Lookups are linear
//! probes over the entry vec.
//!
//! `get` materializes an `Option<V>` value directly. The receiver
//! symbol for the option shape flows from `function.return_type`.

use std::cell::RefCell;
use std::rc::Rc;

use koja_ir::MapMethod;

use crate::error::RuntimeError;
use crate::interpreter::CallResolver;
use crate::intrinsics::{IntrinsicCall, helpers};
use crate::value::Value;

pub(super) fn dispatch<R: CallResolver>(
    method: MapMethod,
    call: IntrinsicCall<'_, R>,
) -> Result<Value, RuntimeError> {
    match method {
        MapMethod::EmptyQ => empty_q(call.args),
        MapMethod::Get => get(call),
        MapMethod::HasQ => has_q(call.args),
        MapMethod::Length => length(call.args),
        MapMethod::New => new(),
        MapMethod::Next => next(call),
        MapMethod::Put => put(call.args),
        MapMethod::Remove => remove(call.args),
    }
}

fn new() -> Result<Value, RuntimeError> {
    Ok(Value::Map(Rc::new(RefCell::new(Vec::new()))))
}

fn length(args: &[Value]) -> Result<Value, RuntimeError> {
    let map = helpers::arg_map(args, 0, "Map.length")?;
    Ok(Value::Int(map.borrow().len() as i64))
}

fn empty_q(args: &[Value]) -> Result<Value, RuntimeError> {
    let map = helpers::arg_map(args, 0, "Map.empty?")?;
    Ok(Value::Bool(map.borrow().is_empty()))
}

fn get<R: CallResolver>(call: IntrinsicCall<'_, R>) -> Result<Value, RuntimeError> {
    let map = helpers::arg_map(call.args, 0, "Map.get")?;
    let key = helpers::arg(call.args, 1, "Map.get")?.clone();
    let option_symbol = helpers::enum_return_symbol(call.function, "Map.get")?;
    let entries = map.borrow();
    let value = entries
        .iter()
        .find(|(k, _)| k == &key)
        .map(|(_, v)| v.clone());
    helpers::option_value(option_symbol, call.resolver, value)
}

fn next<R: CallResolver>(call: IntrinsicCall<'_, R>) -> Result<Value, RuntimeError> {
    let map = helpers::arg_map(call.args, 0, "Map.next")?;
    let slot = helpers::arg_int(call.args, 1, "Map.next")?;
    let option_symbol = helpers::enum_return_symbol(call.function, "Map.next")?;
    let entries = map.borrow();
    let value = usize::try_from(slot)
        .ok()
        .and_then(|index| entries.get(index).map(|entry| (index, entry)))
        .map(|(index, (key, value))| {
            Value::Tuple(vec![
                Value::Tuple(vec![key.clone(), value.clone()]),
                Value::Int((index + 1) as i64),
            ])
        });
    helpers::option_value(option_symbol, call.resolver, value)
}

fn has_q(args: &[Value]) -> Result<Value, RuntimeError> {
    let map = helpers::arg_map(args, 0, "Map.has?")?;
    let key = helpers::arg(args, 1, "Map.has?")?.clone();
    let entries = map.borrow();
    Ok(Value::Bool(entries.iter().any(|(k, _)| k == &key)))
}

pub(super) fn put(args: &[Value]) -> Result<Value, RuntimeError> {
    let map = helpers::arg_map(args, 0, "Map.put")?;
    let key = helpers::arg(args, 1, "Map.put")?.clone();
    let value = helpers::arg(args, 2, "Map.put")?.clone();
    let mut entries = map.borrow().clone();
    if let Some(slot) = entries.iter_mut().find(|(k, _)| k == &key) {
        slot.1 = value;
    } else {
        entries.push((key, value));
    }
    Ok(Value::Map(Rc::new(RefCell::new(entries))))
}

fn remove(args: &[Value]) -> Result<Value, RuntimeError> {
    let map = helpers::arg_map(args, 0, "Map.remove")?;
    let key = helpers::arg(args, 1, "Map.remove")?.clone();
    let mut entries = map.borrow().clone();
    if let Some(idx) = entries.iter().position(|(k, _)| k == &key) {
        entries.remove(idx);
    }
    Ok(Value::Map(Rc::new(RefCell::new(entries))))
}
