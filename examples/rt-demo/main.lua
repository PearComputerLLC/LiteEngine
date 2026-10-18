-- LiteEngine RT Demo — toggling raytracing at runtime.
-- On legacy devices this drives the hybrid software path tracer; on modern
-- (wgpu) devices it drives the screen-space RT effects over the PBR path.
-- Press R to flip raytracing on/off and watch the same scene render both ways.

engine.on_start(function()
    engine.set_title("LiteEngine RT Demo")
    engine.set_background("#1b2a41")

    -- Ray-traced world: checkered floor, glossy + emissive spheres.
    rt.add_plane{ name="floor", y=0, color="#8892a6", checkered=true, metallic=0.25, roughness=0.35 }
    rt.add_sphere{ name="chrome", pos={x=-1.6,y=1.0,z=0}, r=1.0, color="#dfe6f0", metallic=0.95, roughness=0.06 }
    rt.add_sphere{ name="gold",   pos={x=0.2,y=0.8,z=1.4}, r=0.8, color="#ffcf6e", metallic=0.9, roughness=0.18 }
    rt.add_sphere{ name="lamp",   pos={x=1.9,y=1.2,z=-0.8}, r=0.6, color="#ff9a5a", emissive=2.2 }
    rt.set_sun{ dir={x=-0.45,y=-0.85,z=-0.3}, color="#fff6e6", intensity=1.25 }
    rt.set_env{ sky_top="#2a5db0", sky_horizon="#bcd8f5", ground="#33302c" }

    -- Enable raytracing (the config already did, but this is the runtime API):
    rt.enable{ quality="high", denoise=true }

    -- A 3D billboard sprite: a 2D texture displayed inside the 3D world,
    -- camera-facing and textured on BOTH sides.
    local ghost = Sprite.new("ghost", "assets/ball.png")
    ghost:in3d(true)
    ghost:set_pos3(0.0, 1.35, -2.0)
    ghost:set_size(60)
    ghost:say("I am a 3D billboard!\nBoth sides textured.", 6)

    -- Slow camera orbit.
    engine.forever(function(dt)
        local t = engine.now()
        local a = t * 0.25
        local r = 5.2
        engine.set_camera{
            pos = { x = math.sin(a) * r, y = 2.2, z = math.cos(a) * r },
            look_at = { x = 0, y = 0.9, z = 0 },
            fov = 55,
        }
        if t % 5 < dt then
            engine.log("rt status:", rt.status())
        end
    end)
end)

engine.on_key("r", function()
    if rt.enabled() then
        rt.disable()
        engine.log("raytracing OFF (regular 2D/3D sprite mode)")
    else
        rt.enable{ quality="medium" }
        engine.log("raytracing ON")
    end
end)
