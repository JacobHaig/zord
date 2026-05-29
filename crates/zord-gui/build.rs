//! Link the clang macOS runtime (`libclang_rt.osx.a`).
//!
//! ggml-metal (pulled in by whisper-rs) uses `@available`, which emits a call
//! to `___isPlatformVersionAtLeast`. A normal host-target `cargo build` links
//! the clang runtime implicitly, but an explicit `--target` release build (what
//! `dx bundle` does) omits it, so the symbol goes undefined at link time.
//!
//! We resolve the clang resource dir at build time (works on both Command Line
//! Tools and full Xcode, any clang version) and add the archive. Static
//! archives link lazily, so this is a no-op when the symbol is already present.

fn main() {
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        if let Ok(out) = Command::new("clang").arg("-print-resource-dir").output() {
            if out.status.success() {
                let resource_dir = String::from_utf8_lossy(&out.stdout).trim().to_string();
                println!("cargo:rustc-link-search=native={resource_dir}/lib/darwin");
                println!("cargo:rustc-link-lib=static=clang_rt.osx");
            }
        }
    }
}
