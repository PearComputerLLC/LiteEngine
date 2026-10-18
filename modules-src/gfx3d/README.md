# gfx3d

3D support. Sprites with a 2D costume loaded into 3D become camera-facing
billboards textured on BOTH sides (`:set_billboard(true)` default). The
perspective camera is shared with rtmode/pbr3d.

## Lua surface
`:in3d(true)`, `:set_pos3(x,y,z)`, `:set_billboard(bool)`; camera via
`rt.set_camera` / `pbr3d.set_camera` (`pos`, `look_at`, `fov`).
