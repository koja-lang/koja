//! Expo process runtime: cooperative coroutine scheduler with typed
//! mailboxes. Each process runs on its own stack and yields on
//! `receive` when its mailbox is empty.

use std::cell::UnsafeCell;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

const STACK_SIZE: usize = 512 * 1024;

type ProcessFn = extern "C" fn(*const u8);

unsafe extern "C" {
    fn expo_context_switch(save_sp: *mut *mut u8, load_sp: *mut u8);
}

// ---------------------------------------------------------------------------
// Platform-specific initial-frame layout constants
//
// INIT_FRAME_SIZE: total bytes to zero-fill on a fresh process stack.
// RET_ADDR_OFFSET: byte offset within that frame where the trampoline
//                  address is written (so `ret` / `br x30` lands there).
// ---------------------------------------------------------------------------

#[cfg(target_arch = "aarch64")]
const INIT_FRAME_SIZE: usize = 160;
#[cfg(target_arch = "aarch64")]
const RET_ADDR_OFFSET: usize = 88; // x30 in stp x29,x30,[sp,#80]

#[cfg(all(target_arch = "x86_64", not(target_os = "windows")))]
const INIT_FRAME_SIZE: usize = 64; // 6 regs + ret addr + alignment pad
#[cfg(all(target_arch = "x86_64", not(target_os = "windows")))]
const RET_ADDR_OFFSET: usize = 48;

#[cfg(all(target_arch = "x86_64", target_os = "windows"))]
const INIT_FRAME_SIZE: usize = 240; // 8 GPRs + 10 XMMs + ret + pad
#[cfg(all(target_arch = "x86_64", target_os = "windows"))]
const RET_ADDR_OFFSET: usize = 224;

// ---------------------------------------------------------------------------
// Process & scheduler state
// ---------------------------------------------------------------------------

#[derive(PartialEq)]
enum ProcessState {
    Created,
    Runnable,
    Running,
    Blocked,
    Dead,
}

struct Process {
    id: i64,
    func: ProcessFn,
    init_state: *mut u8,
    mailbox: VecDeque<*mut u8>,
    state: ProcessState,
    sp: *mut u8,
    deadline: Option<Instant>,
}

struct Scheduler {
    processes: Vec<Process>,
    next_id: i64,
    current_pid: i64,
    scheduler_sp: *mut u8,
}

impl Scheduler {
    fn new() -> Self {
        Scheduler {
            processes: Vec::new(),
            next_id: 1,
            current_pid: -1,
            scheduler_sp: std::ptr::null_mut(),
        }
    }
}

struct Global(UnsafeCell<Option<Scheduler>>);
unsafe impl Sync for Global {}

static SCHED: Global = Global(UnsafeCell::new(None));

fn sched() -> &'static mut Scheduler {
    unsafe {
        let cell = &*SCHED.0.get();
        if cell.is_none() {
            *SCHED.0.get() = Some(Scheduler::new());
        }
        (*SCHED.0.get()).as_mut().unwrap()
    }
}

// ---------------------------------------------------------------------------
// Stack initialisation & trampoline
// ---------------------------------------------------------------------------

/// Prepare a fresh process stack so the first `expo_context_switch`
/// into it will "return" to `entry`.
unsafe fn init_process_stack(stack_top: *mut u8, entry: unsafe extern "C" fn()) -> *mut u8 {
    unsafe {
        let sp = stack_top.sub(INIT_FRAME_SIZE);
        std::ptr::write_bytes(sp, 0, INIT_FRAME_SIZE);
        let ret_slot = sp.add(RET_ADDR_OFFSET) as *mut usize;
        *ret_slot = entry as usize;
        sp
    }
}

/// Entry point for every process. Reads the current process from the
/// scheduler, calls its function, marks it dead, and switches back.
unsafe extern "C" fn process_trampoline() {
    unsafe {
        let (func, init_state) = {
            let s = sched();
            let idx = (s.current_pid - 1) as usize;
            (s.processes[idx].func, s.processes[idx].init_state)
        };

        func(init_state);
        fflush(std::ptr::null_mut());

        let s = sched();
        let idx = (s.current_pid - 1) as usize;
        s.processes[idx].state = ProcessState::Dead;
        let sched_sp = s.scheduler_sp;
        expo_context_switch(&mut s.processes[idx].sp, sched_sp);
    }
}

// ---------------------------------------------------------------------------
// Runtime intrinsics (C ABI — unchanged from previous version)
// ---------------------------------------------------------------------------

/// # Safety
/// `state_ptr` must point to `state_len` readable bytes (or be null if `state_len` is 0).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn expo_rt_spawn(
    fn_ptr: ProcessFn,
    state_ptr: *const u8,
    state_len: i64,
) -> i64 {
    let s = sched();
    let id = s.next_id;
    s.next_id += 1;

    let heap_state = if state_len > 0 && !state_ptr.is_null() {
        let len = state_len as usize;
        unsafe {
            let layout = std::alloc::Layout::from_size_align(len, 8).unwrap();
            let ptr = std::alloc::alloc(layout);
            std::ptr::copy_nonoverlapping(state_ptr, ptr, len);
            ptr
        }
    } else {
        std::ptr::null_mut()
    };

    let sp = unsafe {
        let layout = std::alloc::Layout::from_size_align(STACK_SIZE, 16).unwrap();
        let base = std::alloc::alloc(layout);
        if base.is_null() {
            std::alloc::handle_alloc_error(layout);
        }
        let stack_top = base.add(STACK_SIZE);
        let stack_top = ((stack_top as usize) & !15) as *mut u8;
        init_process_stack(stack_top, process_trampoline)
    };

    s.processes.push(Process {
        id,
        func: fn_ptr,
        init_state: heap_state,
        mailbox: VecDeque::new(),
        state: ProcessState::Created,
        sp,
        deadline: None,
    });
    id
}

/// # Safety
/// `msg_ptr` must point to `msg_len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn expo_rt_send(pid: i64, msg_ptr: *const u8, msg_len: i64) {
    let s = sched();
    let idx = (pid - 1) as usize;
    if idx >= s.processes.len() {
        return;
    }

    let len = msg_len as usize;
    unsafe {
        let layout = std::alloc::Layout::from_size_align(len, 8).unwrap();
        let ptr = std::alloc::alloc(layout);
        std::ptr::copy_nonoverlapping(msg_ptr, ptr, len);
        s.processes[idx].mailbox.push_back(ptr);
    }

    if s.processes[idx].state == ProcessState::Blocked {
        s.processes[idx].state = ProcessState::Runnable;
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn expo_rt_receive() -> *const u8 {
    let s = sched();
    let idx = (s.current_pid - 1) as usize;

    if let Some(ptr) = s.processes[idx].mailbox.pop_front() {
        return ptr as *const u8;
    }

    s.processes[idx].state = ProcessState::Blocked;
    unsafe {
        let sched_sp = s.scheduler_sp;
        expo_context_switch(&mut s.processes[idx].sp, sched_sp);
    }

    let s = sched();
    let idx = (s.current_pid - 1) as usize;
    s.processes[idx]
        .mailbox
        .pop_front()
        .map(|p| p as *const u8)
        .unwrap_or(std::ptr::null())
}

#[unsafe(no_mangle)]
pub extern "C" fn expo_rt_receive_timeout(timeout_ms: i64) -> *const u8 {
    let s = sched();
    let idx = (s.current_pid - 1) as usize;

    if let Some(ptr) = s.processes[idx].mailbox.pop_front() {
        return ptr as *const u8;
    }

    s.processes[idx].state = ProcessState::Blocked;
    s.processes[idx].deadline = Some(Instant::now() + Duration::from_millis(timeout_ms as u64));
    unsafe {
        let sched_sp = s.scheduler_sp;
        expo_context_switch(&mut s.processes[idx].sp, sched_sp);
    }

    let s = sched();
    let idx = (s.current_pid - 1) as usize;
    s.processes[idx].deadline = None;
    s.processes[idx]
        .mailbox
        .pop_front()
        .map(|p| p as *const u8)
        .unwrap_or(std::ptr::null())
}

#[unsafe(no_mangle)]
pub extern "C" fn expo_rt_self() -> i64 {
    sched().current_pid
}

/// Validates that `len` bytes starting at `ptr` are valid UTF-8.
/// Returns 1 if valid, 0 otherwise.
///
/// # Safety
/// `ptr` must point to at least `len` readable bytes.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn expo_utf8_validate(ptr: *const u8, len: u64) -> i64 {
    let slice = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
    if std::str::from_utf8(slice).is_ok() {
        1
    } else {
        0
    }
}

// ---------------------------------------------------------------------------
// String intrinsics
// ---------------------------------------------------------------------------

/// Returns the number of Unicode scalar values (codepoints) in a NUL-terminated
/// UTF-8 string.
///
/// # Safety
/// `ptr` must point to a valid NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn expo_string_length(ptr: *const u8) -> i64 {
    let s = unsafe { std::ffi::CStr::from_ptr(ptr as *const i8) };
    let s = std::str::from_utf8(s.to_bytes()).unwrap();
    s.chars().count() as i64
}

/// Returns a newly allocated NUL-terminated string containing the single
/// character at the given codepoint index. Panics if `index` is out of bounds.
///
/// The returned pointer uses the standard `[i64 bit_length][payload...][NUL]`
/// layout: it points to the start of `payload`.
///
/// Returns the codepoint at `index`, or null if out of bounds.
///
/// # Safety
/// `ptr` must point to a valid NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn expo_string_get(ptr: *const u8, index: i64) -> *const u8 {
    let s = unsafe { std::ffi::CStr::from_ptr(ptr as *const i8) };
    let s = std::str::from_utf8(s.to_bytes()).unwrap();
    let Some(ch) = s.chars().nth(index as usize) else {
        return std::ptr::null();
    };
    let mut buf = [0u8; 4];
    let encoded = ch.encode_utf8(&mut buf);
    let byte_len = encoded.len();
    unsafe {
        let layout = std::alloc::Layout::from_size_align(8 + byte_len + 1, 8).unwrap();
        let base = std::alloc::alloc(layout);
        let bit_len = (byte_len as i64) * 8;
        std::ptr::copy_nonoverlapping(&bit_len as *const i64 as *const u8, base, 8);
        let payload = base.add(8);
        std::ptr::copy_nonoverlapping(encoded.as_ptr(), payload, byte_len);
        *payload.add(byte_len) = 0;
        payload
    }
}

/// Returns a newly allocated substring spanning the inclusive codepoint range
/// `[start, stop]`. Out-of-bounds endpoints are clamped.
///
/// # Safety
/// `ptr` must point to a valid NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn expo_string_slice(ptr: *const u8, start: i64, stop: i64) -> *const u8 {
    let s = unsafe { std::ffi::CStr::from_ptr(ptr as *const i8) };
    let s = std::str::from_utf8(s.to_bytes()).unwrap();
    let len = s.chars().count();

    let start = (start as usize).min(len);
    let stop = ((stop + 1) as usize).min(len);
    let stop = stop.max(start);

    let byte_start = s
        .char_indices()
        .nth(start)
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    let byte_end = if stop == len {
        s.len()
    } else {
        s.char_indices()
            .nth(stop)
            .map(|(i, _)| i)
            .unwrap_or(s.len())
    };
    let slice = &s[byte_start..byte_end];
    let byte_len = slice.len();

    unsafe {
        let layout = std::alloc::Layout::from_size_align(8 + byte_len + 1, 8).unwrap();
        let base = std::alloc::alloc(layout);
        let bit_len = (byte_len as i64) * 8;
        std::ptr::copy_nonoverlapping(&bit_len as *const i64 as *const u8, base, 8);
        let payload = base.add(8);
        std::ptr::copy_nonoverlapping(slice.as_ptr(), payload, byte_len);
        *payload.add(byte_len) = 0;
        payload
    }
}

/// Attempts to parse a NUL-terminated UTF-8 string as a 64-bit signed integer.
/// On success, writes the parsed value to `*out` and returns 1. On failure,
/// returns 0 and leaves `*out` unchanged.
///
/// # Safety
/// `ptr` must point to a valid NUL-terminated string. `out` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn expo_int_parse(ptr: *const u8, out: *mut i64) -> i64 {
    let s = unsafe { std::ffi::CStr::from_ptr(ptr as *const i8) };
    let s = std::str::from_utf8(s.to_bytes()).unwrap();
    match s.trim().parse::<i64>() {
        Ok(v) => {
            unsafe { *out = v };
            1
        }
        Err(_) => 0,
    }
}

/// Attempts to parse a NUL-terminated UTF-8 string as a 64-bit float.
/// On success, writes the parsed value to `*out` and returns 1. On failure,
/// returns 0 and leaves `*out` unchanged.
///
/// # Safety
/// `ptr` must point to a valid NUL-terminated string. `out` must be writable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn expo_float_parse(ptr: *const u8, out: *mut f64) -> i64 {
    let s = unsafe { std::ffi::CStr::from_ptr(ptr as *const i8) };
    let s = std::str::from_utf8(s.to_bytes()).unwrap();
    match s.trim().parse::<f64>() {
        Ok(v) => {
            unsafe { *out = v };
            1
        }
        Err(_) => 0,
    }
}

// ---------------------------------------------------------------------------
// File I/O intrinsics
// ---------------------------------------------------------------------------

thread_local! {
    static LAST_IO_ERROR: std::cell::RefCell<Option<String>> = const { std::cell::RefCell::new(None) };
}

fn set_last_error(e: impl std::fmt::Display) {
    LAST_IO_ERROR.with(|cell| {
        *cell.borrow_mut() = Some(e.to_string());
    });
}

/// Allocates a length-prefixed Expo string from a byte slice.
/// Layout: `[i64 bit_length][payload...\0]`, returns pointer to payload.
unsafe fn alloc_expo_string(bytes: &[u8]) -> *const u8 {
    let byte_len = bytes.len();
    unsafe {
        let layout = std::alloc::Layout::from_size_align(8 + byte_len + 1, 8).unwrap();
        let base = std::alloc::alloc(layout);
        let bit_len = (byte_len as i64) * 8;
        std::ptr::copy_nonoverlapping(&bit_len as *const i64 as *const u8, base, 8);
        let payload = base.add(8);
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), payload, byte_len);
        *payload.add(byte_len) = 0;
        payload
    }
}

/// Extracts the byte slice from a length-prefixed Expo string pointer.
unsafe fn expo_string_to_slice<'a>(ptr: *const u8) -> &'a [u8] {
    unsafe {
        let hdr = ptr.sub(8) as *const i64;
        let bit_len = *hdr;
        let byte_len = (bit_len / 8) as usize;
        std::slice::from_raw_parts(ptr, byte_len)
    }
}

/// Returns the error message for the most recent failed I/O call.
#[unsafe(no_mangle)]
pub extern "C" fn expo_last_error() -> *const u8 {
    LAST_IO_ERROR.with(|cell| {
        let msg = cell.borrow();
        match msg.as_deref() {
            Some(s) => unsafe { alloc_expo_string(s.as_bytes()) },
            None => unsafe { alloc_expo_string(b"unknown error") },
        }
    })
}

/// Reads up to `count` bytes from a raw file descriptor.
/// Returns a length-prefixed string pointer, or null on error.
#[unsafe(no_mangle)]
pub extern "C" fn expo_fd_read(fd: i64, count: i64) -> *const u8 {
    let mut buf = vec![0u8; count as usize];
    let n = unsafe { libc_read(fd as i32, buf.as_mut_ptr(), buf.len()) };
    if n < 0 {
        set_last_error(std::io::Error::last_os_error());
        return std::ptr::null();
    }
    buf.truncate(n as usize);
    unsafe { alloc_expo_string(&buf) }
}

/// Writes a length-prefixed string's contents to a raw file descriptor.
/// Returns bytes written, or -1 on error.
///
/// # Safety
/// `data_ptr` must point to a valid length-prefixed Expo string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn expo_fd_write(fd: i64, data_ptr: *const u8) -> i64 {
    let slice = unsafe { expo_string_to_slice(data_ptr) };
    let n = unsafe { libc_write(fd as i32, slice.as_ptr(), slice.len()) };
    if n < 0 {
        set_last_error(std::io::Error::last_os_error());
        return -1;
    }
    n as i64
}

/// Closes a raw file descriptor. Returns 0 on success, -1 on error.
#[unsafe(no_mangle)]
pub extern "C" fn expo_fd_close(fd: i64) -> i64 {
    let ret = unsafe { libc_close(fd as i32) };
    if ret < 0 {
        set_last_error(std::io::Error::last_os_error());
        return -1;
    }
    0
}

/// Opens a file. `mode`: 0 = read, 1 = write (create/truncate), 2 = append.
/// Returns fd on success, -1 on error.
///
/// # Safety
/// `path_ptr` must point to a valid NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn expo_file_open(path_ptr: *const u8, mode: i64) -> i64 {
    let path = unsafe { std::ffi::CStr::from_ptr(path_ptr as *const i8) };
    let path = match path.to_str() {
        Ok(s) => s,
        Err(e) => {
            set_last_error(e);
            return -1;
        }
    };

    use std::fs::OpenOptions;
    let file = match mode {
        0 => OpenOptions::new().read(true).open(path),
        1 => OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(path),
        2 => OpenOptions::new().append(true).create(true).open(path),
        _ => {
            set_last_error("invalid file open mode");
            return -1;
        }
    };

    match file {
        Ok(f) => {
            use std::os::fd::IntoRawFd;
            f.into_raw_fd() as i64
        }
        Err(e) => {
            set_last_error(e);
            -1
        }
    }
}

/// Reads the entire contents of a file as a length-prefixed string.
/// Returns null on error.
///
/// # Safety
/// `path_ptr` must point to a valid NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn expo_file_read_all(path_ptr: *const u8) -> *const u8 {
    let path = unsafe { std::ffi::CStr::from_ptr(path_ptr as *const i8) };
    let path = match path.to_str() {
        Ok(s) => s,
        Err(e) => {
            set_last_error(e);
            return std::ptr::null();
        }
    };

    match std::fs::read(path) {
        Ok(bytes) => unsafe { alloc_expo_string(&bytes) },
        Err(e) => {
            set_last_error(e);
            std::ptr::null()
        }
    }
}

/// Writes `content` to the file at `path`, creating or truncating it.
/// Returns 0 on success, -1 on error.
///
/// # Safety
/// Both pointers must point to valid NUL-terminated UTF-8 strings.
/// `content_ptr` must be a length-prefixed Expo string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn expo_file_write_all(path_ptr: *const u8, content_ptr: *const u8) -> i64 {
    let path = unsafe { std::ffi::CStr::from_ptr(path_ptr as *const i8) };
    let path = match path.to_str() {
        Ok(s) => s,
        Err(e) => {
            set_last_error(e);
            return -1;
        }
    };
    let data = unsafe { expo_string_to_slice(content_ptr) };
    match std::fs::write(path, data) {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(e);
            -1
        }
    }
}

/// Returns 1 if the file at `path` exists, 0 otherwise.
///
/// # Safety
/// `path_ptr` must point to a valid NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn expo_file_exists(path_ptr: *const u8) -> i64 {
    let path = unsafe { std::ffi::CStr::from_ptr(path_ptr as *const i8) };
    let path = match path.to_str() {
        Ok(s) => s,
        Err(e) => {
            set_last_error(e);
            return 0;
        }
    };
    if std::path::Path::new(path).exists() {
        1
    } else {
        0
    }
}

/// Deletes the file at `path`. Returns 0 on success, -1 on error.
///
/// # Safety
/// `path_ptr` must point to a valid NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn expo_file_delete(path_ptr: *const u8) -> i64 {
    let path = unsafe { std::ffi::CStr::from_ptr(path_ptr as *const i8) };
    let path = match path.to_str() {
        Ok(s) => s,
        Err(e) => {
            set_last_error(e);
            return -1;
        }
    };
    match std::fs::remove_file(path) {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(e);
            -1
        }
    }
}

/// Renames `src` to `dst`. Returns 0 on success, -1 on error.
///
/// # Safety
/// Both pointers must point to valid NUL-terminated UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn expo_file_rename(src_ptr: *const u8, dst_ptr: *const u8) -> i64 {
    let src = unsafe { std::ffi::CStr::from_ptr(src_ptr as *const i8) };
    let dst = unsafe { std::ffi::CStr::from_ptr(dst_ptr as *const i8) };
    let src = match src.to_str() {
        Ok(s) => s,
        Err(e) => {
            set_last_error(e);
            return -1;
        }
    };
    let dst = match dst.to_str() {
        Ok(s) => s,
        Err(e) => {
            set_last_error(e);
            return -1;
        }
    };
    match std::fs::rename(src, dst) {
        Ok(()) => 0,
        Err(e) => {
            set_last_error(e);
            -1
        }
    }
}

// ---------------------------------------------------------------------------
// System operations
// ---------------------------------------------------------------------------

/// Returns the value of environment variable `key` as a leaked C string,
/// or null if the variable is not set.
///
/// # Safety
/// `key_ptr` must point to a valid NUL-terminated UTF-8 string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn expo_get_env(key_ptr: *const u8) -> *const u8 {
    let key = unsafe { std::ffi::CStr::from_ptr(key_ptr as *const i8) };
    let key = match key.to_str() {
        Ok(s) => s,
        Err(_) => return std::ptr::null(),
    };
    match std::env::var(key) {
        Ok(val) => {
            let c = std::ffi::CString::new(val).unwrap();
            c.into_raw() as *const u8
        }
        Err(_) => std::ptr::null(),
    }
}

/// Sets the environment variable `key` to `value`.
///
/// # Safety
/// Both pointers must point to valid NUL-terminated UTF-8 strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn expo_set_env(key_ptr: *const u8, val_ptr: *const u8) {
    let key = unsafe { std::ffi::CStr::from_ptr(key_ptr as *const i8) };
    let val = unsafe { std::ffi::CStr::from_ptr(val_ptr as *const i8) };
    if let (Ok(k), Ok(v)) = (key.to_str(), val.to_str()) {
        unsafe { std::env::set_var(k, v) };
    }
}

/// Returns the current working directory as a leaked C string, or null on error.
#[unsafe(no_mangle)]
pub extern "C" fn expo_cwd() -> *const u8 {
    match std::env::current_dir() {
        Ok(path) => {
            let s = path.to_string_lossy().into_owned();
            let c = std::ffi::CString::new(s).unwrap();
            c.into_raw() as *const u8
        }
        Err(e) => {
            set_last_error(e);
            std::ptr::null()
        }
    }
}

/// Returns the system hostname as a leaked C string.
#[unsafe(no_mangle)]
pub extern "C" fn expo_hostname() -> *const u8 {
    let mut buf = [0u8; 256];
    let ret = unsafe { libc_gethostname(buf.as_mut_ptr() as *mut i8, buf.len()) };
    if ret != 0 {
        let c = std::ffi::CString::new("unknown").unwrap();
        return c.into_raw() as *const u8;
    }
    let len = buf.iter().position(|&b| b == 0).unwrap_or(0);
    let s = String::from_utf8_lossy(&buf[..len]).into_owned();
    let c = std::ffi::CString::new(s).unwrap();
    c.into_raw() as *const u8
}

// ---------------------------------------------------------------------------
// Time operations
// ---------------------------------------------------------------------------

/// Returns the current wall-clock time as milliseconds since the Unix epoch.
#[unsafe(no_mangle)]
pub extern "C" fn expo_time_now_millis() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// Socket operations
// ---------------------------------------------------------------------------

#[repr(C)]
struct SockaddrIn {
    sin_len: u8,
    sin_family: u8,
    sin_port: u16,
    sin_addr: u32,
    sin_zero: [u8; 8],
}

#[unsafe(no_mangle)]
pub extern "C" fn expo_socket_create() -> i64 {
    let fd = unsafe {
        libc_socket(2 /* AF_INET */, 1 /* SOCK_STREAM */, 0)
    };
    if fd < 0 {
        set_last_error(std::io::Error::last_os_error());
        return -1;
    }
    fd as i64
}

#[unsafe(no_mangle)]
pub extern "C" fn expo_socket_bind(fd: i64, port: i64) -> i64 {
    let addr = SockaddrIn {
        sin_len: std::mem::size_of::<SockaddrIn>() as u8,
        sin_family: 2, // AF_INET
        sin_port: (port as u16).to_be(),
        sin_addr: 0, // INADDR_ANY
        sin_zero: [0; 8],
    };
    let ret = unsafe {
        libc_bind(
            fd as i32,
            &addr as *const SockaddrIn as *const u8,
            std::mem::size_of::<SockaddrIn>() as u32,
        )
    };
    if ret < 0 {
        set_last_error(std::io::Error::last_os_error());
        return -1;
    }
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn expo_socket_listen(fd: i64, backlog: i64) -> i64 {
    let ret = unsafe { libc_listen(fd as i32, backlog as i32) };
    if ret < 0 {
        set_last_error(std::io::Error::last_os_error());
        return -1;
    }
    0
}

#[unsafe(no_mangle)]
pub extern "C" fn expo_socket_accept(fd: i64) -> i64 {
    let client = unsafe { libc_accept(fd as i32, std::ptr::null_mut(), std::ptr::null_mut()) };
    if client < 0 {
        set_last_error(std::io::Error::last_os_error());
        return -1;
    }
    client as i64
}

#[unsafe(no_mangle)]
pub extern "C" fn expo_socket_setsockopt_reuse(fd: i64) -> i64 {
    let optval: i32 = 1;
    let ret = unsafe {
        libc_setsockopt(
            fd as i32,
            0xFFFF, // SOL_SOCKET
            0x0004, // SO_REUSEADDR
            &optval as *const i32 as *const u8,
            std::mem::size_of::<i32>() as u32,
        )
    };
    if ret < 0 {
        set_last_error(std::io::Error::last_os_error());
        return -1;
    }
    0
}

unsafe extern "C" {
    #[link_name = "read"]
    fn libc_read(fd: i32, buf: *mut u8, count: usize) -> isize;
    #[link_name = "write"]
    fn libc_write(fd: i32, buf: *const u8, count: usize) -> isize;
    #[link_name = "close"]
    fn libc_close(fd: i32) -> i32;
    #[link_name = "socket"]
    fn libc_socket(domain: i32, sock_type: i32, protocol: i32) -> i32;
    #[link_name = "bind"]
    fn libc_bind(fd: i32, addr: *const u8, addrlen: u32) -> i32;
    #[link_name = "listen"]
    fn libc_listen(fd: i32, backlog: i32) -> i32;
    #[link_name = "accept"]
    fn libc_accept(fd: i32, addr: *mut u8, addrlen: *mut u32) -> i32;
    #[link_name = "setsockopt"]
    fn libc_setsockopt(fd: i32, level: i32, optname: i32, optval: *const u8, optlen: u32) -> i32;
    fn fflush(stream: *mut u8) -> i32;
    #[link_name = "gethostname"]
    fn libc_gethostname(name: *mut i8, len: usize) -> i32;
}

// ---------------------------------------------------------------------------
// Scheduler loop
// ---------------------------------------------------------------------------

#[unsafe(no_mangle)]
pub extern "C" fn expo_rt_main_done() {
    let s = sched();

    loop {
        // Wake any blocked processes whose deadline has expired.
        let now = Instant::now();
        for proc in s.processes.iter_mut() {
            if proc.state == ProcessState::Blocked
                && let Some(dl) = proc.deadline
                && now >= dl
            {
                proc.state = ProcessState::Runnable;
            }
        }

        // Find the next Created or Runnable process and switch into it.
        let mut ran = false;
        for i in 0..s.processes.len() {
            if s.processes[i].state == ProcessState::Created
                || s.processes[i].state == ProcessState::Runnable
            {
                s.processes[i].state = ProcessState::Running;
                s.current_pid = s.processes[i].id;
                unsafe {
                    expo_context_switch(&mut s.scheduler_sp, s.processes[i].sp);
                }
                ran = true;
                break;
            }
        }

        if ran {
            continue;
        }

        // Nothing was runnable — check if main (pid=1) is dead.
        // When main exits, the program is done (like Erlang's init).
        if !s.processes.is_empty() && s.processes[0].state == ProcessState::Dead {
            break;
        }

        // Check if any process is still alive.
        let any_alive = s.processes.iter().any(|p| p.state != ProcessState::Dead);

        if !any_alive {
            break;
        }

        // All living processes are blocked. Sleep to the nearest
        // deadline, or report deadlock if there are none.
        let nearest = s
            .processes
            .iter()
            .filter(|p| p.state == ProcessState::Blocked)
            .filter_map(|p| p.deadline)
            .min();

        match nearest {
            Some(dl) => {
                let now = Instant::now();
                if dl > now {
                    std::thread::sleep(dl - now);
                }
            }
            None => {
                eprintln!("expo runtime: deadlock — all processes blocked without timeout");
                break;
            }
        }
    }
}
