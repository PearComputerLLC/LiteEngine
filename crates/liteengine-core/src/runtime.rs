//! The engine runtime: owns the Lua VM, scene, assets, modules and host,
//! and drives the fixed-order main loop:
//!
//! ```text
//! poll input -> dispatch events -> resume coroutines -> animate glides
//!            -> expire say() bubbles -> stream assets -> build Frame
//!            -> render (legacy software / hybrid RT / modern wgpu) -> present
//! ```

use crate::assets::{AssetStore, Vfs};
use crate::device::DeviceInfo;
use crate::events::InputEvent;
use crate::host::{Host, Renderer};
use crate::modules::ModuleManager;
use crate::project::ProjectConfig;
use crate::scheduler::Scheduler;
use crate::scene::{parse_color, Camera, Scene};
use crate::snapshot::*;
use crate::EngineResult;
use mlua::{Lua, Value};
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;
use std::time::{Duration, Instant};

/// State shared between the runtime loop and the Lua API closures.
pub struct EngineState {
    pub config: ProjectConfig,
    pub device: DeviceInfo,
    pub scene: Scene,
    pub assets: AssetStore,
    pub modules: ModuleManager,
    pub host: Box<dyn Host>,
    pub keys_held: HashSet<String>,
    pub mouse: (i32, i32),
    pub mouse_down: bool,
    pub time: f64,
    pub quit: bool,
    pub frame: u64,
    pub fps_cap: u32,
    /// Raytracing mode (toggled by `rt.enable` / `rt.disable`).
    pub rt_enabled: bool,
    pub rt_quality: crate::scene::RtQuality,
    pub rt_scale: f32,
    pub rt_bounces: u32,
    pub rt_denoise: bool,
    /// In-flight glide animations.
    pub glides: Vec<Glide>,
    /// Width/height of each costume path once known (for sprite footprints).
    pub tex_sizes: std::cell::RefCell<std::collections::HashMap<String, (u32, u32)>>,
    /// Handlers registered from Lua, drained onto the scheduler by the loop.
    pub pending_handlers: Vec<(String, Option<String>, mlua::RegistryKey)>,
    /// Broadcasts queued from Lua, dispatched by the loop.
    pub pending_broadcasts: Vec<String>,
    /// Parked-coroutine count from the last scheduler update.
    pub debug_pending: usize,
}

/// A `glide_to` / `glide` animation, advanced natively each frame.
#[derive(Debug, Clone)]
pub struct Glide {
    pub sprite_id: u32,
    pub from: (f32, f32),
    pub to: (f32, f32),
    pub start: f64,
    pub dur: f64,
}

pub type SharedRc = Rc<RefCell<EngineState>>;

/// The engine. Create with [`Engine::new`], then [`Engine::run`].
pub struct Engine {
    pub lua: Lua,
    shared: SharedRc,
    sched: Scheduler,
}

impl Engine {
    /// Wire up a full engine from a project config and a window host.
    pub fn new(
        config: ProjectConfig,
        device: DeviceInfo,
        vfs: Vfs,
        host: Box<dyn Host>,
    ) -> EngineResult<Engine> {
        let quiet = std::env::var("LITEENGINE_QUIET")
            .map(|v| v == "1" || v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);
        QUIET.store(quiet, std::sync::atomic::Ordering::Relaxed);

        let budget = (config.assets.budget_mb.max(8) as usize) * 1024 * 1024;
        let assets = AssetStore::new(vfs, budget);

        let mut modules = ModuleManager::new()?;
        // Native features compiled into this build:
        modules.provide_native("core.events");
        modules.provide_native("core.stage");
        modules.provide_native("gfx2d.sprites");
        modules.provide_native("gfx3d.billboards");
        modules.provide_native("ui.speech");
        modules.provide_native("assets.stream");
        modules.provide_native("rt.toggle");
        if device.path == crate::device::GpuPath::ModernWgpu {
            modules.provide_native("pbr3d.renderer");
        }

        let scene = Scene {
            camera: Camera::default(),
            background_color: parse_color(&config.background).unwrap_or([0.1, 0.12, 0.16]),
            background_image: if config.background.starts_with('#') {
                None
            } else if !config.background.is_empty() {
                Some(config.background.clone())
            } else {
                None
            },
            ..Default::default()
        };

        let state = EngineState {
            rt_quality: crate::scene::RtQuality::parse(&config.rt.quality),
            rt_scale: if config.rt.scale > 0.0 {
                config.rt.scale
            } else {
                crate::scene::RtQuality::parse(&config.rt.quality).default_scale()
            },
            rt_bounces: config.rt.bounces.max(1).min(4),
            rt_denoise: config.rt.denoise,
            rt_enabled: config.raytracing,
            fps_cap: config.fps_cap.max(10),
            scene,
            assets,
            modules,
            glides: Vec::new(),
            tex_sizes: std::cell::RefCell::new(std::collections::HashMap::new()),
            pending_handlers: Vec::new(),
            pending_broadcasts: Vec::new(),
            debug_pending: 0,
            config,
            device,
            host,
            keys_held: HashSet::new(),
            mouse: (0, 0),
            mouse_down: false,
            time: 0.0,
            quit: false,
            frame: 0,
        };

        let lua = Lua::new();
        let shared: SharedRc = Rc::new(RefCell::new(state));
        lua.set_app_data::<SharedRc>(shared.clone());

        let mut engine = Engine {
            lua,
            shared: shared.clone(),
            sched: Scheduler::new(),
        };

        // Enable configured modules (runs their init.lua), register the API,
        // then load the game entry script.
        let enabled: Vec<String> = shared.borrow().config.modules.enabled.clone();
        for name in &enabled {
            engine.enable_module(name)?;
        }
        crate::luaapi::register_api(&engine.lua)?;;
        // Re-run module glue AFTER the API exists so init.lua can extend it.
        {
            let st = shared.borrow();
            let inits: Vec<Option<String>> = st
                .modules
                .enabled_sorted()
                .iter()
                .map(|n| st.modules.find(n).and_then(|m| m.init_lua.clone()))
                .collect();
            for glue in inits.into_iter().flatten() {
                engine.lua.load(&glue).set_name("module init.lua").exec()?;
            }
        }

        let entry = shared.borrow().config.entry.clone();
        let source = shared
            .borrow()
            .assets
            .vfs()
            .read(&entry)
            .map_err(crate::EngineError::Io)?;
        let src_str = String::from_utf8_lossy(&source).to_string();
        engine
            .lua
            .load(src_str.as_str())
            .set_name(entry)
            .exec()?;

        // Drain handlers the entry script registered (no Lua runs here).
        {
            let pending: Vec<_> = shared
                .borrow_mut()
                .pending_handlers
                .drain(..)
                .collect();
            for (kind, sub, key) in pending {
                engine.sched.handlers.add(&kind, sub, key);
            }
        }

        // Preload the background image (if any) and manifest preload list.
        {
            let mut st = shared.borrow_mut();
            if let Some(bg) = st.scene.background_image.clone() {
                st.assets.request(&bg, 10);
            }
            let pre = st.config.assets.preload.clone();
            st.assets.prefetch(&pre);
        }

        Ok(engine)
    }

    /// Enable a module by name (also available from Lua).
    pub fn enable_module(&mut self, name: &str) -> EngineResult<()> {
        self.shared.borrow_mut().modules.enable(name)?;
        // Run its init.lua if the API is already registered.
        if self
            .lua
            .globals()
            .get::<Option<mlua::Table>>("engine")?
            .is_some()
        {
            let glue = {
                let st = self.shared.borrow();
                st.modules.find(name).and_then(|m| m.init_lua.clone())
            };
            if let Some(g) = glue {
                self.lua.load(&g).set_name(format!("{name} init.lua")).exec()?;
            }
        }
        Ok(())
    }

    /// Whether the runtime is currently in raytracing mode.
    pub fn raytracing_enabled(&self) -> bool {
        self.shared.borrow().rt_enabled
    }

    /// Request engine shutdown (used by tests and `engine.quit()`).
    pub fn quit(&mut self) {
        self.shared.borrow_mut().quit = true;
    }

    /// Access shared state (tests, CLI introspection).
    pub fn shared(&self) -> SharedRc {
        self.shared.clone()
    }

    /// Run the main loop until quit, window close, or `max_frames` elapse.
    pub fn run(
        &mut self,
        renderer: &mut dyn Renderer,
        max_frames: Option<u64>,
    ) -> EngineResult<()> {
        // Green flag!
        {
            let mut sh = self.shared.borrow_mut();
            sh.time = 0.0;
        }
        self.sched.dispatch_start(&self.lua, 0.0);

        let mut last = Instant::now();
        loop {
            let frame_start = Instant::now();
            let dt = last.elapsed().as_secs_f64().min(0.1);
            last = frame_start;

            let mut events = Vec::new();
            let (mut w, mut h) = (960usize, 540usize);
            {
                let mut st = self.shared.borrow_mut();
                (w, h) = st.host.size();
                events = st.host.poll();
                st.time += dt;
            }

            for ev in events {
                match ev {
                    InputEvent::KeyDown(name) => {
                        let mut st = self.shared.borrow_mut();
                        st.keys_held.insert(name.to_string());
                        drop(st);
                        self.sched.dispatch_key(&self.lua, name, self.time_now());
                    }
                    InputEvent::KeyUp(name) => {
                        self.shared.borrow_mut().keys_held.remove(name);
                    }
                    InputEvent::MouseDown(x, y, button) => {
                        let (w, h) = {
                            let mut st = self.shared.borrow_mut();
                            st.mouse = (x, y);
                            st.mouse_down = true;
                            st.host.size()
                        };
                        let sx = x as f32 - w as f32 / 2.0;
                        let sy = h as f32 / 2.0 - y as f32;
                        self.sched
                            .dispatch_click(&self.lua, sx as i32, sy as i32, button, self.time_now());
                    }
                    InputEvent::MouseUp(..) => {
                        self.shared.borrow_mut().mouse_down = false;
                    }
                    InputEvent::MouseMove(x, y) => {
                        self.shared.borrow_mut().mouse = (x, y);
                    }
                    InputEvent::CloseRequest => {
                        self.shared.borrow_mut().quit = true;
                    }
                    InputEvent::Resized(..) => {}
                }
            }

            let quit = self.shared.borrow().quit;
            if quit {
                break;
            }

            // Per-frame events: held keys + forever/tick handlers.
            let (held, now) = {
                let st = self.shared.borrow();
                (st.keys_held.clone(), st.time)
            };
            self.sched.dispatch_key_held(&self.lua, &held, now);
            self.sched.dispatch_tick(&self.lua, dt, now);
            self.sched.update(&self.lua, now);

            // Drain broadcasts queued by handlers (bounded to avoid storms).
            for _round in 0..32 {
                let queued: Vec<String> = {
                    let mut st = self.shared.borrow_mut();
                    std::mem::take(&mut st.pending_broadcasts)
                };
                if queued.is_empty() {
                    break;
                }
                for name in queued {
                    self.sched.dispatch_broadcast(&self.lua, &name, self.time_now());
                    self.sched.update(&self.lua, self.time_now());
                }
            }
            // Drain handlers registered mid-game.
            {
                let pending: Vec<_> = self
                    .shared
                    .borrow_mut()
                    .pending_handlers
                    .drain(..)
                    .collect();
                for (kind, sub, key) in pending {
                    self.sched.handlers.add(&kind, sub, key);
                }
            }

            // Native animation + expiry + streaming.
            {
                let mut st = self.shared.borrow_mut();
                Self::advance_glides(&mut st, now);
                Self::expire_says(&mut st, now);
                st.assets.tick();
                st.debug_pending = self.sched.pending();
            }

            // Build snapshot & render.
            let frame = self.build_frame();
            {
                let mut st = self.shared.borrow_mut();
                let host: &mut dyn Host = &mut *st.host;
                renderer
                    .render_frame(&frame, host)
                    .map_err(crate::EngineError::Render)?;
            }

            self.shared.borrow_mut().frame += 1;
            if let Some(n) = max_frames {
                if self.shared.borrow().frame >= n {
                    break;
                }
            }

            // FPS cap.
            let cap = self.shared.borrow().fps_cap;
            let min_frame = Duration::from_secs_f64(1.0 / cap as f64);
            let spent = frame_start.elapsed();
            if spent < min_frame {
                std::thread::sleep(min_frame - spent);
            }
        }
        self.sched.clear(&self.lua);
        Ok(())
    }

    fn time_now(&self) -> f64 {
        self.shared.borrow().time
    }

    fn advance_glides(st: &mut EngineState, now: f64) {
        let mut done = Vec::new();
        for g in st.glides.iter_mut() {
            let t = ((now - g.start) / g.dur.max(0.0001)).clamp(0.0, 1.0);
            // smoothstep easing for pleasant Scratch-like motion
            let e = (t * t * (3.0 - 2.0 * t)) as f32;
            if let Some(s) = st.scene.sprite_by_id_mut(g.sprite_id) {
                s.x = g.from.0 + (g.to.0 - g.from.0) * e;
                s.y = g.from.1 + (g.to.1 - g.from.1) * e;
            }
            if t >= 1.0 {
                done.push(g.sprite_id);
            }
        }
        st.glides.retain(|g| !done.contains(&g.sprite_id));
    }

    fn expire_says(st: &mut EngineState, now: f64) {
        for s in st.scene.sprites.iter_mut() {
            if let Some(say) = &s.say {
                if now >= say.until {
                    s.say = None;
                }
            }
        }
    }

    /// Build the renderer-ready snapshot for this frame.
    pub fn build_frame(&self) -> Frame {
        let mut st = self.shared.borrow_mut();
        let (w, h) = st.host.size();
        let mut frame = Frame::empty(w, h);

        // Clone the small scene data we need so `st.assets` can be used
        // mutably while iterating sprites (LRU bookkeeping is &mut).
        let bg_image = st.scene.background_image.clone();
        let sprites: Vec<crate::scene::Sprite> = st.scene.sprites.clone();
        let rt_scene = st.scene.rt.clone();
        let camera = st.scene.camera;
        let rt_enabled = st.rt_enabled;
        let rtmode_on = st.modules.is_enabled("rtmode");

        // Background: color or streamed image.
        frame.bg_color = st.scene.background_color;
        if let Some(bg) = &bg_image {
            frame.background = st.assets.get(bg);
            if frame.background.is_none() {
                st.assets.request(bg, 8);
            }
        }

        // 2D sprites (sorted back-to-front by layer).
        let mut order: Vec<usize> = (0..sprites.len()).collect();
        order.sort_by_key(|&i| sprites[i].layer);
        for i in order {
            let s = &sprites[i];
            if !s.visible || s.in3d {
                continue;
            }
            let texture = s.costume.as_deref().and_then(|c| st.assets.get(c));
            let placeholder = texture.is_none() && s.costume.is_some();
            if let Some(t) = &texture {
                st.tex_sizes
                    .borrow_mut()
                    .insert(s.costume.clone().unwrap(), (t.width, t.height));
            }
            let (fw, fh) = match &texture {
                Some(t) => {
                    let k = s.size / 100.0;
                    (t.width as f32 * k, t.height as f32 * k)
                }
                None => (64.0 * s.size / 100.0, 64.0 * s.size / 100.0),
            };
            frame.sprites.push(SpriteDraw {
                texture,
                cx: w as f32 / 2.0 + s.x,
                cy: h as f32 / 2.0 - s.y,
                layer: s.layer,
                dir_deg: s.direction - 90.0, // Scratch 90 = pointing right
                scale: s.size / 100.0,
                alpha: s.alpha,
                tint: s.tint,
                placeholder,
                name: s.name.clone(),
                foot_w: fw,
                foot_h: fh,
            });
        }

        // 3D billboards (2D costumes displayed in the 3D world).
        for s in sprites.iter().filter(|s| s.visible && s.in3d) {
            let texture = s.costume.as_deref().and_then(|c| st.assets.get(c));
            let placeholder = texture.is_none() && s.costume.is_some();
            let (tw, th) = match &texture {
                Some(t) => (t.width, t.height),
                None => (64, 64),
            };
            let world_h = 2.0 * s.size / 100.0;
            let world_w = world_h * (tw as f32 / th as f32);
            frame.billboards.push(BillboardDraw {
                texture,
                pos: s.pos3,
                width: world_w,
                height: world_h,
                alpha: s.alpha,
                tint: s.tint,
                both_sides: s.billboard_both_sides,
                placeholder,
            });
        }

        // Speech bubbles anchored top-right of each sprite.
        // 2D anchors are computed from the SpriteDraw list we just built.
        for d in frame.sprites.iter() {
            let s = match sprites
                .iter()
                .find(|sp| sp.name == d.name && !sp.in3d && sp.visible && sp.say.is_some())
            {
                Some(s) => s,
                None => continue,
            };
            if let Some(say) = &s.say {
                let ax = d.cx + d.foot_w * 0.5;
                let ay = d.cy - d.foot_h * 0.5;
                frame.bubbles.push(BubbleDraw {
                    text: say.message.clone(),
                    think: say.think,
                    anchor_x: ax,
                    anchor_y: ay,
                });
            }
        }
        for s in sprites.iter().filter(|s| s.in3d && s.visible) {
            if let Some(say) = &s.say {
                let top = s
                    .pos3
                    .add(crate::scene::Vec3::new(0.0, 1.1 * s.size / 100.0, 0.0));
                let (ax, ay) = match camera.project(top, w as f32, h as f32) {
                    Some((x, y, _)) => (x, y),
                    None => continue,
                };
                frame.bubbles.push(BubbleDraw {
                    text: say.message.clone(),
                    think: say.think,
                    anchor_x: ax,
                    anchor_y: ay,
                });
            }
        }

        // Raytracing parameters.
        if rt_enabled && rtmode_on {
            frame.rt = Some(RtParams {
                quality: st.rt_quality,
                scale: st.rt_scale,
                bounces: st.rt_bounces,
                denoise: st.rt_denoise,
                include_billboards: true,
                temporal_floor: if st.glides.is_empty() { 0.03 } else { 0.35 },
                sun: rt_scene.sun,
                env: rt_scene.env,
                scene: rt_scene,
                camera,
            });
        }

        frame.pbr_camera = st.scene.pbr.camera;
        frame.pbr_sun = st.scene.pbr.sun;
        frame.pbr_env = st.scene.pbr.env;
        frame.pbr_meshes = st.scene.pbr.meshes.clone();
        frame
    }
}

/// Global quiet flag used by error reporting (set from LITEENGINE_QUIET).
pub static QUIET: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Engine self-test used by `liteengine doctor`: boots a Lua VM, registers
/// the API and runs a probe script exercising the scheduler end-to-end.
pub fn self_test() -> Result<(), String> {
    let lua = Lua::new();
    crate::luaapi::register_api(&lua).map_err(|e| e.to_string())?;

    // Minimal fake state so API closures work during the probe.
    let device = crate::device::DeviceInfo::detect();
    let assets = crate::assets::AssetStore::new(
        crate::assets::Vfs::Dir(std::env::temp_dir()),
        32 * 1024 * 1024,
    );
    let modules = crate::modules::ModuleManager::new().map_err(|e| e.to_string())?;
    let host: Box<dyn Host> = Box::new(ProbeHost);
    let state = EngineState {
        rt_quality: crate::scene::RtQuality::Medium,
        rt_scale: 0.3,
        rt_bounces: 2,
        rt_denoise: true,
        rt_enabled: false,
        fps_cap: 60,
        scene: Scene::default(),
        assets,
        modules,
        glides: Vec::new(),
        tex_sizes: std::cell::RefCell::new(std::collections::HashMap::new()),
        pending_handlers: Vec::new(),
        pending_broadcasts: Vec::new(),
        debug_pending: 0,
        config: ProjectConfig::default(),
        device,
        host,
        keys_held: HashSet::new(),
        mouse: (0, 0),
        mouse_down: false,
        time: 0.0,
        quit: false,
        frame: 0,
    };
    let rc: crate::luaapi::StateRc = Rc::new(RefCell::new(state));
    lua.set_app_data::<crate::luaapi::StateRc>(rc);
    lua.load(
        r##"
        local s = Sprite.new("probe", nil)
        s:go_to(1, 2)
        s:say("ok", 1)
        engine.set_background("#101010")
        local dev = engine.device()
        assert(dev.os ~= nil)
        assert(s:id() >= 0)
        "##,
    )
    .exec()
    .map_err(|e| format!("probe script failed: {e}"))?;
    Ok(())
}

/// Tiny no-op host for the doctor probe.
struct ProbeHost;

impl Host for ProbeHost {
    fn size(&self) -> (usize, usize) {
        (64, 64)
    }
    fn poll(&mut self) -> Vec<crate::events::InputEvent> {
        Vec::new()
    }
    fn present(&mut self, _: &[u32], _: usize, _: usize) -> Result<(), String> {
        Ok(())
    }
    fn should_quit(&self) -> bool {
        false
    }
    fn name(&self) -> &'static str {
        "probe"
    }
}
