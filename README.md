# LiteEngine

**A lightweight, modular, Lua-scripted game engine — as easy as Scratch, tuned for AAA looks on very old hardware.**

LiteEngine is written in Rust, embeds Lua 5.4, and ships as a tiny set of crates plus a
handful of zip-compressed `.lemodule` components. Games are scripted with a documented,
Scratch-like event API (`engine.on_start`, `sprite:glide_to(...)`, `sprite:say("Hello!")`).

## Highlights

- **Scratch-like Lua API** — green-flag events, broadcasts, `wait`, `glide`, `say` bubbles,
  sprites and stages. If you can Scratch, you can LiteEngine.
- **Modular by design** — every engine component is a zip-compressed `.lemodule`
  (`engine-core`, `gfx2d`, `gfx3d`, `speech`, `rtmode`, `stream`, `pbr3d`, `monitors`, `audio`).
- **Variable & list monitors** — Scratch-style on-screen watchers:
  `monitors.show("score")` re-reads the Lua global every frame; `monitors.show_list(...)`
  renders a scrolling indexed list card. Both render paths, pixel-identical.
- **3D model costumes** — load `.obj` (via tobj) and `.glb`/`.gltf` (embedded buffers)
  as a 3D sprite's costume: `ship:set_model("assets/ship.obj")` renders a real shaded
  mesh — z-buffered rasterizer on the legacy path, the PBR pipeline on wgpu.
- **MP3 / WAV / OGG audio** — `audio.play("coin.ogg", {volume=0.8})` on a dedicated
  playback thread (rodio + symphonia). Looping music, per-sound volume, pause/resume,
  master volume; degrades to a logged no-op when the machine has no sound device.
- **Raytracing you can toggle** — `rt.enable{}` flips to a hybrid software path tracer
  (raster base + path-traced GI/reflections, temporal accumulation, edge-aware denoise,
  sharpened upscale) designed for a 2010 MacBook Air; modern Apple Silicon / Win10+ / 64-bit
  Linux machines use the `wgpu` PBR path instead (shadow maps, IBL-style ambience,
  screen-space RT effects).
- **Asset streaming** — textures *and* triangle meshes stream in and out under a byte
  budget (LRU eviction); nothing is force-loaded up front.
- **Portable** — pure-Rust software path renders anywhere (macOS 10.8+, Windows 7+,
  Linux, 32-bit and 64-bit); `minifb` powers the window; no GPU drivers required.
- **`liteengine` CLI** — `new`, `run`, `build --packed|--folder|--macos|--zip`,
  `module list|add|rm|pack|info|search`, `devices`, `doctor`, `lint`, `guide`.

## Layout

```
crates/liteengine-core    Lua API, event scheduler, module manager, streaming assets, audio thread
crates/liteengine-render  Software rasterizer, mesh rasterizer, monitors, speech bubbles, hybrid path tracer, wgpu PBR path
crates/liteengine-cli     The `liteengine` binary (project manager + game runner)
modules-src/              Sources for the built-in .lemodule files
examples/hello-world      The guided Hello World project
examples/rt-demo          Raytracing demo (toggle RT at runtime)
examples/showcase         Monitors + 3D model costumes + audio in one demo
docs/                     LiteEngine Developer Guide (PDF)
```

## Quick start

```bash
cargo build --release -p liteengine-cli
./target/release/liteengine new mygame
cd mygame && ../target/release/liteengine run
```

Headless / CI:

```bash
./target/release/liteengine run examples/hello-world --headless --frames 90 \
    --screenshot shot.png
```

Read `docs/LiteEngine_Developer_Guide.pdf` for the full manual.

## New in 1.1.0

### Monitors (`monitors` module)

```lua
score = 0
inventory = {"sword", "shield", "potion"}
monitors.show("score")                        -- orange value readout, auto-stacked
monitors.show_list("inventory", {rows = 5})   -- indexed list card with scroll
monitors.set_list_window("inventory", 1, 5)   -- scroll the window
monitors.set_style("score", "large")          -- big value-only readout
```

Monitors watch plain Lua globals — assignments update the display, no API calls needed.
Numbers print Scratch-style (`0.1+0.2` shows `0.3`).

### 3D model costumes (core `gfx3d`)

```lua
local ship = Sprite.new("ship", nil)
ship:set_model("assets/ship.obj")       -- .obj / .glb / .gltf (embedded buffers)
ship:set_pos3(0, 1, 0)
ship:set_size(80)                        -- uniform scale, percent
ship:set_rotation3d(0, engine.now() * 60, 15)
ship:tint(255, 140, 60)                  -- flat base color
```

Models stream through the same asset store as textures (65,535-vertex budget),
recompute smooth normals when missing, and render on both paths: flat-shaded
z-buffered triangles on the software path, full PBR + shadow maps on wgpu.
`pbr3d.add{mesh = "assets/teapot.obj", ...}` also accepts model paths.

### Audio (`audio` module)

```lua
local music = audio.play_loop("assets/theme.mp3")   -- MP3, WAV, OGG
local sfx    = audio.play("assets/coin.ogg", {volume = 0.8})
audio.set_volume(sfx, 0.4)
audio.set_master_volume(0.9)
audio.pause(music); audio.resume(music)
audio.stop_all()
```

Playback runs on its own thread; the Lua world just sends commands. On machines
without a sound device every call degrades to a logged no-op and
`audio.available()` returns `false`.
