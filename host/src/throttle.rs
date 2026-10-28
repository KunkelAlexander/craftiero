// Sending fewer snapshots without losing anything that happened in between.
//
// Networking concept: snapshot rate. The sim runs at 60 Hz, but a snapshot
// every tick costs bandwidth: ~350 bytes x 60 = 21 KB/s per player, and the
// host's upload is the scarce resource (home connections are slow going up).
// Halving the rate halves the bill. The price is that a joiner sees the world
// in 30 Hz steps; smoothing that out is what interpolation (the next step) does.
//
// The catch: *state* can simply be skipped (the newest positions say it all),
// but *events* cannot. An explosion that happened on a skipped tick would never
// reach the friend. So the skipped ticks hand their events to the next snapshot
// that does go out.
//
// Pattern: Event Queue. Events wait here until the next send drains them.

use protocol::Snapshot;
use sim::Event;

pub struct Throttle {
    every: u32,
    /// Events from ticks whose snapshot was held back.
    held: Vec<Event>,
}

impl Throttle {
    /// `every = 2` sends every second tick (30 snapshots per second).
    pub fn new(every: u32) -> Throttle {
        Throttle { every: every.max(1), held: Vec::new() }
    }

    /// Feed in the snapshot of every tick. Returns the one to send, if this
    /// tick is one of those, carrying the events of the ticks it stands for.
    pub fn push(&mut self, mut snapshot: Snapshot) -> Option<Snapshot> {
        self.held.append(&mut snapshot.events);
        // Keyed on the sim tick, not a private counter, so every link that
        // throttles by the same amount sends on the same ticks.
        if snapshot.tick % self.every != 0 {
            return None;
        }
        snapshot.events = std::mem::take(&mut self.held);
        Some(snapshot)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(tick: u32, events: Vec<Event>) -> Snapshot {
        Snapshot { tick, worms: Vec::new(), projectiles: Vec::new(), zones: Vec::new(), lava: Vec::new(), events }
    }

    #[test]
    fn sends_every_nth_tick_and_keeps_every_event() {
        let mut throttle = Throttle::new(2);
        let boom = |x| Event::Explosion { weapon: 0, pos: sim::vec3(x, 0.0, 0.0), normal: sim::Vec3::Y, radius: 1.0 };

        assert!(throttle.push(snapshot(1, vec![boom(1.0)])).is_none());
        let sent = throttle.push(snapshot(2, vec![boom(2.0)])).expect("tick 2 is due");
        assert_eq!(sent.tick, 2);
        assert_eq!(sent.events.len(), 2, "the held event rode along");

        assert!(throttle.push(snapshot(3, Vec::new())).is_none());
        let sent = throttle.push(snapshot(4, Vec::new())).unwrap();
        assert!(sent.events.is_empty(), "events are delivered once, not repeated");
    }

    #[test]
    fn every_one_means_every_tick() {
        let mut throttle = Throttle::new(1);
        assert!(throttle.push(snapshot(7, Vec::new())).is_some());
        assert!(throttle.push(snapshot(8, Vec::new())).is_some());
    }
}
