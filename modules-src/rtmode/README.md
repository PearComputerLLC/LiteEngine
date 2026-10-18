# rtmode

Toggleable raytracing with the most optimized method available per device:
- Legacy path (2010 MacBook Air, pre-macOS-11 / Win7 / 32-bit): hybrid software
  path tracing -- raster base + path-traced GI/reflection buffer composite,
  temporal accumulation, edge-aware denoise, sharpened upscale.
- Modern path (wgpu): screen-space RT effects layered on the PBR pipeline.

## Lua surface
`rt.enable{quality="low|medium|high", scale=0.05..1.0, bounces=1..4, denoise=bool}`,
`rt.disable()`, `rt.enabled()`, `rt.status()`, `rt.set_sun{dir,color,intensity}`,
`rt.set_env{sky_top, sky_horizon, ground}`, `rt.add_sphere{name,pos,r,color,metallic,roughness,emissive}`,
`rt.add_plane{name,y,color,checkered,metallic,roughness}`, `rt.remove(name)`, `rt.clear()`.
