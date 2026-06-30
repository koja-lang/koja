//! End-to-end coverage for eval's cooperative I/O reactor: an entry
//! process `watch`es a file descriptor and must receive the reactor's
//! readiness event as an ordinary `IOReady` union message through
//! `handle` (tag-2 dispatch), exactly like the LLVM backend's
//! `process_io` lang fixture.
//!
//! The fd is the read end of a self-pipe that the test pre-fills, so it
//! is readable the moment the driver idles and polls the reactor. The
//! interpreter runs on a worker thread guarded by a channel timeout so a
//! regression surfaces as a failure rather than a hung test.

mod common;

use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use common::{PACKAGE, typecheck};
use koja_ast::identifier::Identifier;
use koja_ast::util::dedent;
use koja_ir::lower_program;
use koja_ir_eval::{Interpreter, Value};
use koja_parser::ParseMode;

unsafe extern "C" {
    fn pipe(fds: *mut i32) -> i32;
    fn write(fd: i32, buf: *const u8, count: usize) -> isize;
    fn close(fd: i32) -> i32;
}

/// Entry that watches `fd` for readability and exits `Normal` (code 0)
/// once the reactor delivers `IOReady.Read`. Any other message keeps it
/// running, so a missed readiness event hangs (and the test's channel
/// timeout converts that into a failure).
const WATCH_ENTRY: &str = r#"
    alias IO.Ready as IOReady
    alias Process.Step
    alias Process.StopReason

    struct App
    end

    enum AppMsg
      Tick
    end

    impl Process<App, AppMsg | IOReady, ()> for App
      fn start(config: App) -> Result<Self, StopReason>
        Fd{descriptor: __FD__}.watch(0)
        Result.Ok(config)
      end

      fn handle(self, msg: AppMsg | IOReady, from: Option<ReplyTo<()>>) -> Step<Self>
        match msg
          io: IOReady ->
            match io
              IOReady.Read(_) -> Step.Done(StopReason.Normal)
              _ -> Step.Continue(self)
            end

          cmd: AppMsg ->
            Step.Continue(self)
        end
      end
    end
    "#;

fn run_watch_entry(fd: i32) -> i64 {
    let source = dedent(WATCH_ENTRY).replace("__FD__", &fd.to_string());
    let checked = typecheck(&source, ParseMode::File);
    let entry = Identifier::new(PACKAGE, vec!["App".to_string()]);
    let program = lower_program(&checked, &entry).expect("lowering should succeed");
    match Interpreter::run_program(&program, &[]).expect("entry should run to completion") {
        Value::Int(code) => code,
        other => panic!("expected an Int exit code, got {other:?}"),
    }
}

#[test]
fn watch_delivers_io_ready_through_handle() {
    let mut fds = [0i32; 2];
    assert_eq!(unsafe { pipe(fds.as_mut_ptr()) }, 0, "pipe() failed");
    let [read_fd, write_fd] = fds;
    assert_eq!(
        unsafe { write(write_fd, b"x".as_ptr(), 1) },
        1,
        "priming write failed"
    );

    let (tx, rx) = mpsc::channel();
    let worker = thread::spawn(move || {
        let _ = tx.send(run_watch_entry(read_fd));
    });

    let exit_code = rx
        .recv_timeout(Duration::from_secs(10))
        .expect("watch entry hung: IOReady was never delivered");
    worker.join().expect("worker thread panicked");

    unsafe {
        close(read_fd);
        close(write_fd);
    }
    assert_eq!(exit_code, 0, "entry should exit Normal after IOReady.Read");
}
