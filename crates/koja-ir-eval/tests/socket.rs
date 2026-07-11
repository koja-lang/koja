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

use common::{evaluate_qualified_program, fresh_port};
use koja_ast::util::dedent;
use koja_ir_eval::Value;

#[test]
fn tcp_loopback_round_trip() {
    let port = fresh_port();
    let source = dedent(&format!(
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
    ));
    assert_eq!(
        evaluate_qualified_program(&source).expect("fixture should run"),
        Value::string(b"ping/pong".as_slice()),
    );
}

#[test]
fn tcp_try_accept_reports_nothing_pending() {
    let port = fresh_port();
    let source = dedent(&format!(
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
    ));
    assert_eq!(
        evaluate_qualified_program(&source).expect("fixture should run"),
        Value::Bool(true),
    );
}

#[test]
fn udp_loopback_send_and_recv_from() {
    let port = fresh_port();
    let source = dedent(&format!(
        r#"
        alias Net.IPAddress
        alias Net.Socket.Address as SocketAddress
        alias Net.UDPSocket

        fn main -> Binary
          receiver =
            match UDPSocket.bind({port})
              Result.Ok(r) -> r
              Result.Err(_) -> return <<>>
            end

          sender =
            match UDPSocket.bind(0)
              Result.Ok(s) -> s
              Result.Err(_) -> return <<>>
            end

          target = SocketAddress{{ip: IPAddress.loopback(), port: {port}}}
          payload = <<97, 0, 98>>.to_string().unwrap()
          match sender.send_to(payload, target)
            Result.Ok(_) -> ()
            Result.Err(_) -> return <<>>
          end

          match receiver.recv_from(64)
            Result.Ok(received) -> received.first
            Result.Err(_) -> <<>>
          end
        end
        "#
    ));
    assert_eq!(
        evaluate_qualified_program(&source).expect("fixture should run"),
        Value::binary(b"a\0b".as_slice()),
    );
}

#[test]
fn resolve_loopback_hostname() {
    let source = dedent(
        r#"
        alias Net.Socket

        fn main -> Bool
          match Socket.resolve("localhost")
            Result.Ok(addrs) -> addrs.empty?() == false
            Result.Err(_) -> false
          end
        end
        "#,
    );
    assert_eq!(
        evaluate_qualified_program(&source).expect("fixture should run"),
        Value::Bool(true),
    );
}
