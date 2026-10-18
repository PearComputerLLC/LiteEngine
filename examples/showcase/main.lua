-- LiteEngine 1.1 showcase: variable/list monitors, 3D model costumes,
-- and MP3/WAV/OGG audio — everything new in one demo.

engine.set_title("LiteEngine Showcase — monitors + 3D models + audio")

---------------------------------------------------------------- score + monitors
score = 0
inventory = { "sword", "shield", "potion", "torch", "map", "gem" }

monitors.show("score", { x = -452, y = 250 })
monitors.show_list("inventory", { x = -452, y = 160, rows = 6 })

--------------------------------------------------------------- 3D model costumes
-- A torus (OBJ) spinning in the 3D world:
local donut = Sprite.new("donut", nil)
donut:set_model("assets/torus.obj")
donut:set_pos3(-1.6, 1.1, 0)
donut:set_size(60)
donut:tint(255, 140, 60)

-- A GLB crate orbiting it:
local crate = Sprite.new("crate", nil)
crate:set_model("assets/crate.glb")
crate:set_pos3(1.6, 0.9, 0)
crate:set_size(50)
crate:tint(120, 220, 160)

-- An ordinary 2D sprite still works exactly like before:
local cat = Sprite.new("cat", "assets/cat.png")
cat:go_to(-330, -170)
cat:set_size(120)

-- Camera: pull back a little to frame the models.
engine.set_camera({ pos = { x = 0, y = 1.6, z = 6.5 }, look_at = { x = 0, y = 0.8, z = 0 }, fov = 55 })

-------------------------------------------------------------------------- audio
local theme_id = nil
local function start_theme()
    if theme_id == nil then
        theme_id = audio.play_loop("assets/theme.mp3")
        engine.log("music id =", theme_id, "audio.available =", audio.available())
    end
end

----------------------------------------------------------------------- handlers
engine.on_start(function()
    cat:say("Welcome! Click the cat for sounds.", 3)
    start_theme()
end)

engine.on_tick(function(dt)
    -- Spin the models (rotation3d takes Euler degrees).
    donut:set_rotation3d(0, engine.now() * 60, 25)
    crate:set_rotation3d(engine.now() * 40, engine.now() * 70, 0)
    crate:set_pos3(1.6, 0.9 + 0.25 * math.sin(engine.now() * 2), 0)

    -- Live score ticker so the monitor has something to show.
    score = math.floor(engine.now())
end)

engine.on_key("up", function()
    table.insert(inventory, "coin #" .. #inventory)
end)

engine.on_key("down", function()
    table.remove(inventory, #inventory)
end)

engine.on_key("m", function()
    -- Scroll the list window down by one row.
    monitors.set_list_window("inventory", 1, 6)
end)

engine.on_key("l", function()
    monitors.set_style("score", "large")
end)

engine.on_click(function(x, y)
    audio.play("assets/coin.ogg", { volume = 0.8 })
    score = score + 10
    cat:say("+10", 0.7)
end)

engine.on_key("escape", function()
    audio.stop_all()
    engine.quit()
end)
