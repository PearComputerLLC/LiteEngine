//! Coroutine scheduler — how `engine.wait`, `forever`, events and broadcasts
//! behave exactly like their Scratch counterparts.
//!
//! Every handler invocation becomes a Lua coroutine ("thread" in Lua terms).
//! When a handler calls `engine.wait(seconds)`, the API layer performs
//! `coroutine.yield({"timer", seconds})`; the scheduler stores the thread with
//! a wake time and resumes it later. `on_tick`/`forever` handlers are
//! respawned every frame once their previous invocation finishes, giving the
//! classic Scratch `forever` loop semantics with zero Lua-side boilerplate.

use mlua::{Lua, RegistryKey, Thread, Value};
use std::collections::HashSet;

/// A parked coroutine waiting for its wake time.
struct Task {
    thread: RegistryKey,
    wake_at: f64,
    /// Index of the tick handler this task belongs to (for `forever` respawn).
    owner: Option<usize>,
}

/// Interpret a `coroutine.yield` payload. Supported payloads:
/// `{"timer", seconds}` — wake after `seconds` engine time.
fn parse_yield(v: &Value, now: f64) -> f64 {
    match v {
        Value::Table(t) => {
            let tag: String = t
                .raw_get::<String>(1usize)
                .unwrap_or_else(|_| "timer".to_string());
            let secs: f64 = t.raw_get::<f64>(2usize).unwrap_or(0.0);
            if tag == "timer" {
                now + secs.max(0.0)
            } else {
                now
            }
        }
        _ => now,
    }
}

/// Registry of handler functions keyed by event kind.
#[derive(Default)]
pub struct Handlers {
    /// Functions for `on_start` — run once per engine start.
    pub start: Vec<RegistryKey>,
    /// Functions for `on_key(name)` — spawned on key down.
    pub key: Vec<(String, RegistryKey)>,
    /// Functions for `on_key_held(name)` — spawned every frame while held.
    pub key_held: Vec<(String, RegistryKey)>,
    /// Functions for `on_click(x, y, button)`.
    pub click: Vec<RegistryKey>,
    /// Functions for `on_broadcast(name)`.
    pub broadcast: Vec<(String, RegistryKey)>,
    /// Functions for `on_tick(dt)` / `forever(fn)` — respawned each frame.
    pub tick: Vec<RegistryKey>,
}

impl Handlers {
    pub fn add(&mut self, kind: &str, sub: Option<String>, key: RegistryKey) {
        match (kind, sub) {
            ("start", _) => self.start.push(key),
            ("key", Some(name)) => self.key.push((name, key)),
            ("key_held", Some(name)) => self.key_held.push((name, key)),
            ("click", _) => self.click.push(key),
            ("tick", _) => self.tick.push(key),
            ("broadcast", Some(name)) => self.broadcast.push((name, key)),
            _ => {}
        }
    }

    pub fn total(&self) -> usize {
        self.start.len()
            + self.key.len()
            + self.key_held.len()
            + self.click.len()
            + self.broadcast.len()
            + self.tick.len()
    }
}

/// The coroutine scheduler.
#[derive(Default)]
pub struct Scheduler {
    tasks: Vec<Task>,
    pub handlers: Handlers,
    finished_ticks: Vec<usize>,
}

impl Scheduler {
    pub fn new() -> Scheduler {
        Scheduler::default()
    }

    #[allow(clippy::too_many_arguments)]
    fn spawn_key_free(
        lua: &Lua,
        func: &RegistryKey,
        tasks: &mut Vec<Task>,
        finished_ticks: &mut Vec<usize>,
        args: Vec<Value>,
        owner: Option<usize>,
        now: f64,
    ) -> mlua::Result<()> {
        let f: mlua::Function = lua.registry_value(func)?;
        let thread: Thread = lua.create_thread(f)?;
        let key = lua.create_registry_value(thread)?;
        let parked: Thread = lua.registry_value(&key)?;
        match parked.resume::<Value>(args) {
            Ok(Value::Nil) => {
                let _ = lua.remove_registry_value(key);
                if let Some(o) = owner {
                    finished_ticks.push(o);
                }
                Ok(())
            }
            Ok(yielded) => {
                let wake_at = parse_yield(&yielded, now);
                tasks.push(Task {
                    thread: key,
                    wake_at,
                    owner,
                });
                Ok(())
            }
            Err(e) => {
                let _ = lua.remove_registry_value(key);
                Err(e)
            }
        }
    }

    /// Dispatch `on_start` handlers (the green flag).
    pub fn dispatch_start(&mut self, lua: &Lua, now: f64) {
        let n = self.handlers.start.len();
        for i in 0..n {
            let key = &self.handlers.start[i];
            if let Err(e) = Self::spawn_key_free(
                lua,
                key,
                &mut self.tasks,
                &mut self.finished_ticks,
                Vec::new(),
                None,
                now,
            ) {
                crate::luaapi::report_lua_error(&e);
            }
        }
    }

    /// Dispatch `on_key(name)` handlers for a key-down edge.
    pub fn dispatch_key(&mut self, lua: &Lua, name: &str, now: f64) {
        let n = self.handlers.key.len();
        for i in 0..n {
            let matches = self.handlers.key[i].0 == name;
            if !matches {
                continue;
            }
            let key = &self.handlers.key[i].1;
            if let Err(e) = Self::spawn_key_free(
                lua,
                key,
                &mut self.tasks,
                &mut self.finished_ticks,
                Vec::new(),
                None,
                now,
            ) {
                crate::luaapi::report_lua_error(&e);
            }
        }
    }

    /// Dispatch `on_key_held(name)` handlers for every currently-held key.
    pub fn dispatch_key_held(&mut self, lua: &Lua, held: &HashSet<String>, now: f64) {
        let n = self.handlers.key_held.len();
        for i in 0..n {
            let matches = held.contains(&self.handlers.key_held[i].0);
            if !matches {
                continue;
            }
            let key = &self.handlers.key_held[i].1;
            if let Err(e) = Self::spawn_key_free(
                lua,
                key,
                &mut self.tasks,
                &mut self.finished_ticks,
                Vec::new(),
                None,
                now,
            ) {
                crate::luaapi::report_lua_error(&e);
            }
        }
    }

    /// Dispatch `on_click(x, y, button)` handlers.
    pub fn dispatch_click(&mut self, lua: &Lua, x: i32, y: i32, button: u8, now: f64) {
        let n = self.handlers.click.len();
        for i in 0..n {
            let key = &self.handlers.click[i];
            if let Err(e) = Self::spawn_key_free(
                lua,
                key,
                &mut self.tasks,
                &mut self.finished_ticks,
                vec![
                    Value::Integer(x as i64),
                    Value::Integer(y as i64),
                    Value::Integer(button as i64),
                ],
                None,
                now,
            ) {
                crate::luaapi::report_lua_error(&e);
            }
        }
    }

    /// Dispatch `on_broadcast(name)` handlers.
    pub fn dispatch_broadcast(&mut self, lua: &Lua, name: &str, now: f64) {
        let n = self.handlers.broadcast.len();
        for i in 0..n {
            let matches = self.handlers.broadcast[i].0 == name;
            if !matches {
                continue;
            }
            let key = &self.handlers.broadcast[i].1;
            if let Err(e) = Self::spawn_key_free(
                lua,
                key,
                &mut self.tasks,
                &mut self.finished_ticks,
                Vec::new(),
                None,
                now,
            ) {
                crate::luaapi::report_lua_error(&e);
            }
        }
    }

    /// Spawn/respawn `on_tick` (forever) handlers whose previous invocation
    /// has finished. Called once per frame.
    pub fn dispatch_tick(&mut self, lua: &Lua, dt: f64, now: f64) {
        let active = self.active_ticks();
        let n = self.handlers.tick.len();
        for i in 0..n {
            if active.contains(&i) {
                continue;
            }
            let key = &self.handlers.tick[i];
            if let Err(e) = Self::spawn_key_free(
                lua,
                key,
                &mut self.tasks,
                &mut self.finished_ticks,
                vec![Value::Number(dt)],
                Some(i),
                now,
            ) {
                crate::luaapi::report_lua_error(&e);
            }
        }
    }

    /// Resume every due task. Called once per frame with the current engine time.
    pub fn update(&mut self, lua: &Lua, now: f64) {
        for i in (0..self.tasks.len()).rev() {
            if self.tasks[i].wake_at > now {
                continue;
            }
            let task = self.tasks.remove(i);
            let key = task.thread;
            let thread: Thread = match lua.registry_value(&key) {
                Ok(t) => t,
                Err(_) => {
                    self.tasks.remove(i);
                    continue;
                }
            };
            match thread.resume::<Value>(Vec::<Value>::new()) {
                Ok(Value::Nil) => {
                    if let Some(o) = task.owner {
                        self.finished_ticks.push(o);
                    }
                    let _ = lua.remove_registry_value(key);
                }
                Ok(yielded) => {
                    let wake = parse_yield(&yielded, now);
                    self.tasks.push(Task {
                        thread: key,
                        wake_at: wake,
                        owner: task.owner,
                    });
                }
                Err(e) => {
                    crate::luaapi::report_lua_error(&e);
                    if let Some(o) = task.owner {
                        self.finished_ticks.push(o);
                    }
                    let _ = lua.remove_registry_value(key);
                }
            }
        }
    }

    /// Tick handler indices that currently have a live invocation.
    pub fn active_ticks(&self) -> HashSet<usize> {
        self.tasks.iter().filter_map(|t| t.owner).collect()
    }

    /// Number of parked coroutines (exposed via `engine.debug_threads()`).
    pub fn pending(&self) -> usize {
        self.tasks.len()
    }

    /// Total registered handlers.
    pub fn handler_count(&self) -> usize {
        self.handlers.total()
    }

    /// Clear all parked tasks (used when the engine shuts down).
    pub fn clear(&mut self, lua: &Lua) {
        for t in self.tasks.drain(..) {
            let _ = lua.remove_registry_value(t.thread);
        }
    }
}
