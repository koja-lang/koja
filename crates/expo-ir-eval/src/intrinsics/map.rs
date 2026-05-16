//! `Map<K, V>` family — heap-backed associative container. Eval
//! mirrors the LLVM ABI's by-value semantics from the outside
//! (every method takes / returns a `Value::Map`) but stores entries
//! in a shared `Rc<RefCell<Vec<(Value, Value)>>>` so move-self
//! mutators (`put`, `remove`) can mutate in place. A linear probe
//! over the entry vec gives the right semantics — the LLVM
//! backend's open-addressing hash table is purely a perf detail
//! that's invisible at the Expo level.
//!
//! `get` materializes an `Option<V>` value directly. The receiver
//! symbol for the option shape flows from `function.return_type`.

use std::cell::RefCell;
use std::rc::Rc;

use expo_ir::{IRFunction, MapMethod};

use crate::error::RuntimeError;
use crate::intrinsics::helpers;
use crate::value::{MapEntries, Value};

pub(super) fn dispatch(
    method: MapMethod,
    function: &IRFunction,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    match method {
        MapMethod::EmptyQ => empty_q(args),
        MapMethod::FromMap => from_map(args),
        MapMethod::Get => get(function, args),
        MapMethod::HasQ => has_q(args),
        MapMethod::Length => length(args),
        MapMethod::New => new(),
        MapMethod::Put => put(args),
        MapMethod::Remove => remove(args),
    }
}

fn new() -> Result<Value, RuntimeError> {
    Ok(Value::Map(Rc::new(RefCell::new(Vec::new()))))
}

fn length(args: &[Value]) -> Result<Value, RuntimeError> {
    let map = expect_map(args, 0, "Map.length")?;
    Ok(Value::Int(map.borrow().len() as i64))
}

fn empty_q(args: &[Value]) -> Result<Value, RuntimeError> {
    let map = expect_map(args, 0, "Map.empty?")?;
    Ok(Value::Bool(map.borrow().is_empty()))
}

fn from_map(args: &[Value]) -> Result<Value, RuntimeError> {
    let map = expect_map(args, 0, "Map.from_map")?;
    Ok(Value::Map(map))
}

fn get(function: &IRFunction, args: &[Value]) -> Result<Value, RuntimeError> {
    let map = expect_map(args, 0, "Map.get")?;
    let key = expect_arg(args, 1, "Map.get")?.clone();
    let option_symbol = helpers::enum_return_symbol(function, "Map.get")?;
    let entries = map.borrow();
    let value = entries
        .iter()
        .find(|(k, _)| k == &key)
        .map(|(_, v)| v.clone());
    Ok(helpers::option_value(option_symbol, value))
}

fn has_q(args: &[Value]) -> Result<Value, RuntimeError> {
    let map = expect_map(args, 0, "Map.has?")?;
    let key = expect_arg(args, 1, "Map.has?")?.clone();
    let entries = map.borrow();
    Ok(Value::Bool(entries.iter().any(|(k, _)| k == &key)))
}

fn put(args: &[Value]) -> Result<Value, RuntimeError> {
    let map = expect_map(args, 0, "Map.put")?;
    let key = expect_arg(args, 1, "Map.put")?.clone();
    let value = expect_arg(args, 2, "Map.put")?.clone();
    {
        let mut entries = map.borrow_mut();
        if let Some(slot) = entries.iter_mut().find(|(k, _)| k == &key) {
            slot.1 = value;
        } else {
            entries.push((key, value));
        }
    }
    Ok(Value::Map(map))
}

fn remove(args: &[Value]) -> Result<Value, RuntimeError> {
    let map = expect_map(args, 0, "Map.remove")?;
    let key = expect_arg(args, 1, "Map.remove")?.clone();
    {
        let mut entries = map.borrow_mut();
        if let Some(idx) = entries.iter().position(|(k, _)| k == &key) {
            entries.remove(idx);
        }
    }
    Ok(Value::Map(map))
}

fn expect_arg<'a>(args: &'a [Value], index: usize, label: &str) -> Result<&'a Value, RuntimeError> {
    args.get(index).ok_or_else(|| RuntimeError::Unsupported {
        detail: format!("{label} missing arg #{index} (got {} args)", args.len()),
    })
}

fn expect_map(args: &[Value], index: usize, label: &str) -> Result<MapEntries, RuntimeError> {
    match expect_arg(args, index, label)? {
        Value::Map(entries) => Ok(entries.clone()),
        other => Err(RuntimeError::TypeMismatch {
            detail: format!("{label} arg #{index} expected Map, got `{other}`"),
        }),
    }
}
