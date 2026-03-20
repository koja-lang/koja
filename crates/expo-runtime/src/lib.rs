//! Expo process runtime: single-threaded cooperative scheduler with
//! typed mailboxes. Processes run after `main` returns.

use std::cell::UnsafeCell;
use std::collections::VecDeque;

type ProcessFn = extern "C" fn(*const u8);

struct Process {
    id: i64,
    func: ProcessFn,
    init_state: *mut u8,
    mailbox: VecDeque<*mut u8>,
    state: ProcessState,
}

#[derive(PartialEq)]
enum ProcessState {
    Created,
    Running,
    Dead,
}

struct Scheduler {
    processes: Vec<Process>,
    next_id: i64,
    current_pid: i64,
}

impl Scheduler {
    fn new() -> Self {
        Scheduler {
            processes: Vec::new(),
            next_id: 1,
            current_pid: -1,
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

    s.processes.push(Process {
        id,
        func: fn_ptr,
        init_state: heap_state,
        mailbox: VecDeque::new(),
        state: ProcessState::Created,
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
}

#[unsafe(no_mangle)]
pub extern "C" fn expo_rt_receive() -> *const u8 {
    let s = sched();
    let idx = (s.current_pid - 1) as usize;
    if idx >= s.processes.len() {
        return std::ptr::null();
    }
    match s.processes[idx].mailbox.pop_front() {
        Some(ptr) => ptr as *const u8,
        None => std::ptr::null(),
    }
}

/// Receive with timeout. In the single-threaded cooperative scheduler,
/// a timeout always fires immediately when the mailbox is empty since
/// there is no preemption. Returns null on timeout.
#[unsafe(no_mangle)]
pub extern "C" fn expo_rt_receive_timeout(_timeout_ms: i64) -> *const u8 {
    let s = sched();
    let idx = (s.current_pid - 1) as usize;
    if idx >= s.processes.len() {
        return std::ptr::null();
    }
    match s.processes[idx].mailbox.pop_front() {
        Some(ptr) => ptr as *const u8,
        None => std::ptr::null(),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn expo_rt_main_done() {
    let s = sched();
    loop {
        let mut any_ran = false;
        for i in 0..s.processes.len() {
            if s.processes[i].state == ProcessState::Created {
                s.processes[i].state = ProcessState::Running;
                s.current_pid = s.processes[i].id;
                any_ran = true;
                (s.processes[i].func)(s.processes[i].init_state);
                s.processes[i].state = ProcessState::Dead;
            }
        }
        if !any_ran {
            break;
        }
    }
}
