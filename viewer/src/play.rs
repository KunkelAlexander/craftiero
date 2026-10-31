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

/// The seed of the sim's random numbers (weapon spread, bomblets) and of the
/// spawn places: different for every match, so no two start alike. (The
/// clock is only read here, in the viewer; the sim stays free of it.)
pub fn new_seed() -> u32 {
    let since_epoch = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).unwrap_or_default();
    since_epoch.subsec_nanos() ^ since_epoch.as_secs() as u32
}

pub struct Play {
    local: LocalGame,
    joined: Option<JoinGame>,
}

impl Play {
    /// A fresh offline match on `map`, the untouched map number `map_id`: you
    /// and `npcs` dummies. A match you will open to friends has `humans`
    /// seats for them.
    pub fn new(map: Grid, map_id: u8, humans: usize, npcs: usize) -> Play {
        Play { local: LocalGame::new(map, map_id, humans, npcs, new_seed()), joined: None }
    }

    /// The match behind the menu: you against three dummies, until the player
    /// picks how many people and NPCs they want.
    pub fn offline(map: Grid, map_id: u8) -> Play {
        Play::new(map, map_id, 1, 3)
    }

    /// Leave the local match for room `code`. `map` is the untouched map
    /// number `map_id`; the host's Welcome may name another, which is built then.
    pub fn join(&mut self, code: String, map: Grid, map_id: u8) {
        self.joined = Some(JoinGame::new(code, map, map_id));
    }

    /// Leave the local match for a new one the server hosts, on map number
    /// `map_id`. `map` is the untouched map (the server builds the same one).
    pub fn join_hosted(&mut self, map: Grid, map_id: u8, humans: u8, npcs: u8) {
        self.joined = Some(JoinGame::hosted(map, map_id, humans, npcs));
    }

    /// Open a room on the relay for the local match.
    pub fn open_room(&mut self) {
        self.local.open_room();
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
        // Only one of the two moves at a time (the local game is not stepped
        // while a join is on), and each one only ever counts up, so the sum
        // changes exactly when the replica on screen is a new one.
        self.local.generation() + self.joined.as_ref().map_or(0, JoinGame::generation)
    }

    /// Ask for another match, any time. Hosting or joined, it is the same
    /// request, and the host decides.
    pub fn rematch(&mut self) {
        match &mut self.joined {
            Some(joined) => joined.rematch(),
            None => self.local.rematch(),
        }
    }

    /// Tell the host which weapons this player picked for the match.
    pub fn loadout(&mut self, ids: Vec<u8>) {
        match &mut self.joined {
            Some(joined) => joined.loadout(ids),
            None => self.local.loadout(ids),
        }
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
