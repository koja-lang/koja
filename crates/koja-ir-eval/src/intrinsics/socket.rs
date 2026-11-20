//! `@intrinsic` methods on `Socket` from
//! `koja/lib/net/src/net.koja`:
//!
//! * `Socket.recv_from_raw(self, count: Int) -> Result<(Binary, Binary, Int), Int>`
//! * `Socket.resolve_raw(hostname: String) -> Result<List<Binary>, Int>`
//!
//! Both call the runtime helpers (`koja_socket_resolve` /
//! `koja_socket_recv_from`), copy the returned heap buffer into eval
//! [`Value`]s, and free it through `koja_free`. `recv_from` waits for
//! readability through [`crate::reactor`] first. The `Err` payload is
//! the failure value of the runtime call.

use std::cell::RefCell;
use std::ffi::CString;
use std::ptr;
use std::rc::Rc;

use koja_ir::{IRSymbol, IRType, SocketMethod};
use koja_runtime_core::Interest;

use crate::abi;
use crate::error::RuntimeError;
use crate::interpreter::CallResolver;
use crate::intrinsics::{IntrinsicCall, helpers};
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
    fn koja_socket_recv_from(fd: i32, count: i64, out: *mut *mut u8) -> i64;
    fn koja_socket_resolve(hostname: *const u8, out: *mut *mut u8) -> i64;
}

pub(super) async fn dispatch<R: CallResolver>(
    method: SocketMethod,
    call: IntrinsicCall<'_, R>,
) -> Result<Value, RuntimeError> {
    match method {
        SocketMethod::RecvFromRaw => recv_from(call).await,
        SocketMethod::ResolveRaw => resolve(call),
    }
}

fn resolve<R: CallResolver>(call: IntrinsicCall<'_, R>) -> Result<Value, RuntimeError> {
    let [Value::String(hostname)] = call.args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "Socket.resolve_raw expects a single String argument, got {:?}",
                call.args
            ),
        });
    };
    let result_symbol = helpers::enum_return_symbol(call.function, "Socket.resolve_raw")?;
    validate_resolve_payload(&result_symbol, call.resolver)?;

    let c_hostname = CString::new(hostname.as_slice()).map_err(|_| RuntimeError::TypeMismatch {
        detail: "Socket.resolve: hostname contains an interior NUL byte".to_string(),
    })?;
    let mut buffer: *mut u8 = ptr::null_mut();
    let status = unsafe { koja_socket_resolve(c_hostname.as_ptr() as *const u8, &mut buffer) };
    if status < 0 {
        return helpers::result_value(result_symbol, call.resolver, Err(Value::Int(status)));
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
    helpers::result_value(result_symbol, call.resolver, Ok(list))
}

async fn recv_from<R: CallResolver>(call: IntrinsicCall<'_, R>) -> Result<Value, RuntimeError> {
    let [receiver, Value::Int(count)] = call.args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!(
                "Socket.recv_from_raw expects (Socket, Int) arguments, got {:?}",
                call.args
            ),
        });
    };
    let fd = socket_fd(receiver)?;
    let result_symbol = helpers::enum_return_symbol(call.function, "Socket.recv_from_raw")?;
    validate_recv_from_payload(&result_symbol, call.resolver)?;

    // An interrupted wait fails with its own cause and does not read.
    let wait = reactor::io_block(fd, Interest::Readable, None).await;
    if let Some(failed) = reactor::wait_failure(wait) {
        return helpers::result_value(result_symbol, call.resolver, Err(Value::Int(failed)));
    }
    let mut buffer: *mut u8 = ptr::null_mut();
    let status = unsafe { koja_socket_recv_from(fd, *count, &mut buffer) };
    if status < 0 {
        return helpers::result_value(result_symbol, call.resolver, Err(Value::Int(status)));
    }

    let data_payload = unsafe { *(buffer as *const *mut u8) };
    let ip_payload = unsafe { *(buffer.add(RECV_FROM_IP_OFFSET) as *const *mut u8) };
    let port = unsafe { *(buffer.add(RECV_FROM_PORT_OFFSET) as *const i64) };
    let data = Value::binary(abi::take_block_bytes(data_payload));
    let ip = Value::binary(abi::take_block_bytes(ip_payload));
    abi::free_raw_buffer(buffer);

    let received = Value::Tuple(vec![data, ip, Value::Int(port)]);
    helpers::result_value(result_symbol, call.resolver, Ok(received))
}

/// Extract the raw fd from a `Socket{descriptor: IO.Descriptor{raw}}`
/// receiver.
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
            "Socket.recv_from_raw: receiver is not a Socket{{descriptor: IO.Descriptor}} struct: {receiver}"
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
