//! Build script.
//!
//! When the `llama_cpp` feature is enabled, drives a CMake build of the
//! vendored llama.cpp checkout (at `$LLAMA_CPP_PATH` or `../llama.cpp`) and
//! emits the link directives needed to statically link against `libllama` +
//! `libggml*`. Otherwise no-op.
//!
//! The actual extern "C" bindings are hand-written in
//! `src/model/engines/llama_cpp/ffi.rs` — no bindgen, no libclang
//! requirement. Hand-written FFI stays small (~30 functions) and
//! pinned to llama.cpp's published `LLAMA_API` surface.

#[cfg(feature = "llama_cpp")]
fn main() {
    use std::env;
    use std::path::PathBuf;

    println!("cargo:rerun-if-env-changed=LLAMA_CPP_PATH");

    let manifest_dir = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());

    let llama_src = match env::var_os("LLAMA_CPP_PATH") {
        Some(v) => PathBuf::from(v),
        None => manifest_dir
            .parent()
            .expect("manifest dir has parent")
            .parent()
            .expect("workspace parent")
            .join("llama.cpp"),
    };

    if !llama_src.join("CMakeLists.txt").is_file() {
        panic!(
            "feature `llama_cpp` is on but llama.cpp source not found at \
             {}. Set LLAMA_CPP_PATH to the llama.cpp checkout, or place \
             llama.cpp as a sibling directory of the cos workspace.",
            llama_src.display()
        );
    }

    println!("cargo:rerun-if-changed={}", llama_src.join("CMakeLists.txt").display());
    println!("cargo:rerun-if-changed={}", llama_src.join("include").display());

    let dst = cmake::Config::new(&llama_src)
        // Build only the libraries — none of llama.cpp's CLI tools / tests.
        .define("LLAMA_BUILD_COMMON", "OFF")
        .define("LLAMA_BUILD_TESTS", "OFF")
        .define("LLAMA_BUILD_EXAMPLES", "OFF")
        .define("LLAMA_BUILD_SERVER", "OFF")
        .define("LLAMA_BUILD_TOOLS", "OFF")
        // Force static libraries so we don't have to ship .so/.dll alongside cos.
        .define("BUILD_SHARED_LIBS", "OFF")
        .define("CMAKE_POSITION_INDEPENDENT_CODE", "ON")
        .profile("Release")
        .build();

    let lib_dir = dst.join("lib");
    let build_dir = dst.join("build");
    println!("cargo:rustc-link-search=native={}", lib_dir.display());
    // CMake on multi-config generators (Visual Studio) drops libs in lib/Release.
    println!(
        "cargo:rustc-link-search=native={}",
        lib_dir.join("Release").display()
    );
    // Some CMake generators put static libs under build/{src,ggml/src}/Release.
    println!("cargo:rustc-link-search=native={}", build_dir.display());

    // Order: llama depends on ggml-* depends on ggml-base.
    println!("cargo:rustc-link-lib=static=llama");
    println!("cargo:rustc-link-lib=static=ggml");
    println!("cargo:rustc-link-lib=static=ggml-cpu");
    println!("cargo:rustc-link-lib=static=ggml-base");

    // C++ stdlib link.
    if cfg!(target_os = "macos") {
        println!("cargo:rustc-link-lib=dylib=c++");
    } else if cfg!(target_os = "linux") {
        println!("cargo:rustc-link-lib=dylib=stdc++");
    }
    // On MSVC, the C++ runtime is auto-linked by the cc/cmake-rs default.

    // Re-export so the `ffi` submodule can use it for diagnostics.
    println!(
        "cargo:rustc-env=COS_LLAMA_CPP_SRC={}",
        llama_src.display()
    );
}

#[cfg(not(feature = "llama_cpp"))]
fn main() {
    // Nothing to do — engine module compiles as a stub.
}
