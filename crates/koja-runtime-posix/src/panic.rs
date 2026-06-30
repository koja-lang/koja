//! Koja panic handler with backtrace support.
//!
//! Called by compiled Koja code when a runtime panic occurs (e.g. `unwrap`
//! on `None`, explicit `panic()`). Prints the error message followed by a
//! symbolicated stack trace filtered to user-defined Koja functions,
//! formatted in Elixir style with optional ANSI color.

use std::ffi::{CStr, c_char};
use std::io::Write;
use std::path::Path;

use koja_runtime_core::CrashInfo;

unsafe extern "C" {
    static __koja_app_name: [c_char; 0];
}

/// The unwind payload a user-origin crash carries from its panic site up to
/// the [`catch_unwind`](std::panic::catch_unwind) boundary at
/// `process_trampoline`. Raised via [`std::panic::resume_unwind`] (which
/// bypasses the global hook, so the diagnostic is rendered exactly once at
/// the crash site), it ferries the structured capture across the compiled
/// Koja frames so the trampoline can record `ExitReason::Crashed` and stage
/// the watcher's `ExitSignal`.
pub(crate) struct UserCrash {
    pub crash_info: CrashInfo,
}

// ---------------------------------------------------------------------------
// ANSI color helpers
// ---------------------------------------------------------------------------

fn app_name() -> &'static str {
    unsafe {
        let ptr = __koja_app_name.as_ptr();
        CStr::from_ptr(ptr).to_str().unwrap_or("app")
    }
}

fn use_color() -> bool {
    if std::env::var_os("NO_COLOR").is_some() {
        return false;
    }
    unsafe { libc::isatty(libc::STDERR_FILENO) != 0 }
}

struct Colors {
    red: &'static str,
    reset: &'static str,
}

const COLORS_ON: Colors = Colors {
    red: "\x1b[31m",
    reset: "\x1b[0m",
};

const COLORS_OFF: Colors = Colors { red: "", reset: "" };

// ---------------------------------------------------------------------------
// Panic entry points
// ---------------------------------------------------------------------------

/// Distinguishes a user-level Koja panic (`panic()`, `unwrap` on `None`)
/// from an internal runtime panic surfaced via the global hook. The origin
/// selects both the leading tag and the backtrace frame-filter policy: user
/// panics hide runtime/stdlib frames to show only user code, while runtime
/// panics keep `koja_rt_*` / `koja_runtime*` frames since those are exactly
/// the ones worth seeing.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum PanicOrigin {
    User,
    Runtime,
}

/// Entry point called from compiled Koja code on panic. Prints the panic
/// message and a filtered backtrace to stderr, then unwinds the crashing
/// process to the [`catch_unwind`](std::panic::catch_unwind) boundary at
/// `process_trampoline` (see [`crash_unwind`]).
///
/// `extern "C-unwind"` so the unwind may legally cross back into the
/// compiled Koja frames that called it.
///
/// # Safety
///
/// `msg` must be a valid pointer to a null-terminated C string.
#[unsafe(no_mangle)]
pub unsafe extern "C-unwind" fn koja_panic_backtrace(msg: *const c_char) -> ! {
    let message = if msg.is_null() {
        "unknown panic".to_string()
    } else {
        unsafe { CStr::from_ptr(msg) }
            .to_string_lossy()
            .into_owned()
    };

    crash_unwind(&message);
}

/// Render a user crash's diagnostic, then unwind the crashing process to the
/// `catch_unwind` at `process_trampoline`. Uses [`std::panic::resume_unwind`]
/// rather than `panic!` so the global hook (which renders + aborts runtime
/// panics) is *not* invoked — the diagnostic prints exactly once here, with
/// the crashing stack still live. Per-process containment: the unwind takes
/// down one process, not the runtime.
pub(crate) fn crash_unwind(message: &str) -> ! {
    let crash_info = render_diagnostic(PanicOrigin::User, message);
    std::panic::resume_unwind(Box::new(UserCrash { crash_info }));
}

/// Installs a process-global panic hook that routes every Rust panic — on
/// any thread, from any site (`unwrap`, `expect`, `assert!`, allocation
/// failure, a poisoned `SCHED` lock) — through the same diagnostic-and-abort
/// path as user panics. The hook runs before unwinding with the stack
/// intact, so the backtrace is faithful and no unwind ever crosses the
/// C-ABI or poisons a lock (the first panic aborts immediately, so the
/// worker-cascade failure mode cannot happen).
pub(crate) fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let payload = info.payload();
        let message = payload
            .downcast_ref::<&str>()
            .map(|s| (*s).to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "unknown panic".to_string());

        let message = match info.location() {
            Some(loc) => format!(
                "{message} (at {}:{}:{})",
                loc.file(),
                loc.line(),
                loc.column()
            ),
            None => message,
        };

        abort_with_diagnostic(PanicOrigin::Runtime, &message);
    }));
}

/// A single resolved, user-relevant stack frame. Produced by [`capture`]
/// and rendered by [`abort_with_diagnostic`]. Kept structured (rather than
/// pre-formatted) so a future observability reporter (Sentry / OpenTelemetry)
/// can consume the same frames the human renderer does.
struct Frame {
    /// Demangled display name (e.g. `Option.unwrap`, `main`).
    name: String,
    /// Raw source file path as recorded in debug info (may be empty).
    file: String,
    /// 1-based source line, or 0 when debug info carries none.
    line: u32,
    /// Whether the frame is stdlib/runtime (drives the label + path elision).
    is_stdlib: bool,
}

/// Walk the live stack and return the filtered, demangled frames worth
/// showing for `origin`. Must run with the panicking stack still live.
fn capture(origin: PanicOrigin) -> Vec<Frame> {
    const MAX_FRAMES: usize = 128;
    let mut buf = [std::ptr::null_mut::<std::ffi::c_void>(); MAX_FRAMES];
    let n = unsafe { libc::backtrace(buf.as_mut_ptr(), MAX_FRAMES as i32) } as usize;
    let ips = &buf[..n];

    let mut frames = Vec::new();
    for ip in ips {
        let resolve_ip = (*ip as usize).wrapping_sub(1) as *mut std::ffi::c_void;
        backtrace::resolve(resolve_ip, |symbol| {
            let name = symbol
                .name()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "<unknown>".to_string());

            if should_skip_frame(&name, origin) {
                return;
            }

            let file = symbol.filename().and_then(|p| p.to_str()).unwrap_or("");
            frames.push(Frame {
                name: demangle_koja_name(&name),
                file: file.to_string(),
                line: symbol.lineno().unwrap_or(0),
                is_stdlib: is_stdlib_frame(file),
            });
        });
    }
    frames
}

/// Prints `message` and a filtered backtrace to stderr in the Elixir-style
/// koja format, then aborts the process. The terminal path for an internal
/// runtime panic (via the global hook); user crashes render and *unwind*
/// instead (see [`crash_unwind`]). Captures the backtrace at the call site,
/// so callers must invoke it with the panicking stack still live.
pub(crate) fn abort_with_diagnostic(origin: PanicOrigin, message: &str) -> ! {
    render_diagnostic(origin, message);
    std::process::abort();
}

/// Renders `message` plus a filtered, symbolicated backtrace to stderr in the
/// Elixir-style koja format and returns the same capture structured as a
/// [`CrashInfo`] (the plain-text rendering, ANSI-free, for a watcher's
/// `ExitSignal`). Must run with the panicking stack still live so the
/// backtrace is faithful; the caller decides whether to abort (runtime panic)
/// or unwind (user crash) afterwards.
pub(crate) fn render_diagnostic(origin: PanicOrigin, message: &str) -> CrashInfo {
    let c = if use_color() { &COLORS_ON } else { &COLORS_OFF };

    let app = app_name();

    let tag = match origin {
        PanicOrigin::Runtime => "runtime panic",
        PanicOrigin::User => "panic",
    };

    let cwd = std::env::current_dir().ok();
    let frames = capture(origin);

    // Build the backtrace block once (color-free) for both the stderr render
    // and the `CrashInfo` a watcher receives.
    let mut backtrace = String::new();
    for frame in &frames {
        let display_file = format_file_path(&frame.file, cwd.as_deref(), frame.is_stdlib);
        let label = if frame.is_stdlib {
            "(stdlib)".to_string()
        } else {
            format!("({app})")
        };
        if frame.line > 0 {
            backtrace.push_str(&format!(
                "    {label} {display_file}:{}: {}()\n",
                frame.line, frame.name
            ));
        } else {
            backtrace.push_str(&format!("    {label} {display_file}: {}()\n", frame.name));
        }
    }
    if frames.is_empty() {
        backtrace
            .push_str("    <no frames available — was the binary compiled with debug info?>\n");
    }

    eprint!("{}", c.red);
    eprintln!("** ({tag}) {message}");
    eprint!("{backtrace}");
    if let Some(hint) = hint_for_panic(message) {
        eprintln!();
        eprintln!("    hint: {hint}");
    }
    eprint!("{}", c.reset);
    eprintln!();
    let _ = std::io::stderr().flush();

    CrashInfo {
        message: message.to_string(),
        backtrace,
    }
}

// ---------------------------------------------------------------------------
// Frame filtering
// ---------------------------------------------------------------------------

/// True for any Rust frame originating in a runtime crate — `koja_runtime`
/// (this staticlib's lib name), `koja_runtime_core`, or a future
/// `koja_runtime_*`. Bare `koja_runtime` prefix is safe: lowered Koja
/// symbols never carry it (they demangle from `<pkg>.<name>`).
fn is_runtime_frame(name: &str) -> bool {
    name.starts_with("koja_runtime")
}

fn should_skip_frame(name: &str, origin: PanicOrigin) -> bool {
    if name == "__koja_user_main" || name == "main" {
        return false;
    }

    // The panic machinery itself is never interesting in a backtrace.
    // Match any runtime crate (`koja_runtime`, `koja_runtime_core`,
    // `koja_runtime_posix`) since the staticlib keeps the `koja_runtime`
    // lib name but `koja-runtime-core` mangles as `koja_runtime_core`.
    if is_runtime_frame(name) && name.contains("::panic::") {
        return true;
    }

    // Internal runtime panics keep the runtime frames — they are the stack
    // worth seeing. User panics hide them to surface only user Koja code.
    if origin == PanicOrigin::Runtime && (name.starts_with("koja_rt_") || is_runtime_frame(name)) {
        return false;
    }

    if name.starts_with("koja_rt_")
        || name.starts_with("koja_panic")
        || is_runtime_frame(name)
        || name.starts_with("std::")
        || name.starts_with("core::")
        || name.contains("core::ops::function::")
        || name.starts_with("backtrace::")
        || name.starts_with("__")
        || name.starts_with("_start")
        || name.contains("__rust_")
    {
        return true;
    }

    if name.starts_with("_") || name == "start" {
        return true;
    }

    false
}

fn is_stdlib_frame(file_path: &str) -> bool {
    // DWARF reconstructs parentless virtual paths (e.g. the stdlib's
    // `<Global.option>`) with a `./` directory prefix; strip it so the
    // `<…>` marker is still recognized.
    let path = file_path.strip_prefix("./").unwrap_or(file_path);
    path.is_empty()
        || path.starts_with('<')
        || path.contains("/koja-stdlib/")
        || path.contains("/crates/koja-")
}

// ---------------------------------------------------------------------------
// Path formatting
// ---------------------------------------------------------------------------

fn format_file_path(file_path: &str, cwd: Option<&Path>, is_stdlib: bool) -> String {
    if file_path.is_empty() {
        return "<unknown>".to_string();
    }

    if is_stdlib {
        return Path::new(file_path)
            .file_name()
            .and_then(|f| f.to_str())
            .unwrap_or(file_path)
            .to_string();
    }

    if let Some(cwd) = cwd
        && let Ok(rel) = Path::new(file_path).strip_prefix(cwd)
    {
        return rel.to_string_lossy().into_owned();
    }

    file_path.to_string()
}

// ---------------------------------------------------------------------------
// Name demangling
// ---------------------------------------------------------------------------

/// Converts mangled Koja names into a readable form:
/// - `__koja_user_main` -> `main`
/// - `Option_$Int$_unwrap` -> `Option.unwrap`
/// - `Point_distance` -> `Point.distance`
fn demangle_koja_name(name: &str) -> String {
    if name == "__koja_user_main" {
        return "main".to_string();
    }

    let stripped = strip_generic_params(name);

    if let Some(first) = stripped.chars().next()
        && first.is_uppercase()
        && let Some(idx) = stripped.find('_')
    {
        let type_name = &stripped[..idx];
        let method = &stripped[idx + 1..];
        return format!("{type_name}.{method}");
    }

    stripped
}

/// Strips `$TypeParam$` segments from mangled names.
/// `Option_$Int$_unwrap` -> `Option_unwrap`
/// `Map_$String$_$Int$_get` -> `Map_get`
fn strip_generic_params(name: &str) -> String {
    let mut result = String::with_capacity(name.len());
    let mut chars = name.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '$' {
            while let Some(&inner) = chars.peek() {
                chars.next();
                if inner == '$' {
                    if chars.peek() == Some(&'_') {
                        chars.next();
                    }
                    break;
                }
            }
        } else {
            result.push(ch);
        }
    }

    if result.ends_with('_') {
        result.pop();
    }

    result
}

// ---------------------------------------------------------------------------
// Contextual hints
// ---------------------------------------------------------------------------

fn hint_for_panic(msg: &str) -> Option<&'static str> {
    if msg.contains("unwrap on None") {
        return Some("use .unwrap_or(default) or pattern match to handle None safely");
    }
    if msg.contains("unwrap on Err") {
        return Some("use .unwrap_or(default) or pattern match to handle the error");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_panics_keep_runtime_frames_user_panics_hide_them() {
        // Runtime frames are the stack worth seeing for an internal panic,
        // but noise for a user panic.
        // Cover both the staticlib lib name (`koja_runtime`) and the
        // split-out core crate (`koja_runtime_core`) so a frame from
        // either is filtered for user panics but kept for runtime panics.
        for frame in [
            "koja_rt_send",
            "koja_runtime::scheduler::worker_loop",
            "koja_runtime_core::process_table::ProcessTable<X,M>::spawn",
        ] {
            assert!(should_skip_frame(frame, PanicOrigin::User), "{frame}");
            assert!(!should_skip_frame(frame, PanicOrigin::Runtime), "{frame}");
        }
    }

    #[test]
    fn std_and_panic_plumbing_is_dropped_regardless_of_origin() {
        for origin in [PanicOrigin::User, PanicOrigin::Runtime] {
            for frame in [
                "std::rt::lang_start",
                "core::panicking::panic",
                "backtrace::backtrace::trace",
                "koja_panic_backtrace",
                "koja_runtime::panic::abort_with_diagnostic",
                "koja_runtime_core::panic::capture",
                "__rust_try",
            ] {
                assert!(should_skip_frame(frame, origin), "{frame}");
            }
        }
    }

    #[test]
    fn user_code_frames_are_always_kept() {
        for origin in [PanicOrigin::User, PanicOrigin::Runtime] {
            assert!(!should_skip_frame("__koja_user_main", origin));
            assert!(!should_skip_frame("greet", origin));
        }
    }
}
