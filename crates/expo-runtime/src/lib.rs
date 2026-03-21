//! Expo process runtime: cooperative coroutine scheduler with typed
//! mailboxes. Each process runs on its own stack and yields on
//! `receive` when its mailbox is empty.

use std::cell::UnsafeCell;
use std::collections::VecDeque;
use std::time::{Duration, Instant};

const STACK_SIZE: usize = 64 * 1024;

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
        let layout = std::alloc::Layout::from_size_align(len, 1).unwrap();
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
