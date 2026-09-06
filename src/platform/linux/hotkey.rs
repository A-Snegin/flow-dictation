//! Hold-to-dictate on Linux, over a Unix control socket.
//!
//! Wayland has no global keyboard hook, and reading `/dev/input` needs the
//! `input` group. The compositor already knows the key, so it delivers the
//! edges instead: Hyprland binds press and release to `flow-core --key down`
//! and `flow-core --key up`, which connect to `$XDG_RUNTIME_DIR/flow/control.sock`
//! and exit. The resident process turns each line into a `HotkeyEvent` and
//! wakes the main loop, exactly as the Windows keyboard hook does.
//!
//! The same socket carries the commands that were tray menu items on Windows:
//! `toggle`, `cancel`, `reload`, `report`, `quit`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Duration;

pub use crate::control::HotkeyEvent;
use crate::platform::sys::Waker;

/// How long either side waits on the other before giving up. Only reached when
/// something is wedged: the happy path is a single line each way.
const IO_TIMEOUT: Duration = Duration::from_secs(1);

/// evdev key codes, kept so `settings.hotkey.key` means the same thing on both
/// platforms and so an evdev backend can be added later without moving names.
/// The socket backend never looks at them: the compositor owns the binding.
pub mod vk {
    pub const RCONTROL: u32 = 97;
    pub const LCONTROL: u32 = 29;
    pub const RMENU: u32 = 100;
    pub const RSHIFT: u32 = 54;
    pub const F13: u32 = 183;
    pub const CAPITAL: u32 = 58;

    pub fn from_name(name: &str) -> Option<u32> {
        Some(match name.to_ascii_lowercase().as_str() {
            "rightctrl" | "rctrl" | "right_control" => RCONTROL,
            "leftctrl" | "lctrl" => LCONTROL,
            "rightalt" | "ralt" => RMENU,
            "rightshift" | "rshift" => RSHIFT,
            "f13" => F13,
            "capslock" => CAPITAL,
            _ => return None,
        })
    }
}

/// The listener. Dropping it is not enough to stop the thread; pass it to
/// `uninstall` so the socket file goes away with the process that owns it.
pub struct Handle {
    path: PathBuf,
    shutdown: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}

/// Directory holding the control socket: `$XDG_RUNTIME_DIR/flow`, or a
/// per-user directory under `/tmp` when the session has no runtime dir.
fn control_dir() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(d) if !d.is_empty() => PathBuf::from(d).join("flow"),
        _ => {
            let uid = unsafe { libc::getuid() };
            PathBuf::from(format!("/tmp/flow-{uid}"))
        }
    }
}

/// Where `flow-core --key ...` and the resident process meet.
pub fn control_socket_path() -> PathBuf {
    control_dir().join("control.sock")
}

/// Starts the control socket listener. `key` is ignored by this backend: the
/// compositor decides which key produces `down` and `up`.
pub fn install(_key: u32, tx: Sender<HotkeyEvent>, waker: Waker) -> Result<Handle, String> {
    install_at(&control_socket_path(), tx, waker)
}

/// The body of `install`, with the socket path given rather than derived, so
/// tests can run against a temporary directory without touching the
/// environment other threads are reading.
fn install_at(path: &Path, tx: Sender<HotkeyEvent>, waker: Waker) -> Result<Handle, String> {
    let dir = path
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", path.display()))?;
    std::fs::create_dir_all(dir).map_err(|e| format!("create {}: {e}", dir.display()))?;
    // Nobody else on the machine gets to drive this process.
    let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));

    let listener = bind(path)?;
    listener
        .set_nonblocking(false)
        .map_err(|e| format!("control socket: {e}"))?;

    let shutdown = Arc::new(AtomicBool::new(false));
    let thread = {
        let shutdown = Arc::clone(&shutdown);
        std::thread::Builder::new()
            .name("flow-control".into())
            .spawn(move || serve(listener, tx, waker, &shutdown))
            .map_err(|e| e.to_string())?
    };

    Ok(Handle {
        path: path.to_path_buf(),
        shutdown,
        thread: Some(thread),
    })
}

/// Binds the listener, clearing a socket left behind by a crashed process.
/// A socket that still answers means another Flow is already running, which is
/// an error rather than something to stomp on.
fn bind(path: &Path) -> Result<UnixListener, String> {
    match UnixListener::bind(path) {
        Ok(l) => return Ok(l),
        Err(e) if e.kind() != std::io::ErrorKind::AddrInUse => {
            return Err(format!("bind {}: {e}", path.display()));
        }
        Err(_) => {}
    }

    if UnixStream::connect(path).is_ok() {
        return Err(format!(
            "another flow-core already owns {}",
            path.display()
        ));
    }

    std::fs::remove_file(path).map_err(|e| format!("remove stale {}: {e}", path.display()))?;
    UnixListener::bind(path).map_err(|e| format!("bind {}: {e}", path.display()))
}

/// This backend has no key of its own to rebind. Kept so `main.rs` can call it
/// on a settings reload without a `cfg`.
pub fn rebind(_key: u32) {}

pub fn uninstall(mut handle: Handle) {
    handle.shutdown.store(true, Ordering::SeqCst);
    // Unblock the accept: the thread checks the flag before it reads.
    let _ = UnixStream::connect(&handle.path);
    if let Some(t) = handle.thread.take() {
        let _ = t.join();
    }
    let _ = std::fs::remove_file(&handle.path);
}

fn serve(
    listener: UnixListener,
    tx: Sender<HotkeyEvent>,
    waker: Waker,
    shutdown: &AtomicBool,
) {
    // Only the edges count. A compositor that repeats the press, or a stray
    // `up` with no `down` before it, must not start or end an utterance twice.
    let held = AtomicBool::new(false);

    for stream in listener.incoming() {
        if shutdown.load(Ordering::SeqCst) {
            break;
        }
        let Ok(mut stream) = stream else { continue };
        let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
        let _ = stream.set_write_timeout(Some(IO_TIMEOUT));

        let mut line = String::new();
        let read = {
            let mut reader = BufReader::new(&stream);
            reader.read_line(&mut line)
        };
        if read.is_err() {
            continue;
        }

        let reply = match parse_line(&line) {
            Some(event) => {
                if let Some(event) = dedupe(event, &held) {
                    let _ = tx.send(event);
                    waker.wake();
                }
                "ok\n"
            }
            None => "unknown\n",
        };
        let _ = stream.write_all(reply.as_bytes());
        let _ = stream.flush();
        let _ = stream.shutdown(std::net::Shutdown::Both);
    }
}

/// One line of the protocol. Case and surrounding whitespace are forgiven
/// because these lines are typed into a compositor config by hand.
fn parse_line(line: &str) -> Option<HotkeyEvent> {
    Some(match line.trim().to_ascii_lowercase().as_str() {
        "down" => HotkeyEvent::Down,
        "up" => HotkeyEvent::Up,
        "cancel" => HotkeyEvent::Cancel,
        "toggle" => HotkeyEvent::Toggle,
        "reload" => HotkeyEvent::Reload,
        "report" => HotkeyEvent::Report,
        "quit" => HotkeyEvent::Quit,
        _ => return None,
    })
}

/// Collapses repeated `down` into one press and drops an `up` that no press
/// preceded. Everything else passes straight through.
fn dedupe(event: HotkeyEvent, held: &AtomicBool) -> Option<HotkeyEvent> {
    match event {
        HotkeyEvent::Down => {
            if held.swap(true, Ordering::SeqCst) {
                None
            } else {
                Some(HotkeyEvent::Down)
            }
        }
        HotkeyEvent::Up => {
            if held.swap(false, Ordering::SeqCst) {
                Some(HotkeyEvent::Up)
            } else {
                None
            }
        }
        // Cancel ends an utterance too, so the next `up` has nothing to end.
        HotkeyEvent::Cancel => {
            held.store(false, Ordering::SeqCst);
            Some(HotkeyEvent::Cancel)
        }
        other => Some(other),
    }
}

/// Client side: deliver one command to a running flow-core and wait for the
/// acknowledgement. This runs on the compositor's key path, so it does the
/// minimum: connect, write a line, read the reply, exit.
pub fn send_command(cmd: &str) -> Result<(), String> {
    send_command_to(&control_socket_path(), cmd)
}

fn send_command_to(path: &Path, cmd: &str) -> Result<(), String> {
    let mut stream = UnixStream::connect(path)
        .map_err(|e| format!("connect {}: {e}", path.display()))?;
    let _ = stream.set_read_timeout(Some(IO_TIMEOUT));
    let _ = stream.set_write_timeout(Some(IO_TIMEOUT));

    let line = format!("{}\n", cmd.trim());
    stream
        .write_all(line.as_bytes())
        .map_err(|e| format!("write: {e}"))?;
    stream.flush().map_err(|e| format!("flush: {e}"))?;

    let mut reply = String::new();
    BufReader::new(&stream)
        .read_line(&mut reply)
        .map_err(|e| format!("read reply: {e}"))?;
    match reply.trim() {
        "ok" => Ok(()),
        "unknown" => Err(format!("flow-core does not understand \"{}\"", cmd.trim())),
        "" => Err("flow-core closed the connection without replying".into()),
        other => Err(format!("unexpected reply \"{other}\"")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_every_command() {
        assert_eq!(parse_line("down\n"), Some(HotkeyEvent::Down));
        assert_eq!(parse_line("up\n"), Some(HotkeyEvent::Up));
        assert_eq!(parse_line("cancel\n"), Some(HotkeyEvent::Cancel));
        assert_eq!(parse_line("toggle\n"), Some(HotkeyEvent::Toggle));
        assert_eq!(parse_line("reload\n"), Some(HotkeyEvent::Reload));
        assert_eq!(parse_line("report\n"), Some(HotkeyEvent::Report));
        assert_eq!(parse_line("quit\n"), Some(HotkeyEvent::Quit));
    }

    #[test]
    fn forgives_case_and_whitespace() {
        assert_eq!(parse_line("  DOWN  \r\n"), Some(HotkeyEvent::Down));
    }

    #[test]
    fn rejects_anything_else() {
        assert_eq!(parse_line(""), None);
        assert_eq!(parse_line("start\n"), None);
        assert_eq!(parse_line("down up\n"), None);
    }

    #[test]
    fn repeated_down_is_one_press() {
        let held = AtomicBool::new(false);
        assert_eq!(dedupe(HotkeyEvent::Down, &held), Some(HotkeyEvent::Down));
        assert_eq!(dedupe(HotkeyEvent::Down, &held), None);
        assert_eq!(dedupe(HotkeyEvent::Up, &held), Some(HotkeyEvent::Up));
    }

    #[test]
    fn up_without_down_is_ignored() {
        let held = AtomicBool::new(false);
        assert_eq!(dedupe(HotkeyEvent::Up, &held), None);
        assert_eq!(dedupe(HotkeyEvent::Up, &held), None);
    }

    #[test]
    fn cancel_releases_the_hold() {
        let held = AtomicBool::new(false);
        assert_eq!(dedupe(HotkeyEvent::Down, &held), Some(HotkeyEvent::Down));
        assert_eq!(dedupe(HotkeyEvent::Cancel, &held), Some(HotkeyEvent::Cancel));
        assert_eq!(dedupe(HotkeyEvent::Up, &held), None);
        assert_eq!(dedupe(HotkeyEvent::Down, &held), Some(HotkeyEvent::Down));
    }

    #[test]
    fn other_commands_pass_through_unchanged() {
        let held = AtomicBool::new(false);
        for e in [
            HotkeyEvent::Toggle,
            HotkeyEvent::Reload,
            HotkeyEvent::Report,
            HotkeyEvent::Quit,
        ] {
            assert_eq!(dedupe(e, &held), Some(e));
        }
        assert!(!held.load(Ordering::SeqCst));
    }

    /// The whole round trip on a real socket in a temporary runtime dir.
    #[test]
    fn socket_round_trip_delivers_events() {
        let dir = std::env::temp_dir().join(format!("flow-hotkey-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let path = dir.join("control.sock");

        let (tx, rx) = std::sync::mpsc::channel();
        let waker = Waker::new();
        let handle = install_at(&path, tx, waker.clone()).expect("install");

        send_command_to(&path, "down").expect("down");
        send_command_to(&path, "down").expect("repeat down");
        send_command_to(&path, "up").expect("up");
        assert!(send_command_to(&path, "nonsense").is_err());

        assert_eq!(rx.recv_timeout(IO_TIMEOUT), Ok(HotkeyEvent::Down));
        assert_eq!(rx.recv_timeout(IO_TIMEOUT), Ok(HotkeyEvent::Up));
        assert!(rx.try_recv().is_err());
        assert!(waker.wait(Duration::from_millis(50)));

        uninstall(handle);
        assert!(!path.exists());
        assert!(send_command_to(&path, "down").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A socket file left behind by a killed process must not block a restart.
    #[test]
    fn rebinds_over_a_stale_socket() {
        let dir = std::env::temp_dir().join(format!("flow-stale-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("control.sock");
        // Bind and drop the listener without unlinking, the way a crash leaves it.
        let dead = UnixListener::bind(&path).unwrap();
        drop(dead);
        assert!(path.exists());

        let (tx, _rx) = std::sync::mpsc::channel();
        let handle = install_at(&path, tx, Waker::new()).expect("rebind over stale socket");
        send_command_to(&path, "toggle").expect("toggle");
        uninstall(handle);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
