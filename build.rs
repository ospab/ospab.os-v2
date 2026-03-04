// build.rs — AETERNA kernel build script
// Sets cfg flags based on available build artifacts:
//   doom_wad_present  — doom1.wad exists in project root → embed it
//   doom_supported    — target/doom/libdoom.a built → link DOOM engine

use std::path::Path;
use std::env;

fn main() {
    let manifest = env::var("CARGO_MANIFEST_DIR").unwrap_or_default();

    // Check for doom1.wad (shareware / registered WAD)
    let wad = Path::new(&manifest).join("doom1.wad");
    if wad.exists() {
        println!("cargo:rustc-cfg=doom_wad_present");
        println!("cargo:rerun-if-changed=doom1.wad");
    }

    // Check for compiled DOOM C library
    let lib = Path::new(&manifest).join("target/doom/libdoom.a");
    if lib.exists() {
        println!("cargo:rustc-cfg=doom_supported");
        // Note: RUSTFLAGS in build.sh already passes -l static=doom -L target/doom
        // We set the cfg so Rust code can conditionally compile DOOM FFI.
    }

    // Declare custom cfg names so rustc doesn't warn about unexpected_cfgs
    println!("cargo::rustc-check-cfg=cfg(doom_wad_present)");
    println!("cargo::rustc-check-cfg=cfg(doom_supported)");

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=target/doom/libdoom.a");

    // Pass linker script with absolute path so rust-lld can find it regardless
    // of what directory cargo invokes the linker from.
    let linker_script = Path::new(&manifest).join("linker.ld");
    if linker_script.exists() {
        println!("cargo:rustc-link-arg=-T{}", linker_script.display());
    }
}
