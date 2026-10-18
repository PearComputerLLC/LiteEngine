# pbr3d

PBR mesh rendering on the modern wgpu path (Apple Silicon macOS 11+, Windows
8/10/11, 64-bit Linux). Metallic-roughness materials, sun shadow maps,
hemispheric IBL-style ambience, ACES tonemapping. On the legacy software path
this module soft-disables (native feature `pbr3d.renderer` unavailable).

## Lua surface
`pbr3d.set_camera{pos,look_at,fov}`, `pbr3d.set_sun{dir,color,intensity}`,
`pbr3d.add{name,mesh="plane|cube|sphere|file.obj",pos,rot,scale,color,metallic,roughness,texture}`,
`pbr3d.remove(name)`, `pbr3d.clear()`.
