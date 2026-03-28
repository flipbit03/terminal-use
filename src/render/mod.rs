//! Terminal screen rendering in text and image formats.
//!
//! The render pipeline starts with [`screen::ScreenSnapshot`], an owned extraction
//! of the vt100 emulator's cell grid that captures content, colors, and attributes.
//! From there, two output paths are available:
//!
//! - [`text`] — plain-text or JSON dump of screen contents (used by `tu screenshot`).
//! - [`image`] — rasterized PNG screenshot using an embedded JetBrains Mono font
//!   (used by `tu screenshot --png`).
//!
//! [`colors`] provides the xterm-256color palette lookup shared by the image renderer.

pub mod colors;
pub mod image;
pub mod screen;
pub mod text;
