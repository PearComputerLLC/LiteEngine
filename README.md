# LiteEngine

**A lightweight, modular, Lua-scripted game engine — as easy as Scratch, tuned for AAA looks on very old hardware.**

LiteEngine is written in Rust, embeds Lua 5.4, and ships as a tiny set of crates plus a
handful of zip-compressed `.lemodule` components. Games are scripted with a documented,
Scratch-like event API (`engine.on_start`, `sprite:glide_to(...)`, `sprite:say("Hello!")`).

## Highlights

- **Scratch-like Lua API** — green-flag events, broadcasts, `wait`, `glide`, `say` bubbles,
  sprites and stages. If you can Scratch, you can LiteEngine.
- **Modular by design** — every engine component is a zip-compressed `.lemodule`
  (`engine-core`, `gfx2d`, `gfx3d`, `speech`, `rtmode`, `stream`, `pbr3d`).
- **Raytracing you can toggle** — `rt.enable{}` flips to a hybrid software path tracer
  (raster base + path-traced GI/reflections, temporal accumulation, edge-aware denoise,
  sharpened upscale) designed for a 2010 MacBook Air; modern Apple Silicon / Win10+ / 64-bit
  Linux machines use the `wgpu` PBR path instead (shadow maps, IBL-style ambience,
  screen-space RT effects).
- **Asset streaming** — textures stream in and out under a byte budget (LRU eviction);
  nothing is force-loaded up front.
- **Portable** — pure-Rust software path renders anywhere (macOS 10.8+, Windows 7+,
  Linux, 32-bit and 64-bit); `minifb` powers the window; no GPU drivers required.
- **`liteengine` CLI** — `new`, `run`, `build --packed|--folder|--macos|--zip`,
  `module list|add|rm|pack|info|search`, `devices`, `doctor`, `lint`, `guide`.

## Layout

```
crates/liteengine-core    Lua API, event scheduler, module manager, streaming assets
crates/liteengine-render  Software rasterizer, speech bubbles, hybrid path tracer, wgpu PBR path
crates/liteengine-cli     The `liteengine` binary (project manager + game runner)
modules-src/              Sources for the built-in .lemodule files
examples/hello-world      The guided Hello World project
examples/rt-demo          Raytracing demo (toggle RT at runtime)
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
