//! `@intrinsic` methods on `Socket` from
//! [`expo/lib/net/src/net.expo`]:
//!
//! * `Socket.recv_from(self, count: Int) -> Result<Pair<String, SocketAddress>, String>`
//! * `Socket.resolve(hostname: String) -> Result<List<IPAddress>, String>`
//!
//! Both bridge into the runtime's `expo_socket_*` C ABI in the
//! LLVM backend. The eval interpreter is deliberately I/O-free
//! (no real fds, no real DNS), so these surface as
//! [`RuntimeError::Unsupported`] — programs that exercise the
//! networking path must run with `--backend=llvm`. Wiring the
//! arm here (rather than letting it fall through to a panic)
//! gives a clean, actionable diagnostic instead of a "no eval
//! handler" generic.

use expo_alpha_ir::{IRFunction, SocketMethod};

use crate::error::RuntimeError;
use crate::value::Value;

pub(super) fn dispatch(
    method: SocketMethod,
    _function: &IRFunction,
    _args: &[Value],
) -> Result<Value, RuntimeError> {
    let name = match method {
        SocketMethod::RecvFrom => "Socket.recv_from",
        SocketMethod::Resolve => "Socket.resolve",
    };
    Err(RuntimeError::Unsupported {
        detail: format!(
            "{name} performs real network I/O and is only available on \
             the LLVM backend; re-run with `--backend=llvm`",
        ),
    })
}
