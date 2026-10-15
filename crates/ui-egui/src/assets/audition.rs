//! In-app audition of the Resources panel (ressources-v2, L2): replaces the external `afplay`.
//!
//! One voice for the whole app. The sound is decoded in a background thread through FilmCraft's
//! media layer, at the device rate, from the requested point onwards (a music file starts at 30 %
//! of its length); the device callback ([`crate::AudioOut`]) plays what is decoded, with 30 ms
//! fades in and out (no click). Seeking inside what is already decoded is instant; further on, the
//! decoder restarts there. The position feeds the play head drawn on the waveform.
//!
//! **Previews** (previews-cache): when the sound has a light preview on the local disk
//! ([`filmcraft_engine::sound_previews`]) covering the requested point, it is read instead of the
//! original (the library is on a slow NAS); elsewhere (outside a music excerpt, no preview yet) the
//! original is read as before. The lookup (which reads the file's date) runs in the decoder thread.
//!
//! The audition never plays over the timeline or the Source monitor: when they play, it stops
//! (and does not start). The output device is shared with them: the audition starts it with its
//! own callback and stops it when the sound ends.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use filmcraft_engine::Services;
use filmcraft_engine::media_pool::MediaPool;

/// Fade length in seconds.
const FADE: f32 = 0.030;
/// How far the decoder runs ahead of the play position.
const AHEAD_SECONDS: u64 = 20;

#[derive(Default)]
struct Voice {
    generation: u64,
    sr: u32,
    /// Absolute frame (device rate) of `buf[0]`.
    base: u64,
    /// Interleaved stereo.
    buf: Vec<f32>,
    /// Absolute play position (frames).
    pos: u64,
    /// Length of the file in frames, when known.
    total: Option<u64>,
    /// The decoder reached the end of the file.
    complete: bool,
    playing: bool,
    gain: f32,
    target: f32,
    error: Option<String>,
    /// What is being read: the light preview or the original file.
    from_preview: bool,
}

impl Voice {
    fn have(&self) -> u64 {
        self.base + (self.buf.len() / 2) as u64
    }
    fn fill(&mut self, out: &mut [f32], ch: usize) {
        let ch = ch.max(1);
        let step = 1.0 / (FADE * self.sr.max(1) as f32);
        for frame in out.chunks_mut(ch) {
            let (mut l, mut r) = (0.0f32, 0.0f32);
            if self.playing {
                if self.pos >= self.base && self.pos < self.have() {
                    let i = ((self.pos - self.base) * 2) as usize;
                    (l, r) = (self.buf[i], self.buf[i + 1]);
                    self.pos += 1;
                } else if self.complete && self.pos >= self.have() {
                    self.target = 0.0;
                    self.gain = 0.0;
                    self.playing = false;
                }
                // otherwise: waiting for the decoder (silence, position held)
                self.gain = if self.gain < self.target { (self.gain + step).min(self.target) } else { (self.gain - step).max(self.target) };
                if self.target == 0.0 && self.gain <= 0.0 {
                    self.playing = false;
                }
            }
            let (l, r) = (l * self.gain, r * self.gain);
            match frame.len() {
                1 => frame[0] = (l + r) * 0.5,
                _ => {
                    frame[0] = l;
                    frame[1] = r;
                    for s in frame.iter_mut().skip(2) {
                        *s = 0.0;
                    }
                }
            }
        }
    }
}

/// The audition player.
#[derive(Default)]
pub struct Audition {
    voice: Arc<Mutex<Voice>>,
    gens: Arc<AtomicU64>,
    /// The file being auditioned.
    pub path: Option<String>,
    /// The audition started the output device (and must stop it).
    owns_output: bool,
    media: Option<(Arc<MediaPool>, Arc<dyn Services>)>,
    /// Where the light previews are (None = always the original).
    previews: Option<Arc<filmcraft_engine::sound_previews::PreviewStore>>,
}

/// What the panel needs each frame.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AuditionState {
    pub path: Option<String>,
    pub playing: bool,
    /// Seconds.
    pub position: f64,
    /// Fraction of the file (when its length is known).
    pub progress: Option<f32>,
    pub error: Option<String>,
    /// The light preview is being read (else the original).
    pub from_preview: bool,
}

impl Audition {
    fn lock(&self) -> std::sync::MutexGuard<'_, Voice> {
        self.voice.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn state(&self) -> AuditionState {
        let v = self.lock();
        let position = if v.sr > 0 { v.pos as f64 / v.sr as f64 } else { 0.0 };
        AuditionState {
            path: self.path.clone(),
            playing: v.playing,
            position,
            progress: v.total.filter(|t| *t > 0).map(|t| (v.pos as f64 / t as f64).clamp(0.0, 1.0) as f32),
            error: v.error.clone(),
            from_preview: v.from_preview,
        }
    }

    /// Play `path` from `frac` (0-1) of its length on `out`. Media layer from the session.
    pub fn play(&mut self, out: &mut dyn crate::AudioOut, media: Arc<MediaPool>, services: Arc<dyn Services>, path: &str, frac: f32) -> Result<(), String> {
        self.play_with(out, media, services, None, path, frac)
    }

    /// [`play`](Self::play), reading the light preview of `path` from `previews` when there is one.
    pub fn play_with(
        &mut self,
        out: &mut dyn crate::AudioOut,
        media: Arc<MediaPool>,
        services: Arc<dyn Services>,
        previews: Option<Arc<filmcraft_engine::sound_previews::PreviewStore>>,
        path: &str,
        frac: f32,
    ) -> Result<(), String> {
        self.previews = previews;
        let sr = out.sample_rate();
        if sr == 0 {
            return Err("no audio output".into());
        }
        if self.path.as_deref() == Some(path) && self.lock().sr == sr && self.owns_output {
            self.seek_frac(frac);
            let mut v = self.lock();
            v.playing = true;
            v.target = 1.0;
            return Ok(());
        }
        self.media = Some((media, services));
        self.path = Some(path.to_string());
        {
            let mut v = self.lock();
            *v = Voice { sr, playing: true, target: 1.0, generation: v.generation, ..Default::default() };
        }
        self.start_decoder(path.to_string(), None, frac);
        if !self.owns_output {
            let voice = self.voice.clone();
            out.stop();
            out.start(Box::new(move |buf: &mut [f32], ch: usize| {
                voice.lock().unwrap_or_else(|e| e.into_inner()).fill(buf, ch);
            }))?;
            self.owns_output = true;
        }
        Ok(())
    }

    /// (Re)start decoding `path` at absolute frame `at` (or at `frac` of its length).
    fn start_decoder(&mut self, path: String, at: Option<u64>, frac: f32) {
        let Some((media, services)) = self.media.clone() else { return };
        let previews = self.previews.clone();
        let generation = self.gens.fetch_add(1, Ordering::SeqCst) + 1;
        let voice = self.voice.clone();
        {
            let mut v = voice.lock().unwrap_or_else(|e| e.into_inner());
            v.generation = generation;
            v.buf.clear();
            v.complete = false;
            v.error = None;
            if let Some(a) = at {
                v.base = a;
                v.pos = a;
            }
        }
        let _ = std::thread::Builder::new().name("resources-audition".into()).spawn(move || {
            let sr = voice.lock().unwrap_or_else(|e| e.into_inner()).sr;
            let fail = |e: String| {
                let mut v = voice.lock().unwrap_or_else(|e| e.into_inner());
                if v.generation == generation {
                    v.error = Some(e);
                    v.playing = false;
                }
            };
            // the light preview when it covers the starting point: frames of the original
            // [offset, end) are read from the preview at (frame - offset)
            let preview = previews.as_ref().and_then(|p| p.get(&path)).filter(|(_, r)| {
                let start_s = match at {
                    Some(a) => a as f64 / sr as f64,
                    None => frac.clamp(0.0, 0.99) as f64 * r.duration,
                };
                start_s >= r.start - 1e-3 && start_s < r.start + r.len - 0.05
            });
            let opened = match &preview {
                Some((file, r)) => media.open_file(&file.to_string_lossy(), &*services).map(|s| {
                    (s, (r.duration * sr as f64).round() as u64, (r.start * sr as f64).round() as u64, ((r.start + r.len) * sr as f64).round() as u64)
                }),
                None => media.open_file(&path, &*services).map(|s| {
                    let t = (s.info().duration.seconds() * sr as f64).round() as u64;
                    (s, t, 0, t)
                }),
            };
            let (src, total, offset, end) = match opened {
                Ok(o) => o,
                Err(e) => return fail(e.to_string()),
            };
            let mut cursor = {
                let mut v = voice.lock().unwrap_or_else(|e| e.into_inner());
                if v.generation != generation {
                    return;
                }
                v.total = Some(total);
                v.from_preview = preview.is_some();
                if at.is_none() {
                    let start = ((frac.clamp(0.0, 0.99) as f64) * total as f64) as u64;
                    v.base = start;
                    v.pos = start;
                }
                v.base
            };
            let chunk = (sr as usize / 4).max(1024);
            while cursor < end {
                // stay a little ahead of the play position
                loop {
                    let v = voice.lock().unwrap_or_else(|e| e.into_inner());
                    if v.generation != generation {
                        return;
                    }
                    if v.have() < v.pos + AHEAD_SECONDS * sr as u64 {
                        break;
                    }
                    drop(v);
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                let n = chunk.min((end - cursor) as usize);
                let Ok(b) = src.audio(cursor.saturating_sub(offset) as i64, n, sr) else { break };
                let l = b.channels.first().cloned().unwrap_or_default();
                let r = b.channels.get(1).cloned().unwrap_or_else(|| l.clone());
                let frames = l.len().min(r.len());
                let mut v = voice.lock().unwrap_or_else(|e| e.into_inner());
                if v.generation != generation {
                    return;
                }
                v.buf.reserve(frames * 2);
                for i in 0..frames {
                    v.buf.push(l[i]);
                    v.buf.push(r[i]);
                }
                cursor += frames.max(1) as u64;
            }
            let mut v = voice.lock().unwrap_or_else(|e| e.into_inner());
            if v.generation == generation {
                v.complete = true;
            }
        });
    }

    /// Jump to `frac` of the file (scrub): instant inside what is decoded, else decoded from there.
    pub fn seek_frac(&mut self, frac: f32) {
        let (target, inside) = {
            let v = self.lock();
            let Some(total) = v.total else { return };
            let t = ((frac.clamp(0.0, 1.0) as f64) * total as f64) as u64;
            (t, t >= v.base && t < v.have())
        };
        if inside {
            self.lock().pos = target;
        } else if let Some(p) = self.path.clone() {
            self.start_decoder(p, Some(target), 0.0);
        }
    }

    /// Fade out and stop (at once when no device callback runs).
    pub fn stop(&mut self) {
        let owns = self.owns_output;
        let mut v = self.lock();
        v.target = 0.0;
        if !owns {
            v.playing = false;
        }
    }

    pub fn toggle_pause(&mut self) {
        let mut v = self.lock();
        if v.playing && v.target > 0.0 {
            v.target = 0.0;
        } else if v.sr > 0 {
            v.playing = true;
            v.target = 1.0;
        }
    }

    /// Each frame: release the output when the sound ended; give way to the timeline / Source
    /// monitor when they play (they start the device with their own callback).
    pub fn tick(&mut self, out: Option<&mut Box<dyn crate::AudioOut>>, other_playing: bool) {
        if other_playing {
            if self.owns_output || self.path.is_some() {
                self.owns_output = false; // the device now belongs to the timeline
                self.path = None;
                let mut v = self.lock();
                v.playing = false;
                v.generation = v.generation.wrapping_add(1);
            }
            return;
        }
        let finished = !self.lock().playing;
        if finished && self.owns_output {
            if let Some(o) = out {
                o.stop();
            }
            self.owns_output = false;
            if self.lock().complete || self.lock().error.is_some() {
                self.path = None;
            }
        }
    }

    pub fn owns_output(&self) -> bool {
        self.owns_output
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_voice_fades_in_plays_what_is_decoded_and_stops_at_the_end() {
        let mut v = Voice { sr: 1000, playing: true, target: 1.0, complete: true, ..Default::default() };
        v.buf = (0..200).flat_map(|_| [1.0f32, 1.0]).collect();
        let mut out = vec![0.0f32; 2 * 300];
        v.fill(&mut out, 2);
        // 30 ms fade in at 1 kHz = 30 frames
        assert!(out[0] < 0.1 && (out[2 * 40] - 1.0).abs() < 1e-6, "{} {}", out[0], out[80]);
        assert_eq!(v.pos, 200);
        assert!(!v.playing, "stopped at the end");
        assert!(out[2 * 250].abs() < 1e-6);
        // mono device
        let mut v = Voice { sr: 1000, playing: true, target: 1.0, gain: 1.0, ..Default::default() };
        v.buf = vec![0.5, 0.25];
        let mut m = vec![0.0f32; 1];
        v.fill(&mut m, 1);
        assert!((m[0] - 0.375).abs() < 1e-6);
    }

    #[test]
    fn waiting_for_the_decoder_holds_the_position_and_stop_fades_out() {
        let mut v = Voice { sr: 1000, playing: true, target: 1.0, gain: 1.0, base: 100, pos: 50, ..Default::default() };
        let mut out = vec![0.0f32; 20];
        v.fill(&mut out, 2);
        assert_eq!(v.pos, 50, "nothing decoded at 50 yet");
        v.buf = vec![1.0; 2000];
        v.base = 50;
        v.target = 0.0;
        let mut out = vec![0.0f32; 2 * 100];
        v.fill(&mut out, 2);
        assert!(!v.playing && out[2 * 99] == 0.0 && out[0] > 0.9);
    }
}
