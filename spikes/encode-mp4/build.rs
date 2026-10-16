//! Link configuration for the prebuilt minimal FFmpeg at `D:\CapLite\ffmpeg-min`.
//!
//! The `bin\` directory holds native x64 COFF import libraries (`avcodec.lib`,
//! `avformat.lib`, `avutil.lib`, `swscale.lib`) that MSVC `link.exe` consumes
//! directly.  (The `lib\` directory only has `.def` files and MinGW
//! `lib*.dll.a` import libraries, which are *not* needed.)
//!
//! Override the location with the `FFMPEG_MIN_DIR` environment variable.

use std::env;
use std::path::PathBuf;

fn main() {
    let root = env::var("FFMPEG_MIN_DIR").unwrap_or_else(|_| r"D:\CapLite\ffmpeg-min".to_string());
    let bin = PathBuf::from(&root).join("bin");

    if !bin.join("avcodec.lib").exists() {
        println!(
            "cargo:warning=FFMPEG_MIN_DIR='{}' does not contain bin\\avcodec.lib",
            root
        );
    }

    println!("cargo:rustc-link-search=native={}", bin.display());
    // Order matters for MSVC: dependents first.
    for lib in ["avformat", "avcodec", "swscale", "avutil"] {
        println!("cargo:rustc-link-lib=dylib={lib}");
    }

    println!("cargo:rerun-if-env-changed=FFMPEG_MIN_DIR");
    println!("cargo:rerun-if-changed=build.rs");
}
