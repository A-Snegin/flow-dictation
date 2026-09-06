//! Windows process plumbing: waking the message thread, thread priority and
//! the monotonic clock. The Linux counterpart lives in platform/linux/sys.rs.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;

use windows::Win32::Foundation::{LPARAM, WPARAM};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};
use windows::Win32::System::Threading::GetCurrentThreadId;
use windows::Win32::UI::WindowsAndMessaging::PostThreadMessageW;

/// Posted to the message thread whenever there is something to service, so the
/// loop can block rather than poll. Polling cost 1.4% of a core at idle and put
/// up to 2 ms between the final transcript and the paste.
pub const WM_FLOW_WAKE: u32 = 0x0400 + 2;

/// Where to post a wake-up when an event is queued, so the UI thread can sit
/// in a blocking wait instead of polling. Zero means nobody is listening.
#[derive(Clone, Default)]
pub struct Waker {
    thread_id: Arc<AtomicU32>,
    message: u32,
}

impl Waker {
    pub fn for_current_thread(message: u32) -> Waker {
        let id = unsafe { GetCurrentThreadId() };
        Waker {
            thread_id: Arc::new(AtomicU32::new(id)),
            message,
        }
    }

    pub fn wake(&self) {
        let id = self.thread_id.load(Ordering::Relaxed);
        if id != 0 {
            unsafe {
                let _ = PostThreadMessageW(id, self.message, WPARAM(0), LPARAM(0));
            }
        }
    }
}

/// Nudges the calling thread above normal priority. Best effort: if the call
/// fails the worker simply runs at the default priority.
pub fn raise_priority() {
    use windows::Win32::System::Threading::{
        GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_ABOVE_NORMAL,
    };
    unsafe {
        let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_ABOVE_NORMAL);
    }
}

/// Raw counter ticks.
pub fn now() -> i64 {
    let mut t = 0i64;
    unsafe {
        let _ = QueryPerformanceCounter(&mut t);
    }
    t
}

pub fn freq() -> i64 {
    let mut f = 0i64;
    unsafe {
        let _ = QueryPerformanceFrequency(&mut f);
    }
    if f == 0 {
        1
    } else {
        f
    }
}
