//! `TCP_NODELAY` through the runtime's C surface.
//!
//! Sets the option on both ends of a loopback connection and reads it
//! back through std, which carries its own copy of the platform
//! constants. A wrong level or option number in the runtime shows up
//! here as a mismatch. The call never parks, so the scheduler is never
//! started and the file can hold more than one `#[test]`.

mod common;

use std::net::{TcpListener, TcpStream};
use std::os::fd::AsRawFd;

extern crate koja_runtime;

unsafe extern "C" {
    fn koja_socket_setsockopt_nodelay(fd: i32, enabled: i64) -> i64;
}

/// A connected loopback pair, client first.
fn loopback_pair() -> (TcpStream, TcpStream) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind loopback");
    let client = TcpStream::connect(listener.local_addr().expect("addr")).expect("connect");
    let (server, _) = listener.accept().expect("accept");
    (client, server)
}

#[test]
fn nodelay_starts_off_and_follows_each_call() {
    let (client, server) = loopback_pair();

    for stream in [&client, &server] {
        let fd = stream.as_raw_fd();
        assert!(
            !stream.nodelay().expect("read option"),
            "a new connection has the option off",
        );

        assert_eq!(unsafe { koja_socket_setsockopt_nodelay(fd, 1) }, 0);
        assert!(
            stream.nodelay().expect("read option"),
            "the call turns it on",
        );

        assert_eq!(unsafe { koja_socket_setsockopt_nodelay(fd, 0) }, 0);
        assert!(
            !stream.nodelay().expect("read option"),
            "the call turns it off",
        );
    }
}

#[test]
fn nodelay_on_a_bad_descriptor_fails() {
    assert_eq!(unsafe { koja_socket_setsockopt_nodelay(-1, 1) }, -1);
}
