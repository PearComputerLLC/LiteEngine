//! The Lua API — the "Scratch blocks" of LiteEngine.
//!
//! Globals exposed to every game:
//!
//! * `engine` — events, timing, stage, modules, device info, logging.
//! * `Sprite` — sprite class (2D and 3D billboards), Scratch-style methods.
//! * `rt` — raytracing scene control (`rtmode` module).
//! * `stream` — streaming asset control (`stream` module).
//! * `pbr3d` — PBR 3D scene control (`pbr3d` module, modern path only).
//!
//! Implementation notes: every closure fetches the engine state through Lua
//! *app data* (`Rc<RefCell<EngineState>>`) via the [`with_state`] helpers,
//! which keeps mlua lifetimes simple and avoids holding borrows while Lua
//! code runs. Handler functions are parked as registry keys in
//! `EngineState::pending_handlers` and drained onto the scheduler by the
//! runtime loop — never while a Lua coroutine is executing.

use crate::scene::*;
use crate::runtime::EngineState;
use crate::{EngineResult, ENGINE_VERSION};
use mlua::{Function, MetaMethod, RegistryKey, UserData, UserDataMethods, Value, Variadic};
use std::cell::RefCell;
use std::rc::Rc;

/// Shared runtime state handle stored as Lua app data.
pub type StateRc = Rc<RefCell<EngineState>>;

/// Run `f` with immutable access to the engine state.
pub fn with_state<R>(
    lua: &mlua::Lua,
    f: impl FnOnce(&EngineState) -> Result<R, mlua::Error>,
) -> Result<R, mlua::Error> {
    let rc = lua
        .app_data_ref::<StateRc>()
        .ok_or_else(|| mlua::Error::runtime("engine state unavailable"))?
        .clone();
    let st = rc.borrow();
    f(&st)
}

/// Run `f` with mutable access to the engine state.
pub fn with_state_mut<R>(
    lua: &mlua::Lua,
    f: impl FnOnce(&mut EngineState) -> Result<R, mlua::Error>,
) -> Result<R, mlua::Error> {
    let rc = lua
        .app_data_ref::<StateRc>()
        .ok_or_else(|| mlua::Error::runtime("engine state unavailable"))?
        .clone();
    let mut st = rc.borrow_mut();
    f(&mut st)
}

/// Print a nicely formatted script error (unless LITEENGINE_QUIET).
pub fn report_lua_error(e: &mlua::Error) {
    if crate::runtime::QUIET.load(std::sync::atomic::Ordering::Relaxed) {
        return;
    }
    eprintln!("[liteengine] script error: {e}");
}

/// Sprite handle exposed to Lua as a userdata with methods.
#[derive(Clone, Copy)]
pub struct SpriteHandle(pub u32);

impl UserData for SpriteHandle {
    fn add_methods<M: UserDataMethods<Self>>(m: &mut M) {
        // ---- Motion -------------------------------------------------------
        m.add_method("go_to", |lua, this, (x, y): (f64, f64)| {
            // NOTE: `goto` is a reserved word in Lua 5.4, so the Scratch
            // "go to x/y" block is spelled `go_to` (alias: `goto_xy`).
            with_state_mut(lua, |st| {
                if let Some(s) = st.scene.sprite_by_id_mut(this.0) {
                    s.x = x as f32;
                    s.y = y as f32;
                }
                Ok(())
            })
        });
        m.add_method("goto_xy", |lua, this, (x, y): (f64, f64)| {
            with_state_mut(lua, |st| {
                if let Some(s) = st.scene.sprite_by_id_mut(this.0) {
                    s.x = x as f32;
                    s.y = y as f32;
                }
                Ok(())
            })
        });
        m.add_method("set_x", |lua, this, x: f64| {
            with_state_mut(lua, |st| {
                if let Some(s) = st.scene.sprite_by_id_mut(this.0) {
                    s.x = x as f32;
                }
                Ok(())
            })
        });
        m.add_method("set_y", |lua, this, y: f64| {
            with_state_mut(lua, |st| {
                if let Some(s) = st.scene.sprite_by_id_mut(this.0) {
                    s.y = y as f32;
                }
                Ok(())
            })
        });
        m.add_method("change_x", |lua, this, dx: f64| {
            with_state_mut(lua, |st| {
                if let Some(s) = st.scene.sprite_by_id_mut(this.0) {
                    s.x += dx as f32;
                }
                Ok(())
            })
        });
        m.add_method("change_y", |lua, this, dy: f64| {
            with_state_mut(lua, |st| {
                if let Some(s) = st.scene.sprite_by_id_mut(this.0) {
                    s.y += dy as f32;
                }
                Ok(())
            })
        });
        m.add_method("move_steps", |lua, this, n: f64| {
            // Scratch: 0 degrees = up, 90 = right (clockwise).
            with_state_mut(lua, |st| {
                if let Some(s) = st.scene.sprite_by_id_mut(this.0) {
                    let rad = (90.0 - s.direction).to_radians();
                    s.x += (n as f32) * rad.cos();
                    s.y += (n as f32) * rad.sin();
                }
                Ok(())
            })
        });
        m.add_method("turn", |lua, this, deg: f64| {
            with_state_mut(lua, |st| {
                if let Some(s) = st.scene.sprite_by_id_mut(this.0) {
                    s.direction = (s.direction + deg as f32).rem_euclid(360.0);
                }
                Ok(())
            })
        });
        m.add_method("point_in_direction", |lua, this, deg: f64| {
            with_state_mut(lua, |st| {
                if let Some(s) = st.scene.sprite_by_id_mut(this.0) {
                    s.direction = deg as f32;
                }
                Ok(())
            })
        });
        m.add_method("direction", |lua, this, ()| {
            with_state(lua, |st| {
                Ok(st
                    .scene
                    .sprite_by_id(this.0)
                    .map(|s| s.direction as f64)
                    .unwrap_or(0.0))
            })
        });
        m.add_method("glide_to", |lua, this, (x, y, secs): (f64, f64, f64)| {
            with_state_mut(lua, |st| {
                let now = st.time;
                if let Some(s) = st.scene.sprite_by_id(this.0) {
                    let g = crate::runtime::Glide {
                        sprite_id: s.id,
                        from: (s.x, s.y),
                        to: (x as f32, y as f32),
                        start: now,
                        dur: secs.max(0.01),
                    };
                    st.glides.retain(|g| g.sprite_id != this.0);
                    st.glides.push(g);
                }
                Ok(())
            })
        });
        m.add_method("glide", |lua, this, (dx, dy, secs): (f64, f64, f64)| {
            with_state_mut(lua, |st| {
                let now = st.time;
                if let Some(s) = st.scene.sprite_by_id(this.0) {
                    let g = crate::runtime::Glide {
                        sprite_id: s.id,
                        from: (s.x, s.y),
                        to: (s.x + dx as f32, s.y + dy as f32),
                        start: now,
                        dur: secs.max(0.01),
                    };
                    st.glides.retain(|g| g.sprite_id != this.0);
                    st.glides.push(g);
                }
                Ok(())
            })
        });

        // ---- Looks --------------------------------------------------------
        m.add_method("say", |lua, this, (msg, secs): (String, Option<f64>)| {
            with_state_mut(lua, |st| {
                let now = st.time;
                if let Some(s) = st.scene.sprite_by_id_mut(this.0) {
                    s.say = Some(SayState {
                        message: msg,
                        think: false,
                        until: now + secs.unwrap_or(f64::INFINITY),
                    });
                }
                Ok(())
            })
        });
        m.add_method("think", |lua, this, (msg, secs): (String, Option<f64>)| {
            with_state_mut(lua, |st| {
                let now = st.time;
                if let Some(s) = st.scene.sprite_by_id_mut(this.0) {
                    s.say = Some(SayState {
                        message: msg,
                        think: true,
                        until: now + secs.unwrap_or(f64::INFINITY),
                    });
                }
                Ok(())
            })
        });
        m.add_method("stop_saying", |lua, this, ()| {
            with_state_mut(lua, |st| {
                if let Some(s) = st.scene.sprite_by_id_mut(this.0) {
                    s.say = None;
                }
                Ok(())
            })
        });
        m.add_method("set_size", |lua, this, pct: f64| {
            with_state_mut(lua, |st| {
                if let Some(s) = st.scene.sprite_by_id_mut(this.0) {
                    s.size = pct as f32;
                }
                Ok(())
            })
        });
        m.add_method("change_size", |lua, this, dpct: f64| {
            with_state_mut(lua, |st| {
                if let Some(s) = st.scene.sprite_by_id_mut(this.0) {
                    s.size += dpct as f32;
                }
                Ok(())
            })
        });
        m.add_method("size", |lua, this, ()| {
            with_state(lua, |st| {
                Ok(st
                    .scene
                    .sprite_by_id(this.0)
                    .map(|s| s.size as f64)
                    .unwrap_or(100.0))
            })
        });
        m.add_method("show", |lua, this, ()| {
            with_state_mut(lua, |st| {
                if let Some(s) = st.scene.sprite_by_id_mut(this.0) {
                    s.visible = true;
                }
                Ok(())
            })
        });
        m.add_method("hide", |lua, this, ()| {
            with_state_mut(lua, |st| {
                if let Some(s) = st.scene.sprite_by_id_mut(this.0) {
                    s.visible = false;
                }
                Ok(())
            })
        });
        m.add_method("set_costume", |lua, this, path: String| {
            with_state_mut(lua, |st| {
                st.assets.request(&path, 5);
                if let Some(s) = st.scene.sprite_by_id_mut(this.0) {
                    s.costume = Some(path);
                }
                Ok(())
            })
        });
        m.add_method("set_alpha", |lua, this, a: f64| {
            with_state_mut(lua, |st| {
                if let Some(s) = st.scene.sprite_by_id_mut(this.0) {
                    s.alpha = (a as f32 / 100.0).clamp(0.0, 1.0);
                }
                Ok(())
            })
        });
        m.add_method("tint", |lua, this, (r, g, b): (f64, f64, f64)| {
            with_state_mut(lua, |st| {
                if let Some(s) = st.scene.sprite_by_id_mut(this.0) {
                    s.tint = [
                        (r as f32 / 255.0).clamp(0.0, 1.0),
                        (g as f32 / 255.0).clamp(0.0, 1.0),
                        (b as f32 / 255.0).clamp(0.0, 1.0),
                    ];
                }
                Ok(())
            })
        });

        // ---- Layers -------------------------------------------------------
        m.add_method("set_layer", |lua, this, n: i64| {
            with_state_mut(lua, |st| {
                if let Some(s) = st.scene.sprite_by_id_mut(this.0) {
                    s.layer = n as i32;
                }
                Ok(())
            })
        });
        m.add_method("go_front", |lua, this, ()| {
            with_state_mut(lua, |st| {
                let max = st.scene.sprites.iter().map(|x| x.layer).max().unwrap_or(0);
                if let Some(s) = st.scene.sprite_by_id_mut(this.0) {
                    s.layer = max + 1;
                }
                Ok(())
            })
        });
        m.add_method("go_back", |lua, this, ()| {
            with_state_mut(lua, |st| {
                let min = st.scene.sprites.iter().map(|x| x.layer).min().unwrap_or(0);
                if let Some(s) = st.scene.sprite_by_id_mut(this.0) {
                    s.layer = min - 1;
                }
                Ok(())
            })
        });

        // ---- 3D / billboards ----------------------------------------------
        m.add_method("in3d", |lua, this, flag: Option<bool>| {
            with_state_mut(lua, |st| {
                if let Some(s) = st.scene.sprite_by_id_mut(this.0) {
                    if let Some(f) = flag {
                        s.in3d = f;
                    }
                }
                Ok(())
            })
        });
        m.add_method("set_pos3", |lua, this, (x, y, z): (f64, f64, f64)| {
            with_state_mut(lua, |st| {
                if let Some(s) = st.scene.sprite_by_id_mut(this.0) {
                    s.pos3 = Vec3::new(x as f32, y as f32, z as f32);
                }
                Ok(())
            })
        });
        m.add_method("set_billboard", |lua, this, flag: bool| {
            with_state_mut(lua, |st| {
                if let Some(s) = st.scene.sprite_by_id_mut(this.0) {
                    s.billboard_both_sides = flag;
                }
                Ok(())
            })
        });

        // ---- Sensing ------------------------------------------------------
        m.add_method("touching", |lua, this, other: String| {
            with_state(lua, |st| {
                let (w, h) = st.host.size();
                let me = match st.scene.sprite_by_id(this.0) {
                    Some(s) => s,
                    None => return Ok(false),
                };
                let (fw, fh) = footprint(st, me);
                match other.as_str() {
                    "edge" => {
                        Ok(me.x - fw * 0.5 <= -(w as f32) / 2.0
                            || me.x + fw * 0.5 >= w as f32 / 2.0
                            || me.y - fh * 0.5 <= -(h as f32) / 2.0
                            || me.y + fh * 0.5 >= h as f32 / 2.0)
                    }
                    "pointer" => {
                        let mx = st.mouse.0 as f32 - w as f32 / 2.0;
                        let my = h as f32 / 2.0 - st.mouse.1 as f32;
                        Ok((mx - me.x).abs() <= fw * 0.5 && (my - me.y).abs() <= fh * 0.5)
                    }
                    _ => match st.scene.sprite_by_name(&other) {
                        Some(o) => {
                            let (fw2, fh2) = footprint(st, o);
                            Ok((me.x - o.x).abs() <= (fw + fw2) * 0.5
                                && (me.y - o.y).abs() <= (fh + fh2) * 0.5)
                        }
                        None => Ok(false),
                    },
                }
            })
        });
        m.add_method("distance_to", |lua, this, other: String| {
            with_state(lua, |st| {
                let (w, h) = st.host.size();
                let me = match st.scene.sprite_by_id(this.0) {
                    Some(s) => s,
                    None => return Ok(-1.0f64),
                };
                match other.as_str() {
                    "pointer" => {
                        let mx = st.mouse.0 as f32 - w as f32 / 2.0;
                        let my = h as f32 / 2.0 - st.mouse.1 as f32;
                        Ok((((mx - me.x).powi(2) + (my - me.y).powi(2)) as f64).sqrt())
                    }
                    _ => match st.scene.sprite_by_name(&other) {
                        Some(o) => {
                            Ok((((o.x - me.x).powi(2) + (o.y - me.y).powi(2)) as f64).sqrt())
                        }
                        None => Ok(-1.0),
                    },
                }
            })
        });
        m.add_method("mouse_over", |lua, this, ()| {
            with_state(lua, |st| {
                let (w, h) = st.host.size();
                let me = match st.scene.sprite_by_id(this.0) {
                    Some(s) => s,
                    None => return Ok(false),
                };
                let (fw, fh) = footprint(st, me);
                let mx = st.mouse.0 as f32 - w as f32 / 2.0;
                let my = h as f32 / 2.0 - st.mouse.1 as f32;
                Ok((mx - me.x).abs() <= fw * 0.5 && (my - me.y).abs() <= fh * 0.5)
            })
        });

        // ---- Lifecycle ----------------------------------------------------
        m.add_method("name", |lua, this, ()| {
            with_state(lua, |st| {
                Ok(st
                    .scene
                    .sprite_by_id(this.0)
                    .map(|s| s.name.clone())
                    .unwrap_or_default())
            })
        });
        m.add_method("id", |lua, this, ()| Ok(this.0));
        m.add_method("destroy", |lua, this, ()| {
            with_state_mut(lua, |st| {
                st.scene.remove_sprite(this.0);
                Ok(())
            })
        });
        m.add_method("clone", |lua, this, name: Option<String>| {
            let new_id = with_state_mut(lua, |st| {
                match st.scene.sprite_by_id(this.0) {
                    None => Ok(None),
                    Some(src) => {
                        let src = src.clone();
                        let new_name = name.unwrap_or_else(|| format!("{}-clone", src.name));
                        let ns = st.scene.new_sprite(&new_name);
                        ns.costume = src.costume.clone();
                        ns.x = src.x;
                        ns.y = src.y;
                        ns.layer = src.layer;
                        ns.direction = src.direction;
                        ns.size = src.size;
                        ns.in3d = src.in3d;
                        ns.pos3 = src.pos3;
                        ns.visible = src.visible;
                        if let Some(c) = &ns.costume {
                            st.assets.request(c, 5);
                        }
                        Ok(Some(ns.id))
                    }
                }
            })?;
            match new_id {
                Some(id) => {
                    let ud = lua.create_userdata(SpriteHandle(id))?;
                    Ok(Some(ud))
                }
                None => Ok(None),
            }
        });

        // Property access: sprite.x, sprite.visible, ...
        m.add_meta_method(MetaMethod::Index, |lua, this, key: String| {
            with_state(lua, |st| {
                Ok(match st.scene.sprite_by_id(this.0) {
                    Some(s) => match key.as_str() {
                        "x" => Value::Number(s.x as f64),
                        "y" => Value::Number(s.y as f64),
                        "direction" => Value::Number(s.direction as f64),
                        "size" => Value::Number(s.size as f64),
                        "visible" => Value::Boolean(s.visible),
                        "name" => Value::String(lua.create_string(&s.name)?),
                        "id" => Value::Integer(s.id as i64),
                        "saying" => match &s.say {
                            Some(say) => Value::String(lua.create_string(&say.message)?),
                            None => Value::Nil,
                        },
                        _ => Value::Nil,
                    },
                    None => Value::Nil,
                })
            })
        });
        m.add_meta_method_mut(
            MetaMethod::NewIndex,
            |lua, this, (key, v): (String, Value)| {
                with_state_mut(lua, |st| {
                    if let Some(s) = st.scene.sprite_by_id_mut(this.0) {
                        match (key.as_str(), v) {
                            ("x", Value::Number(n)) => s.x = n as f32,
                            ("y", Value::Number(n)) => s.y = n as f32,
                            ("direction", Value::Number(n)) => s.direction = n as f32,
                            ("size", Value::Number(n)) => s.size = n as f32,
                            ("visible", Value::Boolean(b)) => s.visible = b,
                            _ => {}
                        }
                    }
                Ok(())
                })
            },
        );
    }
}

fn footprint(st: &EngineState, s: &Sprite) -> (f32, f32) {
    let k = s.size / 100.0;
    match s.costume.as_ref().and_then(|c| st.tex_sizes.borrow().get(c).copied()) {
        Some((w, h)) => (w as f32 * k, h as f32 * k),
        None => (64.0 * k, 64.0 * k),
    }
}

/// Register the full API on a fresh Lua VM.
pub fn register_api(lua: &mlua::Lua) -> EngineResult<()> {
    let globals = lua.globals();

    macro_rules! queue_handler {
        ($kind:expr, $with_sub:expr) => {
            lua.create_function(|lua, f: Function| {
                let key: RegistryKey = lua.create_registry_value(f)?;
                with_state_mut(lua, |st| {
                    st.pending_handlers
                        .push(($kind.to_string(), $with_sub, key));
                    Ok(())
                })
            })?
        };
    }

    // ------------------------------------------------------------------ engine
    let engine = lua.create_table()?;

    engine.set("on_start", queue_handler!("start", None))?;
    engine.set("on_click", queue_handler!("click", None))?;
    engine.set("on_tick", queue_handler!("tick", None))?;
    engine.set("forever", queue_handler!("tick", None))?;
    engine.set(
        "on_key",
        lua.create_function(|lua, (name, f): (String, Function)| {
            let key: RegistryKey = lua.create_registry_value(f)?;
            with_state_mut(lua, |st| {
                st.pending_handlers.push(("key".into(), Some(name), key));
                Ok(())
            })
        })?,
    )?;
    engine.set(
        "on_key_held",
        lua.create_function(|lua, (name, f): (String, Function)| {
            let key: RegistryKey = lua.create_registry_value(f)?;
            with_state_mut(lua, |st| {
                st.pending_handlers
                    .push(("key_held".into(), Some(name), key));
                Ok(())
            })
        })?,
    )?;
    engine.set(
        "on_broadcast",
        lua.create_function(|lua, (name, f): (String, Function)| {
            let key: RegistryKey = lua.create_registry_value(f)?;
            with_state_mut(lua, |st| {
                st.pending_handlers
                    .push(("broadcast".into(), Some(name), key));
                Ok(())
            })
        })?,
    )?;
    engine.set(
        "broadcast",
        lua.create_function(|lua, name: String| {
            with_state_mut(lua, |st| {
                st.pending_broadcasts.push(name);
                Ok(())
            })
        })?,
    )?;
    engine.set(
        "log",
        lua.create_function(|_, msg: Variadic<Value>| {
            if crate::runtime::QUIET.load(std::sync::atomic::Ordering::Relaxed) {
                return Ok(());
            }
            let parts: Vec<String> = msg
                .iter()
                .map(|v| match v {
                    Value::String(s) => s.to_string_lossy().to_string(),
                    Value::Number(n) => format!("{n}"),
                    Value::Integer(i) => format!("{i}"),
                    Value::Boolean(b) => format!("{b}"),
                    Value::Nil => "nil".to_string(),
                    other => format!("{other:?}"),
                })
                .collect();
            println!("[game] {}", parts.join("  "));
            Ok(())
        })?,
    )?;
    engine.set("version", {
        let v = ENGINE_VERSION.to_string();
        lua.create_function(move |_, ()| Ok(v.clone()))?
    })?;

    engine.set(
        "set_background",
        lua.create_function(|lua, v: Value| {
            with_state_mut(lua, |st| {
                match &v {
                    Value::String(s) => {
                        let s = s.to_string_lossy().to_string();
                        if s.starts_with('#') {
                            st.scene.background_color =
                                parse_color(&s).unwrap_or([0.1, 0.12, 0.16]);
                            st.scene.background_image = None;
                        } else {
                            st.assets.request(&s, 8);
                            st.scene.background_image = Some(s);
                        }
                        Ok(())
                    }
                    Value::Table(t) => {
                        let r: f64 = t.raw_get(1)?;
                        let g: f64 = t.raw_get(2)?;
                        let b: f64 = t.raw_get(3)?;
                        st.scene.background_color = [
                            r as f32 / 255.0,
                            g as f32 / 255.0,
                            b as f32 / 255.0,
                        ];
                        st.scene.background_image = None;
                        Ok(())
                    }
                    _ => Err(mlua::Error::runtime("expected #hex or asset path")),
                }
            })?;
            Ok(())
        })?,
    )?;
    engine.set(
        "stage_size",
        lua.create_function(|lua, ()| {
            with_state(lua, |st| {
                let (w, h) = st.host.size();
                Ok((w as i64, h as i64))
            })
        })?,
    )?;
    engine.set(
        "set_title",
        lua.create_function(|lua, title: String| {
            with_state_mut(lua, |st| {
                st.host.set_title(&title);
                Ok(())
            })
        })?,
    )?;
    engine.set(
        "quit",
        lua.create_function(|lua, ()| {
            with_state_mut(lua, |st| {
                st.quit = true;
                Ok(())
            })
        })?,
    )?;
    engine.set(
        "fps_cap",
        lua.create_function(|lua, n: u32| {
            with_state_mut(lua, |st| {
                st.fps_cap = n.max(10);
                Ok(())
            })
        })?,
    )?;
    engine.set(
        "now",
        lua.create_function(|lua, ()| with_state(lua, |st| Ok(st.time)))?,
    )?;
    engine.set(
        "rand",
        lua.create_function(|_, (a, b): (f64, f64)| {
            let t = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .subsec_nanos() as f64
                / 1e9;
            Ok(a + (b - a) * t)
        })?,
    )?;
    engine.set(
        "key_pressed",
        lua.create_function(|lua, name: String| {
            with_state(lua, |st| Ok(st.keys_held.contains(&name)))
        })?,
    )?;
    engine.set(
        "mouse_x",
        lua.create_function(|lua, ()| {
            with_state(lua, |st| {
                let (w, _) = st.host.size();
                Ok(st.mouse.0 as f64 - w as f64 / 2.0)
            })
        })?,
    )?;
    engine.set(
        "mouse_y",
        lua.create_function(|lua, ()| {
            with_state(lua, |st| {
                let (_, h) = st.host.size();
                Ok(h as f64 / 2.0 - st.mouse.1 as f64)
            })
        })?,
    )?;
    engine.set("mouse_down", {
        lua.create_function(|lua, ()| with_state(lua, |st| Ok(st.mouse_down)))?
    })?;

    engine.set(
        "module_enabled",
        lua.create_function(|lua, name: String| {
            with_state(lua, |st| Ok(st.modules.is_enabled(&name)))
        })?,
    )?;
    engine.set(
        "enable_module",
        lua.create_function(|lua, name: String| {
            // Enable while borrowed; run glue AFTER dropping the borrow,
            // because init.lua may call back into this API.
            let glue = with_state_mut(lua, |st| {
                st.modules
                    .enable(&name)
                    .map_err(mlua::Error::external)?;
                if !st.config.modules.enabled.contains(&name) {
                    st.config.modules.enabled.push(name.clone());
                }
                Ok(st.modules.find(&name).and_then(|m| m.init_lua.clone()))
            })?;
            if let Some(g) = glue {
                lua.load(&g).set_name(format!("{name} init.lua")).exec()?;
            }
            Ok(())
        })?,
    )?;
    engine.set(
        "disable_module",
        lua.create_function(|lua, name: String| {
            with_state_mut(lua, |st| {
                st.modules.disable(&name);
                Ok(())
            })
        })?,
    )?;
    engine.set(
        "modules",
        lua.create_function(|lua, ()| {
            with_state(lua, |st| {
                let t = lua.create_table()?;
                for (i, m) in st.modules.installed.iter().enumerate() {
                    let mt = lua.create_table()?;
                    mt.set("name", m.manifest.module.name.clone())?;
                    mt.set("version", m.manifest.module.version.clone())?;
                    mt.set("description", m.manifest.module.description.clone())?;
                    mt.set("enabled", st.modules.is_enabled(&m.manifest.module.name))?;
                    t.raw_set(i + 1, mt)?;
                }
                Ok(t)
            })
        })?,
    )?;
    engine.set(
        "device",
        lua.create_function(|lua, ()| {
            with_state(lua, |st| {
                let d = &st.device;
                let t = lua.create_table()?;
                t.set("os", d.os.clone())?;
                t.set("arch", d.arch.clone())?;
                t.set("os_pretty", d.os_pretty.clone())?;
                t.set("cores", d.cores)?;
                t.set("ram_mb", d.ram_mb)?;
                t.set(
                    "path",
                    match d.path {
                        crate::device::GpuPath::LegacySoftware => "legacy",
                        crate::device::GpuPath::ModernWgpu => "modern",
                    },
                )?;
                t.set("reason", d.reason.clone())?;
                Ok(t)
            })
        })?,
    )?;
    engine.set(
        "set_camera",
        lua.create_function(|lua, t: mlua::Table| {
            with_state_mut(lua, |st| {
                let pos: Option<Vec3T> = t.get("pos").ok();
                let look: Option<Vec3T> = t.get("look_at").ok();
                if let Some(p) = pos {
                    st.scene.camera.pos = Vec3::new(p.x as f32, p.y as f32, p.z as f32);
                }
                if let Some(l) = look {
                    st.scene.camera.look_at = Vec3::new(l.x as f32, l.y as f32, l.z as f32);
                }
                if let Ok(fov) = t.get::<f64>("fov") {
                    st.scene.camera.fov_y_deg = fov as f32;
                }
                Ok(())
            })?;
            Ok(())
        })?,
    )?;
    engine.set(
        "camera",
        lua.create_function(|lua, ()| {
            with_state(lua, |st| {
                let c = st.scene.camera;
                let t = lua.create_table()?;
                t.set("x", c.pos.x as f64)?;
                t.set("y", c.pos.y as f64)?;
                t.set("z", c.pos.z as f64)?;
                t.set("fov", c.fov_y_deg as f64)?;
                Ok(t)
            })
        })?,
    )?;

    engine.set(
        "debug_threads",
        lua.create_function(|lua, ()| with_state(lua, |st| Ok(st.debug_pending)))?,
    )?;

    globals.set("engine", engine)?;

    // ------------------------------------------------------------------ Sprite
    let sprite_table = lua.create_table()?;
    sprite_table.set(
        "new",
        lua.create_function(|lua, (name, costume): (String, Option<String>)| {
            let id = with_state_mut(lua, |st| {
                let s = st.scene.new_sprite(&name);
                if let Some(c) = &costume {
                    st.assets.request(c, 5);
                }
                s.costume = costume;
                Ok(s.id)
            })?;
            let ud = lua.create_userdata(SpriteHandle(id))?;
            Ok(Some(ud))
        })?,
    )?;
    globals.set("Sprite", sprite_table)?;

    // ------------------------------------------------------------------ rt
    let rt = lua.create_table()?;
    rt.set(
        "enable",
        lua.create_function(|lua, opts: Option<mlua::Table>| {
            with_state_mut(lua, |st| {
                require_module(st, "rtmode")?;
                st.rt_enabled = true;
                if let Some(o) = &opts {
                    if let Ok(q) = o.get::<String>("quality") {
                        st.rt_quality = RtQuality::parse(&q);
                        st.rt_scale = st.rt_quality.default_scale();
                    }
                    if let Ok(s) = o.get::<f64>("scale") {
                        st.rt_scale = (s as f32).clamp(0.05, 1.0);
                    }
                    if let Ok(b) = o.get::<u32>("bounces") {
                        st.rt_bounces = b.clamp(1, 4);
                    }
                    if let Ok(d) = o.get::<bool>("denoise") {
                        st.rt_denoise = d;
                    }
                }
                Ok(())
            })?;
            Ok(())
        })?,
    )?;
    rt.set(
        "disable",
        lua.create_function(|lua, ()| {
            with_state_mut(lua, |st| {
                require_module(st, "rtmode")?;
                st.rt_enabled = false;
                Ok(())
            })?;
            Ok(())
        })?,
    )?;
    rt.set(
        "enabled",
        lua.create_function(|lua, ()| {
            with_state(lua, |st| {
                Ok(st.rt_enabled && st.modules.is_enabled("rtmode"))
            })
        })?,
    )?;
    rt.set(
        "status",
        lua.create_function(|lua, ()| {
            with_state(lua, |st| {
                Ok(format!(
                    "raytracing={} quality={:?} scale={:.2} bounces={} denoise={} spheres={} planes={}",
                    st.rt_enabled,
                    st.rt_quality,
                    st.rt_scale,
                    st.rt_bounces,
                    st.rt_denoise,
                    st.scene.rt.spheres.len(),
                    st.scene.rt.planes.len()
                ))
            })
        })?,
    )?;
    rt.set(
        "set_sun",
        lua.create_function(|lua, t: mlua::Table| {
            with_state_mut(lua, |st| {
                require_module(st, "rtmode")?;
                if let Ok(dir) = t.get::<Vec3T>("dir") {
                    st.scene.rt.sun.dir =
                        Vec3::new(dir.x as f32, dir.y as f32, dir.z as f32).normalize();
                }
                if let Ok(c) = t.get::<Value>("color") {
                    if let Some(col) = value_to_color(&c) {
                        st.scene.rt.sun.color = col;
                    }
                }
                if let Ok(i) = t.get::<f64>("intensity") {
                    st.scene.rt.sun.intensity = i as f32;
                }
                Ok(())
            })?;
            Ok(())
        })?,
    )?;
    rt.set(
        "set_env",
        lua.create_function(|lua, t: mlua::Table| {
            with_state_mut(lua, |st| {
                require_module(st, "rtmode")?;
                for (key, slot) in [("sky_top", 0usize), ("sky_horizon", 1), ("ground", 2)] {
                    if let Ok(c) = t.get::<Value>(key) {
                        if let Some(col) = value_to_color(&c) {
                            match slot {
                                0 => st.scene.rt.env.sky_top = col,
                                1 => st.scene.rt.env.sky_horizon = col,
                                _ => st.scene.rt.env.ground = col,
                            }
                        }
                    }
                }
                Ok(())
            })?;
            Ok(())
        })?,
    )?;
    rt.set(
        "add_sphere",
        lua.create_function(|lua, t: mlua::Table| {
            with_state_mut(lua, |st| {
                require_module(st, "rtmode")?;
                let name: String = t.get("name").unwrap_or_else(|_| "sphere".into());
                let pos: Vec3T = t
                    .get("pos")
                    .unwrap_or(Vec3T { x: 0.0, y: 1.0, z: 0.0 });
                let r: f64 = t.get("r").unwrap_or(1.0);
                let color = t
                    .get::<Value>("color")
                    .ok()
                    .and_then(|c| value_to_color(&c))
                    .unwrap_or([0.8, 0.2, 0.2]);
                st.scene.rt.spheres.push(RtSphere {
                    name,
                    center: Vec3::new(pos.x as f32, pos.y as f32, pos.z as f32),
                    radius: r as f32,
                    color,
                    metallic: t.get::<f64>("metallic").unwrap_or(0.0) as f32,
                    roughness: t.get::<f64>("roughness").unwrap_or(0.35) as f32,
                    emissive: t.get::<f64>("emissive").unwrap_or(0.0) as f32,
                });
                Ok(())
            })?;
            Ok(())
        })?,
    )?;
    rt.set(
        "add_plane",
        lua.create_function(|lua, t: mlua::Table| {
            with_state_mut(lua, |st| {
                require_module(st, "rtmode")?;
                let name: String = t.get("name").unwrap_or_else(|_| "plane".into());
                let y: f64 = t.get("y").unwrap_or(0.0);
                let color = t
                    .get::<Value>("color")
                    .ok()
                    .and_then(|c| value_to_color(&c))
                    .unwrap_or([0.55, 0.55, 0.58]);
                st.scene.rt.planes.push(RtPlane {
                    name,
                    point: Vec3::new(0.0, y as f32, 0.0),
                    normal: Vec3::new(0.0, 1.0, 0.0),
                    color,
                    checkered: t.get::<bool>("checkered").unwrap_or(false),
                    metallic: t.get::<f64>("metallic").unwrap_or(0.0) as f32,
                    roughness: t.get::<f64>("roughness").unwrap_or(0.5) as f32,
                });
                Ok(())
            })?;
            Ok(())
        })?,
    )?;
    rt.set(
        "remove",
        lua.create_function(|lua, name: String| {
            with_state_mut(lua, |st| {
                require_module(st, "rtmode")?;
                st.scene.rt.spheres.retain(|s| s.name != name);
                st.scene.rt.planes.retain(|p| p.name != name);
                Ok(())
            })?;
            Ok(())
        })?,
    )?;
    rt.set(
        "clear",
        lua.create_function(|lua, ()| {
            with_state_mut(lua, |st| {
                require_module(st, "rtmode")?;
                st.scene.rt.spheres.clear();
                st.scene.rt.planes.clear();
                Ok(())
            })?;
            Ok(())
        })?,
    )?;
    globals.set("rt", rt)?;

    // ------------------------------------------------------------------ stream
    let stream = lua.create_table()?;
    stream.set(
        "load",
        lua.create_function(|lua, path: String| {
            with_state_mut(lua, |st| {
                require_module(st, "stream")?;
                st.assets.request(&path, 5);
                Ok(())
            })
        })?,
    )?;
    stream.set(
        "prefetch",
        lua.create_function(|lua, paths: Vec<String>| {
            with_state_mut(lua, |st| {
                require_module(st, "stream")?;
                st.assets.prefetch(&paths);
                Ok(())
            })
        })?,
    )?;
    stream.set(
        "release",
        lua.create_function(|lua, path: String| {
            with_state_mut(lua, |st| {
                require_module(st, "stream")?;
                st.assets.release(&path);
                Ok(())
            })
        })?,
    )?;
    stream.set(
        "set_budget",
        lua.create_function(|lua, mb: u64| {
            with_state_mut(lua, |st| {
                require_module(st, "stream")?;
                st.assets.set_budget((mb.max(8) as usize) * 1024 * 1024);
                Ok(())
            })
        })?,
    )?;
    stream.set(
        "stats",
        lua.create_function(|lua, ()| {
            with_state(lua, |st| {
                require_module(st, "stream")?;
                let s = st.assets.stats();
                let t = lua.create_table()?;
                t.set("loaded", s.loaded)?;
                t.set("loading", s.loading)?;
                t.set("resident_mb", s.resident_bytes as f64 / 1048576.0)?;
                t.set("budget_mb", s.budget_bytes as f64 / 1048576.0)?;
                t.set("evictions", s.evictions)?;
                t.set("loads", s.loads_completed)?;
                Ok(t)
            })
        })?,
    )?;
    globals.set("stream", stream)?;

    // ------------------------------------------------------------------ pbr3d
    let pbr = lua.create_table()?;
    pbr.set(
        "set_camera",
        lua.create_function(|lua, t: mlua::Table| {
            with_state_mut(lua, |st| {
                require_module(st, "pbr3d")?;
                let pos: Vec3T = t
                    .get("pos")
                    .unwrap_or(Vec3T { x: 0.0, y: 1.0, z: 5.0 });
                let look: Vec3T = t
                    .get("look_at")
                    .unwrap_or(Vec3T { x: 0.0, y: 0.0, z: 0.0 });
                let fov: f64 = t.get("fov").unwrap_or(60.0);
                let mut cam = st.scene.pbr.camera.unwrap_or_default();
                cam.pos = Vec3::new(pos.x as f32, pos.y as f32, pos.z as f32);
                cam.look_at = Vec3::new(look.x as f32, look.y as f32, look.z as f32);
                cam.fov_y_deg = fov as f32;
                st.scene.pbr.camera = Some(cam);
                Ok(())
            })
        })?,
    )?;
    pbr.set(
        "set_sun",
        lua.create_function(|lua, t: mlua::Table| {
            with_state_mut(lua, |st| {
                require_module(st, "pbr3d")?;
                let mut sun = st.scene.pbr.sun.unwrap_or_default();
                if let Ok(dir) = t.get::<Vec3T>("dir") {
                    sun.dir = Vec3::new(dir.x as f32, dir.y as f32, dir.z as f32).normalize();
                }
                if let Ok(c) = t.get::<Value>("color") {
                    if let Some(col) = value_to_color(&c) {
                        sun.color = col;
                    }
                }
                if let Ok(i) = t.get::<f64>("intensity") {
                    sun.intensity = i as f32;
                }
                st.scene.pbr.sun = Some(sun);
                Ok(())
            })
        })?,
    )?;
    pbr.set(
        "add",
        lua.create_function(|lua, t: mlua::Table| {
            with_state_mut(lua, |st| {
                require_module(st, "pbr3d")?;
                let name: String = t.get("name").unwrap_or_else(|_| "mesh".into());
                let pos: Vec3T = t.get("pos").unwrap_or(Vec3T { x: 0.0, y: 0.0, z: 0.0 });
                let rot: Vec3T = t.get("rot").unwrap_or(Vec3T { x: 0.0, y: 0.0, z: 0.0 });
                let s: f64 = t.get("scale").unwrap_or(1.0);
                let color = t
                    .get::<Value>("color")
                    .ok()
                    .and_then(|c| value_to_color(&c))
                    .unwrap_or([0.7, 0.7, 0.75]);
                st.scene.pbr.meshes.push(PbrMesh {
                    name,
                    mesh: t.get("mesh").unwrap_or_else(|_| "cube".to_string()),
                    pos: Vec3::new(pos.x as f32, pos.y as f32, pos.z as f32),
                    rot: Vec3::new(rot.x as f32, rot.y as f32, rot.z as f32),
                    scale: Vec3::new(s as f32, s as f32, s as f32),
                    color,
                    metallic: t.get::<f64>("metallic").unwrap_or(0.0) as f32,
                    roughness: t.get::<f64>("roughness").unwrap_or(0.5) as f32,
                    texture: t.get::<Option<String>>("texture").unwrap_or(None),
                });
                Ok(())
            })
        })?,
    )?;
    pbr.set(
        "remove",
        lua.create_function(|lua, name: String| {
            with_state_mut(lua, |st| {
                require_module(st, "pbr3d")?;
                st.scene.pbr.meshes.retain(|m| m.name != name);
                Ok(())
            })
        })?,
    )?;
    pbr.set(
        "clear",
        lua.create_function(|lua, ()| {
            with_state_mut(lua, |st| {
                require_module(st, "pbr3d")?;
                st.scene.pbr.meshes.clear();
                Ok(())
            })
        })?,
    )?;
    globals.set("pbr3d", pbr)?;

    // ---------------------------------------------------------------- bootstrap
    // `engine.wait` is coroutine-based; `forever` sugar.
    let boot = r#"engine.wait = function(sec) coroutine.yield({"timer", sec or 0}) end"#;
    lua.load(boot).set_name("bootstrap").exec()?;

    Ok(())
}

/// Reject an API call when its owning module is not enabled.
pub fn require_module(st: &EngineState, name: &str) -> Result<(), mlua::Error> {
    if st.modules.is_enabled(name) {
        Ok(())
    } else {
        Err(mlua::Error::runtime(format!(
            "this API requires the '{name}' module (enable it in liteengine.toml)"
        )))
    }
}

/// Support types for value conversion.
pub mod support {
    use crate::scene::parse_color;
    use mlua::Value;

    /// Lua table {x=,y=,z=}.
    #[derive(Debug, Clone, Copy)]
    pub struct Vec3T {
        pub x: f64,
        pub y: f64,
        pub z: f64,
    }

    impl mlua::FromLua for Vec3T {
        fn from_lua(v: Value, _lua: &mlua::Lua) -> mlua::Result<Self> {
            match v {
                Value::Table(t) => Ok(Vec3T {
                    x: t.get::<f64>("x").unwrap_or(0.0),
                    y: t.get::<f64>("y").unwrap_or(0.0),
                    z: t.get::<f64>("z").unwrap_or(0.0),
                }),
                _ => Err(mlua::Error::runtime("expected table {x,y,z}")),
            }
        }
    }

    /// Accept `#rrggbb` strings or `{r,g,b}` tables (0..255 or 0..1).
    pub fn value_to_color(v: &Value) -> Option<[f32; 3]> {
        match v {
            Value::String(s) => parse_color(&s.to_string_lossy()),
            Value::Table(t) => {
                let r: f64 = t.raw_get(1).ok()?;
                let g: f64 = t.raw_get(2).ok()?;
                let b: f64 = t.raw_get(3).ok()?;
                let k = if r > 1.0 || g > 1.0 || b > 1.0 {
                    1.0 / 255.0
                } else {
                    1.0
                };
                Some([
                    (r as f32 * k).clamp(0.0, 1.0),
                    (g as f32 * k).clamp(0.0, 1.0),
                    (b as f32 * k).clamp(0.0, 1.0),
                ])
            }
            _ => None,
        }
    }
}

use support::{value_to_color, Vec3T};
