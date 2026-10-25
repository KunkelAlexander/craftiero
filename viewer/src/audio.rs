// Sound. Everything here is synthesised at run time from oscillators, noise and
// envelopes: there are no audio files. The sim is pure and knows nothing about
// it, so `Audio` listens to the same `Event` stream the effects use and never
// writes back into the rules.
//
// The shape of a gunshot is three layers (see `docs/WEAPON_FEEL.md`):
//
//   1. transient  a 5-30 ms click or crack; this is what makes it read as a shot
//   2. body       the weapon's character: a filtered noise burst, a pitch sweep,
//                 a metallic ring
//   3. tail       a low rumble or a ringing ear, 0.1-3 s depending on the weapon
//
// A voice is mixed on the audio thread and panned/filtered by where it happened
// relative to the listener. A WebAudio backend can later implement `Backend`
// and feed it the same `Sfx`; no game code changes.

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use glam::Vec3;
use sim::weapons::{
    BLASTER, BOMB, BOUNCER, BUILD, CLUSTER, NOVA, RAILGUN, ROCKET, SWARM, TELEPORT, TRACTOR, WHIP,
};
use sim::Event;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// Sound travels at about this speed through air. The delay is tiny at fighting
/// range, but it is what makes a distant Nova arrive after you see it.
const SOUND_SPEED: f32 = 343.0;
/// Sounds further than this are inaudible. The arena is 128 m across.
const MAX_AUDIBLE: f32 = 90.0;
/// Overall output level. The mix is soft-clipped, so layers can sum above 1.0
/// without turning into digital distortion.
const MASTER: f32 = 0.55;

#[derive(Clone, Copy, PartialEq)]
enum Wave {
    Sine,
    /// White noise. `f0`/`f1` are ignored; the low-pass still applies.
    Noise,
}

/// One ingredient of a voice. A gunshot is two or three of these.
struct Layer {
    wave: Wave,
    gain: f32,
    /// Seconds to reach full volume. A click uses a fraction of a millisecond.
    attack: f32,
    /// Exponential decay rate, 1/s. Bigger is shorter.
    decay: f32,
    /// Hard stop at this age, seconds.
    life: f32,
    /// Start and end frequency; interpolated geometrically over `life`. For
    /// noise it is unused, and for a hiss the filter cutoff is the pitch.
    f0: f32,
    f1: f32,
    /// One-pole low-pass cutoff in Hz.
    cutoff: f32,
    // Runtime state.
    age: f32,
    phase: f32,
    lp: f32,
    seed: u32,
}

impl Layer {
    fn new(wave: Wave, gain: f32, attack: f32, decay: f32, life: f32) -> Layer {
        Layer {
            wave,
            gain,
            attack,
            decay,
            life,
            f0: 0.0,
            f1: 0.0,
            cutoff: 18_000.0,
            age: 0.0,
            phase: 0.0,
            lp: 0.0,
            seed: 1,
        }
    }

    fn sine(gain: f32, attack: f32, decay: f32, life: f32, f0: f32, f1: f32) -> Layer {
        Layer { f0, f1, ..Layer::new(Wave::Sine, gain, attack, decay, life) }
    }

    fn noise(gain: f32, attack: f32, decay: f32, life: f32, cutoff: f32) -> Layer {
        Layer { cutoff, ..Layer::new(Wave::Noise, gain, attack, decay, life) }
    }

    fn next(&mut self, dt: f32, rate: f32, voice_gain: f32, voice_cutoff: f32) -> f32 {
        if self.age >= self.life {
            return 0.0;
        }
        self.age += dt;
        let t = (self.age / self.life).clamp(0.0, 1.0);
        let attack = if self.attack > 0.0 { (self.age / self.attack).min(1.0) } else { 1.0 };
        let env = attack * (-self.decay * self.age).exp();
        if env <= 1.0e-6 {
            return 0.0;
        }
        let raw = match self.wave {
            Wave::Noise => {
                // xorshift32, deterministic and cheap. Audio randomness need not
                // match between runs, but it must not allocate.
                self.seed ^= self.seed << 13;
                self.seed ^= self.seed >> 17;
                self.seed ^= self.seed << 5;
                (self.seed as f32 / u32::MAX as f32) * 2.0 - 1.0
            }
            Wave::Sine => {
                let freq = if self.f0 > 0.0 && self.f1 > 0.0 {
                    self.f0 * (self.f1 / self.f0).powf(t)
                } else {
                    self.f0
                };
                self.phase = (self.phase + std::f32::consts::TAU * freq * dt).rem_euclid(std::f32::consts::TAU);
                self.phase.sin()
            }
        };
        // One-pole low-pass. `voice_cutoff` is the distance muffle; fold it in
        // so a far blast loses its crack before it loses its body.
        let cutoff = (self.cutoff * voice_cutoff).clamp(20.0, rate * 0.45);
        let alpha = 1.0 - (-std::f32::consts::TAU * cutoff / rate).exp();
        self.lp += (raw - self.lp) * alpha;
        self.lp * env * self.gain * voice_gain
    }
}

struct Voice {
    layers: Vec<Layer>,
    age: f32,
    life: f32,
    gain: f32,
    pan: f32,
    /// Distance muffle, 0..1 (1 = no muffle).
    cutoff: f32,
    /// Seconds before the voice starts. Distance delay is applied here.
    delay: f32,
}

impl Voice {
    fn new(layers: Vec<Layer>) -> Voice {
        let life = layers.iter().map(|l| l.life).fold(0.0, f32::max);
        Voice { layers, age: 0.0, life, gain: 1.0, pan: 0.0, cutoff: 1.0, delay: 0.0 }
    }

    fn delayed(mut self, seconds: f32) -> Voice {
        self.delay = seconds;
        self
    }

    fn next(&mut self, dt: f32, rate: f32) -> (f32, f32) {
        if self.delay > 0.0 {
            self.delay -= dt;
            return (0.0, 0.0);
        }
        let mut mono = 0.0;
        for layer in &mut self.layers {
            mono += layer.next(dt, rate, self.gain, self.cutoff);
        }
        // Constant-power pan: a sound moved to one side does not get quieter.
        let pan = self.pan.clamp(-1.0, 1.0);
        let left = ((1.0 - pan) * 0.5).sqrt();
        let right = ((1.0 + pan) * 0.5).sqrt();
        self.age += dt;
        (mono * left, mono * right)
    }
}

/// Where a sound sits relative to the listener: level, muffle, delay and pan.
#[derive(Clone, Copy)]
struct Place {
    gain: f32,
    cutoff: f32,
    delay: f32,
    pan: f32,
}

impl Place {
    fn at(pos: Vec3, listener: Vec3, right: Vec3) -> Place {
        let d = (pos - listener).length();
        // 1/r-ish falloff, gentle enough that a fight is still audible across
        // the arena, with a fade to nothing at the edge of hearing.
        let near = 1.0 / (1.0 + d * 0.035);
        let far = (1.0 - ((d - 70.0) / (MAX_AUDIBLE - 70.0)).clamp(0.0, 1.0)).max(0.0);
        let gain = near * far;
        // Distance is a low-pass filter: a far shot is body, not crack.
        let cutoff = (1.0 / (1.0 + d * 0.018)).clamp(0.22, 1.0);
        let delay = d / SOUND_SPEED;
        let dir = if d > 1.0e-4 { (pos - listener) / d } else { Vec3::ZERO };
        let pan = (dir.dot(right) * 0.75).clamp(-1.0, 1.0);
        Place { gain, cutoff, delay, pan }
    }
}

/// The synthesiser itself. Public so the tests can render it without a device.
pub struct Sfx {
    rate: f32,
    voices: Vec<Voice>,
    rng: u32,
    muted: bool,
    /// Nova-style wind-up, 0..1. Drawn from game state every frame, not an event.
    charge: f32,
    charge_phase: f32,
}

impl Sfx {
    pub fn new(rate: f32) -> Sfx {
        Sfx { rate, voices: Vec::with_capacity(32), rng: 0x9e37_79b9, muted: false, charge: 0.0, charge_phase: 0.0 }
    }

    #[cfg(test)]
    pub fn rate(&self) -> f32 {
        self.rate
    }

    pub fn set_muted(&mut self, muted: bool) {
        if self.muted != muted {
            self.muted = muted;
            if muted {
                self.voices.clear();
            }
        }
    }

    pub fn set_charge(&mut self, charge: f32) {
        self.charge = charge.clamp(0.0, 1.0);
    }

    pub fn stop_all(&mut self) {
        self.voices.clear();
        self.charge = 0.0;
    }

    fn rand(&mut self) -> f32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 17;
        self.rng ^= self.rng << 5;
        self.rng as f32 / u32::MAX as f32
    }

    /// Play the sound for one sim event, from where it happened.
    pub fn play(&mut self, event: &Event, listener: Vec3, right: Vec3) {
        match *event {
            Event::Shot { weapon, pos, .. } => {
                let p = Place::at(pos, listener, right);
                self.shot(weapon, p);
            }
            Event::Explosion { pos, radius } => {
                let p = Place::at(pos, listener, right);
                self.explosion(radius, p);
            }
            Event::Beam { from, to } => {
                let p = Place::at(to, listener, right);
                self.beam(p);
                // The muzzle end is quieter, but it still exists.
                let p = Place::at(from, listener, right);
                self.add(Voice::new(vec![Layer::noise(0.18, 0.0005, 90.0, 0.03, 10_000.0)]), p);
            }
            Event::Whip { from, .. } => {
                let p = Place::at(from, listener, right);
                self.whip(p);
            }
            Event::Hit { pos, damage, .. } => {
                let p = Place::at(pos, listener, right);
                self.hit(damage, p);
            }
            Event::Died { pos, .. } => {
                let p = Place::at(pos, listener, right);
                self.died(p);
            }
            Event::Teleported { from, to, .. } => {
                self.teleport(Place::at(from, listener, right));
                self.teleport(Place::at(to, listener, right));
            }
            // Terrain edits are continuous (digging, the tractor) or already
            // have a louder explosion. The tractor's own `Shot` covers it.
            Event::Edit { .. } | Event::Respawned { .. } => {}
        }
    }

    fn add(&mut self, mut voice: Voice, p: Place) {
        if p.gain <= 1.0e-4 {
            return;
        }
        voice.gain *= p.gain;
        voice.cutoff *= p.cutoff;
        voice.pan += p.pan;
        voice.delay += p.delay;
        // A little pitch variance per shot: without it, rapid fire turns into a
        // machine-gun of identical clicks.
        let pitch = 0.95 + self.rand() * 0.10;
        for layer in &mut voice.layers {
            layer.seed = (self.rand() * u32::MAX as f32) as u32 | 1;
            if layer.f0 > 0.0 {
                layer.f0 *= pitch;
            }
            if layer.f1 > 0.0 {
                layer.f1 *= pitch;
            }
        }
        // Keep the voice budget bounded. A busy fight drops the oldest sound,
        // exactly as the renderer drops the oldest dust.
        if self.voices.len() >= 64 {
            self.voices.remove(0);
        }
        self.voices.push(voice);
    }

    fn shot(&mut self, weapon: u8, p: Place) {
        let voices = match weapon {
            BLASTER => vec![
                // The metronome: a dry brass tick and a small hot flash. No tail:
                // the Blaster must stay at the quiet end of the mix.
                Voice::new(vec![
                    Layer::noise(0.26, 0.0004, 95.0, 0.025, 7_500.0),
                    Layer::sine(0.14, 0.001, 46.0, 0.055, 820.0, 470.0),
                ]),
            ],
            ROCKET => vec![
                Voice::new(vec![
                    Layer::noise(0.34, 0.001, 26.0, 0.10, 3_200.0),
                    Layer::sine(0.50, 0.002, 5.0, 0.40, 150.0, 48.0),
                ]),
                Voice::new(vec![
                    Layer::noise(0.22, 0.010, 3.2, 0.70, 620.0),
                    Layer::sine(0.24, 0.050, 2.0, 1.35, 48.0, 32.0),
                ]),
            ],
            RAILGUN => vec![
                Voice::new(vec![
                    Layer::noise(0.48, 0.0004, 110.0, 0.03, 13_000.0),
                    Layer::sine(0.17, 0.001, 12.0, 0.35, 2_600.0, 1_850.0),
                    Layer::sine(0.09, 0.002, 18.0, 0.45, 4_200.0, 3_900.0),
                ]),
                // The ringing ear arrives a beat later.
                Voice::new(vec![Layer::sine(0.045, 0.010, 3.0, 0.90, 5_600.0, 5_200.0)]).delayed(0.045),
            ],
            BOUNCER => vec![
                Voice::new(vec![
                    Layer::sine(0.25, 0.001, 22.0, 0.20, 690.0, 1_240.0),
                    Layer::sine(0.10, 0.001, 30.0, 0.12, 1_360.0, 1_980.0),
                    Layer::noise(0.10, 0.001, 70.0, 0.025, 8_000.0),
                ]),
            ],
            BOMB => vec![
                // The throw is just a soft clunk; the boom comes from the
                // projectile's Explosion a second later.
                Voice::new(vec![
                    Layer::noise(0.14, 0.001, 30.0, 0.06, 1_500.0),
                    Layer::sine(0.10, 0.001, 12.0, 0.12, 180.0, 90.0),
                ]),
            ],
            CLUSTER => vec![
                Voice::new(vec![
                    Layer::noise(0.30, 0.005, 8.0, 0.32, 520.0),
                    Layer::sine(0.26, 0.005, 6.0, 0.38, 150.0, 70.0),
                    Layer::noise(0.12, 0.020, 3.0, 0.65, 300.0),
                ]),
            ],
            SWARM => (0..6)
                .map(|k| {
                    Voice::new(vec![
                        Layer::noise(0.09, 0.005, 14.0, 0.18, 2_600.0),
                        Layer::sine(0.045, 0.005, 12.0, 0.15, 480.0, 920.0),
                    ])
                    .delayed(k as f32 * 0.035)
                })
                .collect(),
            TELEPORT => vec![
                Voice::new(vec![
                    Layer::noise(0.30, 0.001, 16.0, 0.12, 9_000.0),
                    Layer::sine(0.26, 0.002, 5.0, 0.45, 1_400.0, 180.0),
                ]),
                Voice::new(vec![Layer::sine(0.07, 0.010, 2.5, 0.85, 2_600.0, 320.0)]).delayed(0.03),
            ],
            NOVA => vec![
                // The cavern eraser: a crack, a body below the speakers, and a
                // tail that keeps going after the flash.
                Voice::new(vec![
                    Layer::noise(0.55, 0.001, 20.0, 0.16, 6_000.0),
                    Layer::sine(0.62, 0.005, 2.4, 0.85, 90.0, 26.0),
                ]),
                Voice::new(vec![
                    Layer::noise(0.34, 0.020, 1.3, 2.1, 240.0),
                    Layer::sine(0.26, 0.050, 1.0, 2.6, 40.0, 28.0),
                ]),
            ],
            BUILD => vec![
                Voice::new(vec![
                    Layer::noise(0.34, 0.001, 32.0, 0.10, 2_200.0),
                    Layer::sine(0.34, 0.001, 9.0, 0.24, 120.0, 55.0),
                ]),
            ],
            TRACTOR => vec![
                // One soft lump of clay. The 16/s placement is absorbed by the
                // short, quiet envelope so it reads as a stream, not a rattle.
                Voice::new(vec![
                    Layer::noise(0.075, 0.002, 30.0, 0.055, 900.0),
                    Layer::sine(0.05, 0.002, 25.0, 0.075, 180.0, 120.0),
                ]),
            ],
            WHIP => vec![
                Voice::new(vec![
                    Layer::noise(0.48, 0.0004, 80.0, 0.045, 12_000.0),
                    Layer::sine(0.16, 0.001, 10.0, 0.38, 900.0, 180.0),
                    Layer::noise(0.12, 0.010, 4.0, 0.32, 3_500.0),
                ]),
            ],
            _ => vec![Voice::new(vec![Layer::sine(0.15, 0.001, 20.0, 0.12, 500.0, 200.0)])],
        };
        for voice in voices {
            self.add(voice, p);
        }
    }

    fn explosion(&mut self, radius: f32, p: Place) {
        // `Event::Explosion` does not carry a weapon id yet (see
        // docs/WEAPON_FEEL.md 6.1). Radius is enough to separate the three
        // sizes: bomblet/swarm, rocket/bomb, Nova.
        let voices = if radius >= 6.0 {
            vec![
                Voice::new(vec![
                    Layer::noise(0.60, 0.001, 18.0, 0.18, 5_500.0),
                    Layer::sine(0.62, 0.005, 2.0, 0.95, 78.0, 24.0),
                ]),
                Voice::new(vec![
                    Layer::noise(0.40, 0.020, 1.2, 2.3, 220.0),
                    Layer::sine(0.30, 0.050, 0.9, 2.8, 38.0, 26.0),
                ]),
            ]
        } else if radius >= 2.5 {
            vec![
                Voice::new(vec![
                    Layer::noise(0.46, 0.001, 28.0, 0.12, 4_000.0),
                    Layer::sine(0.48, 0.004, 4.5, 0.55, 100.0, 36.0),
                ]),
                Voice::new(vec![
                    Layer::noise(0.22, 0.015, 3.5, 1.0, 360.0),
                    Layer::sine(0.20, 0.04, 2.2, 1.3, 45.0, 30.0),
                ]),
            ]
        } else {
            vec![
                Voice::new(vec![
                    Layer::noise(0.22, 0.001, 55.0, 0.07, 4_500.0),
                    Layer::sine(0.20, 0.002, 18.0, 0.22, 180.0, 75.0),
                ]),
            ]
        };
        for voice in voices {
            self.add(voice, p);
        }
    }

    fn beam(&mut self, p: Place) {
        let voice = Voice::new(vec![
            Layer::noise(0.50, 0.0004, 110.0, 0.03, 14_000.0),
            Layer::sine(0.16, 0.001, 10.0, 0.38, 2_500.0, 1_800.0),
        ]);
        self.add(voice, p);
    }

    fn whip(&mut self, p: Place) {
        let voice = Voice::new(vec![
            Layer::noise(0.50, 0.0004, 80.0, 0.045, 12_000.0),
            Layer::sine(0.17, 0.001, 10.0, 0.40, 950.0, 170.0),
            Layer::noise(0.13, 0.010, 4.0, 0.34, 3_800.0),
        ]);
        self.add(voice, p);
    }

    fn hit(&mut self, damage: f32, p: Place) {
        let level = (damage / 70.0).clamp(0.25, 1.15);
        let voice = Voice::new(vec![
            Layer::noise(0.22 * level, 0.0005, 60.0, 0.055, 6_000.0),
            Layer::sine(0.12 * level, 0.001, 26.0, 0.10, 320.0, 190.0),
        ]);
        self.add(voice, p);
    }

    fn died(&mut self, p: Place) {
        let voice = Voice::new(vec![
            Layer::noise(0.30, 0.001, 25.0, 0.12, 5_000.0),
            Layer::sine(0.26, 0.002, 5.0, 0.55, 620.0, 90.0),
            Layer::sine(0.10, 0.020, 2.0, 1.0, 180.0, 60.0),
        ]);
        self.add(voice, p);
    }

    fn teleport(&mut self, p: Place) {
        let voice = Voice::new(vec![
            Layer::noise(0.28, 0.001, 16.0, 0.11, 9_000.0),
            Layer::sine(0.24, 0.002, 5.0, 0.42, 1_400.0, 180.0),
        ]);
        self.add(voice, p);
    }

    /// Mix one stereo frame. `dt` is `1 / rate`.
    fn next(&mut self) -> (f32, f32) {
        if self.muted {
            return (0.0, 0.0);
        }
        let dt = 1.0 / self.rate;
        let mut l = 0.0;
        let mut r = 0.0;
        for voice in &mut self.voices {
            let (a, b) = voice.next(dt, self.rate);
            l += a;
            r += b;
        }
        self.voices.retain(|v| v.age < v.life || v.delay > 0.0);
        // A wind-up is a rising, slightly unstable hum. It is a state, not an
        // event, so it is drawn every frame and stops the instant the shot fires.
        if self.charge > 0.0 {
            let t = self.charge;
            let freq = 90.0 + 380.0 * t * t;
            self.charge_phase = (self.charge_phase + std::f32::consts::TAU * freq * dt).rem_euclid(std::f32::consts::TAU);
            let wobble = 1.0 + 0.10 * (self.charge_phase * 3.7).sin();
            let s = (self.charge_phase.sin() * 0.075 + (self.charge_phase * 2.01).sin() * 0.035) * t * wobble;
            l += s;
            r += s;
        }
        (soft_clip(l * MASTER), soft_clip(r * MASTER))
    }
}

fn soft_clip(x: f32) -> f32 {
    x.tanh()
}

/// One thing the game thread asks the audio thread to do. Commands are queued
/// instead of sharing the synthesiser behind a lock: the audio callback must
/// never wait for the frame, and the frame must never wait for a whole buffer.
enum Command {
    Sound { event: Event, listener: Vec3, right: Vec3 },
    Stop,
}

/// A tiny abstraction over the output device. The game loop only ever calls
/// `Audio`; a `WebAudio` backend can be added behind this later.
trait Backend: Sized {
    fn open(
        queue: Arc<Mutex<Vec<Command>>>,
        charge: Arc<AtomicU32>,
        muted: Arc<AtomicBool>,
    ) -> Result<(Self, u32, String), String>;
}

/// Native output through `cpal`. It asks for f32 and falls back to i16/u16 when
/// that is what the device offers; anything else (or no device at all) leaves
/// the game silent instead of failing to start.
struct CpalBackend {
    /// Kept alive for as long as `Audio` exists: dropping the stream stops it.
    _stream: cpal::Stream,
}

trait Store {
    fn store(&mut self, value: f32);
}

impl Store for f32 {
    fn store(&mut self, value: f32) {
        *self = value;
    }
}

impl Store for i16 {
    fn store(&mut self, value: f32) {
        *self = (value.clamp(-1.0, 1.0) * i16::MAX as f32) as i16;
    }
}

impl Store for u16 {
    fn store(&mut self, value: f32) {
        *self = ((value.clamp(-1.0, 1.0) * 0.5 + 0.5) * u16::MAX as f32) as u16;
    }
}

fn fill<T: Store>(data: &mut [T], sfx: &mut Sfx, channels: usize) {
    for frame in data.chunks_mut(channels) {
        let (l, r) = sfx.next();
        match frame.len() {
            1 => frame[0].store((l + r) * 0.5),
            _ => {
                frame[0].store(l);
                frame[1].store(r);
                for s in &mut frame[2..] {
                    s.store((l + r) * 0.5);
                }
            }
        }
    }
}

/// Apply the lock-free state and any queued events at the head of a buffer.
fn apply_commands(sfx: &mut Sfx, queue: &Mutex<Vec<Command>>, charge: &AtomicU32, muted: &AtomicBool) {
    sfx.set_muted(muted.load(Ordering::Relaxed));
    sfx.set_charge(f32::from_bits(charge.load(Ordering::Relaxed)));
    // Never wait for the game thread here. If it happens to be pushing, the
    // sounds start one buffer later, which is inaudible; a missed audio
    // deadline would be a click.
    if let Ok(mut queue) = queue.try_lock() {
        for command in queue.drain(..) {
            match command {
                Command::Sound { event, listener, right } => sfx.play(&event, listener, right),
                Command::Stop => sfx.stop_all(),
            }
        }
    }
}

fn build_stream<T: Store + cpal::SizedSample>(
    device: &cpal::Device,
    config: cpal::StreamConfig,
    rate: u32,
    queue: Arc<Mutex<Vec<Command>>>,
    charge: Arc<AtomicU32>,
    muted: Arc<AtomicBool>,
) -> Result<cpal::Stream, String> {
    let channels = config.channels as usize;
    let mut sfx = Sfx::new(rate as f32);
    device
        .build_output_stream::<T, _, _>(
            config,
            move |data: &mut [T], _| {
                apply_commands(&mut sfx, &queue, &charge, &muted);
                fill(data, &mut sfx, channels);
            },
            |e| eprintln!("audio: {e}"),
            None,
        )
        .map_err(|e| format!("cannot open stream: {e}"))
}

impl Backend for CpalBackend {
    fn open(
        queue: Arc<Mutex<Vec<Command>>>,
        charge: Arc<AtomicU32>,
        muted: Arc<AtomicBool>,
    ) -> Result<(CpalBackend, u32, String), String> {
        let host = cpal::default_host();
        let device = host.default_output_device().ok_or("no output device")?;
        let name = device.to_string();
        let supported = device.default_output_config().map_err(|e| format!("no output config: {e}"))?;
        let rate = supported.sample_rate();
        let format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();
        let build = |config| match format {
            cpal::SampleFormat::F32 => {
                build_stream::<f32>(&device, config, rate, queue.clone(), charge.clone(), muted.clone())
            }
            cpal::SampleFormat::I16 => {
                build_stream::<i16>(&device, config, rate, queue.clone(), charge.clone(), muted.clone())
            }
            cpal::SampleFormat::U16 => {
                build_stream::<u16>(&device, config, rate, queue.clone(), charge.clone(), muted.clone())
            }
            other => return Err(format!("unsupported sample format {other:?}")),
        };
        let stream = build(config)?;
        stream.play().map_err(|e| format!("cannot start stream: {e}"))?;
        Ok((CpalBackend { _stream: stream }, rate, name))
    }
}

/// The game's handle on the mixer. Holds the stream alive; dropping `Audio`
/// stops the sound.
pub struct Audio {
    backend: Option<CpalBackend>,
    queue: Arc<Mutex<Vec<Command>>>,
    charge: Arc<AtomicU32>,
    muted: Arc<AtomicBool>,
}

impl Audio {
    pub fn new() -> Audio {
        let queue = Arc::new(Mutex::new(Vec::with_capacity(32)));
        let charge = Arc::new(AtomicU32::new(0.0f32.to_bits()));
        let muted = Arc::new(AtomicBool::new(false));
        match CpalBackend::open(queue.clone(), charge.clone(), muted.clone()) {
            Ok((backend, rate, name)) => {
                println!("audio: {name} at {rate} Hz");
                Audio { backend: Some(backend), queue, charge, muted }
            }
            Err(why) => {
                eprintln!("audio: {why}; running silent (the game still works)");
                Audio { backend: None, queue, charge, muted }
            }
        }
    }

    pub fn available(&self) -> bool {
        self.backend.is_some()
    }

    pub fn play(&self, event: &Event, listener: Vec3, right: Vec3) {
        if !self.available() || self.muted() {
            return;
        }
        if let Ok(mut queue) = self.queue.lock() {
            // A long fight must not grow the queue without bound. Drop the
            // oldest request, exactly as the renderer drops the oldest dust.
            if queue.len() >= 256 {
                queue.remove(0);
            }
            queue.push(Command::Sound { event: *event, listener, right });
        }
    }

    pub fn set_charge(&self, charge: f32) {
        if self.available() {
            self.charge.store(charge.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
        }
    }

    pub fn toggle_mute(&self) -> bool {
        !self.muted.fetch_xor(true, Ordering::Relaxed)
    }

    pub fn muted(&self) -> bool {
        self.muted.load(Ordering::Relaxed)
    }

    pub fn stop_all(&self) {
        if !self.available() {
            return;
        }
        if let Ok(mut queue) = self.queue.lock() {
            queue.push(Command::Stop);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim::Event;

    fn render(sfx: &mut Sfx, seconds: f32) -> Vec<f32> {
        let n = (seconds * sfx.rate()) as usize;
        let mut out = Vec::with_capacity(n * 2);
        for _ in 0..n {
            let (l, r) = sfx.next();
            out.push(l);
            out.push(r);
        }
        out
    }

    fn peak(samples: &[f32]) -> f32 {
        samples.iter().fold(0.0f32, |m, s| m.max(s.abs()))
    }

    fn rms(samples: &[f32]) -> f32 {
        (samples.iter().map(|s| s * s).sum::<f32>() / samples.len() as f32).sqrt()
    }

    fn shot(weapon: u8, at: Vec3) -> Event {
        Event::Shot { worm: 0, weapon, pos: at }
    }

    #[test]
    fn every_weapon_makes_a_sound_and_then_stops() {
        for &weapon in &sim::ARSENAL {
            let mut sfx = Sfx::new(48_000.0);
            sfx.play(&shot(weapon, Vec3::ZERO), Vec3::ZERO, Vec3::X);
            let first = render(&mut sfx, 0.4);
            assert!(peak(&first) > 0.01, "weapon {weapon} is silent");
            let tail = render(&mut sfx, 3.0);
            // The tail may still be ringing, but it has to be over by the end
            // of three seconds even for the Nova.
            let end = &tail[tail.len() - 9_600..];
            assert!(
                peak(end) < 1.0e-6,
                "weapon {weapon} never stops: peak {} voices {}",
                peak(end),
                sfx.voices.len()
            );
        }
    }

    #[test]
    fn the_blaster_stays_quieter_than_the_nova() {
        let mut quiet = Sfx::new(48_000.0);
        quiet.play(&shot(BLASTER, Vec3::ZERO), Vec3::ZERO, Vec3::X);
        let q = render(&mut quiet, 0.5);
        let mut loud = Sfx::new(48_000.0);
        loud.play(&shot(NOVA, Vec3::ZERO), Vec3::ZERO, Vec3::X);
        let l = render(&mut loud, 0.5);
        assert!(rms(&l) > rms(&q) * 2.0, "contrast is the currency: nova {} vs blaster {}", rms(&l), rms(&q));
    }

    #[test]
    fn a_distant_shot_is_quieter_and_more_muffled() {
        let mut near = Sfx::new(48_000.0);
        near.play(&shot(ROCKET, Vec3::new(0.0, 0.0, 2.0)), Vec3::ZERO, Vec3::X);
        let n = render(&mut near, 0.6);
        let mut far = Sfx::new(48_000.0);
        far.play(&shot(ROCKET, Vec3::new(0.0, 0.0, 60.0)), Vec3::ZERO, Vec3::X);
        let f = render(&mut far, 0.6);
        assert!(peak(&f) < peak(&n) * 0.7, "distance turns it down: {} vs {}", peak(&f), peak(&n));
    }

    #[test]
    fn a_bigger_blast_lasts_longer() {
        let mut small = Sfx::new(48_000.0);
        small.play(&Event::Explosion { pos: Vec3::ZERO, radius: 1.8 }, Vec3::ZERO, Vec3::X);
        let s = render(&mut small, 0.4);
        assert!(peak(&s) > 0.01);
        assert!(peak(&render(&mut small, 0.6)) < 0.02, "a bomblet is over in under a second");

        let mut big = Sfx::new(48_000.0);
        big.play(&Event::Explosion { pos: Vec3::ZERO, radius: 8.0 }, Vec3::ZERO, Vec3::X);
        let b = render(&mut big, 0.4);
        assert!(rms(&b) > rms(&s) * 1.5);
        assert!(peak(&render(&mut big, 1.5)) > 0.01, "the Nova still rumbles a second later");
    }

    #[test]
    fn mute_is_silence() {
        let mut sfx = Sfx::new(48_000.0);
        sfx.play(&shot(ROCKET, Vec3::ZERO), Vec3::ZERO, Vec3::X);
        sfx.set_muted(true);
        assert_eq!(peak(&render(&mut sfx, 0.5)), 0.0);
    }

    #[test]
    fn the_wind_up_hums_and_stops_with_the_charge() {
        let mut sfx = Sfx::new(48_000.0);
        sfx.set_charge(1.0);
        let on = render(&mut sfx, 0.2);
        assert!(peak(&on) > 0.005, "a charging Nova is audible");
        sfx.set_charge(0.0);
        let off = render(&mut sfx, 0.2);
        assert_eq!(peak(&off), 0.0);
    }

    #[test]
    fn panning_puts_a_sound_on_the_side_it_happened() {
        // The listener looks down +Z, so +X is to its right.
        let mut sfx = Sfx::new(48_000.0);
        sfx.play(&shot(ROCKET, Vec3::new(20.0, 0.0, 0.0)), Vec3::ZERO, Vec3::X);
        let out = render(&mut sfx, 0.4);
        let left: f32 = out.iter().step_by(2).map(|s| s.abs()).sum();
        let right: f32 = out.iter().skip(1).step_by(2).map(|s| s.abs()).sum();
        assert!(right > left * 1.5, "the right channel is louder: {right} vs {left}");
    }
}
