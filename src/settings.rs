//! Settings, read from TOML at startup and reloadable from the tray.
//!
//! Deliberately a file rather than a settings window: the resident process
//! stays native and small, and nothing here is worth a WebView.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub hotkey: Hotkey,
    pub model: Model,
    pub insertion: Insertion,
    pub formatting: Formatting,
    /// Spoken form on the left, written form on the right. Feeds both the
    /// decoder's keyterm biasing and the post-processing replacement.
    pub dictionary: HashMap<String, String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Hotkey {
    /// rightctrl, rightalt, rightshift, f13, capslock, leftctrl
    pub key: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Model {
    /// "fast" is tiny-streaming-en, "balanced" is small-streaming-en.
    pub profile: String,
    /// Overrides the model directory entirely when set.
    pub dir: String,
    /// Gap between live partial hypotheses. Lower means fresher on-screen text
    /// and a busier ASR thread; the thread being busy at key-up is the single
    /// largest contributor to tail latency, so this is not a free dial.
    pub partial_cadence_ms: u64,
    /// Bypass the library's own 200 ms throttle. Costs CPU, buys freshness.
    pub force_partials: bool,
    /// Extra collection window after key-up so the last device packet is not
    /// clipped. Paid once per dictation and worth it below about 20 ms.
    pub release_tail_ms: u64,
    /// Run inference on one thread instead of letting ONNX Runtime spread it
    /// across every core. Measured on this machine: real-time factor is
    /// unchanged (0.28 against 0.31 for tiny, 0.62 against 0.63 for small)
    /// because the graphs are too small for threading to pay, while the tail
    /// improves markedly because there is no thread-pool synchronisation to be
    /// descheduled in the middle of. It also means one busy core rather than
    /// eight, which is the difference between Flow being unnoticeable and Flow
    /// being felt by whatever else is running.
    pub single_thread: bool,
    /// Bias strength for dictionary terms. Upstream measured 2.0 as the point
    /// where terms come out most accurately; higher invents them.
    pub keyterm_boost: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Insertion {
    /// "paste" or "type", for ordinary applications.
    pub mode: String,
    /// Terminals get their own mode. Typing is the safe default there: paste
    /// keybindings vary between consoles and remote-desktop clients, while
    /// synthesised Unicode keystrokes work anywhere a console reads input.
    pub terminal_mode: String,
    /// Extra executables to treat as terminals, on top of the built-in list.
    pub terminal_apps: Vec<String>,
    /// At a shell prompt a line break is the Enter key, so dictating "new
    /// line" would run the command. Inside a terminal, breaks become this.
    pub terminal_newline_replacement: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Formatting {
    pub capitalise_sentences: bool,
    pub spoken_punctuation: bool,
    pub trailing_space: bool,
}

impl Default for Hotkey {
    fn default() -> Self {
        Hotkey {
            key: "rightctrl".into(),
        }
    }
}

impl Default for Model {
    fn default() -> Self {
        Model {
            profile: "balanced".into(),
            dir: String::new(),
            partial_cadence_ms: 250,
            force_partials: false,
            release_tail_ms: 15,
            single_thread: true,
            keyterm_boost: 2.0,
        }
    }
}

impl Default for Insertion {
    fn default() -> Self {
        Insertion {
            mode: "paste".into(),
            terminal_mode: "type".into(),
            terminal_apps: Vec::new(),
            terminal_newline_replacement: " ".into(),
        }
    }
}

impl Default for Formatting {
    fn default() -> Self {
        Formatting {
            capitalise_sentences: true,
            spoken_punctuation: true,
            trailing_space: true,
        }
    }
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            hotkey: Hotkey::default(),
            model: Model::default(),
            insertion: Insertion::default(),
            formatting: Formatting::default(),
            dictionary: HashMap::new(),
        }
    }
}

impl Settings {
    pub fn path() -> PathBuf {
        data_root().join("settings.toml")
    }

    /// Reads the settings file, writing a commented default if none exists.
    /// A malformed file is reported and ignored rather than stopping startup:
    /// the app must always come up ready to dictate.
    pub fn load() -> (Settings, Option<String>) {
        let path = Self::path();
        if !path.exists() {
            let s = Settings::default();
            if let Err(e) = s.write_default(&path) {
                return (s, Some(format!("could not write {}: {e}", path.display())));
            }
            return (s, None);
        }
        match std::fs::read_to_string(&path) {
            Ok(text) => match toml::from_str::<Settings>(&text) {
                Ok(s) => (s, None),
                Err(e) => (
                    Settings::default(),
                    Some(format!("{} is not valid: {e}", path.display())),
                ),
            },
            Err(e) => (
                Settings::default(),
                Some(format!("could not read {}: {e}", path.display())),
            ),
        }
    }

    fn write_default(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let body = toml::to_string_pretty(self).unwrap_or_default();
        let commented = format!(
            "# Flow settings.\n\
             # Dictionary and formatting changes apply from the tray: Reload dictionary.\n\
             # Hotkey and model changes need Flow restarted.\n\
             #\n\
             # hotkey.key            rightctrl | rightalt | rightshift | f13 | capslock | leftctrl\n\
             # model.profile         fast (tiny, about twice as quick) | balanced (small)\n\
             # model.single_thread   true is both faster at the tail and far lighter on CPU\n\
             # insertion.mode        paste (default, flat cost) | type (for apps that block paste)\n\
             # insertion.terminal_mode  how to insert into PowerShell, cmd, Windows Terminal and\n\
             #                       friends. Typing is the default there, and line breaks are\n\
             #                       replaced with a space so a dictation can never run a command.\n\
             #\n\
             # [dictionary] entries bias the recogniser and correct the output.\n\
             # Spoken form on the left, exactly what you want written on the right:\n\
             #   \"lift off\" = \"Lift-Off\"\n\
             #   \"dddm\" = \"DDDM\"\n\n{body}"
        );
        std::fs::write(path, commented)
    }

    /// Resolves the model directory and its architecture constant.
    pub fn resolve_model(&self) -> (PathBuf, u32) {
        if !self.model.dir.is_empty() {
            let dir = PathBuf::from(&self.model.dir);
            let arch = if dir.to_string_lossy().contains("tiny") {
                crate::ffi::ARCH_TINY_STREAMING
            } else if dir.to_string_lossy().contains("medium") {
                crate::ffi::ARCH_MEDIUM_STREAMING
            } else {
                crate::ffi::ARCH_SMALL_STREAMING
            };
            return (dir, arch);
        }
        match self.model.profile.to_ascii_lowercase().as_str() {
            "fast" | "tiny" => (
                models_root().join("tiny-streaming-en"),
                crate::ffi::ARCH_TINY_STREAMING,
            ),
            "accurate" | "medium" => (
                models_root().join("medium-streaming-en"),
                crate::ffi::ARCH_MEDIUM_STREAMING,
            ),
            _ => (
                models_root().join("small-streaming-en"),
                crate::ffi::ARCH_SMALL_STREAMING,
            ),
        }
    }
}

/// %APPDATA%\Flow: settings and anything the user edits.
pub fn data_root() -> PathBuf {
    let base = std::env::var("APPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("Flow")
}

/// %LOCALAPPDATA%\Flow: models, traces, anything large or machine-local.
/// Kept off OneDrive on purpose; sync churn costs measurable CPU.
pub fn local_root() -> PathBuf {
    let base = std::env::var("LOCALAPPDATA").unwrap_or_else(|_| ".".into());
    PathBuf::from(base).join("Flow")
}

pub fn models_root() -> PathBuf {
    local_root().join("models")
}
