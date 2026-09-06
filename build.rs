use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
}

/// Where the prebuilt Moonshine runtime lives. Outside the source tree, and on
/// Windows outside OneDrive: it is 130 MB of static libraries that no source
/// tree should carry and no sync client should churn on.
/// scripts/fetch-runtime.ps1 (Windows) or scripts/fetch-runtime.sh (Linux)
/// puts it there.
fn runtime_root(target_os: &str) -> String {
    if let Ok(dir) = std::env::var("FLOW_MOONSHINE_DIR") {
        return dir;
    }
    if target_os == "windows" {
        let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
        format!("{local}\\Flow\\moonshine")
    } else {
        let data = std::env::var("XDG_DATA_HOME")
            .ok()
            .filter(|v| !v.is_empty())
            .unwrap_or_else(|| {
                let home = std::env::var("HOME").unwrap_or_default();
                format!("{home}/.local/share")
            });
        format!("{data}/flow/moonshine")
    }
}

fn main() {
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let root = runtime_root(&target_os);
    let lib_dir = PathBuf::from(&root).join("lib");
    // Windows ships static libraries plus onnxruntime.dll; Linux ships one
    // shared libmoonshine.so that already carries its onnxruntime dependency.
    let probe = if target_os == "windows" {
        "moonshine.lib"
    } else {
        "libmoonshine.so"
    };

    if !lib_dir.join(probe).exists() {
        panic!(
            "Moonshine runtime not found at {}. Run scripts/fetch-runtime.{}, \
             or set FLOW_MOONSHINE_DIR.",
            lib_dir.display(),
            if target_os == "windows" { "ps1" } else { "sh" }
        );
    }

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    if target_os == "windows" {
        for lib in ["moonshine", "ort-utils", "moonshine-utils", "bin-tokenizer"] {
            println!("cargo:rustc-link-lib=static={lib}");
        }
        println!("cargo:rustc-link-lib=dylib=onnxruntime");
    } else {
        println!("cargo:rustc-link-lib=dylib=moonshine");
        // The .so files stay in the runtime directory; bake its path in so
        // nothing has to be copied next to the executable.
        println!("cargo:rustc-link-arg-bins=-Wl,-rpath,{}", lib_dir.display());
    }
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/flow.ico");
    println!("cargo:rerun-if-env-changed=FLOW_MOONSHINE_DIR");

    if target_os == "windows" {
        windows_extras(&lib_dir);
    }
}

#[cfg(windows)]
fn windows_extras(lib_dir: &std::path::Path) {
    // Compile the application icon into the executable. It becomes the icon
    // Explorer and the taskbar show, and the tray loads the same resource so
    // the notification area matches.
    let icon = manifest_dir().join("assets/flow.ico");
    if icon.exists() {
        let mut res = winresource::WindowsResource::new();
        res.set_icon_with_id(&icon.to_string_lossy(), "1");
        res.set("FileDescription", "Flow: local dictation");
        res.set("ProductName", "Flow");
        if let Err(e) = res.compile() {
            // Not fatal: without the resource the tray falls back to the
            // system icon and the app is otherwise identical.
            println!("cargo:warning=icon resource not compiled: {e}");
        }
    }

    // onnxruntime.dll must sit next to the executable.
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let exe_dir = out.ancestors().nth(3).expect("OUT_DIR shape").to_path_buf();
    let src = lib_dir.join("onnxruntime.dll");
    for dest in [
        exe_dir.join("onnxruntime.dll"),
        exe_dir.join("deps/onnxruntime.dll"),
    ] {
        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::copy(&src, &dest);
    }
}

#[cfg(not(windows))]
fn windows_extras(_lib_dir: &std::path::Path) {
    let _ = manifest_dir;
}
