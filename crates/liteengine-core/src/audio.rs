//! Audio playback — MP3, WAV and OGG (Vorbis) via a dedicated thread.
//!
//! Decoding is handled by `rodio`'s symphonia-backed [`Decoder`], which
//! auto-detects MP3, WAV and OGG/Vorbis from the raw bytes. All control
//! objects live on an audio thread so the single-threaded `Rc<RefCell>`
//! Lua world never touches them: the engine only sends [`AudioMsg`]s over
//! a channel.
//!
//! Headless machines (CI containers, servers without a sound card) fail the
//! device probe at startup: the handle reports `available == false` and
//! every API degrades to a logged no-op, exactly like the wgpu path without
//! a GPU adapter. The game keeps running.

use std::collections::HashMap;
use std::sync::mpsc::{channel, Receiver, Sender};
use rodio::Source;

/// One playback request / command. Bytes ride along so the audio thread
/// never needs filesystem access (assets may live inside a zip package).
#[derive(Debug)]
pub enum AudioMsg {
    Play {
        id: u64,
        bytes: std::sync::Arc<Vec<u8>>,
        volume: f32,
        looped: bool,
    },
    Stop {
        id: u64,
    },
    StopAll,
    SetVolume {
        id: u64,
        volume: f32,
    },
    SetMaster {
        volume: f32,
    },
    Pause {
        id: u64,
    },
    Resume {
        id: u64,
    },
    Shutdown,
}

/// Game-thread handle. Cheap to clone; the id counter lives on the sender
/// side so `audio.play()` can hand back ids without a round-trip.
pub struct AudioHandle {
    tx: Sender<AudioMsg>,
    /// False when no output device could be opened (headless CI, etc).
    pub available: bool,
    next_id: u64,
}

impl Clone for AudioHandle {
    fn clone(&self) -> Self {
        AudioHandle {
            tx: self.tx.clone(),
            available: self.available,
            next_id: self.next_id,
        }
    }
}

impl AudioHandle {
    /// Probe for an output device and spawn the audio thread.
    /// Never fails: without a device the handle becomes a no-op stub.
    pub fn open() -> AudioHandle {
        let (tx, rx) = channel::<AudioMsg>();
        match rodio::stream::DeviceSinkBuilder::open_default_sink() {
            Ok(sink) => {
                // Keep the MixerDeviceSink alive for the thread's lifetime;
                // dropping it closes the device.
                std::thread::Builder::new()
                    .name("liteengine-audio".into())
                    .spawn(move || audio_thread(rx, sink))
                    .expect("spawn audio thread");
                AudioHandle {
                    tx,
                    available: true,
                    next_id: 1,
                }
            }
            Err(e) => {
                // Drain silently so senders never block.
                std::thread::Builder::new()
                    .name("liteengine-audio-stub".into())
                    .spawn(move || {
                        while rx.recv().is_ok() {}
                    })
                    .ok();
                eprintln!("[liteengine] audio: no output device ({e}); sound disabled");
                AudioHandle {
                    tx,
                    available: false,
                    next_id: 1,
                }
            }
        }
    }

    pub fn send(&mut self, msg: AudioMsg) {
        let _ = self.tx.send(msg);
    }

    /// Allocate the next sound id.
    pub fn alloc_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }
}

fn audio_thread(rx: Receiver<AudioMsg>, sink: rodio::stream::MixerDeviceSink) {
    let mixer = sink.mixer().clone();
    // Player + the per-sound base volume, so SetMaster can rescale the mix.
    let mut players: HashMap<u64, (rodio::Player, f32)> = HashMap::new();
    let mut master: f32 = 1.0;

    while let Ok(msg) = rx.recv() {
        match msg {
            AudioMsg::Play {
                id,
                bytes,
                volume,
                looped,
            } => {
                let cursor = std::io::Cursor::new((*bytes).clone());
                let source = match rodio::Decoder::new(cursor) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("[liteengine] audio: decode failed: {e}");
                        continue;
                    }
                };
                let player = rodio::Player::connect_new(&mixer);
                player.set_volume(volume.clamp(0.0, 4.0) * master);
                if looped {
                    player.append(source.repeat_infinite());
                } else {
                    player.append(source);
                }
                players.insert(id, (player, volume.clamp(0.0, 4.0)));
            }
            AudioMsg::Stop { id } => {
                if let Some((p, _)) = players.remove(&id) {
                    p.stop();
                }
            }
            AudioMsg::StopAll => {
                for (_, (p, _)) in players.drain() {
                    p.stop();
                }
            }
            AudioMsg::SetVolume { id, volume } => {
                if let Some((p, base)) = players.get_mut(&id) {
                    *base = volume.clamp(0.0, 4.0);
                    p.set_volume(*base * master);
                }
            }
            AudioMsg::SetMaster { volume } => {
                master = volume.clamp(0.0, 4.0);
                for (_, (p, base)) in players.iter() {
                    p.set_volume(*base * master);
                }
            }
            AudioMsg::Pause { id } => {
                if let Some((p, _)) = players.get(&id) {
                    p.pause();
                }
            }
            AudioMsg::Resume { id } => {
                if let Some((p, _)) = players.get(&id) {
                    p.play();
                }
            }
            AudioMsg::Shutdown => break,
        }
        // Garbage-collect finished players lazily (looping never empties).
        players.retain(|_, (p, _)| !p.empty());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn handle_opens_without_panic() {
        // On a machine with no audio device this must yield available=false
        // rather than panicking; with a device it yields true. Both fine.
        let mut h = AudioHandle::open();
        let id = h.alloc_id();
        assert!(id >= 1);
        // Sending to a stub or live thread must never block/panic.
        h.send(AudioMsg::StopAll);
        h.send(AudioMsg::Shutdown);
        assert!(h.available == true || h.available == false);
    }
}
