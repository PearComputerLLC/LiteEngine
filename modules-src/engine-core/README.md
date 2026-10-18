# engine-core

The heartbeat of every LiteEngine game. Provides the `engine` global's event
model, stage control, timing and logging.

## Lua surface (selected)
| Function | Scratch equivalent |
|---|---|
| `engine.on_start(fn)` | when green flag clicked |
| `engine.on_key("space", fn)` | when space key pressed |
| `engine.on_key_held("right", fn)` | key held (per frame) |
| `engine.on_click(fn(x,y,btn))` | when stage clicked |
| `engine.on_broadcast("msg", fn)` | when I receive msg |
| `engine.on_tick(fn(dt))` / `forever(fn)` | forever |
| `engine.broadcast("msg")` | broadcast msg |
| `engine.wait(sec)` | wait N seconds (inside handlers) |
| `engine.set_background("#87ceeb")` | stage backdrop |
| `engine.log(...)`, `engine.quit()`, `engine.rand(a,b)` | utility |
