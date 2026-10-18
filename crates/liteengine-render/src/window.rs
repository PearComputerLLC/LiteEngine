//! Window hosts: `minifb` (real OS window, works on very old OSes) and
//! `headless` (in-memory framebuffer for CI, tests and screenshots).

use liteengine_core::events::InputEvent;
use liteengine_core::host::Host;
use std::path::PathBuf;

#[cfg(feature = "window")]
mod minifb_host {
    use super::*;
    use minifb::{Key, MouseMode, Window, WindowOptions};
    use std::collections::HashSet;

    /// Translate a minifb key into a lowercase Scratch-style key name.
    fn key_name(k: Key) -> Option<&'static str> {
        use Key::*;
        Some(match k {
            Space => "space",
            Up => "up",
            Down => "down",
            Left => "left",
            Right => "right",
            Enter => "enter",
            Backspace => "backspace",
            Tab => "tab",
            Escape => "escape",
            A => "a", B => "b", C => "c", D => "d", E => "e", F => "f", G => "g",
            H => "h", I => "i", J => "j", K => "k", L => "l", M => "m", N => "n",
            O => "o", P => "p", Q => "q", R => "r", S => "s", T => "t", U => "u",
            V => "v", W => "w", X => "x", Y => "y", Z => "z",
            Num0 => "0", Num1 => "1", Num2 => "2", Num3 => "3", Num4 => "4",
            Num5 => "5", Num6 => "6", Num7 => "7", Num8 => "8", Num9 => "9",
            _ => return None,
        })
    }

    pub struct MinifbHost {
        window: Window,
        prev_keys: HashSet<&'static str>,
        mouse_prev: bool,
        closed: bool,
    }

    impl MinifbHost {
        pub fn new(width: usize, height: usize, title: &str) -> Result<MinifbHost, String> {
            let window = Window::new(
                title,
                width,
                height,
                WindowOptions {
                    resize: false,
                    scale: minifb::Scale::X1,
                    ..WindowOptions::default()
                },
            )
            .map_err(|e| format!("cannot open window: {e}"))?;
            Ok(MinifbHost {
                window,
                prev_keys: HashSet::new(),
                mouse_prev: false,
                closed: false,
            })
        }
    }

    impl Host for MinifbHost {
        fn size(&self) -> (usize, usize) {
            self.window.get_size()
        }

        fn poll(&mut self) -> Vec<InputEvent> {
            let mut out = Vec::new();
            if !self.window.is_open() {
                if !self.closed {
                    self.closed = true;
                    out.push(InputEvent::CloseRequest);
                }
                return out;
            }

            // Key down/up edges.
            let mut current = HashSet::new();
            for k in self.window.get_keys() {
                if let Some(name) = key_name(k) {
                    current.insert(name);
                }
            }
            for name in &current {
                if !self.prev_keys.contains(name) {
                    out.push(InputEvent::KeyDown(name));
                }
            }
            for name in &self.prev_keys {
                if !current.contains(name) {
                    out.push(InputEvent::KeyUp(name));
                }
            }
            self.prev_keys = current;

            // Mouse.
            if let Some((mx, my)) = self.window.get_mouse_pos(MouseMode::Clamp) {
                let pressed = self.window.get_mouse_down(minifb::MouseButton::Left);
                if pressed && !self.mouse_prev {
                    out.push(InputEvent::MouseDown(mx as i32, my as i32, 0));
                }
                if !pressed && self.mouse_prev {
                    out.push(InputEvent::MouseUp(mx as i32, my as i32, 0));
                }
                out.push(InputEvent::MouseMove(mx as i32, my as i32));
                self.mouse_prev = pressed;
            }
            out
        }

        fn present(&mut self, argb: &[u32], width: usize, height: usize) -> Result<(), String> {
            self.window
                .update_with_buffer(argb, width, height)
                .map_err(|e| e.to_string())
        }

        fn should_quit(&self) -> bool {
            self.closed
        }

        fn name(&self) -> &'static str {
            "minifb"
        }

        fn set_title(&mut self, title: &str) {
            self.window.set_title(title);
        }
    }
}

#[cfg(feature = "window")]
pub use minifb_host::MinifbHost;

/// In-memory host for CI, tests and `--screenshot`.
pub struct HeadlessHost {
    pub width: usize,
    pub height: usize,
    pub frames_limit: Option<u64>,
    pub screenshot_at: Option<u64>,
    pub screenshot_path: Option<PathBuf>,
    frame_count: u64,
    last: Option<(Vec<u32>, usize, usize)>,
    quit: bool,
}

impl HeadlessHost {
    pub fn new(width: usize, height: usize) -> HeadlessHost {
        HeadlessHost {
            width,
            height,
            frames_limit: None,
            screenshot_at: None,
            screenshot_path: None,
            frame_count: 0,
            last: None,
            quit: false,
        }
    }
}

impl Host for HeadlessHost {
    fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    fn poll(&mut self) -> Vec<InputEvent> {
        Vec::new()
    }

    fn present(&mut self, argb: &[u32], width: usize, height: usize) -> Result<(), String> {
        self.last = Some((argb.to_vec(), width, height));
        self.frame_count += 1;
        if let Some(at) = self.screenshot_at {
            if self.frame_count == at {
                self.last = Some((argb.to_vec(), width, height));
            }
        }
        if let Some(limit) = self.frames_limit {
            if self.frame_count >= limit {
                self.quit = true;
            }
        }
        Ok(())
    }

    fn should_quit(&self) -> bool {
        self.quit
    }

    fn name(&self) -> &'static str {
        "headless"
    }

    fn last_frame(&self) -> Option<(&[u32], usize, usize)> {
        self.last.as_ref().map(|(v, w, h)| (v.as_slice(), *w, *h))
    }
}

/// Build the appropriate host. `headless` forces the in-memory host.
pub fn make_host(
    width: usize,
    height: usize,
    title: &str,
    headless: bool,
) -> Result<Box<dyn Host>, String> {
    if headless {
        return Ok(Box::new(HeadlessHost::new(width, height)));
    }
    #[cfg(feature = "window")]
    {
        match MinifbHost::new(width, height, title) {
            Ok(h) => return Ok(Box::new(h)),
            Err(e) => {
                eprintln!(
                    "[liteengine] no window available ({e}); falling back to headless"
                );
                return Ok(Box::new(HeadlessHost::new(width, height)));
            }
        }
    }
    #[cfg(not(feature = "window"))]
    {
        let _ = title;
        Ok(Box::new(HeadlessHost::new(width, height)))
    }
}
