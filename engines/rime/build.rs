//! The tests of the settings window need Slint's element debug information. Generate
//! it only in the debug profile and gate those tests with the same condition, as
//! oh-my-ime's settings do.

fn main() {
    println!("cargo:rustc-check-cfg=cfg(slint_debug_info)");
    if std::env::var("PROFILE").as_deref() == Ok("debug") {
        println!("cargo:rustc-env=SLINT_EMIT_DEBUG_INFO=1");
        println!("cargo:rustc-cfg=slint_debug_info");
    }
}
