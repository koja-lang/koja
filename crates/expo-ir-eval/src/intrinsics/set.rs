//! `Set<T>` family — heap-backed unique-element container. Eval
//! mirrors the LLVM ABI's by-value semantics from the outside
//! (every method takes / returns a `Value::Set`) but stores
//! elements in a shared `Rc<RefCell<Vec<Value>>>` so move-self
//! mutators (`insert`, `remove`) can mutate in place. A linear
//! probe over the element vec gives the right semantics — the LLVM
//! backend's open-addressing hash table is purely a perf detail
//! that's invisible at the Expo level.

use std::cell::RefCell;
use std::rc::Rc;

use expo_alpha_ir::{IRFunction, SetMethod};

use crate::error::RuntimeError;
use crate::value::{SetEntries, Value};

pub(super) fn dispatch(
    method: SetMethod,
    _function: &IRFunction,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    match method {
        SetMethod::EmptyQ => empty_q(args),
        SetMethod::FromList => from_list(args),
        SetMethod::HasQ => has_q(args),
        SetMethod::Insert => insert(args),
        SetMethod::Length => length(args),
        SetMethod::New => new(),
        SetMethod::Remove => remove(args),
    }
}

fn new() -> Result<Value, RuntimeError> {
    Ok(Value::Set(Rc::new(RefCell::new(Vec::new()))))
}

fn length(args: &[Value]) -> Result<Value, RuntimeError> {
    let set = expect_set(args, 0, "Set.length")?;
    Ok(Value::Int(set.borrow().len() as i64))
}

fn empty_q(args: &[Value]) -> Result<Value, RuntimeError> {
    let set = expect_set(args, 0, "Set.empty?")?;
    Ok(Value::Bool(set.borrow().is_empty()))
}

fn has_q(args: &[Value]) -> Result<Value, RuntimeError> {
    let set = expect_set(args, 0, "Set.has?")?;
    let item = expect_arg(args, 1, "Set.has?")?.clone();
    Ok(Value::Bool(set.borrow().iter().any(|v| v == &item)))
}

fn insert(args: &[Value]) -> Result<Value, RuntimeError> {
    let set = expect_set(args, 0, "Set.insert")?;
    let item = expect_arg(args, 1, "Set.insert")?.clone();
    {
        let mut items = set.borrow_mut();
        if !items.iter().any(|v| v == &item) {
            items.push(item);
        }
    }
    Ok(Value::Set(set))
}

fn remove(args: &[Value]) -> Result<Value, RuntimeError> {
    let set = expect_set(args, 0, "Set.remove")?;
    let item = expect_arg(args, 1, "Set.remove")?.clone();
    {
        let mut items = set.borrow_mut();
        if let Some(idx) = items.iter().position(|v| v == &item) {
            items.remove(idx);
        }
    }
    Ok(Value::Set(set))
}

/// `from_list(move list: List<T>) -> Set<T>` — the `ListLiteral`
/// path goes here when the resolver synthesizes
/// `Set.from_list([a, b, c])`. Walks the list once, deduping; the
/// list is consumed (move-self semantics).
fn from_list(args: &[Value]) -> Result<Value, RuntimeError> {
    let list = match expect_arg(args, 0, "Set.from_list")? {
        Value::List(items) => items.clone(),
        other => {
            return Err(RuntimeError::TypeMismatch {
                detail: format!("Set.from_list expected List, got `{other}`"),
            });
        }
    };
    let mut deduped: Vec<Value> = Vec::new();
    for item in list.borrow().iter() {
        if !deduped.iter().any(|v| v == item) {
            deduped.push(item.clone());
        }
    }
    Ok(Value::Set(Rc::new(RefCell::new(deduped))))
}

fn expect_arg<'a>(args: &'a [Value], index: usize, label: &str) -> Result<&'a Value, RuntimeError> {
    args.get(index).ok_or_else(|| RuntimeError::Unsupported {
        detail: format!("{label} missing arg #{index} (got {} args)", args.len()),
    })
}

fn expect_set(args: &[Value], index: usize, label: &str) -> Result<SetEntries, RuntimeError> {
    match expect_arg(args, index, label)? {
        Value::Set(items) => Ok(items.clone()),
        other => Err(RuntimeError::TypeMismatch {
            detail: format!("{label} arg #{index} expected Set, got `{other}`"),
        }),
    }
}
