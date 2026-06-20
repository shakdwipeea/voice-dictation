//! Platform adapter for the desktop integration layer.
//!
//! On Linux this re-exports the X11 implementation (`x11::linux`). On other
//! platforms (macOS, where the real adapters live in the `sunoto-macos`
//! crate via the `sunoto-desktop` facade) a compile-stub is provided so the
//! workspace builds everywhere; the daemon only links the real macOS adapters
//! through `sunoto-desktop`, never these stubs.

#[cfg(target_os = "linux")]
mod ffi;
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(not(target_os = "linux"))]
mod stub;
#[cfg(not(target_os = "linux"))]
pub use stub::*;
