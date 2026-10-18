# monitors — Variable & List Monitors (Scratch-style)

On-screen watchers for Lua globals, just like Scratch's variable and list
reporters. A monitor re-reads the watched global **every frame**, so plain
assignments update the display — no extra API calls:

```lua
score = 0
monitors.show("score")                 -- variable readout (auto-positioned)
monitors.show("score", { x = -460, y = 240, large = false })

inventory = {"sword", "shield", "potion"}
monitors.show_list("inventory")        -- scrolling list watcher
monitors.show_list("inventory", { x = -460, y = 80, rows = 5 })
monitors.set_list_window("inventory", 1, 3)  -- scroll: offset, rows
```

## API

| Function | Effect |
|---|---|
| `monitors.show(name, opts?)` | Watch global `name` as a variable readout. `opts`: `x`, `y` (stage coords, top-left of the box, centre origin / +y up), `large` (value-only big style). |
| `monitors.show_list(name, opts?)` | Watch a table global as a list watcher. Extra opt: `rows` (visible rows, 1..24, default 8). |
| `monitors.hide(name)` | Hide (keeps position/style). |
| `monitors.remove(name)` | Remove the monitor entirely. |
| `monitors.set_position(name, x, y)` | Move the box. |
| `monitors.set_style(name, "normal"\|"large")` | Switch readout style. |
| `monitors.set_list_window(name, offset, rows?)` | Scroll a list window. |
| `monitors.hide_all()` | Hide every monitor. |

## Display rules (Scratch-like)

* numbers: integers without a point, floats rounded to 4 trimmed decimals
  (`0.1+0.2` shows `0.3`, never `0.30000000000000004`),
* booleans: `true` / `false`, strings verbatim, `nil` as empty,
* lists show an indexed window plus the total count footer.

Requires the `monitors` module in `liteengine.toml`
(`enabled = [..., "monitors"]`). Works on both the legacy software path and
the modern wgpu path (drawn on the CPU overlay layer, pixel-identical).
