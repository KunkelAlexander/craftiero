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
    BLASTER, BOMB, BOMBLET, BOUNCER, BUILD, CLUSTER, LAVA, NOVA, RAILGUN, ROCKET, SWARM, TELEPORT, TRACTOR,
    WHIP,
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
/// without turning into digital distortion. This is deliberately hot: a weapon
/// should move air, not sit politely under the rest of the mix.
const MASTER: f32 = 1.35;

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

    fn gain(mut self, gain: f32) -> Voice {
        self.gain = gain;
        self
    }

    fn pan(mut self, pan: f32) -> Voice {
        self.pan = pan;
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
                // A beam and a lash each have their own event with the impact
                // or curve, so the Shot event must not double their voice.
                if weapon == RAILGUN || weapon == WHIP {
                    return;
                }
                let p = Place::at(pos, listener, right);
                self.shot(weapon, p);
            }
            Event::Explosion { weapon, pos, .. } => {
                let p = Place::at(pos, listener, right);
                self.explosion(weapon, p);
            }
            Event::Beam { from, to } => {
                // The impact end carries the body; the muzzle still cracks.
                self.beam(Place::at(to, listener, right));
                self.add(
                    Voice::new(vec![
                        Layer::noise(0.50, 0.0003, 100.0, 0.05, 15_000.0),
                        Layer::sine(0.22, 0.001, 12.0, 0.40, 120.0, 50.0),
                    ]),
                    Place::at(from, listener, right),
                );
            }
            Event::Whip { from, .. } => {
                let p = Place::at(from, listener, right);
                self.whip(p);
            }
            Event::Hit { pos, damage, weapon, .. } => {
                let p = Place::at(pos, listener, right);
                self.hit(damage, weapon, p);
            }
            Event::Impact { weapon, pos, .. } => {
                let p = Place::at(pos, listener, right);
                self.impact(weapon, p);
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
            Event::ZoneStarted { pos, kind, .. } => {
                // A low swell for the moon and a dropping thud for the heavy
                // pull, so the two read apart with the eyes shut.
                let (f0, f1) = match kind {
                    sim::ZoneKind::Gravity(scale) if scale < 1.0 => (180.0, 420.0),
                    sim::ZoneKind::Gravity(_) => (160.0, 45.0),
                };
                self.add(
                    Voice::new(vec![
                        Layer::sine(0.50, 0.02, 2.2, 0.9, f0, f1),
                        Layer::noise(0.14, 0.02, 2.0, 0.7, 1_400.0),
                    ]),
                    Place::at(pos, listener, right),
                );
            }
            Event::Edit { .. } | Event::LavaFroze { .. } | Event::Respawned { .. } => {}
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
                // The metronome, but a heavy one: a dry crack over a short
                // chest thump. Still the quiet end of the arsenal, just no
                // longer a click.
                Voice::new(vec![
                    Layer::noise(0.30, 0.0003, 85.0, 0.045, 9_000.0),
                    Layer::sine(0.26, 0.001, 26.0, 0.14, 230.0, 80.0),
                    Layer::sine(0.16, 0.001, 34.0, 0.10, 900.0, 360.0),
                ]),
            ],
            ROCKET => vec![
                // The rocket jump: a crack and a sub-bass shove as it leaves,
                // then a long dirty tail of exhaust and dust.
                Voice::new(vec![
                    Layer::noise(0.55, 0.0005, 30.0, 0.12, 4_500.0),
                    Layer::sine(0.70, 0.002, 3.5, 1.0, 130.0, 34.0),
                ]),
                Voice::new(vec![
                    Layer::noise(0.40, 0.008, 2.4, 1.4, 520.0),
                    Layer::sine(0.45, 0.030, 1.4, 2.2, 48.0, 24.0),
                    Layer::noise(0.22, 0.050, 0.8, 2.4, 160.0),
                ]),
            ],
            BOUNCER => vec![
                // A bright metal ping with enough body to read as a ball, not a
                // UI blip.
                Voice::new(vec![
                    Layer::noise(0.30, 0.0004, 70.0, 0.05, 9_000.0),
                    Layer::sine(0.42, 0.001, 12.0, 0.50, 620.0, 1_500.0),
                    Layer::sine(0.22, 0.001, 16.0, 0.40, 1_500.0, 2_400.0),
                    Layer::sine(0.18, 0.002, 7.0, 0.35, 140.0, 60.0),
                ]),
            ],
            BOMB => vec![
                // The throw is a clunk; the boom comes from the projectile's
                // Explosion a second later.
                Voice::new(vec![
                    Layer::noise(0.24, 0.001, 24.0, 0.10, 2_000.0),
                    Layer::sine(0.22, 0.001, 8.0, 0.35, 150.0, 55.0),
                ]),
            ],
            CLUSTER => vec![
                // A mortar leaves the tube with a low thump and keeps rumbling.
                Voice::new(vec![
                    Layer::noise(0.50, 0.002, 7.0, 0.45, 700.0),
                    Layer::sine(0.55, 0.003, 4.0, 1.0, 120.0, 42.0),
                ]),
                Voice::new(vec![
                    Layer::noise(0.28, 0.020, 2.2, 1.5, 240.0),
                    Layer::sine(0.24, 0.060, 1.1, 2.0, 42.0, 25.0),
                ]),
            ],
            SWARM => {
                let mut voices = vec![
                    Voice::new(vec![Layer::sine(0.40, 0.002, 4.5, 0.7, 110.0, 36.0)]),
                ];
                voices.extend((0..6).map(|k| {
                    Voice::new(vec![
                        Layer::noise(0.16, 0.003, 10.0, 0.30, 3_200.0),
                        Layer::sine(0.10, 0.003, 9.0, 0.25, 320.0, 1_100.0),
                    ])
                    .delayed(k as f32 * 0.032)
                }));
                voices
            }
            TELEPORT => vec![
                Voice::new(vec![
                    Layer::noise(0.45, 0.0005, 20.0, 0.14, 11_000.0),
                    Layer::sine(0.48, 0.002, 4.5, 0.90, 1_600.0, 130.0),
                ]),
                Voice::new(vec![
                    Layer::sine(0.18, 0.010, 1.5, 1.8, 2_600.0, 180.0),
                    Layer::noise(0.12, 0.020, 1.2, 1.4, 1_200.0),
                ])
                .delayed(0.04),
            ],
            NOVA => vec![
                // The cavern eraser: a crack, a body below the speakers, and a
                // tail that keeps going after the flash.
                Voice::new(vec![
                    Layer::noise(0.85, 0.0005, 18.0, 0.22, 7_000.0),
                    Layer::sine(0.95, 0.003, 2.0, 1.4, 90.0, 22.0),
                ]),
                Voice::new(vec![
                    Layer::noise(0.60, 0.015, 1.1, 3.2, 240.0),
                    Layer::sine(0.55, 0.040, 0.65, 2.8, 38.0, 20.0),
                ]),
            ],
            BUILD => vec![
                // Rock slams into place: a hard crack over a floor-shaking thud.
                Voice::new(vec![
                    Layer::noise(0.52, 0.0005, 24.0, 0.18, 3_500.0),
                    Layer::sine(0.50, 0.001, 6.0, 0.80, 110.0, 40.0),
                ]),
                Voice::new(vec![
                    Layer::noise(0.25, 0.020, 2.5, 1.4, 300.0),
                ])
                .delayed(0.02),
            ],
            TRACTOR => vec![
                // One soft lump of clay. The 16/s placement is absorbed by the
                // short, quiet envelope so it reads as a stream, not a rattle.
                Voice::new(vec![
                    Layer::noise(0.13, 0.001, 26.0, 0.08, 1_200.0),
                    Layer::sine(0.10, 0.001, 18.0, 0.16, 170.0, 80.0),
                ]),
            ],
            LAVA => vec![
                // One short wet hiss. Thirty a second run together into the
                // sound of a hose, so each one stays quiet and brief.
                Voice::new(vec![
                    Layer::noise(0.12, 0.002, 18.0, 0.09, 2_400.0),
                    Layer::sine(0.09, 0.002, 14.0, 0.12, 120.0, 70.0),
                ]),
            ],
            // The railgun and the whip are heard through their own `Beam` and
            // `Whip` events, which carry the impact point and the swing.
            _ => vec![Voice::new(vec![Layer::sine(0.15, 0.001, 20.0, 0.12, 500.0, 200.0)])],
        };
        for voice in voices {
            self.add(voice, p);
        }
    }

    fn explosion(&mut self, weapon: u8, p: Place) {
        // The weapon shapes the blast: a Nova is a cavern event, a bomb is
        // deep and dirty, a rocket is a sharp shove, a bomblet is a pop.
        let voices = match weapon {
            NOVA => vec![
                // The cavern eraser: a white-hot crack, a body below the
                // speakers, then a low rolling tail that outlives the flash.
                Voice::new(vec![
                    Layer::noise(0.90, 0.0004, 16.0, 0.22, 6_000.0),
                    Layer::sine(0.98, 0.003, 1.5, 1.5, 84.0, 20.0),
                ]),
                Voice::new(vec![
                    Layer::noise(0.70, 0.010, 1.0, 2.8, 220.0),
                    Layer::sine(0.65, 0.040, 0.45, 4.5, 36.0, 18.0),
                ]),
                // Delayed, darker copies to either side read as the room
                // answering back: the tail is wider than the blast.
                Voice::new(vec![
                    Layer::noise(0.32, 0.020, 0.8, 3.2, 120.0),
                    Layer::sine(0.30, 0.060, 0.40, 3.6, 28.0, 20.0),
                ])
                .gain(0.85)
                .pan(-0.35)
                .delayed(0.16),
                Voice::new(vec![
                    Layer::noise(0.30, 0.022, 0.8, 3.4, 110.0),
                    Layer::sine(0.28, 0.065, 0.38, 3.8, 26.0, 19.0),
                ])
                .gain(0.85)
                .pan(0.35)
                .delayed(0.19),
            ],
            BOMB => vec![
                // Ground, not fire: most of the energy is below the speakers,
                // and the dark cloud takes a long time to clear.
                Voice::new(vec![
                    Layer::noise(0.62, 0.0006, 32.0, 0.16, 3_400.0),
                    Layer::sine(0.88, 0.003, 2.5, 1.6, 82.0, 20.0),
                ]),
                Voice::new(vec![
                    Layer::noise(0.62, 0.012, 0.8, 3.0, 180.0),
                    Layer::sine(0.52, 0.045, 0.35, 3.4, 34.0, 16.0),
                ]),
            ],
            ROCKET => vec![
                Voice::new(vec![
                    Layer::noise(0.70, 0.0005, 24.0, 0.16, 4_500.0),
                    Layer::sine(0.80, 0.003, 3.0, 1.1, 100.0, 28.0),
                ]),
                Voice::new(vec![
                    Layer::noise(0.48, 0.010, 1.8, 2.2, 300.0),
                    Layer::sine(0.42, 0.030, 0.9, 3.0, 40.0, 22.0),
                ]),
            ],
            // A cluster's seven children, or a swarm missile: short and dry, so
            // they read as a sequence instead of one wall.
            SWARM | BOMBLET => vec![
                Voice::new(vec![
                    Layer::noise(0.35, 0.0005, 45.0, 0.08, 5_000.0),
                    Layer::sine(0.34, 0.002, 12.0, 0.45, 180.0, 60.0),
                ]),
                Voice::new(vec![Layer::noise(0.18, 0.010, 4.0, 0.9, 400.0)]),
            ],
            _ => vec![
                Voice::new(vec![
                    Layer::noise(0.48, 0.0005, 30.0, 0.12, 4_000.0),
                    Layer::sine(0.50, 0.002, 6.0, 0.8, 120.0, 40.0),
                ]),
            ],
        };
        for voice in voices {
            self.add(voice, p);
        }
    }

    fn beam(&mut self, p: Place) {
        // The line through the room: the same voice as the railgun's shot, with
        // the impact point carrying the weight.
        self.add(
            Voice::new(vec![
                Layer::noise(0.62, 0.0003, 100.0, 0.06, 15_000.0),
                Layer::sine(0.30, 0.001, 9.0, 0.55, 2_600.0, 1_100.0),
                Layer::sine(0.20, 0.001, 13.0, 0.80, 4_200.0, 2_900.0),
                Layer::sine(0.22, 0.002, 4.0, 0.45, 100.0, 40.0),
            ]),
            p,
        );
        self.add(
            Voice::new(vec![Layer::sine(0.13, 0.010, 1.4, 2.2, 5_800.0, 4_800.0)]).delayed(0.05),
            p,
        );
    }

    fn whip(&mut self, p: Place) {
        self.add(
            Voice::new(vec![
                Layer::noise(0.68, 0.0003, 90.0, 0.06, 14_000.0),
                Layer::sine(0.30, 0.001, 10.0, 0.55, 230.0, 55.0),
            ]),
            p,
        );
        self.add(
            Voice::new(vec![
                Layer::noise(0.28, 0.008, 2.8, 0.90, 3_800.0),
                Layer::sine(0.15, 0.010, 3.0, 1.1, 820.0, 100.0),
            ])
            .delayed(0.03),
            p,
        );
    }

    fn hit(&mut self, damage: f32, weapon: u8, p: Place) {
        let level = (damage / 70.0).clamp(0.30, 1.30);
        // The thump every hit shares: a crack, a body and a sub-bass shove.
        self.add(
            Voice::new(vec![
                Layer::noise(0.38 * level, 0.0004, 60.0, 0.09, 7_000.0),
                Layer::sine(0.28 * level, 0.001, 18.0, 0.28, 260.0, 80.0),
                Layer::sine(0.22 * level, 0.002, 10.0, 0.35, 95.0, 45.0),
            ]),
            p,
        );
        // Then the weapon's own mark, so a railgun hit rings and a blaster
        // tick does not.
        let extra = match weapon {
            RAILGUN => Some(Voice::new(vec![
                Layer::sine(0.16 * level, 0.004, 2.0, 0.85, 5_600.0, 4_200.0),
            ])),
            WHIP => Some(Voice::new(vec![Layer::noise(0.20 * level, 0.006, 3.0, 0.75, 6_000.0)])),
            BOUNCER => Some(Voice::new(vec![Layer::sine(0.20 * level, 0.001, 10.0, 0.35, 1_250.0, 900.0)])),
            _ => None,
        };
        if let Some(voice) = extra {
            self.add(voice, p);
        }
    }

    /// A shot stopping against rock: a short dry crack, quieter than a body hit
    /// but present, so a wall of missed fire still sounds like fire. The weapon
    /// nudges the pitch, so a railgun ping stays distinct from a blaster tick.
    fn impact(&mut self, weapon: u8, p: Place) {
        let body = match weapon {
            RAILGUN => 1_500.0,
            WHIP => 900.0,
            _ => 460.0,
        };
        self.add(
            Voice::new(vec![
                Layer::noise(0.30, 0.0004, 90.0, 0.05, 8_000.0),
                Layer::sine(0.18, 0.001, 28.0, 0.13, body, body * 0.35),
            ]),
            p,
        );
    }

    fn died(&mut self, p: Place) {
        let voice = Voice::new(vec![
            Layer::noise(0.50, 0.0004, 20.0, 0.18, 6_000.0),
            Layer::sine(0.45, 0.002, 4.5, 0.90, 700.0, 60.0),
            Layer::sine(0.30, 0.020, 1.5, 2.0, 140.0, 28.0),
        ]);
        self.add(voice, p);
        self.add(
            Voice::new(vec![Layer::noise(0.20, 0.020, 1.2, 2.2, 180.0)]).delayed(0.10),
            p,
        );
    }

    fn teleport(&mut self, p: Place) {
        self.add(
            Voice::new(vec![
                Layer::noise(0.45, 0.0005, 20.0, 0.14, 11_000.0),
                Layer::sine(0.48, 0.002, 4.5, 0.90, 1_600.0, 130.0),
            ]),
            p,
        );
        self.add(
            Voice::new(vec![
                Layer::sine(0.18, 0.010, 1.5, 1.8, 2_600.0, 180.0),
                Layer::noise(0.12, 0.020, 1.2, 1.4, 1_200.0),
            ])
            .delayed(0.04),
            p,
        );
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

    /// An `Audio` with no device, for the time before the first click on the web.
    pub fn silent() -> Audio {
        Audio {
            backend: None,
            queue: Arc::new(Mutex::new(Vec::new())),
            charge: Arc::new(AtomicU32::new(0.0f32.to_bits())),
            muted: Arc::new(AtomicBool::new(false)),
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

    /// The event a real shot makes: a beam and a lash have their own, carrying
    /// the impact point or the swing.
    fn trigger(weapon: u8) -> Event {
        match weapon {
            RAILGUN => Event::Beam { from: Vec3::ZERO, to: Vec3::Z * 20.0 },
            WHIP => Event::Whip {
                worm: 0,
                from: Vec3::ZERO,
                fwd: Vec3::Z,
                side: Vec3::NEG_X,
                lean: 0.0,
                style: sim::WhipStyle::Sweep,
                flip: false,
            },
            _ => shot(weapon, Vec3::ZERO),
        }
    }

    #[test]
    fn every_weapon_makes_a_sound_and_then_stops() {
        for &weapon in &sim::ARSENAL {
            let mut sfx = Sfx::new(48_000.0);
            sfx.play(&trigger(weapon), Vec3::ZERO, Vec3::X);
            let first = render(&mut sfx, 0.4);
            assert!(peak(&first) > 0.01, "weapon {weapon} is silent");
            let tail = render(&mut sfx, 5.0);
            // The tail may still be ringing, but it has to be over by the end
            // even for the Nova.
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
        small.play(&Event::Explosion { weapon: BOMBLET, pos: Vec3::ZERO, normal: Vec3::Y, radius: 1.8 }, Vec3::ZERO, Vec3::X);
        let s = render(&mut small, 0.4);
        assert!(peak(&s) > 0.01);
        assert!(peak(&render(&mut small, 1.2)) < 0.02, "a bomblet is over in about a second");

        let mut big = Sfx::new(48_000.0);
        big.play(&Event::Explosion { weapon: NOVA, pos: Vec3::ZERO, normal: Vec3::Y, radius: 8.0 }, Vec3::ZERO, Vec3::X);
        let b = render(&mut big, 0.4);
        assert!(rms(&b) > rms(&s) * 1.5);
        assert!(peak(&render(&mut big, 1.5)) > 0.01, "the Nova still rumbles a second later");
    }

    #[test]
    fn a_bomb_and_a_rocket_have_different_voices() {
        let mut bomb = Sfx::new(48_000.0);
        bomb.play(&Event::Explosion { weapon: BOMB, pos: Vec3::ZERO, normal: Vec3::Y, radius: 4.0 }, Vec3::ZERO, Vec3::X);
        let b = render(&mut bomb, 1.0);

        let mut rocket = Sfx::new(48_000.0);
        rocket.play(&Event::Explosion { weapon: ROCKET, pos: Vec3::ZERO, normal: Vec3::Y, radius: 3.0 }, Vec3::ZERO, Vec3::X);
        let r = render(&mut rocket, 1.0);

        // The bomb is the longer, heavier one; the rocket is over sooner.
        assert!(rms(&b) > rms(&r), "the bomb carries more low end: {} vs {}", rms(&b), rms(&r));
        let bomb_tail = peak(&render(&mut bomb, 2.0));
        let rocket_tail = peak(&render(&mut rocket, 2.0));
        assert!(bomb_tail > rocket_tail, "and its cloud outlasts the rocket: {bomb_tail} vs {rocket_tail}");
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
    fn the_heavy_weapons_are_loud_and_long() {
        let mut quiet = Sfx::new(48_000.0);
        quiet.play(&trigger(BLASTER), Vec3::ZERO, Vec3::X);
        let blaster = render(&mut quiet, 1.0);

        let mut loud = Sfx::new(48_000.0);
        loud.play(&trigger(NOVA), Vec3::ZERO, Vec3::X);
        let nova = render(&mut loud, 1.0);
        assert!(peak(&nova) > 0.80, "the Nova should hit the limiter: {}", peak(&nova));
        assert!(rms(&nova) > rms(&blaster) * 6.0, "and dwarf the Blaster: {} vs {}", rms(&nova), rms(&blaster));

        let mut boom = Sfx::new(48_000.0);
        boom.play(&Event::Explosion { weapon: NOVA, pos: Vec3::ZERO, normal: Vec3::Y, radius: 8.0 }, Vec3::ZERO, Vec3::X);
        let one_second = render(&mut boom, 1.0);
        assert!(rms(&one_second) > 0.30, "a Nova is a wall of sound: {}", rms(&one_second));
        // Still rolling two and a half seconds after the flash.
        assert!(peak(&render(&mut boom, 2.5)) > 0.005);
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
