# audio — MP3 / WAV / OGG Playback

Sound effects and music on a dedicated playback thread, decoded by
symphonia-backed rodio. The Lua world stays single-threaded: every call just
sends a message to the audio thread.

```lua
local music = audio.play_loop("assets/theme.mp3")
local blip  = audio.play("assets/blip.wav", { volume = 0.6 })
audio.play("assets/coin.ogg", { loop = false })

audio.set_volume(blip, 0.2)
audio.pause(music); audio.resume(music)
audio.set_master_volume(0.8)
audio.stop_all()
```

## API

| Function | Effect |
|---|---|
| `audio.play(path, opts?)` | Play `path` (`.mp3`, `.wav`, `.ogg`). Returns a sound id. Options: `volume` (0..4, default 1), `loop` (bool). |
| `audio.play_loop(path)` | Shortcut for looping playback (music). Returns an id. |
| `audio.stop(id)` / `audio.stop_all()` | Stop one sound / everything. |
| `audio.set_volume(id, v)` | Per-sound volume (multiplied by master). |
| `audio.set_master_volume(v)` | Rescale the whole mix. |
| `audio.pause(id)` / `audio.resume(id)` | Suspend / continue a sound. |
| `audio.available()` | `false` on machines without an output device. |

## Behaviour notes

* Assets stream from the same VFS as textures — zip packages (`.litepkg`,
  `.lemodule`) work with no extra code.
* Without a sound card (CI containers, servers) the engine logs one warning,
  `audio.available()` returns `false`, and every call degrades to a no-op —
  the game keeps running.
* Non-looping sounds are garbage-collected on the audio thread once they
  finish; looping sounds run until stopped or the engine quits.
