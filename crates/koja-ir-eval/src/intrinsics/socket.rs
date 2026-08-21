//! `@intrinsic` methods on `Socket` from
//! [`koja/lib/net/src/net.koja`]:
//!
//! * `Socket.recv_from_raw(self, count: Int) -> Result<(Binary, Binary, Int), String>`
//! * `Socket.resolve_raw(hostname: String) -> Result<List<Binary>, String>`
//!
//! Both call the same runtime helpers the LLVM backend declares
//! (`koja_socket_resolve` / `koja_socket_recv_from`), branch on the
//! null sentinel, and unpack the returned heap buffer into eval
//! [`Value`]s, the eval analogue of the LLVM backend's
//! `intrinsics/socket.rs` emitters. Where LLVM transfers buffer
//! ownership into the constructed value, eval copies the bytes out
//! and frees the blocks through `koja_free` (keeping the runtime's
//! live-block accounting balanced).
//!
//! `recv_from` waits for the socket to be readable through eval's
//! [`crate::reactor`] (cooperatively parking the process, or blocking the
//! thread in function mode) before delegating to the native receiver, the
//! same pre-wait-then-delegate pattern as the `externs/net.rs` wrappers.

use std::cell::RefCell;
use std::ffi::CString;
use std::rc::Rc;

use koja_ir::{IRFunction, IRSymbol, IRType, SocketMethod};
use koja_runtime_core::Interest;

use crate::abi;
use crate::error::RuntimeError;
use crate::interpreter::CallResolver;
use crate::intrinsics::helpers;
use crate::reactor;
use crate::value::Value;

/// Byte count of the `i64 count` header at the front of the
/// `koja_socket_resolve` buffer. The IP-pointer array follows.
const RESOLVE_HEADER_BYTES: usize = 8;
/// Offset of `*u8 ip_bin` inside the runtime's
/// `koja_socket_recv_from` `[*u8 data, *u8 ip_bin, i64 port]` triple.
const RECV_FROM_IP_OFFSET: usize = 8;
/// Offset of `i64 port` inside the same triple.
const RECV_FROM_PORT_OFFSET: usize = 16;

unsafe extern "C" {
    fn koja_last_error() -> *mut u8;
    fn koja_socket_recv_from(fd: i32, count: i64) -> *mut u8;
    fn koja_socket_resolve(hostname: *const u8) -> *mut u8;
}

pub(super) async fn dispatch<R: CallResolver>(
    method: SocketMethod,
    function: &IRFunction,
    args: &[Value],
    resolver: &R,
) -> Result<Value, RuntimeError> {
    match method {
        SocketMethod::LastError => Ok(last_error_value()),
        SocketMethod::RecvFromRaw => recv_from(function, args, resolver).await,
        SocketMethod::ResolveRaw => resolve(function, args, resolver),
    }
}

fn resolve<R: CallResolver>(
    function: &IRFunction,
    args: &[Value],
    resolver: &R,
) -> Result<Value, RuntimeError> {
    let [Value::String(hostname)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!("Socket.resolve_raw expects a single String argument, got {args:?}"),
        });
    };
    let result_symbol = helpers::enum_return_symbol(function, "Socket.resolve_raw")?;
    validate_resolve_payload(&result_symbol, resolver)?;

    let c_hostname = CString::new(hostname.as_slice()).map_err(|_| RuntimeError::TypeMismatch {
        detail: "Socket.resolve: hostname contains an interior NUL byte".to_string(),
    })?;
    let buffer = unsafe { koja_socket_resolve(c_hostname.as_ptr() as *const u8) };
    if buffer.is_null() {
        return Ok(helpers::result_value(
            result_symbol,
            Err(last_error_value()),
        ));
    }

    let count = unsafe { *(buffer as *const i64) }.max(0) as usize;
    let ip_pointers = unsafe { buffer.add(RESOLVE_HEADER_BYTES) } as *const *mut u8;
    let mut addresses = Vec::with_capacity(count);
    for i in 0..count {
        let payload = unsafe { *ip_pointers.add(i) };
        addresses.push(Value::binary(abi::take_block_bytes(payload)));
    }
    abi::free_raw_buffer(buffer);

    let list = Value::List(Rc::new(RefCell::new(addresses)));
    Ok(helpers::result_value(result_symbol, Ok(list)))
}

async fn recv_from<R: CallResolver>(
    function: &IRFunction,
    args: &[Value],
    resolver: &R,
) -> Result<Value, RuntimeError> {
    let [receiver, Value::Int(count)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!("Socket.recv_from_raw expects (Socket, Int) arguments, got {args:?}"),
        });
    };
    let fd = socket_fd(receiver)?;
    let result_symbol = helpers::enum_return_symbol(function, "Socket.recv_from_raw")?;
    validate_recv_from_payload(&result_symbol, resolver)?;

    // Interrupted by a signal: surface an error instead of reading.
    if reactor::io_block(fd, Interest::Readable).await {
        return Ok(helpers::result_value(
            result_symbol,
            Err(last_error_value()),
        ));
    }
    let buffer = unsafe { koja_socket_recv_from(fd, *count) };
    if buffer.is_null() {
        return Ok(helpers::result_value(
            result_symbol,
            Err(last_error_value()),
        ));
    }

    let data_payload = unsafe { *(buffer as *const *mut u8) };
    let ip_payload = unsafe { *(buffer.add(RECV_FROM_IP_OFFSET) as *const *mut u8) };
    let port = unsafe { *(buffer.add(RECV_FROM_PORT_OFFSET) as *const i64) };
    let data = Value::binary(abi::take_block_bytes(data_payload));
    let ip = Value::binary(abi::take_block_bytes(ip_payload));
    abi::free_raw_buffer(buffer);

    let received = Value::Tuple(vec![data, ip, Value::Int(port)]);
    Ok(helpers::result_value(result_symbol, Ok(received)))
}

/// `Err` payload for a failed socket call: the runtime's last-error
/// message as a `Value::String`. Mirrors the LLVM emitters' `Result.Err(
/// koja_last_error())` shape.
fn last_error_value() -> Value {
    let payload = unsafe { koja_last_error() };
    Value::string(abi::take_block_bytes(payload))
}

/// Extract the raw fd from a `Socket{fd: Fd{descriptor}}` receiver.
fn socket_fd(receiver: &Value) -> Result<i32, RuntimeError> {
    if let Value::Struct { fields, .. } = receiver
        && let [
            Value::Struct {
                fields: fd_fields, ..
            },
        ] = fields.as_slice()
        && let [Value::Int(descriptor)] = fd_fields.as_slice()
    {
        return Ok(*descriptor as i32);
    }
    Err(RuntimeError::TypeMismatch {
        detail: format!(
            "Socket.recv_from_raw: receiver is not a Socket{{fd: Fd}} struct: {receiver}"
        ),
    })
}

fn validate_resolve_payload<R: CallResolver>(
    result_symbol: &IRSymbol,
    resolver: &R,
) -> Result<(), RuntimeError> {
    match helpers::single_ok_payload(result_symbol, resolver, "Socket.resolve_raw")? {
        IRType::List(element) if *element == IRType::Binary => Ok(()),
        other => Err(payload_shape_error("Socket.resolve_raw", &other)),
    }
}

fn validate_recv_from_payload<R: CallResolver>(
    result_symbol: &IRSymbol,
    resolver: &R,
) -> Result<(), RuntimeError> {
    match helpers::single_ok_payload(result_symbol, resolver, "Socket.recv_from_raw")? {
        IRType::Tuple(elements)
            if elements == vec![IRType::Binary, IRType::Binary, IRType::Int64] =>
        {
            Ok(())
        }
        other => Err(payload_shape_error("Socket.recv_from_raw", &other)),
    }
}

fn payload_shape_error(label: &str, got: &IRType) -> RuntimeError {
    RuntimeError::TypeMismatch {
        detail: format!("{label}: unexpected Ok payload shape `{got:?}`"),
    }
}
