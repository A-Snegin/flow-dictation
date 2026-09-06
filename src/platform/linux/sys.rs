//! Linux process plumbing: waking the main thread, thread priority and the
//! monotonic clock. The Windows counterpart lives in platform/windows/sys.rs.

use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::sync::Arc;
use std::time::Duration;

/// An eventfd the main loop blocks on. Any thread with a clone can `wake()`
/// it; the main thread `wait()`s with a timeout and drains it.
#[derive(Clone, Default)]
pub struct Waker {
    fd: Option<Arc<OwnedFd>>,
}

impl Waker {
    pub fn new() -> Waker {
        let raw = unsafe { libc::eventfd(0, libc::EFD_NONBLOCK | libc::EFD_CLOEXEC) };
        if raw < 0 {
            return Waker { fd: None };
        }
        Waker {
            fd: Some(Arc::new(unsafe { OwnedFd::from_raw_fd(raw) })),
        }
    }

    pub fn fd(&self) -> Option<RawFd> {
        self.fd.as_ref().map(|f| f.as_raw_fd())
    }

    pub fn wake(&self) {
        if let Some(fd) = &self.fd {
            let one: u64 = 1;
            unsafe {
                let _ = libc::write(fd.as_raw_fd(), &one as *const u64 as *const _, 8);
            }
        }
    }

    /// Blocks until woken or `timeout` passes, then drains the counter.
    /// Returns true when a wake arrived.
    pub fn wait(&self, timeout: Duration) -> bool {
        let Some(fd) = &self.fd else {
            std::thread::sleep(timeout);
            return false;
        };
        let mut pfd = libc::pollfd {
            fd: fd.as_raw_fd(),
            events: libc::POLLIN,
            revents: 0,
        };
        let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
        let n = unsafe { libc::poll(&mut pfd, 1, ms) };
        if n > 0 {
            let mut v: u64 = 0;
            unsafe {
                let _ = libc::read(fd.as_raw_fd(), &mut v as *mut u64 as *mut _, 8);
            }
            true
        } else {
            false
        }
    }
}

/// Nudges the calling thread above normal priority. Best effort: without
/// CAP_SYS_NICE the call fails and the worker runs at the default priority.
pub fn raise_priority() {
    unsafe {
        let tid = libc::syscall(libc::SYS_gettid) as libc::id_t;
        let _ = libc::setpriority(libc::PRIO_PROCESS, tid, -5);
    }
}

/// CLOCK_MONOTONIC in nanoseconds.
pub fn now() -> i64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
    }
    ts.tv_sec as i64 * 1_000_000_000 + ts.tv_nsec as i64
}

pub fn freq() -> i64 {
    1_000_000_000
}
