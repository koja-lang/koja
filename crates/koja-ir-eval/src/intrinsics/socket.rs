//! `@intrinsic` methods on `Socket` from
//! [`koja/lib/net/src/net.koja`]:
//!
//! * `Socket.recv_from(self, count: Int) -> Result<Pair<Binary, SocketAddress>, String>`
//! * `Socket.resolve(hostname: String) -> Result<List<IPAddress>, String>`
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
        SocketMethod::RecvFrom => recv_from(function, args, resolver).await,
        SocketMethod::Resolve => resolve(function, args, resolver),
    }
}

fn resolve<R: CallResolver>(
    function: &IRFunction,
    args: &[Value],
    resolver: &R,
) -> Result<Value, RuntimeError> {
    let [Value::String(hostname)] = args else {
        return Err(RuntimeError::TypeMismatch {
            detail: format!("Socket.resolve expects a single String argument, got {args:?}"),
        });
    };
    let result_symbol = helpers::enum_return_symbol(function, "Socket.resolve")?;
    let ip_symbol = resolve_element_symbol(&result_symbol, resolver)?;

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
        addresses.push(Value::Struct {
            symbol: ip_symbol.clone(),
            fields: vec![Value::binary(abi::take_block_bytes(payload))],
        });
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
            detail: format!("Socket.recv_from expects (Socket, Int) arguments, got {args:?}"),
        });
    };
    let fd = socket_fd(receiver)?;
    let result_symbol = helpers::enum_return_symbol(function, "Socket.recv_from")?;
    let pair_symbol = recv_from_pair_symbol(&result_symbol, resolver)?;
    let address_symbol = struct_field_symbol(&pair_symbol, 1, resolver)?;
    let ip_symbol = struct_field_symbol(&address_symbol, 0, resolver)?;

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
    let ip = Value::Struct {
        symbol: ip_symbol,
        fields: vec![Value::binary(abi::take_block_bytes(ip_payload))],
    };
    abi::free_raw_buffer(buffer);

    let address = Value::Struct {
        symbol: address_symbol,
        fields: vec![ip, Value::Int(port)],
    };
    let pair = Value::Struct {
        symbol: pair_symbol,
        fields: vec![data, address],
    };
    Ok(helpers::result_value(result_symbol, Ok(pair)))
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
        detail: format!("Socket.recv_from: receiver is not a Socket{{fd: Fd}} struct: {receiver}"),
    })
}

/// Walk `Result<List<IPAddress>, _>` down to the `IPAddress` struct
/// symbol via the program's enum decl.
fn resolve_element_symbol<R: CallResolver>(
    result_symbol: &IRSymbol,
    resolver: &R,
) -> Result<IRSymbol, RuntimeError> {
    match helpers::single_ok_payload(result_symbol, resolver, "Socket.resolve")? {
        IRType::List(element) => match *element {
            IRType::Struct(symbol) => Ok(symbol),
            other => Err(payload_shape_error("Socket.resolve", &other)),
        },
        other => Err(payload_shape_error("Socket.resolve", &other)),
    }
}

/// Walk `Result<Pair<Binary, SocketAddress>, _>` down to the `Pair`
/// struct symbol.
fn recv_from_pair_symbol<R: CallResolver>(
    result_symbol: &IRSymbol,
    resolver: &R,
) -> Result<IRSymbol, RuntimeError> {
    match helpers::single_ok_payload(result_symbol, resolver, "Socket.recv_from")? {
        IRType::Struct(symbol) => Ok(symbol),
        other => Err(payload_shape_error("Socket.recv_from", &other)),
    }
}

/// The struct symbol at field `index` of `struct_symbol`'s decl.
/// Used to walk `Pair -> SocketAddress -> IPAddress` without
/// hardcoding identifier strings.
fn struct_field_symbol<R: CallResolver>(
    struct_symbol: &IRSymbol,
    index: usize,
    resolver: &R,
) -> Result<IRSymbol, RuntimeError> {
    let decl = resolver
        .struct_decl(struct_symbol.mangled())
        .ok_or_else(|| RuntimeError::TypeMismatch {
            detail: format!("struct decl `{struct_symbol}` not found in program"),
        })?;
    let field = decl
        .fields
        .get(index)
        .ok_or_else(|| RuntimeError::TypeMismatch {
            detail: format!("struct `{struct_symbol}` has no field at index {index}"),
        })?;
    match &field.ir_type {
        IRType::Struct(symbol) => Ok(symbol.clone()),
        other => Err(RuntimeError::TypeMismatch {
            detail: format!(
                "field {index} of struct `{struct_symbol}` expected to be a struct, \
                 got `{other:?}`",
            ),
        }),
    }
}

fn payload_shape_error(label: &str, got: &IRType) -> RuntimeError {
    RuntimeError::TypeMismatch {
        detail: format!("{label}: unexpected Ok payload shape `{got:?}`"),
    }
}
