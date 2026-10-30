// Which match the player is in, and where it runs.
//
//   offline / hosting   a `LocalGame`: the Host runs in this process, and (once
//                       `open_room` is called) friends join it through the relay;
//   joined              a `JoinGame`: somebody else's Host, over the relay.
//
// Either way the screen draws a `Replica`, so the rest of the viewer asks `Play`
// for "the replica" and never learns which kind it is (see CLAUDE.md: clients
// always draw a Replica, even offline).
//
// The offline `LocalGame` is kept while joining: it is what is on screen behind
// the "connecting" notice, and the Replica of the joined match only exists once
// the host's Welcome has arrived.

use protocol::replica::Replica;
use sdf::{Grid, Op, Shape};
use sim::Input;

use crate::join::JoinGame;
use crate::local::LocalGame;
use crate::share::Share;
use crate::world;

/// The seed of the sim's random numbers (weapon spread, bomblets).
pub const SEED: u32 = 1;

pub struct Play {
    local: LocalGame,
    joined: Option<JoinGame>,
}

impl Play {
    /// A fresh offline match on `map`, the untouched map.
    pub fn offline(map: Grid) -> Play {
        Play { local: LocalGame::new(map, &world::SPAWNS, SEED), joined: None }
    }

    /// Leave the local match for room `code`. `map` is the untouched map.
    pub fn join(&mut self, code: String, map: Grid) {
        self.joined = Some(JoinGame::new(code, map));
    }

    /// Leave the local match for a new one the server hosts. `map` is the
    /// untouched map (the server builds the same one).
    pub fn join_hosted(&mut self, map: Grid) {
        self.joined = Some(JoinGame::hosted(map));
    }

    /// Open a room on the relay for the local match.
    pub fn open_room(&mut self) {
        self.local.open_room();
    }

    pub fn is_joined(&self) -> bool {
        self.joined.is_some()
    }

    pub fn joined(&self) -> Option<&JoinGame> {
        self.joined.as_ref()
    }

    /// The room this process hosts, if it does.
    pub fn room(&self) -> Option<&Share> {
        self.local.room()
    }

    /// What to draw: the joined match once it has begun, otherwise the local one.
    pub fn replica(&self) -> &Replica {
        self.joined.as_ref().and_then(JoinGame::replica).unwrap_or(&self.local.replica)
    }

    pub fn replica_mut(&mut self) -> &mut Replica {
        match &mut self.joined {
            Some(joined) if joined.replica().is_some() => joined.replica_mut().expect("just checked"),
            _ => &mut self.local.replica,
        }
    }

    /// Changes whenever `replica()` becomes a different `Replica` (a join
    /// starting, a resync rebuilding it). The renderer holds a GPU copy of one
    /// specific grid, so when this changes it has to be rebuilt.
    pub fn replica_id(&self) -> u32 {
        self.joined.as_ref().map_or(0, JoinGame::generation)
    }

    /// One fixed step with the player's input.
    pub fn tick(&mut self, input: Input) {
        match &mut self.joined {
            Some(joined) => joined.tick(input),
            None => self.local.tick(input),
        }
    }

    /// The infinite-ammo cheat. It changes the host's rules, so only the host
    /// can use it: a joiner's key press does nothing.
    pub fn toggle_infinite_ammo(&mut self) -> Option<bool> {
        self.joined.is_none().then(|| self.local.toggle_infinite_ammo())
    }

    /// The fly-mode brush. Same rule: only the host can edit the world.
    pub fn debug_edit(&mut self, shape: Shape, op: Op) {
        if self.joined.is_none() {
            self.local.debug_edit(shape, op);
        }
    }
}
