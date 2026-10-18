//! Event model — the Scratch-like backbone of LiteEngine.
//!
//! The host window produces low-level [`InputEvent`]s; the runtime translates
//! them (plus timers, broadcasts and the per-frame tick) into script-visible
//! behaviour:
//!
//! | Scratch block                      | LiteEngine Lua                       |
//! |------------------------------------|--------------------------------------|
//! | `when green flag clicked`          | `engine.on_start(function() ... end)`|
//! | `when [space] key pressed`         | `engine.on_key("space", fn)`         |
//! | `when this sprite clicked`         | `engine.on_click(fn)`                |
//! | `broadcast [jump]`                 | `engine.broadcast("jump")`           |
//! | `when I receive [jump]`            | `engine.on_broadcast("jump", fn)`    |
//! | `forever` / `when I start as clone`| `engine.on_tick(fn)` / `forever(fn)` |
//! | `wait (1) seconds`                 | `engine.wait(1)`                     |

/// Low-level input coming from the window host.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum InputEvent {
    KeyDown(&'static str),
    KeyUp(&'static str),
    /// `button`: 0 = left, 1 = middle, 2 = right.
    MouseDown(i32, i32, u8),
    MouseUp(i32, i32, u8),
    MouseMove(i32, i32),
    /// Window close requested (or Ctrl-C in a headless terminal).
    CloseRequest,
    /// Window client size changed.
    Resized(usize, usize),
}

/// Script-visible engine events. Handlers registered for a kind are spawned
/// as Lua coroutines, so they may call `engine.wait` freely.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EngineEvent {
    Start,
    Key(String),
    Broadcast(String),
    Tick,
}
