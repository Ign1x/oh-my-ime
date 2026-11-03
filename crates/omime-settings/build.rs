//! The tests find the window's elements by what they are, which takes Slint's debug
//! information in the code the `slint!` macro generates: the compiler, which runs in
//! rustc, reads this variable.

fn main() {
    println!("cargo:rustc-env=SLINT_EMIT_DEBUG_INFO=1");
}
