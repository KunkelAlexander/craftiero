// The whole game state and the one function that advances it.
//
// Pattern: Game Loop (fixed timestep). `step` always advances exactly 1/60 s.
// The caller (the game, a server, a test) decides how often to call it. A
// fixed step means physics behaves the same on a 30 Hz laptop and a 144 Hz
// monitor, and that two machines fed the same inputs compute the same result.
//
// Pattern: Update Method. Each kind of thing (player, projectile) has a step
// function that moves it one tick. Plain loops over plain structs, no engine.
//
// Pattern: Event Queue. When something noteworthy happens (a shot, an
// explosion, a death) the sim pushes an `Event`. It never plays a sound or
// spawns a particle itself. Audio, particles and the network each read the
// queue and react, and the sim stays ignorant of all of them.
//
// Networking note ("authority"): whoever calls `step` is the authority. It is
// the only place where state is allowed to change. Today that is the local
// game; in online play it is the host (a player's browser or, later, a
// dedicated server). Everyone else just receives the result.

use glam::{vec2, Vec2};
use serde::{Deserialize, Serialize};

use crate::input::Input;
use crate::rng::Rng;
use crate::tilemap::{tile_center, tile_coords, Tile, Tilemap, BRICK_HP, TILE};
use crate::weapons::{Kind, Special, WeaponDef, WEAPONS};

pub const DT: f32 = 1.0 / 60.0;
pub const PLAYER_RADIUS: f32 = 10.0;
const PROJECTILE_RADIUS: f32 = 3.0;
const MOVE_SPEED: f32 = 220.0;
/// How quickly velocity chases the wanted velocity. The same rule also bleeds
/// off knockback, so being blown across the room needs no special code.
const ACCEL: f32 = 12.0;
const MAX_SPEED: f32 = 900.0;
pub const MAX_HP: f32 = 100.0;
pub const RESPAWN_TIME: f32 = 3.0;
const SPAWN_INVULN: f32 = 2.0;
/// The default kills to win. A match can choose another (`World::score_limit`).
pub const SCORE_LIMIT: i32 = 15;
/// You hurt yourself half as much as others, but get thrown just as far, so
/// explosions work as a movement tool without being free.
const SELF_DAMAGE: f32 = 0.5;
/// A beam hits a player whose body is this close to its line, not only one it
/// crosses. Networking note: the host sees players where they are now, but a
/// joiner aims at a picture that is ~100 ms of interpolation plus their own lag
/// in the past. An instant beam exactly as wide as a body would land far more
/// often for the host than for friends; a little slack evens that out. (The
/// full answer is lag compensation: the host rewinds to what the shooter saw.)
const BEAM_SLACK: f32 = 4.0;

// The dash has no button: let go of the movement keys (or lift the move thumb),
// then push again, and the first push is a burst of speed. Phones play with two
// thumbsticks and have no thumb left for a dash button, so the gesture IS the
// button, and it works the same on a keyboard.
/// How long the stick must rest before a push counts as a dash. Long enough
/// that switching from W to D (a gap of a frame or two) never dashes.
const DASH_REST: f32 = 0.12;
/// A thumb on a floating stick starts at zero and takes a few frames to reach a
/// full push, so the push may arrive this long after the rest ends.
const DASH_WINDOW: f32 = 0.15;
/// How far the stick must be pushed: enough to show which way you mean.
const DASH_PUSH: f32 = 0.6;
/// Speed at the start of a dash. The usual `ACCEL` rule then slows you back
/// to walking speed within a quarter second, which adds about 1.5 tiles to the
/// distance travelled: a dodge, not a teleport. Below `MAX_SPEED`.
const DASH_SPEED: f32 = 850.0;
const DASH_COOLDOWN: f32 = 1.2;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WeaponState {
    /// Index into `WEAPONS`.
    pub id: u8,
    pub ammo: u8,
    pub cooldown: f32,
    /// Seconds of reload left; > 0 means "reloading". Each weapon reloads on
    /// its own timer, so you can switch away and it keeps reloading.
    pub reload: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Player {
    pub pos: Vec2,
    pub vel: Vec2,
    pub aim: f32,
    pub hp: f32,
    pub alive: bool,
    pub respawn_timer: f32,
    /// Seconds of spawn protection left.
    pub invuln: f32,
    pub weapons: Vec<WeaponState>,
    /// Index into `weapons`.
    pub current: usize,
    pub kills: i32,
    pub deaths: i32,
    /// Seconds the move input has been released (see `DASH_REST`).
    pub still: f32,
    /// Seconds left in which a push still counts as a dash (see `DASH_WINDOW`).
    pub dash_armed: f32,
    /// Seconds until the next dash is allowed.
    pub dash_cooldown: f32,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Projectile {
    pub pos: Vec2,
    pub vel: Vec2,
    pub weapon: u8,
    pub owner: u8,
    /// Seconds left to live.
    pub life: f32,
}

/// Things that just happened. Consumers read these; the sim never reads them.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Event {
    Shot { player: u8, weapon: u8, pos: Vec2 },
    Beam { from: Vec2, to: Vec2 },
    Explosion { pos: Vec2, radius: f32 },
    Hit { victim: u8, pos: Vec2, damage: f32 },
    /// A brick got damaged or destroyed. This doubles as the "dirty flag" of
    /// the terrain: online, only these changes need to be sent, never the
    /// whole map.
    TileChanged { x: i32, y: i32, tile: Tile },
    Died { victim: u8, killer: u8, pos: Vec2 },
    /// `player` jumped from `from` to `to` (the Teleport gun).
    Teleported { player: u8, from: Vec2, to: Vec2 },
    /// `player` dashed from `pos` in direction `dir`.
    Dashed { player: u8, pos: Vec2, dir: Vec2 },
    Respawned { player: u8 },
    MatchWon { player: u8 },
}

pub struct World {
    pub map: Tilemap,
    pub players: Vec<Player>,
    pub projectiles: Vec<Projectile>,
    /// Ticks simulated so far. Online, this is the shared clock that snapshots
    /// and inputs are labelled with ("tick 1234").
    pub tick: u32,
    pub winner: Option<u8>,
    /// Kills needed to win. A rule of this match, set by whoever creates it.
    pub score_limit: i32,
    events: Vec<Event>,
    rng: Rng,
}

impl World {
    /// `loadout` lists the weapon ids every player carries (a per-player
    /// loadout comes with the loadout screen in M4).
    pub fn new(map: Tilemap, player_count: usize, loadout: &[u8], seed: u32) -> World {
        let players = (0..player_count)
            .map(|i| Player {
                pos: map.spawns[i % map.spawns.len()],
                vel: Vec2::ZERO,
                aim: 0.0,
                hp: MAX_HP,
                alive: true,
                respawn_timer: 0.0,
                invuln: 0.0,
                weapons: loadout.iter().map(|&id| full_weapon(id)).collect(),
                current: 0,
                kills: 0,
                deaths: 0,
                still: 0.0,
                dash_armed: 0.0,
                dash_cooldown: 0.0,
            })
            .collect();
        World {
            map,
            players,
            projectiles: Vec::with_capacity(64),
            tick: 0,
            winner: None,
            score_limit: SCORE_LIMIT,
            events: Vec::new(),
            rng: Rng::new(seed),
        }
    }

    /// Replace a player's weapons with the ones they chose, all fully loaded.
    /// The world does not judge the choice; whoever calls this (the host) has
    /// already checked it with `weapons::valid_loadout`.
    pub fn set_loadout(&mut self, player: usize, loadout: &[u8]) {
        let p = &mut self.players[player];
        p.weapons = loadout.iter().map(|&id| full_weapon(id)).collect();
        p.current = 0;
    }

    /// Take the events produced since the last call.
    pub fn drain_events(&mut self) -> Vec<Event> {
        std::mem::take(&mut self.events)
    }

    /// Advance the world by one tick. `inputs[i]` belongs to player `i`.
    /// A missing input (shorter slice) counts as "no buttons pressed", which is
    /// also exactly what we want for a player whose packet has not arrived.
    pub fn step(&mut self, inputs: &[Input]) {
        if self.winner.is_some() {
            return; // match over: freeze the world
        }
        for i in 0..self.players.len() {
            let input = inputs.get(i).copied().unwrap_or_default();
            self.step_player(i, &input);
        }
        self.step_projectiles();
        self.tick += 1;
    }

    // ---------------------------------------------------------------- players

    fn step_player(&mut self, i: usize, input: &Input) {
        if !self.players[i].alive {
            self.players[i].respawn_timer -= DT;
            if self.players[i].respawn_timer <= 0.0 {
                self.respawn(i);
            }
            return;
        }

        let p = &mut self.players[i];
        p.invuln = (p.invuln - DT).max(0.0);
        if input.aim.is_finite() {
            p.aim = input.aim;
        }

        for w in &mut p.weapons {
            w.cooldown = (w.cooldown - DT).max(0.0);
            if w.reload > 0.0 {
                w.reload -= DT;
                if w.reload <= 0.0 {
                    w.reload = 0.0;
                    w.ammo = WEAPONS[w.id as usize].mag;
                }
            }
        }
        if (input.weapon as usize) < p.weapons.len() {
            p.current = input.weapon as usize;
        }

        // Movement. Never trust the input: clamp the length so nobody can
        // send move_dir = (1000, 0) and run 1000x faster.
        let dir = if input.move_dir.is_finite() { input.move_dir.clamp_length_max(1.0) } else { Vec2::ZERO };
        let wanted = dir * MOVE_SPEED;
        p.vel += (wanted - p.vel) * (ACCEL * DT).min(1.0);

        // Dash: resting arms it, pushing uses it up (see `DASH_REST`).
        p.dash_cooldown = (p.dash_cooldown - DT).max(0.0);
        let mut dashed = false;
        if dir.length() < 0.2 {
            p.still += DT;
            if p.still >= DASH_REST {
                p.dash_armed = DASH_WINDOW;
            }
        } else {
            p.still = 0.0;
            if p.dash_armed > 0.0 && dir.length() >= DASH_PUSH && p.dash_cooldown == 0.0 {
                // Set, not add: a dash out of a blast's knockback goes where
                // you point, which is what makes it an escape.
                p.vel = dir.normalize() * DASH_SPEED;
                p.dash_armed = 0.0;
                p.dash_cooldown = DASH_COOLDOWN;
                dashed = true;
            }
            p.dash_armed = (p.dash_armed - DT).max(0.0);
        }
        p.vel = p.vel.clamp_length_max(MAX_SPEED);

        // Move one axis at a time so a player slides along walls instead of
        // sticking to them.
        let nx = p.pos + vec2(p.vel.x * DT, 0.0);
        if self.map.circle_hits_wall(nx, PLAYER_RADIUS) {
            p.vel.x = 0.0;
        } else {
            p.pos = nx;
        }
        let ny = p.pos + vec2(0.0, p.vel.y * DT);
        if self.map.circle_hits_wall(ny, PLAYER_RADIUS) {
            p.vel.y = 0.0;
        } else {
            p.pos = ny;
        }
        if dashed {
            let (pos, vel) = (p.pos, p.vel);
            self.events.push(Event::Dashed { player: i as u8, pos, dir: vel.normalize_or_zero() });
        }

        if input.fire {
            self.try_fire(i);
        }
    }

    fn try_fire(&mut self, i: usize) {
        let p = &self.players[i];
        let (pos, aim, slot) = (p.pos, p.aim, p.current);
        let w = &p.weapons[slot];
        if w.cooldown > 0.0 || w.reload > 0.0 || w.ammo == 0 {
            return;
        }
        let id = w.id;
        let def = &WEAPONS[id as usize];
        let dir = vec2(aim.cos(), aim.sin());
        // Find the build spot before spending a charge: aiming at a wall
        // should not cost you a block.
        let build_at = match def.kind {
            Kind::Build => match self.build_spot(pos, dir, def.range) {
                Some(spot) => Some(spot),
                None => return,
            },
            _ => None,
        };

        // Shooting gives up spawn protection: it is there to let you get your
        // bearings, not to fight for two seconds without being hurt.
        self.players[i].invuln = 0.0;
        let w = &mut self.players[i].weapons[slot];
        w.ammo -= 1;
        w.cooldown = def.cooldown;
        if w.ammo == 0 {
            w.reload = def.reload; // auto-reload as soon as the magazine empties
        }

        // A weapon with no speed (the bomb) is dropped at your feet, not thrown.
        // A block "fires" where it appears, so the flash marks the new brick.
        let muzzle = match build_at {
            Some((tx, ty)) => tile_center(tx, ty),
            None if def.kind == Kind::Projectile && def.speed == 0.0 => pos,
            None => pos + dir * (PLAYER_RADIUS + 4.0),
        };
        self.events.push(Event::Shot { player: i as u8, weapon: id, pos: muzzle });

        match def.kind {
            // A swarm is one shot that makes several projectiles, each with
            // its own random error.
            Kind::Projectile => {
                for _ in 0..def.pellets {
                    let angle = aim + self.rng.range(-def.spread, def.spread);
                    self.projectiles.push(Projectile {
                        pos: muzzle,
                        vel: vec2(angle.cos(), angle.sin()) * def.speed,
                        weapon: id,
                        owner: i as u8,
                        life: def.lifetime,
                    });
                }
            }
            Kind::Beam => {
                let angle = aim + self.rng.range(-def.spread, def.spread);
                self.fire_beam(i, def, muzzle, vec2(angle.cos(), angle.sin()));
            }
            // A placed block is an ordinary brick, so it needs no new rules,
            // drawing or network message: joiners learn about it from the same
            // `TileChanged` event that reports a destroyed one.
            Kind::Build => {
                if let Some((tx, ty)) = build_at {
                    self.map.set(tx, ty, Tile::Brick(BRICK_HP));
                    self.events.push(Event::TileChanged { x: tx, y: ty, tile: Tile::Brick(BRICK_HP) });
                }
            }
        }
    }

    /// Where the build tool puts its brick: the first empty tile along the aim,
    /// within `range`, that no living player touches (a brick on top of someone
    /// would trap them inside a wall) and that is not a spawn point (or someone
    /// could wall it up and respawn inside it). A wall on the way ends the
    /// search: you cannot build on the far side of one.
    fn build_spot(&self, from: Vec2, dir: Vec2, range: f32) -> Option<(i32, i32)> {
        let mut d = 0.0;
        while d <= range {
            let (tx, ty) = tile_coords(from + dir * d);
            if self.map.get(tx, ty).blocks() {
                return None;
            }
            let min = vec2(tx as f32 * TILE, ty as f32 * TILE);
            let occupied = self
                .players
                .iter()
                .any(|p| p.alive && p.pos.clamp(min, min + Vec2::splat(TILE)).distance(p.pos) < PLAYER_RADIUS);
            if !occupied && !self.map.spawns.contains(&tile_center(tx, ty)) {
                return Some((tx, ty));
            }
            d += 4.0;
        }
        None
    }

    /// Hitscan: no flying object, the line is resolved instantly.
    fn fire_beam(&mut self, owner: usize, def: &WeaponDef, origin: Vec2, dir: Vec2) {
        // 1) The beam ends at the first wall or brick, and leaves it whole.
        let end = origin + dir * self.map.distance_to_wall(origin, dir, def.range);
        self.events.push(Event::Beam { from: origin, to: end });

        // 2) Everything within reach of that line gets hit. The beam pierces
        //    players, so it is not a "first hit only" test.
        for v in 0..self.players.len() {
            if v == owner || !self.players[v].alive {
                continue;
            }
            if point_segment_distance(self.players[v].pos, origin, end) < PLAYER_RADIUS + BEAM_SLACK {
                self.damage_player(v, def.damage, owner, dir * def.knockback);
            }
        }
    }

    fn respawn(&mut self, i: usize) {
        // Pick the spawn point farthest from living enemies, with a little
        // random jitter so ties are not always broken the same way.
        let enemies: Vec<Vec2> = (0..self.players.len())
            .filter(|&j| j != i && self.players[j].alive)
            .map(|j| self.players[j].pos)
            .collect();
        let mut best = (f32::MIN, self.map.spawns[0]);
        for &s in &self.map.spawns.clone() {
            let nearest = enemies.iter().map(|e| e.distance(s)).fold(f32::MAX, f32::min);
            let score = nearest.min(2000.0) + self.rng.range(0.0, 50.0);
            if score > best.0 {
                best = (score, s);
            }
        }
        let p = &mut self.players[i];
        p.pos = best.1;
        p.vel = Vec2::ZERO;
        p.hp = MAX_HP;
        p.alive = true;
        p.invuln = SPAWN_INVULN;
        p.weapons = p.weapons.iter().map(|w| full_weapon(w.id)).collect();
        self.events.push(Event::Respawned { player: i as u8 });
    }

    /// The single place where a player loses health. Funnelling every source of
    /// damage through one function means self-damage, spawn protection, kill
    /// credit and scoring live in exactly one spot.
    fn damage_player(&mut self, victim: usize, amount: f32, source: usize, impulse: Vec2) {
        let p = &mut self.players[victim];
        if !p.alive || p.invuln > 0.0 {
            return;
        }
        let amount = if victim == source { amount * SELF_DAMAGE } else { amount };
        p.hp -= amount;
        p.vel += impulse; // knockback is NOT reduced for self-hits
        let pos = p.pos;
        let dead = p.hp <= 0.0;
        if amount > 0.0 {
            self.events.push(Event::Hit { victim: victim as u8, pos, damage: amount });
        }
        if dead {
            let p = &mut self.players[victim];
            p.alive = false;
            p.hp = 0.0;
            p.respawn_timer = RESPAWN_TIME;
            p.deaths += 1;
            // Killing yourself costs a point; killing someone else earns one.
            self.players[source].kills += if victim == source { -1 } else { 1 };
            self.events.push(Event::Died { victim: victim as u8, killer: source as u8, pos });
            let score = self.players[source].kills;
            if score >= self.score_limit && self.winner.is_none() {
                self.winner = Some(source as u8);
                self.events.push(Event::MatchWon { player: source as u8 });
            }
        }
    }

    // ------------------------------------------------------------ projectiles

    fn step_projectiles(&mut self) {
        // Take the list out so we can call `&mut self` methods while looping,
        // then put the survivors back.
        let old = std::mem::take(&mut self.projectiles);
        let mut keep = Vec::with_capacity(old.len());
        'next: for mut pr in old {
            let def = &WEAPONS[pr.weapon as usize];
            pr.life -= DT;
            if let Special::Homing(turn) = def.special {
                self.steer(&mut pr, turn);
            }

            // Move in sub-steps of at most 8 px so a fast bolt cannot jump
            // over a thin wall or a player between two ticks ("tunnelling").
            let n = ((pr.vel.length() * DT) / 8.0).ceil().max(1.0) as i32;
            let mut step = pr.vel * DT / n as f32;
            let mut dir = pr.vel.normalize_or_zero();
            for _ in 0..n {
                pr.pos += step;

                let (tx, ty) = tile_coords(pr.pos);
                if self.map.get(tx, ty).blocks() {
                    if def.bounce {
                        // Ricochet: undo the step, then find which wall face we
                        // hit by trying each axis on its own, and mirror that
                        // part of the velocity.
                        pr.pos -= step;
                        let hit_x = self.map.tile_at(pr.pos + vec2(step.x, 0.0)).blocks();
                        let hit_y = self.map.tile_at(pr.pos + vec2(0.0, step.y)).blocks();
                        // Neither axis alone is blocked: we came at a corner,
                        // so bounce straight back.
                        let corner = !hit_x && !hit_y;
                        if hit_x || corner {
                            pr.vel.x = -pr.vel.x;
                            step.x = -step.x;
                        }
                        if hit_y || corner {
                            pr.vel.y = -pr.vel.y;
                            step.y = -step.y;
                        }
                        dir = pr.vel.normalize_or_zero();
                        continue;
                    }
                    self.impact(&pr, def, dir, None, Some((tx, ty)));
                    continue 'next;
                }
                // A Cluster shell flies through players and bursts only at a
                // wall or at the end of its fuse. Bursting on someone put all
                // seven bomblets on them at once: a one-shot kill up close,
                // which is not what an area weapon is for.
                let passes_players = matches!(def.special, Special::Cluster(..));
                let hit = (0..self.players.len()).find(|&j| {
                    !passes_players
                        && j != pr.owner as usize
                        && self.players[j].alive
                        && self.players[j].pos.distance(pr.pos) < PLAYER_RADIUS + PROJECTILE_RADIUS
                });
                if let Some(j) = hit {
                    self.impact(&pr, def, dir, Some(j), None);
                    continue 'next;
                }
            }

            if pr.life <= 0.0 {
                // The end of the flight is an impact with nothing: shells burst,
                // orbs land and rockets blow up where they are.
                if def.blast_radius > 0.0 || matches!(def.special, Special::Cluster(..) | Special::Teleport) {
                    self.impact(&pr, def, dir, None, None);
                }
                continue;
            }
            keep.push(pr);
        }
        // A burst this tick put its children into the (emptied) list: keep them.
        keep.append(&mut self.projectiles);
        self.projectiles = keep;
    }

    /// Homing: turn the velocity towards the nearest living enemy, by at most
    /// `turn` radians per second. The speed never changes, only the heading, so
    /// a missile can be out-run around a corner and it still dies on walls.
    fn steer(&self, pr: &mut Projectile, turn: f32) {
        let target = (0..self.players.len())
            .filter(|&j| j != pr.owner as usize && self.players[j].alive)
            .map(|j| self.players[j].pos)
            .min_by(|a, b| a.distance_squared(pr.pos).total_cmp(&b.distance_squared(pr.pos)));
        let Some(target) = target else { return };
        let want = target - pr.pos;
        let diff = want.y.atan2(want.x) - pr.vel.y.atan2(pr.vel.x);
        // Wrap to -PI..PI so it always turns the short way round.
        let diff = (diff + std::f32::consts::PI).rem_euclid(std::f32::consts::TAU) - std::f32::consts::PI;
        let (sin, cos) = diff.clamp(-turn * DT, turn * DT).sin_cos();
        pr.vel = vec2(pr.vel.x * cos - pr.vel.y * sin, pr.vel.x * sin + pr.vel.y * cos);
    }

    /// The nearest spot to `pos`, walking back along `-dir`, where a circle of
    /// `radius` fits. A projectile that hit a wall is a few pixels inside it.
    fn free_point(&self, pos: Vec2, dir: Vec2, radius: f32) -> Option<Vec2> {
        (0..20).map(|k| pos - dir * (2.0 * k as f32)).find(|&p| !self.map.circle_hits_wall(p, radius))
    }

    /// Cluster: the shell splits into `count` children, spread evenly around the
    /// circle with a random twist so no two bursts look alike. The twist comes
    /// from the sim's own `rng`, so every machine bursts the same way.
    fn burst(&mut self, pr: &Projectile, dir: Vec2, count: u8, child: u8) {
        let def = &WEAPONS[child as usize];
        let at = self.free_point(pr.pos, dir, PROJECTILE_RADIUS).unwrap_or(pr.pos);
        let twist = self.rng.range(0.0, std::f32::consts::TAU);
        for k in 0..count {
            let a = twist + std::f32::consts::TAU * k as f32 / count as f32;
            self.projectiles.push(Projectile {
                pos: at,
                vel: vec2(a.cos(), a.sin()) * def.speed * self.rng.range(0.6, 1.0),
                weapon: child,
                owner: pr.owner,
                life: def.lifetime * self.rng.range(0.7, 1.0),
            });
        }
    }

    /// Teleport: the owner appears where the orb landed, or swaps places with
    /// the player it hit. If the orb landed in a spot too tight to stand in,
    /// nothing happens (a fizzle), which is better than getting stuck in a wall.
    fn teleport(&mut self, pr: &Projectile, dir: Vec2, victim: Option<usize>) {
        let owner = pr.owner as usize;
        if !self.players[owner].alive {
            return; // shot, then died before it landed
        }
        let from = self.players[owner].pos;
        let to = match victim {
            Some(v) => {
                let to = self.players[v].pos;
                self.players[v].pos = from;
                self.players[v].vel = Vec2::ZERO;
                Some(to)
            }
            None => self.free_point(pr.pos, dir, PLAYER_RADIUS),
        };
        let Some(to) = to else { return };
        self.players[owner].pos = to;
        self.players[owner].vel = Vec2::ZERO;
        self.events.push(Event::Teleported { player: owner as u8, from, to });
    }

    /// A projectile ended its flight against a wall or a player.
    fn impact(&mut self, pr: &Projectile, def: &WeaponDef, dir: Vec2, victim: Option<usize>, wall: Option<(i32, i32)>) {
        match def.special {
            Special::Cluster(count, child) => return self.burst(pr, dir, count, child),
            Special::Teleport => return self.teleport(pr, dir, victim),
            _ => {}
        }
        if def.blast_radius > 0.0 {
            // A shell that hit a wall is a few pixels inside it. Blow it up on
            // the side it came from, so the wall it hit is between the blast
            // and whoever hides behind it.
            let at = self.free_point(pr.pos, dir, PROJECTILE_RADIUS).unwrap_or(pr.pos);
            self.explode(at, def, pr.owner as usize);
            return;
        }
        if let Some(v) = victim {
            self.damage_player(v, def.damage, pr.owner as usize, dir * def.knockback);
        }
        if let Some((tx, ty)) = wall {
            if def.tile_damage > 0 {
                self.hurt_tile(tx, ty, def.tile_damage);
            }
        }
    }

    /// Damage falls off linearly with distance, and only reaches players who
    /// can see the blast: walls and bricks are cover. That is what makes a
    /// built block an answer to a rocket or a Nova. Players are hurt before
    /// the bricks are, so the brick that a blast destroys still shields you
    /// from that blast: it absorbs exactly one hit.
    fn explode(&mut self, pos: Vec2, def: &WeaponDef, owner: usize) {
        self.events.push(Event::Explosion { pos, radius: def.blast_radius });

        for v in 0..self.players.len() {
            let offset = self.players[v].pos - pos;
            let reach = def.blast_radius + PLAYER_RADIUS;
            let d = offset.length();
            if !self.players[v].alive || d >= reach || !self.map.line_of_sight(pos, self.players[v].pos) {
                continue;
            }
            let falloff = 1.0 - d / reach;
            let impulse = offset.normalize_or_zero() * def.knockback * falloff;
            self.damage_player(v, def.blast_damage * falloff, owner, impulse);
        }

        if def.tile_damage > 0 {
            let (x0, y0) = tile_coords(pos - Vec2::splat(def.blast_radius));
            let (x1, y1) = tile_coords(pos + Vec2::splat(def.blast_radius));
            for ty in y0..=y1 {
                for tx in x0..=x1 {
                    if tile_center(tx, ty).distance(pos) < def.blast_radius + TILE * 0.25 {
                        self.hurt_tile(tx, ty, def.tile_damage);
                    }
                }
            }
        }
    }


    fn hurt_tile(&mut self, tx: i32, ty: i32, amount: u8) {
        if let Some(tile) = self.map.damage(tx, ty, amount) {
            self.events.push(Event::TileChanged { x: tx, y: ty, tile });
        }
    }
}

fn full_weapon(id: u8) -> WeaponState {
    WeaponState { id, ammo: WEAPONS[id as usize].mag, cooldown: 0.0, reload: 0.0 }
}

fn point_segment_distance(p: Vec2, a: Vec2, b: Vec2) -> f32 {
    let ab = b - a;
    let len2 = ab.length_squared();
    let t = if len2 == 0.0 { 0.0 } else { ((p - a).dot(ab) / len2).clamp(0.0, 1.0) };
    p.distance(a + ab * t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tilemap::MAP_GRID;
    use crate::weapons::{ARSENAL, BLASTER, BOMB, BOMBLET, BOUNCER, BUILD, CLUSTER, NOVA, RAILGUN, ROCKET, SWARM, TELEPORT};

    // These tests need no window, GPU or sound: the payoff of keeping the sim
    // free of rendering code.

    fn two_player_world() -> World {
        let mut w = World::new(Tilemap::from_ascii(MAP_GRID), 2, &[BLASTER, ROCKET, RAILGUN], 1);
        // Row 1 of the Grid map: tiles 2 and 3 are empty, 4 and 5 are bricks, 6 is empty.
        w.players[0].pos = tile_center(1, 1);
        w.players[1].pos = tile_center(3, 1);
        w
    }

    fn fire_once(w: &mut World, who: usize, weapon: u8, aim: f32) {
        let mut inputs = vec![Input::default(); w.players.len()];
        inputs[who] = Input { fire: true, weapon, aim, ..Default::default() };
        w.step(&inputs);
        for _ in 0..30 {
            w.step(&[]);
        }
    }

    #[test]
    fn blaster_bolt_damages_enemy() {
        let mut w = two_player_world();
        fire_once(&mut w, 0, 0, 0.0);
        assert!(w.players[1].hp < MAX_HP);
    }

    #[test]
    fn killing_scores_and_victim_respawns() {
        let mut w = two_player_world();
        w.players[1].hp = 5.0;
        fire_once(&mut w, 0, 0, 0.0);
        assert!(!w.players[1].alive);
        assert_eq!(w.players[0].kills, 1);
        for _ in 0..(RESPAWN_TIME / DT) as i32 + 5 {
            w.step(&[]);
        }
        assert!(w.players[1].alive);
        assert_eq!(w.players[1].hp, MAX_HP);
    }

    #[test]
    fn rocket_destroys_brick_and_reports_it() {
        let mut w = two_player_world();
        w.players[1].pos = tile_center(30, 10); // out of the way
        w.drain_events();
        fire_once(&mut w, 0, 1, 0.0);
        assert_eq!(w.map.get(4, 1), Tile::Empty);
        let events = w.drain_events();
        assert!(events.iter().any(|e| matches!(e, Event::TileChanged { x: 4, y: 1, tile: Tile::Empty })));
    }

    #[test]
    fn rocket_at_own_feet_is_halved_and_scores_nothing() {
        let mut w = two_player_world();
        w.players[1].pos = tile_center(30, 10);
        w.players[0].pos = tile_center(1, 1);
        // Aim at the wall right behind us (left).
        fire_once(&mut w, 0, 1, std::f32::consts::PI);
        let p = &w.players[0];
        assert!(p.hp < MAX_HP && p.hp > MAX_HP - 40.0, "self damage is halved, hp = {}", p.hp);
        assert_eq!(w.players[0].kills, 0); // surviving a self-hit scores nothing
    }

    #[test]
    fn railgun_stops_at_a_brick_and_leaves_it_whole() {
        let mut w = two_player_world();
        w.players[1].pos = tile_center(6, 1); // behind bricks at tiles 4 and 5
        fire_once(&mut w, 0, 2, 0.0);
        assert_eq!(w.map.get(4, 1), Tile::Brick(BRICK_HP));
        assert_eq!(w.players[1].hp, MAX_HP, "a brick is cover from the rail");
    }

    #[test]
    fn railgun_forgives_a_near_miss() {
        // 12 px off the beam's line: outside the body (10 px), inside the
        // beam's extra width.
        let mut w = World::new(Tilemap::from_ascii(crate::tilemap::MAP_HUB), 2, &[RAILGUN], 1);
        w.players[0].pos = tile_center(5, 4);
        w.players[1].pos = tile_center(15, 4) + vec2(0.0, 12.0);
        fire_once(&mut w, 0, 0, 0.0);
        assert!(w.players[1].hp < MAX_HP);
    }

    #[test]
    fn firing_ends_spawn_protection() {
        let mut w = two_player_world();
        w.players[0].invuln = SPAWN_INVULN;
        w.step(&[Input { fire: true, ..Default::default() }]);
        assert_eq!(w.players[0].invuln, 0.0);
    }

    #[test]
    fn bouncer_kills_in_four_hits() {
        let damage = WEAPONS[BOUNCER as usize].damage;
        assert!(4.0 * damage >= MAX_HP && 3.0 * damage < MAX_HP);
    }

    #[test]
    fn empty_magazine_triggers_reload() {
        let mut w = two_player_world();
        w.players[1].pos = tile_center(30, 10);
        // Railgun has a 1-round magazine.
        let shoot = [Input { fire: true, weapon: 2, ..Default::default() }];
        w.step(&shoot);
        assert_eq!(w.players[0].weapons[2].ammo, 0);
        assert!(w.players[0].weapons[2].reload > 0.0);
        for _ in 0..(2.6 / DT) as i32 {
            w.step(&[Input { weapon: 2, ..Default::default() }]);
        }
        assert_eq!(w.players[0].weapons[2].ammo, 1);
    }

    /// Two players in the Grid map's top-left corner with every weapon, and the
    /// slot number of `weapon` in that loadout.
    fn armed_world(weapon: u8) -> (World, u8) {
        let mut w = World::new(Tilemap::from_ascii(MAP_GRID), 2, &ARSENAL, 1);
        w.players[0].pos = tile_center(1, 1);
        w.players[1].pos = tile_center(3, 1);
        (w, ARSENAL.iter().position(|&id| id == weapon).unwrap() as u8)
    }

    #[test]
    fn block_is_built_on_the_first_free_tile_in_front() {
        let (mut w, slot) = armed_world(BUILD);
        w.drain_events();
        // Aim down: our own tile is taken by us, so the block lands one below.
        w.step(&[Input { fire: true, weapon: slot, aim: std::f32::consts::FRAC_PI_2, ..Default::default() }]);
        assert_eq!(w.map.get(1, 2), Tile::Brick(BRICK_HP));
        assert_eq!(w.players[0].weapons[slot as usize].ammo, WEAPONS[BUILD as usize].mag - 1);
        assert!(w.drain_events().iter().any(|e| matches!(e, Event::TileChanged { x: 1, y: 2, .. })));
    }

    #[test]
    fn building_into_a_wall_costs_nothing() {
        let (mut w, slot) = armed_world(BUILD);
        w.step(&[Input { fire: true, weapon: slot, aim: std::f32::consts::PI, ..Default::default() }]);
        assert_eq!(w.players[0].weapons[slot as usize].ammo, WEAPONS[BUILD as usize].mag);
    }

    #[test]
    fn a_block_never_traps_a_player() {
        let (mut w, slot) = armed_world(BUILD);
        w.players[1].pos = tile_center(2, 1); // standing right in front of us
        w.step(&[Input { fire: true, weapon: slot, ..Default::default() }]);
        assert_eq!(w.map.get(2, 1), Tile::Empty, "not on top of the enemy");
        assert_eq!(w.map.get(3, 1), Tile::Brick(BRICK_HP), "but just behind them");
    }

    #[test]
    fn a_block_absorbs_one_rocket_blast() {
        // A rocket fired at a player with a block right in front of them: the
        // blast is close enough to hurt, but the block is in between.
        let mut w = World::new(Tilemap::from_ascii(crate::tilemap::MAP_HUB), 2, &[ROCKET], 1);
        w.players[0].pos = tile_center(20, 4);
        w.players[1].pos = tile_center(10, 4);
        w.map.set(11, 4, Tile::Brick(BRICK_HP));
        let fire_left = |w: &mut World| {
            w.step(&[Input { fire: true, aim: std::f32::consts::PI, ..Default::default() }]);
            for _ in 0..90 {
                w.step(&[]);
            }
        };
        fire_left(&mut w);
        assert_eq!(w.players[1].hp, MAX_HP, "the block took the whole blast");
        assert_eq!(w.map.get(11, 4), Tile::Empty, "and was destroyed by it");
        fire_left(&mut w);
        assert!(w.players[1].hp < MAX_HP, "so the next rocket gets through");
    }

    #[test]
    fn bouncer_ricochets_off_a_wall() {
        let (mut w, slot) = armed_world(BOUNCER);
        w.players[1].pos = tile_center(30, 10);
        // Fire left, straight into the border wall two tiles away.
        w.step(&[Input { fire: true, weapon: slot, aim: std::f32::consts::PI, ..Default::default() }]);
        for _ in 0..10 {
            w.step(&[]);
        }
        assert_eq!(w.projectiles.len(), 1, "it must survive the wall");
        assert!(w.projectiles[0].vel.x > 0.0, "and now fly back the other way");
    }

    /// Two players in the open middle of the menu map, the first one carrying
    /// only `weapon`.
    fn open_world(weapon: u8, me: (i32, i32), enemy: (i32, i32)) -> World {
        let mut w = World::new(Tilemap::from_ascii(crate::tilemap::MAP_HUB), 2, &[weapon], 1);
        w.players[0].pos = tile_center(me.0, me.1);
        w.players[1].pos = tile_center(enemy.0, enemy.1);
        w
    }

    #[test]
    fn bomb_blast_is_round_and_reaches_the_diagonal() {
        let mut w = open_world(BOMB, (19, 4), (20, 5)); // diagonal: the old plus missed here
        w.map.set(17, 4, Tile::Brick(BRICK_HP));
        w.step(&[Input { fire: true, ..Default::default() }]);
        // Dropped at the owner's feet, not thrown.
        assert_eq!(w.projectiles[0].pos, tile_center(19, 4));
        for _ in 0..(1.3 / DT) as i32 {
            w.step(&[]);
        }
        assert!(w.projectiles.is_empty(), "the fuse ran out");
        assert!(w.players[1].hp < MAX_HP - 50.0, "hp = {}", w.players[1].hp);
        assert!(w.players[0].hp < MAX_HP, "the one who dropped it is hurt too");
        assert_eq!(w.map.get(17, 4), Tile::Empty, "and the brick next to it is gone");
    }

    #[test]
    fn bomb_goes_off_when_an_enemy_steps_on_it() {
        let mut w = open_world(BOMB, (19, 4), (22, 4));
        w.step(&[Input { fire: true, ..Default::default() }]);
        w.players[0].pos = tile_center(19, 1); // walk away from it
        let walk_left = Input { move_dir: vec2(-1.0, 0.0), ..Default::default() };
        for _ in 0..45 {
            w.step(&[Input::default(), walk_left]);
        }
        assert!(w.projectiles.is_empty(), "it went off before its fuse");
        assert!(w.players[1].hp < MAX_HP);
    }

    #[test]
    fn cluster_shell_flies_through_players() {
        // Point-blank, the shell used to burst on the enemy and all seven
        // bomblets went off on them: a one-shot kill. Now it bursts far behind.
        let mut w = open_world(CLUSTER, (10, 4), (12, 4));
        fire_once(&mut w, 0, 0, 0.0);
        for _ in 0..60 {
            w.step(&[]);
        }
        assert_eq!(w.players[1].hp, MAX_HP);
    }

    #[test]
    fn cluster_shell_bursts_into_bomblets() {
        let (mut w, slot) = armed_world(CLUSTER);
        w.players[1].pos = tile_center(30, 10);
        w.drain_events();
        // Straight into the border wall, 16 px away: it bursts at once.
        w.step(&[Input { fire: true, weapon: slot, aim: std::f32::consts::PI, ..Default::default() }]);
        assert_eq!(w.projectiles.len(), 7);
        assert!(w.projectiles.iter().all(|p| p.weapon == BOMBLET));
        assert!(w.projectiles.iter().all(|p| !w.map.circle_hits_wall(p.pos, 3.0)), "none may start inside the wall");
        for _ in 0..60 {
            w.step(&[]);
        }
        assert!(w.projectiles.is_empty(), "every bomblet went off");
        let blasts = w.drain_events().iter().filter(|e| matches!(e, Event::Explosion { .. })).count();
        assert!(blasts >= 5, "blasts = {blasts}");
    }

    #[test]
    fn swarm_missile_curls_towards_the_enemy() {
        // The same projectile, once as a homing missile and once as a plain
        // bolt: the enemy is 32 px off its line, so only homing can hit.
        let fly = |weapon: u8| {
            let (mut w, _) = armed_world(SWARM);
            w.players[0].pos = tile_center(14, 9);
            w.players[1].pos = tile_center(22, 8);
            w.projectiles.push(Projectile { pos: tile_center(15, 9), vel: vec2(300.0, 0.0), weapon, owner: 0, life: 2.0 });
            for _ in 0..60 {
                w.step(&[]);
            }
            w.players[1].hp
        };
        assert!(fly(SWARM) < MAX_HP, "the missile should find its target");
        assert_eq!(fly(BLASTER), MAX_HP, "a plain bolt flies past");
    }

    #[test]
    fn teleport_orb_moves_the_owner_to_where_it_lands() {
        let (mut w, slot) = armed_world(TELEPORT);
        w.players[1].pos = tile_center(30, 10);
        w.drain_events();
        w.step(&[Input { fire: true, weapon: slot, ..Default::default() }]);
        for _ in 0..20 {
            w.step(&[]);
        }
        // The orb stopped at the brick in tile 4 (x from 128 px): the owner stands just short of it.
        let p = &w.players[0];
        assert!(p.pos.x > 100.0 && p.pos.x < 119.0, "x = {}", p.pos.x);
        assert!(!w.map.circle_hits_wall(p.pos, PLAYER_RADIUS));
        assert_eq!(w.map.get(4, 1), Tile::Brick(3), "teleporting hurts no brick");
        assert!(w.drain_events().iter().any(|e| matches!(e, Event::Teleported { player: 0, .. })));
    }

    #[test]
    fn teleport_orb_hitting_a_player_swaps_places() {
        let (mut w, slot) = armed_world(TELEPORT);
        w.step(&[Input { fire: true, weapon: slot, ..Default::default() }]);
        for _ in 0..20 {
            w.step(&[]);
        }
        assert_eq!(w.players[0].pos, tile_center(3, 1));
        assert_eq!(w.players[1].pos, tile_center(1, 1));
        assert_eq!(w.players[1].hp, MAX_HP, "a swap is harmless");
    }

    #[test]
    fn nova_blast_clears_a_room() {
        let (mut w, slot) = armed_world(NOVA);
        w.players[1].pos = tile_center(30, 10);
        w.drain_events();
        w.step(&[Input { fire: true, weapon: slot, ..Default::default() }]);
        for _ in 0..40 {
            w.step(&[]);
        }
        assert_eq!(w.map.get(4, 1), Tile::Empty);
        assert_eq!(w.map.get(5, 1), Tile::Empty);
        assert!(w.players[0].hp < MAX_HP - 30.0, "even halved, standing this close hurts: hp = {}", w.players[0].hp);
        assert!(w.drain_events().iter().any(|e| matches!(e, Event::Explosion { radius, .. } if *radius == 230.0)));
    }

    #[test]
    fn a_chosen_loadout_replaces_the_weapons() {
        let mut w = two_player_world();
        w.players[0].current = 2;
        w.set_loadout(0, &[NOVA, BOMB]);
        let ids: Vec<u8> = w.players[0].weapons.iter().map(|s| s.id).collect();
        assert_eq!(ids, [NOVA, BOMB]);
        assert_eq!(w.players[0].current, 0);
        assert_eq!(w.players[1].weapons.len(), 3, "nobody else is touched");
        // And a respawn keeps what was chosen.
        w.players[0].alive = false;
        w.players[0].respawn_timer = 0.0;
        w.step(&[]);
        assert_eq!(w.players[0].weapons.iter().map(|s| s.id).collect::<Vec<_>>(), [NOVA, BOMB]);
    }

    #[test]
    fn only_five_different_arsenal_weapons_are_a_loadout() {
        use crate::weapons::{valid_loadout, DEFAULT_LOADOUT};
        assert!(valid_loadout(&DEFAULT_LOADOUT));
        assert!(!valid_loadout(&[BLASTER, ROCKET]), "too few");
        assert!(!valid_loadout(&[0, 1, 2, 3, 4, 5]), "too many");
        assert!(!valid_loadout(&[BLASTER, BLASTER, ROCKET, BOMB, RAILGUN]), "a weapon twice");
        assert!(!valid_loadout(&[BLASTER, ROCKET, BOMB, RAILGUN, BOMBLET]), "bomblets are not for picking");
        assert!(!valid_loadout(&[BLASTER, ROCKET, BOMB, RAILGUN, 200]), "no such weapon");
    }

    #[test]
    fn oversized_move_input_is_clamped() {
        let mut w = two_player_world();
        w.players[1].pos = tile_center(30, 10);
        let cheat = Input { move_dir: vec2(1000.0, 0.0), ..Default::default() };
        for _ in 0..60 {
            w.step(&[cheat]);
        }
        // One second at full speed is at most ~MOVE_SPEED px.
        assert!(w.players[0].vel.length() <= MOVE_SPEED + 1.0);
    }

    /// One player in the open middle of the menu map, feeding it one move
    /// input per tick. Returns the fastest speed reached and the dash count.
    fn run_moves(moves: &[Vec2]) -> (f32, usize) {
        let mut w = World::new(Tilemap::from_ascii(crate::tilemap::MAP_HUB), 1, &[BLASTER], 1);
        w.players[0].pos = tile_center(19, 4);
        let mut top = 0.0f32;
        for &move_dir in moves {
            w.step(&[Input { move_dir, ..Default::default() }]);
            top = top.max(w.players[0].vel.length());
        }
        let dashes = w.drain_events().iter().filter(|e| matches!(e, Event::Dashed { .. })).count();
        (top, dashes)
    }

    fn ticks(n: usize, v: Vec2) -> Vec<Vec2> {
        vec![v; n]
    }

    #[test]
    fn rest_then_push_is_a_dash() {
        let moves = [ticks(10, Vec2::ZERO), ticks(5, vec2(1.0, 0.0))].concat();
        let (top, dashes) = run_moves(&moves);
        assert_eq!(dashes, 1);
        assert!(top > MOVE_SPEED * 3.0, "top = {top}");
    }

    #[test]
    fn a_slow_thumb_still_dashes() {
        // A floating stick grows from zero over a few frames.
        let ramp = [0.3, 0.45, 0.7].map(|x| vec2(x, 0.0));
        let moves = [ticks(10, Vec2::ZERO), ramp.to_vec()].concat();
        assert_eq!(run_moves(&moves).1, 1);
    }

    #[test]
    fn walking_and_switching_keys_never_dash() {
        // From a standstill at spawn (no rest yet), then a two-tick gap
        // between letting go of D and pressing W.
        let moves = [ticks(30, vec2(1.0, 0.0)), ticks(2, Vec2::ZERO), ticks(30, vec2(0.0, -1.0))].concat();
        let (top, dashes) = run_moves(&moves);
        assert_eq!(dashes, 0);
        assert!(top <= MOVE_SPEED + 1.0);
    }

    #[test]
    fn dash_has_a_cooldown() {
        let tap = [ticks(10, Vec2::ZERO), ticks(5, vec2(1.0, 0.0))].concat();
        assert_eq!(run_moves(&tap.repeat(2)).1, 1, "15 ticks apart is too soon");
        assert_eq!(run_moves(&tap.repeat(6)).1, 2, "every ~1.2 s is fine");
    }

    #[test]
    fn same_inputs_give_same_world() {
        // The property netcode will lean on: the sim is deterministic.
        let script = |t: u32| Input {
            move_dir: vec2((t as f32 * 0.1).cos(), (t as f32 * 0.07).sin()),
            aim: t as f32 * 0.05,
            fire: t % 7 == 0,
            weapon: ((t / 50) % 3) as u8,
            ..Default::default()
        };
        let run = || {
            let mut w = World::new(Tilemap::from_ascii(MAP_GRID), 3, &[BLASTER, ROCKET, RAILGUN], 42);
            for t in 0..900 {
                w.step(&[script(t), script(t + 100), script(t + 200)]);
            }
            w
        };
        let (a, b) = (run(), run());
        for (pa, pb) in a.players.iter().zip(&b.players) {
            assert_eq!(pa.pos, pb.pos);
            assert_eq!(pa.hp, pb.hp);
            assert_eq!(pa.kills, pb.kills);
        }
        for y in 0..crate::tilemap::MAP_H as i32 {
            for x in 0..crate::tilemap::MAP_W as i32 {
                assert_eq!(a.map.get(x, y), b.map.get(x, y));
            }
        }
    }
}
