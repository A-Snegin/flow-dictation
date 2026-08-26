use std::path::PathBuf;

fn main() {
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let lib_dir = manifest.join("vendor/moonshine/lib");

    if !lib_dir.join("moonshine.lib").exists() {
        panic!(
            "Moonshine runtime not found at {}. Run scripts/fetch-runtime.ps1 first.",
            lib_dir.display()
        );
    }

    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    for lib in ["moonshine", "ort-utils", "moonshine-utils", "bin-tokenizer"] {
        println!("cargo:rustc-link-lib=static={lib}");
    }
    println!("cargo:rustc-link-lib=dylib=onnxruntime");
    println!("cargo:rerun-if-changed=build.rs");

    // onnxruntime.dll must sit next to the executable.
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let exe_dir = out
        .ancestors()
        .nth(3)
        .expect("OUT_DIR shape")
        .to_path_buf();
    let src = lib_dir.join("onnxruntime.dll");
    for dest in [exe_dir.join("onnxruntime.dll"), exe_dir.join("deps/onnxruntime.dll")] {
        if let Some(parent) = dest.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = std::fs::copy(&src, &dest);
    }
}
