use koja_ir::RuntimeBlockMethod;

use crate::abi;
use crate::error::RuntimeError;
use crate::value::Value;

pub(super) fn dispatch(method: RuntimeBlockMethod, args: &[Value]) -> Result<Value, RuntimeError> {
    match method {
        RuntimeBlockMethod::AdoptBinary => adopt_binary(args),
    }
}

fn adopt_binary(args: &[Value]) -> Result<Value, RuntimeError> {
    let [Value::CPtr(ptr)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!("RuntimeBlock.adopt_binary expects one CPtr argument, got {args:?}"),
        });
    };
    if ptr.is_null() {
        return Err(RuntimeError::Unsupported {
            detail: "RuntimeBlock.adopt_binary cannot adopt a null pointer".to_string(),
        });
    }
    Ok(Value::binary(abi::take_block_bytes(*ptr)))
}
