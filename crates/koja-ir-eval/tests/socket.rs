//! Loopback smoke tests for the eval-side socket surface: the
//! `koja_socket_*` externs in `externs/net.rs` (blocking-fd
//! semantics) and the `Socket.resolve` / `Socket.recv_from`
//! intrinsics. Fixtures use the qualified `Net` stdlib package via
//! [`common::evaluate_qualified_program`].
//!
//! Everything runs single-threaded: a loopback `connect` to a
//! listening backlog completes without a concurrent `accept`, and a
//! datagram queued by `send_to` is immediately readable, so the
//! sequential fixtures never deadlock on the blocking fds.

mod common;

use std::process;
use std::sync::atomic::{AtomicU16, Ordering};

use common::evaluate_qualified_program;
use koja_ir_eval::Value;

/// Sequential per-test port offset on top of a pid-derived base, so
/// parallel test threads (and concurrent test processes) bind
/// distinct loopback ports.
static PORT_OFFSET: AtomicU16 = AtomicU16::new(0);

fn fresh_port() -> u16 {
    let base = 20000 + (process::id() % 20000) as u16;
    base + PORT_OFFSET.fetch_add(1, Ordering::Relaxed)
}

#[test]
fn tcp_loopback_round_trip() {
    let port = fresh_port();
    let source = format!(
        r#"
alias Net.TCPListener
alias Net.TCPSocket

fn main -> String
  listener =
    match TCPListener.bind({port})
      Result.Ok(l) -> l
      Result.Err(e) -> return "bind failed: " <> e.message()
    end

  client =
    match TCPSocket.connect("127.0.0.1", {port})
      Result.Ok(c) -> c
      Result.Err(e) -> return "connect failed: " <> e.message()
    end

  server =
    match listener.accept()
      Result.Ok(s) -> s
      Result.Err(e) -> return "accept failed: " <> e.message()
    end

  match client.write("ping")
    Result.Ok(_) -> ()
    Result.Err(_) -> return "client write failed"
  end

  inbound =
    match server.read(4)
      Result.Ok(text) -> text
      Result.Err(_) -> return "server read failed"
    end

  match server.write(inbound <> "/pong")
    Result.Ok(_) -> ()
    Result.Err(_) -> return "server write failed"
  end

  match client.read(16)
    Result.Ok(text) -> text
    Result.Err(_) -> "client read failed"
  end
end
"#
    );
    assert_eq!(
        evaluate_qualified_program(&source).expect("fixture should run"),
        Value::String(b"ping/pong".to_vec()),
    );
}

#[test]
fn tcp_try_accept_reports_nothing_pending() {
    let port = fresh_port();
    let source = format!(
        r#"
alias Net.TCPListener

fn main -> Bool
  listener =
    match TCPListener.bind({port})
      Result.Ok(l) -> l
      Result.Err(_) -> return false
    end

  match listener.try_accept()
    Option.Some(_) -> false
    Option.None -> true
  end
end
"#
    );
    assert_eq!(
        evaluate_qualified_program(&source).expect("fixture should run"),
        Value::Bool(true),
    );
}

#[test]
fn udp_loopback_send_and_recv_from() {
    let port = fresh_port();
    let source = format!(
        r#"
alias Net.IPAddress
alias Net.SocketAddress
alias Net.UDPSocket

fn main -> String
  receiver =
    match UDPSocket.bind({port})
      Result.Ok(r) -> r
      Result.Err(e) -> return "bind failed: " <> e.message()
    end

  sender =
    match UDPSocket.bind(0)
      Result.Ok(s) -> s
      Result.Err(e) -> return "sender bind failed: " <> e.message()
    end

  target = SocketAddress{{ip: IPAddress.loopback(), port: {port}}}
  match sender.send_to("datagram", target)
    Result.Ok(_) -> ()
    Result.Err(_) -> return "send_to failed"
  end

  match receiver.recv_from(64)
    Result.Ok(received) -> received.first
    Result.Err(_) -> "recv_from failed"
  end
end
"#
    );
    assert_eq!(
        evaluate_qualified_program(&source).expect("fixture should run"),
        Value::String(b"datagram".to_vec()),
    );
}

#[test]
fn resolve_loopback_hostname() {
    let source = r#"
alias Net.Socket

fn main -> Bool
  match Socket.resolve("localhost")
    Result.Ok(addrs) -> addrs.empty?() == false
    Result.Err(_) -> false
  end
end
"#;
    assert_eq!(
        evaluate_qualified_program(source).expect("fixture should run"),
        Value::Bool(true),
    );
}
