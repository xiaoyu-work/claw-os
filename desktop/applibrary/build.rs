use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR");
    let version = env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION");
    let out = PathBuf::from(&manifest_dir).join("src").join("config.rs");
    let content = format!(
        "pub const APP_ID: &str = \"com.clawos.AppLibrary\";\n\
         pub const VERSION: &str = \"{version}\";\n",
    );
    fs::write(&out, content).expect("write src/config.rs");
    println!("cargo:rerun-if-changed=build.rs");
}
