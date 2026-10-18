-- Hello LiteEngine!
-- This is the default project script created by `liteengine new`.
-- Everything below is Scratch thinking in Lua.

-- When green flag clicked:
engine.on_start(function()
    engine.set_background("#87ceeb")

    local cat = Sprite.new("cat", "assets/cat.png")
    cat:go_to(-140, 0)
    cat:say("Hello, World!", 3)

    engine.wait(3.5)
    cat:glide_to(140, 0, 2)
    cat:say("I moved with one block!", 3)

    forever(function(dt)
        cat:turn(1)
    end)
end)

-- When space key pressed:
engine.on_key("space", function()
    engine.broadcast("jump")
end)

engine.on_broadcast("jump", function()
    engine.log("The stage jumped!")
end)
