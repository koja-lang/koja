//! Expo panic handler with backtrace support.
//!
//! Called by compiled Expo code when a runtime panic occurs (e.g. `unwrap`
//! on `None`, explicit `panic()`). Prints the error message followed by a
//! symbolicated stack trace filtered to user-defined Expo functions,
//! formatted in Elixir style with optional ANSI color.

use std::ffi::{CStr, c_char};
use std::io::Write;
use std::path::Path;

unsafe extern "C" {
    static __expo_app_name: [c_char; 0];
}

// ---------------------------------------------------------------------------
// ANSI color helpers
// ---------------------------------------------------------------------------

fn app_name() -> &'static str {
    unsafe {
        let ptr = __expo_app_name.as_ptr();
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
// Panic entry point
// ---------------------------------------------------------------------------

/// Entry point called from compiled Expo code on panic. Prints the panic
/// message and a filtered backtrace to stderr, then aborts the process.
///
/// # Safety
///
/// `msg` must be a valid pointer to a null-terminated C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn expo_panic_backtrace(msg: *const i8) {
    let message = if msg.is_null() {
        "unknown panic".to_string()
    } else {
        unsafe { CStr::from_ptr(msg) }
            .to_string_lossy()
            .into_owned()
    };

    let c = if use_color() { &COLORS_ON } else { &COLORS_OFF };

    let app = app_name();

    eprint!("{}", c.red);
    eprintln!("** (panic) {message}");

    let cwd = std::env::current_dir().ok();

    const MAX_FRAMES: usize = 128;
    let mut buf = [std::ptr::null_mut::<std::ffi::c_void>(); MAX_FRAMES];
    let n = unsafe { libc::backtrace(buf.as_mut_ptr(), MAX_FRAMES as i32) } as usize;
    let ips = &buf[..n];

    let mut frame_num = 0;
    for ip in ips {
        let resolve_ip = (*ip as usize).wrapping_sub(1) as *mut std::ffi::c_void;
        backtrace::resolve(resolve_ip, |symbol| {
            let name = symbol
                .name()
                .map(|n| n.to_string())
                .unwrap_or_else(|| "<unknown>".to_string());

            if should_skip_frame(&name) {
                return;
            }

            let file_path = symbol.filename().and_then(|p| p.to_str()).unwrap_or("");
            let line = symbol.lineno().unwrap_or(0);
            let display_name = demangle_expo_name(&name);
            let is_stdlib = is_stdlib_frame(file_path);

            let display_file = format_file_path(file_path, cwd.as_deref(), is_stdlib);
            let label = if is_stdlib {
                "(stdlib)".to_string()
            } else {
                format!("({app})")
            };
            if line > 0 {
                eprintln!("    {label} {display_file}:{line}: {display_name}()");
            } else {
                eprintln!("    {label} {display_file}: {display_name}()");
            }

            frame_num += 1;
        });
    }

    if frame_num == 0 {
        eprintln!("    <no frames available — was the binary compiled with debug info?>");
    }

    if let Some(hint) = hint_for_panic(&message) {
        eprintln!();
        eprintln!("    hint: {hint}");
    }

    eprint!("{}", c.reset);
    eprintln!();
    let _ = std::io::stderr().flush();
    std::process::abort();
}

// ---------------------------------------------------------------------------
// Frame filtering
// ---------------------------------------------------------------------------

fn should_skip_frame(name: &str) -> bool {
    if name == "__expo_user_main" || name == "main" {
        return false;
    }

    if name.starts_with("expo_rt_")
        || name.starts_with("expo_panic")
        || name.starts_with("expo_runtime::")
        || name.starts_with("std::")
        || name.starts_with("core::")
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
    file_path.is_empty()
        || file_path.starts_with('<')
        || file_path.contains("/expo-stdlib/")
        || file_path.contains("/crates/expo-")
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

/// Converts mangled Expo names into a readable form:
/// - `__expo_user_main` -> `main`
/// - `Option_$Int$_unwrap` -> `Option.unwrap`
/// - `Point_distance` -> `Point.distance`
fn demangle_expo_name(name: &str) -> String {
    if name == "__expo_user_main" {
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
