//! `List<T>` family: heap-backed dynamic array. Eval stores elements
//! in `Rc<RefCell<Vec<Value>>>`, but under value semantics every
//! mutator (`append`, `concat`, `pop`, `replace_at`) is
//! copy-on-write: it clones the receiver's backing vec into a fresh
//! `Rc` before mutating, so a shared binding (`b = a`) is never
//! observably mutated through another alias.
//!
//! `get` and `pop` materialize `Option<T>` / `Pair<Option<T>, List<T>>`
//! values directly. The receiver symbol for the option / pair shape
//! flows from `function.return_type`. The inner Option symbol for
//! `pop` is resolved off Pair's struct decl through the resolver,
//! so neither path fabricates an [`IRSymbol`] from a string.

use std::cell::RefCell;
use std::rc::Rc;

use koja_ir::{IRFunction, IRSymbol, IRType, ListMethod};

use crate::error::RuntimeError;
use crate::interpreter::CallResolver;
use crate::intrinsics::helpers;
use crate::value::Value;

pub(super) fn dispatch<R: CallResolver>(
    method: ListMethod,
    function: &IRFunction,
    args: &[Value],
    resolver: &R,
) -> Result<Value, RuntimeError> {
    match method {
        ListMethod::Append => append(args),
        ListMethod::Concat => concat(args),
        ListMethod::EmptyQ => empty_q(args),
        ListMethod::FromList => from_list(args),
        ListMethod::Get => get(function, args),
        ListMethod::Length => length(args),
        ListMethod::New => new(),
        ListMethod::Pop => pop(function, resolver, args),
        ListMethod::ReplaceAt => replace_at(args),
        ListMethod::Slice => slice(args),
    }
}

fn new() -> Result<Value, RuntimeError> {
    Ok(Value::List(Rc::new(RefCell::new(Vec::new()))))
}

fn length(args: &[Value]) -> Result<Value, RuntimeError> {
    let list = expect_list(args, 0, "List.length")?;
    Ok(Value::Int(list.borrow().len() as i64))
}

fn empty_q(args: &[Value]) -> Result<Value, RuntimeError> {
    let list = expect_list(args, 0, "List.empty?")?;
    Ok(Value::Bool(list.borrow().is_empty()))
}

fn from_list(args: &[Value]) -> Result<Value, RuntimeError> {
    let list = expect_list(args, 0, "List.from_list")?;
    Ok(Value::List(list))
}

fn append(args: &[Value]) -> Result<Value, RuntimeError> {
    let list = expect_list(args, 0, "List.append")?;
    let item = expect_arg(args, 1, "List.append")?.clone();
    let mut items = list.borrow().clone();
    items.push(item);
    Ok(Value::List(Rc::new(RefCell::new(items))))
}

fn concat(args: &[Value]) -> Result<Value, RuntimeError> {
    let lhs = expect_list(args, 0, "List.concat")?;
    let rhs = expect_list(args, 1, "List.concat")?;
    let mut combined = lhs.borrow().clone();
    combined.extend(rhs.borrow().iter().cloned());
    Ok(Value::List(Rc::new(RefCell::new(combined))))
}

fn get(function: &IRFunction, args: &[Value]) -> Result<Value, RuntimeError> {
    let list = expect_list(args, 0, "List.get")?;
    let index = expect_int(args, 1, "List.get")?;
    let option_symbol = helpers::enum_return_symbol(function, "List.get")?;
    let items = list.borrow();
    let value = if index < 0 {
        None
    } else {
        items.get(index as usize).cloned()
    };
    Ok(helpers::option_value(option_symbol, value))
}

fn pop<R: CallResolver>(
    function: &IRFunction,
    resolver: &R,
    args: &[Value],
) -> Result<Value, RuntimeError> {
    let list = expect_list(args, 0, "List.pop")?;
    let pair_symbol = struct_return_symbol(function, "List.pop")?;
    let option_symbol = pair_first_option_symbol(&pair_symbol, resolver)?;
    let mut items = list.borrow().clone();
    let popped = items.pop();
    let option = helpers::option_value(option_symbol, popped);
    let remainder = Value::List(Rc::new(RefCell::new(items)));
    Ok(Value::Struct {
        symbol: pair_symbol,
        fields: vec![option, remainder],
    })
}

fn replace_at(args: &[Value]) -> Result<Value, RuntimeError> {
    let list = expect_list(args, 0, "List.replace_at")?;
    let index = expect_int(args, 1, "List.replace_at")?;
    let value = expect_arg(args, 2, "List.replace_at")?.clone();
    let mut items = list.borrow().clone();
    if index >= 0
        && let Some(slot) = items.get_mut(index as usize)
    {
        *slot = value;
    }
    Ok(Value::List(Rc::new(RefCell::new(items))))
}

fn slice(args: &[Value]) -> Result<Value, RuntimeError> {
    let list = expect_list(args, 0, "List.slice")?;
    let start = expect_int(args, 1, "List.slice")?.max(0) as usize;
    let count = expect_int(args, 2, "List.slice")?.max(0) as usize;
    let items = list.borrow();
    let len = items.len();
    let clamped_start = start.min(len);
    let remaining = len - clamped_start;
    let clamped_count = count.min(remaining);
    let copied: Vec<Value> = items[clamped_start..clamped_start + clamped_count].to_vec();
    Ok(Value::List(Rc::new(RefCell::new(copied))))
}

fn expect_arg<'a>(args: &'a [Value], index: usize, label: &str) -> Result<&'a Value, RuntimeError> {
    args.get(index).ok_or_else(|| RuntimeError::Unsupported {
        detail: format!("{label} missing arg #{index} (got {} args)", args.len()),
    })
}

fn expect_list(
    args: &[Value],
    index: usize,
    label: &str,
) -> Result<Rc<RefCell<Vec<Value>>>, RuntimeError> {
    match expect_arg(args, index, label)? {
        Value::List(items) => Ok(items.clone()),
        other => Err(RuntimeError::TypeMismatch {
            detail: format!("{label} arg #{index} expected List, got `{other}`"),
        }),
    }
}

fn expect_int(args: &[Value], index: usize, label: &str) -> Result<i64, RuntimeError> {
    match expect_arg(args, index, label)? {
        Value::Int(value) => Ok(*value),
        other => Err(RuntimeError::TypeMismatch {
            detail: format!("{label} arg #{index} expected Int, got `{other}`"),
        }),
    }
}

fn struct_return_symbol(function: &IRFunction, label: &str) -> Result<IRSymbol, RuntimeError> {
    match &function.return_type {
        IRType::Struct(symbol) => Ok(symbol.clone()),
        other => Err(RuntimeError::TypeMismatch {
            detail: format!("{label} expected Struct return type, got `{other:?}`"),
        }),
    }
}

/// Resolve `Pair<Option<T>, List<T>>`'s `first` field type to the
/// `Option<T>` symbol via the resolver. Keeps `IRSymbol` opaque
/// (no string-mangled fabrication).
fn pair_first_option_symbol<R: CallResolver>(
    pair_symbol: &IRSymbol,
    resolver: &R,
) -> Result<IRSymbol, RuntimeError> {
    let decl =
        resolver
            .struct_decl(pair_symbol.mangled())
            .ok_or_else(|| RuntimeError::Unsupported {
                detail: format!(
                    "List.pop: Pair struct `{pair_symbol}` missing from IR \
                 (seal invariant violation)",
                ),
            })?;
    let first = decl
        .fields
        .first()
        .ok_or_else(|| RuntimeError::Unsupported {
            detail: format!("List.pop: Pair struct `{pair_symbol}` has no fields"),
        })?;
    match &first.ir_type {
        IRType::Enum(symbol) => Ok(symbol.clone()),
        other => Err(RuntimeError::TypeMismatch {
            detail: format!("List.pop: Pair `first` expected Enum (Option), got `{other:?}`",),
        }),
    }
}
