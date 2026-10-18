-- Hello LiteEngine — the guided first project.
-- Full walkthrough: docs/LiteEngine_Developer_Guide.pdf, Chapter "Quick Start".

-- when green flag clicked
engine.on_start(function()
    engine.set_title("Hello LiteEngine")
    engine.set_background("assets/sky.png")

    -- A sprite is 10 blocks of Scratch in one line:
    local cat = Sprite.new("cat", "assets/cat.png")
    cat:set_size(150)
    cat:go_to(-180, -40)
    cat:say("Hello, World!", 3)

    -- Sequencing uses Scratch-style waits:
    engine.wait(3.5)
    cat:glide_to(180, 40, 2)            -- smooth native glide
    cat:say("I glided here with one block!", 3)
    engine.wait(3)

    -- forever loops respawn every frame, exactly like Scratch:
    forever(function(dt)
        cat:turn(120 * dt)              -- 120 degrees per second
    end)

    -- Streaming is automatic, but you can observe it:
    forever(function()
        local stats = stream.stats()
        -- engine.log("assets loaded:", stats.loaded)
    end)
end)

-- when space key pressed
engine.on_key("space", function()
    engine.broadcast("jump")
end)

-- when I receive jump
engine.on_broadcast("jump", function()
    local cat = Sprite.new("bouncer", "assets/ball.png")
    cat:go_to(0, -120)
    cat:glide(0, 160, 0.45)
    engine.wait(0.5)
    cat:glide(0, -160, 0.45)
    engine.wait(0.5)
    cat:destroy()
end)

-- when this sprite clicked (stage click)
engine.on_click(function(x, y)
    engine.log("stage clicked at", x, y)
end)
