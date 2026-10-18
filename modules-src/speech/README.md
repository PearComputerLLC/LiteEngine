# speech

Scratch speech bubbles, rendered as billboards anchored to the TOP-RIGHT of
the sprite footprint (2D) or projected above the 3D sprite (3D).

## Lua surface
| Function | Notes |
|---|---|
| `sprite:say(msg)` | show until replaced/cleared |
| `sprite:say(msg, secs)` | auto-hide after secs |
| `sprite:think(msg[, secs])` | thought-bubble variant |
| `sprite:stop_saying()` | clear |
