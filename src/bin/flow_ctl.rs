//! The keybind client. Writes one line to the running flow-core's control
//! socket and exits.
//!
//! `flow-core --key down` does the same job, but flow-core links the
//! Moonshine and ONNX Runtime shared libraries, and mapping them costs about
//! 4 ms on every key press. This binary depends on nothing but std, so the
//! whole round trip from the compositor is about a millisecond and a half.
//! Linux only: on Windows the hook lives inside flow-core.

#[cfg(target_os = "linux")]
fn main() {
    use std::io::{BufRead, BufReader, Write};
    use std::os::unix::net::UnixStream;
    use std::path::PathBuf;
    use std::time::Duration;

    let cmd = std::env::args().nth(1).unwrap_or_default();
    if !matches!(
        cmd.as_str(),
        "down" | "up" | "cancel" | "toggle" | "reload" | "report" | "quit"
    ) {
        eprintln!("usage: flow-ctl down|up|cancel|toggle|reload|report|quit");
        std::process::exit(2);
    }

    // Same location flow-core listens on: $XDG_RUNTIME_DIR/flow/control.sock,
    // falling back to /tmp/flow-<uid> when the runtime dir is unset.
    let dir = match std::env::var("XDG_RUNTIME_DIR") {
        Ok(v) if !v.is_empty() => PathBuf::from(v).join("flow"),
        _ => PathBuf::from(format!("/tmp/flow-{}", unsafe { libc_getuid() })),
    };
    let path = dir.join("control.sock");

    let result = (|| -> Result<(), String> {
        let mut stream = UnixStream::connect(&path)
            .map_err(|e| format!("connect {}: {e}", path.display()))?;
        let _ = stream.set_read_timeout(Some(Duration::from_millis(500)));
        let _ = stream.set_write_timeout(Some(Duration::from_millis(500)));
        stream
            .write_all(format!("{cmd}\n").as_bytes())
            .map_err(|e| format!("write: {e}"))?;
        let mut reply = String::new();
        BufReader::new(&stream)
            .read_line(&mut reply)
            .map_err(|e| format!("read reply: {e}"))?;
        match reply.trim() {
            "ok" => Ok(()),
            other => Err(format!("flow-core replied \"{other}\"")),
        }
    })();

    if let Err(e) = result {
        eprintln!("flow-core is not running ({e})");
        std::process::exit(1);
    }
}

#[cfg(target_os = "linux")]
unsafe fn libc_getuid() -> u32 {
    extern "C" {
        fn getuid() -> u32;
    }
    getuid()
}

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("flow-ctl is the Linux keybind client; on Windows the hook lives inside flow-core.");
    std::process::exit(2);
}
