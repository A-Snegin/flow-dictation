use std::path::PathBuf;

fn manifest_dir() -> PathBuf {
    PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
}

fn main() {
    // The runtime bundle lives outside the source tree, and outside OneDrive:
    // it is 130 MB of static libraries that no source tree should carry and no
    // sync client should churn on. scripts/fetch-runtime.ps1 puts it there.
    let root = std::env::var("FLOW_MOONSHINE_DIR").unwrap_or_else(|_| {
        let local = std::env::var("LOCALAPPDATA").unwrap_or_default();
        format!("{local}\\Flow\\moonshine")
    });
    let lib_dir = PathBuf::from(&root).join("lib");

    if !lib_dir.join("moonshine.lib").exists() {
        panic!(
            "Moonshine runtime not found at {}. Run scripts/fetch-runtime.ps1, \
             or set FLOW_MOONSHINE_DIR.",
            lib_dir.display()
        );
    }

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    for lib in ["moonshine", "ort-utils", "moonshine-utils", "bin-tokenizer"] {
        println!("cargo:rustc-link-lib=static={lib}");
    }
    println!("cargo:rustc-link-lib=dylib=onnxruntime");
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=assets/flow.ico");

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
    println!("cargo:rerun-if-env-changed=FLOW_MOONSHINE_DIR");

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
