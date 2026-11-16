//! What oh-my-ime's windows share: the settings, and the windows of the engines that
//! come with it.
//!
//! [`renderer`] keeps a window on the screen where OpenGL cannot start. The crate has
//! no dependency on Slint: the programs with windows bring their own.

pub mod renderer;
