//! `@intrinsic` methods on `Socket` from
//! [`expo/lib/net/src/net.expo`]:
//!
//! * `Socket.recv_from(self, count: Int) -> Result<Pair<String, SocketAddress>, String>`
//!   — receive one datagram + sender address. Suspends the
//!   process until the fd is readable.
//! * `Socket.resolve(hostname: String) -> Result<List<IPAddress>, String>`
//!   — synchronous `getaddrinfo` shim. Blocks the worker thread.
//!
//! The runtime crate already exports the C-ABI helpers
//! (`expo_socket_recv_from` / `expo_socket_resolve`) that v1
//! codegen leans on. Wiring up the real emitters requires the
//! same Result-enum / Pair / List monomorphic-layout plumbing
//! that the other heap-returning intrinsics use; until that's
//! ported, both bodies emit an `unreachable` trap (mirroring
//! [`super::parse`]) so the `@intrinsic` dispatch succeeds at
//! lift time and the only runtime cost is a crash if a caller
//! actually exercises the path.

use expo_alpha_ir::{IRFunction, SocketMethod};
use inkwell::values::FunctionValue;

use crate::ctx::EmitContext;
use crate::emit::inkwell_err;
use crate::error::LlvmError;

pub(super) fn emit_socket<'ctx>(
    ctx: &EmitContext<'ctx>,
    function: &IRFunction,
    llvm_function: FunctionValue<'ctx>,
    _method: SocketMethod,
) -> Result<(), LlvmError> {
    let entry = ctx.context.append_basic_block(llvm_function, "entry");
    ctx.builder.position_at_end(entry);
    ctx.builder.build_unreachable().map(|_| ()).map_err(|e| {
        inkwell_err(
            format_args!("build_unreachable for `{}`", function.symbol),
            e,
        )
    })
}
