//! SIGPIPE on stdout.
//!
//! A write to stdout after its reader has gone must end the program
//! with SIGPIPE. The signal ends the process that observes it, so the
//! test re-runs this binary as a child and asserts on how the child
//! died. The write fails before `block_until_ready` parks, so the
//! scheduler is never started.

mod common;

use std::env;
use std::os::unix::process::ExitStatusExt;
use std::process::{Command, Stdio};

extern crate koja_runtime;

unsafe extern "C" {
    fn koja_fd_write(fd: i32, data_ptr: *const u8, data_len: i64, timeout_ms: i64) -> i64;
}

const CHILD_FLAG: &str = "KOJA_SIGPIPE_CHILD";
const TEST_NAME: &str = "stdout_write_after_reader_closed_dies_from_sigpipe";

fn write_to_closed_stdout() -> ! {
    koja_runtime::signals::install();

    let mut fds = [0i32; 2];
    assert_eq!(unsafe { libc::pipe(fds.as_mut_ptr()) }, 0, "pipe(2)");
    let [read_end, write_end] = fds;
    unsafe {
        libc::dup2(write_end, 1);
        libc::close(write_end);
        libc::close(read_end);
    }

    let data = b"x";
    let written = unsafe { koja_fd_write(1, data.as_ptr(), data.len() as i64, -1) };
    eprintln!("koja_fd_write returned {written} instead of ending the program");
    std::process::exit(3);
}

#[test]
fn stdout_write_after_reader_closed_dies_from_sigpipe() {
    if env::var_os(CHILD_FLAG).is_some() {
        write_to_closed_stdout();
    }

    let output = Command::new(env::current_exe().expect("test binary path"))
        .arg("--exact")
        .arg(TEST_NAME)
        .env(CHILD_FLAG, "1")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .expect("spawn the child half");

    assert_eq!(
        output.status.signal(),
        Some(libc::SIGPIPE),
        "expected death by SIGPIPE, got {:?}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
}
