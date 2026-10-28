// The client's copy of the match.
//
// A client never runs `World::step`. It keeps a `World` purely as a container
// and fills it with what the host said. That is a *replica*. Because it is a
// real `World`, all the drawing code works on it unchanged: rendering cannot
// tell whether it is looking at the real thing (offline) or a copy (online).
//
// Networking concept: interpolation. Snapshots arrive in steps (every 33 ms at
// 30/s) and with jitter (the network delivers them a little early or late).
// Drawing the newest one the moment it arrives makes everything move in jerks.
// Instead the replica plays the match back from a small buffer, deliberately
// `delay` ticks BEHIND the newest snapshot, and draws the moment *between* the
// two snapshots around the playhead. It trades 100 ms of extra delay for smooth
// motion, and the buffer absorbs late packets.
//
// Pattern: Double Buffer. The world on screen (`world`) is never half-updated:
// it is rewritten as a whole from the two snapshots in `queue` once per frame.
//
// The terrain is the one part that is not rewritten: it is far too big for a
// snapshot. The replica starts from the untouched map and re-applies every
// `Edit` event the host reports, in order. Edits are deterministic (the same
// shape and operation on the same grid give the same voxels), so both copies
// stay identical; `Grid::checksum` is how a test, and later the network, can
// check that.

use std::collections::VecDeque;
use std::f32::consts::{PI, TAU};

use sdf::Grid;
use sim::{Event, World, DT};

use crate::{HostMsg, PlayerId, Snapshot, Welcome};

/// How hard the playhead speeds up or slows down to stay `delay` ticks behind
/// the host (per tick of error). Gentle, so the correction itself is invisible.
const CATCH_UP: f32 = 0.1;
/// If we are this many ticks off (the tab was hidden, a lag spike), do not
/// glide back: jump.
const MAX_ERROR: f32 = 30.0;
/// A worm that moved further than this (metres) between two snapshots
/// teleported or respawned; sliding it across the map would look wrong. Running
/// is 12 m/s, so a snapshot gap of even 10 ticks stays well inside.
const TELEPORT: f32 = 5.0;

pub struct Replica {
    /// What to draw.
    pub world: World,
    /// Which worm this client controls.
    pub me: PlayerId,
    /// Events whose moment has come but that nobody has shown yet.
    events: Vec<Event>,
    /// Ticks to stay behind the newest snapshot. 0 = no interpolation (the
    /// local player, whose "network" is a function call, does not need it).
    delay: u32,
    /// Snapshots in tick order. `queue[0]` is the one the playhead has passed
    /// (already shown); `queue[1]` is what we are moving towards.
    queue: VecDeque<Snapshot>,
    /// The tick being shown right now. Fractional: it moves smoothly between
    /// the whole ticks the snapshots are labelled with.
    playhead: f32,
    /// Seconds since the newest snapshot arrived. Snapshots come in steps, but
    /// time does not, so this makes the target move smoothly too.
    since_newest: f32,
}

impl Replica {
    /// `map` is the untouched terrain the match started from, built by the
    /// client's own copy of the generator (the `Welcome` does not carry it).
    pub fn new(welcome: Welcome, mut map: Grid, delay: u32) -> Replica {
        // The match may be minutes old and full of craters: lay its edits over
        // the fresh map.
        for (shape, op) in &welcome.edits {
            map.apply(*shape, *op);
        }
        // No spawn columns: the worms come from the snapshot, and a replica
        // never respawns anybody.
        let world = World::new(map, &[], 0);
        let playhead = welcome.snapshot.tick as f32;
        let mut replica = Replica {
            world,
            me: welcome.you,
            events: Vec::new(),
            delay,
            queue: VecDeque::from([welcome.snapshot]),
            playhead,
            since_newest: 0.0,
        };
        replica.present();
        replica
    }

    /// Feed in one message from the host.
    pub fn apply(&mut self, msg: HostMsg) {
        match msg {
            // A second Welcome means the host restarted the match; the caller
            // (who owns the map generator) builds a new `Replica` for it.
            HostMsg::Welcome(_) => {}
            // Only queued: nothing is drawn from it until `advance` gets there.
            HostMsg::Snapshot(s) => {
                self.queue.push_back(s);
                self.since_newest = 0.0;
            }
            // About the connection, not the match: the session reads it.
            HostMsg::Pong { .. } | HostMsg::Full => {}
        }
    }

    pub fn delay(&self) -> u32 {
        self.delay
    }

    pub fn set_delay(&mut self, delay: u32) {
        self.delay = delay;
    }

    /// Move the playhead forward by `seconds` of real time and update `world`.
    /// Call once per rendered frame, before drawing.
    pub fn advance(&mut self, seconds: f32) {
        // The queue is never empty: it always keeps the snapshot passed last.
        let newest = self.queue.back().map_or(0.0, |s| s.tick as f32);
        self.since_newest += seconds;

        if self.delay == 0 {
            self.playhead = newest;
        } else {
            // Where we want to be: `delay` ticks behind where the host is now
            // (the newest snapshot, plus the time that passed since it came).
            let target = newest + self.since_newest / DT - self.delay as f32;
            let step = seconds / DT; // one tick per 1/60 s of real time
            let error = target - (self.playhead + step);
            if error.abs() > MAX_ERROR {
                self.playhead = target;
            } else {
                // A bit faster than real time when behind and slower when
                // ahead, so jitter is smoothed out instead of shown.
                let rate = (1.0 + error * CATCH_UP).clamp(0.5, 1.5);
                self.playhead += step * rate;
            }
        }
        // Never show a future we have no data for.
        self.playhead = self.playhead.min(newest);
        self.present();
    }

    /// Fill `world` with the match as it looks at the playhead.
    fn present(&mut self) {
        // Everything the playhead has passed has now "happened" for this
        // client: release its events, and patch its terrain edits into the grid
        // (Pattern: Dirty Flag, the renderer re-uploads what changed) at the
        // same moment, so a crater opens exactly when its explosion is shown.
        while self.queue.len() >= 2 && self.queue[1].tick as f32 <= self.playhead {
            self.queue.pop_front();
            let passed = &mut self.queue[0];
            for event in &passed.events {
                if let Event::Edit { shape, op } = *event {
                    self.world.grid.apply(shape, op);
                }
            }
            self.events.append(&mut passed.events);
        }

        let from = &self.queue[0];
        self.playhead = self.playhead.max(from.tick as f32);
        self.world.tick = from.tick;
        // Overwrite, don't merge: the host is the authority, so its word is
        // final. (Over a lossy network like UDP we would also drop a snapshot
        // older than the one we already have. WebSocket is ordered, so we
        // don't need to.)
        self.world.worms = from.worms.clone();
        self.world.projectiles = from.projectiles.clone();
        self.world.zones = from.zones.clone();
        self.world.lava = from.lava.clone();

        // Projectiles fly in (nearly) straight lines, so instead of matching
        // them up between two snapshots (they have no id) we just move each one
        // on from where the older snapshot saw it. Lava is the same.
        let since_from = (self.playhead - from.tick as f32) * DT;
        for p in &mut self.world.projectiles {
            p.pos += p.vel * since_from;
        }
        for bit in &mut self.world.lava {
            bit.pos += bit.vel * since_from;
        }

        // Worms: slide from where they were towards where they will be.
        if let Some(to) = self.queue.get(1) {
            let t = (self.playhead - from.tick as f32) / (to.tick - from.tick) as f32;
            for (w, next) in self.world.worms.iter_mut().zip(&to.worms) {
                if w.alive && next.alive && w.pos.distance(next.pos) < TELEPORT {
                    w.pos = w.pos.lerp(next.pos, t);
                    w.yaw = lerp_angle(w.yaw, next.yaw, t);
                    w.pitch += (next.pitch - w.pitch) * t;
                    w.face_yaw = lerp_angle(w.face_yaw, next.face_yaw, t);
                }
            }
        }
    }

    /// Same idea as `World::drain_events`, for the events that arrived by network.
    pub fn drain_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }
}

/// Blend two angles the short way round (359° to 1° is a 2° turn, not 358°).
fn lerp_angle(from: f32, to: f32, t: f32) -> f32 {
    let diff = (to - from + PI).rem_euclid(TAU) - PI;
    from + diff * t
}

#[cfg(test)]
mod tests {
    use super::*;
    use glam::IVec3;
    use sdf::{Op, Shape};
    use sim::{vec3, Vec3};

    /// A small solid block of world: enough for worms to exist in, and for
    /// terrain edits to have something to change.
    fn map() -> Grid {
        Grid::new(IVec3::new(4, 4, 4))
    }

    fn world() -> World {
        World::new(map(), &[(8.0, 8.0), (12.0, 8.0)], 0)
    }

    /// Worm 0 stands at x = `tick` metres, so a position tells the tick (and
    /// stays under the teleport distance between snapshots).
    fn snapshot(tick: u32, events: Vec<Event>) -> Snapshot {
        let mut worms = world().worms;
        worms[0].pos = vec3(tick as f32, 0.0, 0.0);
        Snapshot { tick, worms, projectiles: Vec::new(), zones: Vec::new(), lava: Vec::new(), events }
    }

    fn replica(delay: u32) -> Replica {
        Replica::new(Welcome { you: 0, edits: Vec::new(), snapshot: snapshot(0, Vec::new()) }, map(), delay)
    }

    fn arrive(r: &mut Replica, tick: u32, events: Vec<Event>) {
        r.apply(HostMsg::Snapshot(snapshot(tick, events)));
    }

    fn crater() -> (Shape, Op) {
        (Shape::Sphere { center: vec3(8.0, 8.0, 8.0), radius: 2.0 }, Op::Carve)
    }

    #[test]
    fn the_playhead_blends_between_two_snapshots() {
        let mut r = replica(6);
        arrive(&mut r, 2, Vec::new());
        r.playhead = 1.0; // halfway between tick 0 and tick 2
        r.present();
        assert_eq!(r.world.worms[0].pos.x, 1.0);
    }

    #[test]
    fn events_wait_for_the_playhead() {
        let mut r = replica(6);
        let boom = Event::Explosion { weapon: 0, pos: Vec3::ZERO, normal: Vec3::Y, radius: 1.0 };
        arrive(&mut r, 2, vec![boom]);
        r.playhead = 1.0;
        r.present();
        assert!(r.drain_events().is_empty(), "the explosion is still in our future");
        r.playhead = 2.0;
        r.present();
        assert_eq!(r.drain_events().len(), 1);
        assert!(r.drain_events().is_empty(), "and it is shown once");
    }

    #[test]
    fn no_delay_means_the_newest_snapshot_at_once() {
        let mut r = replica(0);
        arrive(&mut r, 2, Vec::new());
        r.advance(0.0);
        assert_eq!(r.world.tick, 2);
        assert_eq!(r.world.worms[0].pos.x, 2.0);
    }

    #[test]
    fn playback_settles_the_chosen_delay_behind_the_host() {
        let mut r = replica(6);
        // Five seconds of 60 fps frames; the host sends a snapshot every other tick.
        for frame in 1..=300u32 {
            if frame % 2 == 0 {
                arrive(&mut r, frame, Vec::new());
            }
            r.advance(DT);
        }
        // Measured from where the host is right now: the newest snapshot plus the time since.
        let behind = 300.0 + r.since_newest / DT - r.playhead;
        assert!((behind - 6.0).abs() < 1.5, "playhead is {behind} ticks behind, wanted 6");
    }

    #[test]
    fn a_hidden_tab_catches_up_by_jumping() {
        let mut r = replica(6);
        for tick in (2..=600).step_by(2) {
            arrive(&mut r, tick, Vec::new()); // ten seconds arrive with no frame drawn
        }
        r.advance(DT);
        assert!(r.playhead > 590.0 - 6.0, "playhead jumped to {}", r.playhead);
    }

    #[test]
    fn angles_blend_the_short_way() {
        let a = lerp_angle(PI - 0.1, -PI + 0.1, 0.5);
        assert!((a.abs() - PI).abs() < 1e-4, "got {a}");
    }

    #[test]
    fn a_terrain_edit_waits_for_the_playhead_like_any_event() {
        let mut r = replica(6);
        let before = r.world.grid.checksum();
        let (shape, op) = crater();
        arrive(&mut r, 2, vec![Event::Edit { shape, op }]);
        r.playhead = 1.0;
        r.present();
        assert_eq!(r.world.grid.checksum(), before, "the crater is still in our future");
        r.playhead = 2.0;
        r.present();
        assert_ne!(r.world.grid.checksum(), before, "and opens when the playhead gets there");
    }

    #[test]
    fn a_newcomer_replays_the_edit_log() {
        let (shape, op) = crater();
        let mut host_map = map();
        host_map.apply(shape, op);
        let r = Replica::new(Welcome { you: 0, edits: vec![(shape, op)], snapshot: snapshot(0, Vec::new()) }, map(), 0);
        assert_eq!(r.world.grid.checksum(), host_map.checksum());
    }
}
