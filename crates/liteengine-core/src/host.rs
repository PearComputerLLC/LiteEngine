//! The [`Host`] trait: everything the engine needs from a window system.
//!
//! Two hosts ship with the engine:
//!
//! * `minifb` host (in `liteengine-render`) — a real OS window on macOS,
//!   Windows and Linux, using only AppKit/Win32/X11. Works on old OSes.
//! * `headless` host (in `liteengine-render`) — renders into an in-memory
//!   framebuffer; used for CI, testing, and `--screenshot` captures.

use crate::events::InputEvent;
use crate::snapshot::Frame;

/// A rendering backend. Consumes [`Frame`] snapshots and presents pixels
/// through a [`Host`].
pub trait Renderer {
    /// Render one frame and present it. Implementations must be cheap to
    /// switch (the engine may flip between regular and raytracing mode
    /// mid-game when `rt.enable()` / `rt.disable()` is called).
    fn render_frame(&mut self, frame: &Frame, host: &mut dyn Host) -> Result<(), String>;
    /// Backend name for logs and `doctor`.
    fn name(&self) -> &'static str;
}

/// A window/framebuffer provider. The engine drives it once per frame.
pub trait Host {
    /// Current client size in pixels.
    fn size(&self) -> (usize, usize);
    /// Poll OS events for this frame.
    fn poll(&mut self) -> Vec<InputEvent>;
    /// Present an ARGB framebuffer (0xAARRGGBB, row-major, top-left origin).
    fn present(&mut self, argb: &[u32], width: usize, height: usize) -> Result<(), String>;
    /// True when the host wants the engine to stop (window closed etc.).
    fn should_quit(&self) -> bool;
    /// Human-readable backend name.
    fn name(&self) -> &'static str;
    /// Last presented framebuffer (headless screenshots / testing).
    fn last_frame(&self) -> Option<(&[u32], usize, usize)> {
        None
    }
    /// Update the window title (no-op when unsupported).
    fn set_title(&mut self, _title: &str) {}
}
