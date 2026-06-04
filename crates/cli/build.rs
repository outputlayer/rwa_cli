fn main() {
    // Re-export the exact compile target triple so `rwa update` can build the
    // precise release asset name (e.g. "aarch64-apple-darwin") rather than
    // re-deriving it from `uname` at runtime.
    let target = std::env::var("TARGET").unwrap_or_default();
    println!("cargo:rustc-env=RWA_TARGET={target}");
    println!("cargo:rerun-if-changed=build.rs");
}
