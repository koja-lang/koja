//! Loopback TLS handshake + echo through the eval interpreter,
//! covering the `lib/net/src/tls.koja` extern surface registered in
//! `externs/tls.rs` (BIO/PEM config loading, `SSL_CTX_*` setup,
//! `SSL_accept`/`SSL_connect` handshakes, `SSL_read`/`SSL_write`).
//!
//! The handshake needs both peers to make progress concurrently and
//! eval fds are blocking, so the server runs in its own interpreter
//! on a spawned thread while the client runs on the test thread.
//! Sequencing: the server writes a sentinel file once its listener
//! is bound (loopback `connect` succeeds from that point, before
//! `accept` is even called). The client waits for the sentinel.
//!
//! Certificate fixtures are the stdlib's own
//! `lib/net/test/fixtures/localhost_*.pem` (self-signed,
//! CN=localhost), reused here via absolute paths.

mod common;

use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use common::{evaluate_qualified_program, fresh_port};
use koja_ast::util::dedent;
use koja_ir_eval::Value;

const ECHO_PAYLOAD: &str = "hello over tls";

fn fixture_path(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../lib/net/test/fixtures")
        .join(name)
        .canonicalize()
        .expect("stdlib TLS fixture should exist")
        .to_string_lossy()
        .into_owned()
}

fn server_source(port: u16, ready_path: &str) -> String {
    let cert_path = fixture_path("localhost_cert.pem");
    let key_path = fixture_path("localhost_key.pem");
    dedent(&format!(
        r#"
        alias Crypto.Certificate
        alias Crypto.PrivateKey
        alias Net.IPAddress
        alias Net.Socket.Address as SocketAddress
        alias Net.TCPListener
        alias Net.TLSConfig

        fn main -> String
          cert_text =
            match File.read("{cert_path}")
              Result.Ok(t) -> t
              Result.Err(e) -> return "read cert: " <> e
            end

          cert =
            match Certificate.parse(cert_text)
              Result.Ok(c) -> c
              Result.Err(e) -> return "parse cert: " <> e.message()
            end

          key_text =
            match File.read("{key_path}")
              Result.Ok(t) -> t
              Result.Err(e) -> return "read key: " <> e
            end

          key =
            match PrivateKey.parse(key_text)
              Result.Ok(k) -> k
              Result.Err(e) -> return "parse key: " <> e.message()
            end

          listener =
            match TCPListener.bind_addr(SocketAddress{{ip: IPAddress.loopback(), port: {port}}})
              Result.Ok(l) -> l
              Result.Err(e) -> return "bind: " <> e.message()
            end

          match File.write("{ready_path}", "ready")
            Result.Ok(_) -> ()
            Result.Err(e) -> return "sentinel: " <> e
          end

          raw =
            match listener.accept()
              Result.Ok(s) -> s
              Result.Err(e) -> return "accept: " <> e.message()
            end

          secured =
            match raw.accept_tls(TLSConfig.server(cert, key))
              Result.Ok(s) -> s
              Result.Err(_) -> return "accept_tls failed"
            end

          data =
            match secured.read(64)
              Result.Ok(d) -> d
              Result.Err(_) -> return "server read failed"
            end

          match secured.write(data)
            Result.Ok(_) -> ()
            Result.Err(_) -> return "server write failed"
          end

          match secured.close()
            _ -> ()
          end

          data
        end
        "#
    ))
}

fn client_source(port: u16) -> String {
    dedent(&format!(
        r#"
        alias Net.IPAddress
        alias Net.Socket.Address as SocketAddress
        alias Net.TCPSocket
        alias Net.TLSConfig

        fn main -> String
          plain =
            match TCPSocket.connect_addr(SocketAddress{{ip: IPAddress.loopback(), port: {port}}})
              Result.Ok(s) -> s
              Result.Err(e) -> return "connect: " <> e.message()
            end

          client =
            match plain.upgrade_tls("localhost", TLSConfig.insecure())
              Result.Ok(s) -> s
              Result.Err(_) -> return "upgrade_tls failed"
            end

          match client.write("{ECHO_PAYLOAD}")
            Result.Ok(_) -> ()
            Result.Err(_) -> return "client write failed"
          end

          echoed =
            match client.read(64)
              Result.Ok(d) -> d
              Result.Err(_) -> return "client read failed"
            end

          match client.close()
            _ -> ()
          end

          echoed
        end
        "#
    ))
}

#[test]
fn tls_loopback_handshake_and_echo() {
    let port = fresh_port();
    let ready_path = std::env::temp_dir().join(format!("koja-eval-tls-ready-{port}"));
    let _ = std::fs::remove_file(&ready_path);

    // `Value` is not `Send` (`Rc`-backed collections), so unwrap the
    // server's result to a plain `String` before it crosses the
    // thread boundary.
    let server = thread::spawn({
        let source = server_source(port, &ready_path.to_string_lossy());
        move || match evaluate_qualified_program(&source).expect("server fixture should run") {
            Value::String(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            other => panic!("server fixture returned non-string value: {other}"),
        }
    });

    let deadline = Instant::now() + Duration::from_secs(20);
    while !ready_path.exists() {
        assert!(
            Instant::now() < deadline,
            "server never signalled readiness",
        );
        thread::sleep(Duration::from_millis(10));
    }
    let _ = std::fs::remove_file(&ready_path);

    let echoed = evaluate_qualified_program(&client_source(port)).expect("client fixture run");
    let served = server.join().expect("server thread should not panic");

    assert_eq!(echoed, Value::string(ECHO_PAYLOAD.as_bytes()));
    assert_eq!(served, ECHO_PAYLOAD);
}
