// Bots are just another source of `Input`.
//
// A bot looks at the world (read-only), thinks, and returns an `Input` - the
// exact same struct a keyboard or a network packet produces. The sim cannot
// tell the difference, which is the Command pattern paying off: no special
// "AI player" code exists inside the sim at all.
//
// Bot state (`Bot`) lives OUTSIDE the `World`. The world is only rules and
// physics; a bot's moods and timers belong to whoever runs the bot (the host).

use glam::{vec2, Vec2};

use crate::input::Input;
use crate::rng::Rng;
use crate::weapons::{BLASTER, BOUNCER, RAILGUN, ROCKET};
use crate::world::World;

pub struct Bot {
    rng: Rng,
    /// +1 / -1: which way to circle-strafe around the target.
    strafe: f32,
    /// Random detour angle used when the target is behind a wall. It changes
    /// every second or so, which is what stops a bot staying stuck in a corner.
    wander: f32,
    timer: f32,
    seq: u32,
}

impl Bot {
    pub fn new(seed: u32) -> Bot {
        Bot { rng: Rng::new(seed), strafe: 1.0, wander: 0.0, timer: 0.0, seq: 0 }
    }

    pub fn think(&mut self, world: &World, me: usize) -> Input {
        self.seq += 1;
        let mut input = Input { seq: self.seq, ..Default::default() };
        let player = &world.players[me];
        if !player.alive {
            return input;
        }

        self.timer -= crate::world::DT;
        if self.timer <= 0.0 {
            self.timer = self.rng.range(0.6, 1.6);
            self.strafe = if self.rng.f32() < 0.5 { 1.0 } else { -1.0 };
            self.wander = self.rng.range(-1.2, 1.2);
        }

        // Nearest living enemy.
        let target = (0..world.players.len())
            .filter(|&j| j != me && world.players[j].alive)
            .min_by(|&a, &b| {
                let da = world.players[a].pos.distance_squared(player.pos);
                let db = world.players[b].pos.distance_squared(player.pos);
                da.total_cmp(&db)
            });
        let Some(target) = target else {
            return input;
        };

        let to_target = world.players[target].pos - player.pos;
        let dist = to_target.length();
        let toward = to_target.normalize_or_zero();
        let side = vec2(-toward.y, toward.x) * self.strafe;
        let sees = world.map.line_of_sight(player.pos, world.players[target].pos);

        // Movement.
        input.move_dir = if sees {
            let range_push = if dist > 320.0 { 1.0 } else if dist < 160.0 { -1.0 } else { 0.0 };
            (toward * range_push + side * 0.7).clamp_length_max(1.0)
        } else {
            rotate(toward, self.wander)
        };

        // Aim with a little error so bots are beatable.
        let aim_error = self.rng.range(-0.06, 0.06);
        input.aim = to_target.y.atan2(to_target.x) + aim_error;

        // Shoot when we can see the target. Without line of sight, shoot
        // sometimes anyway: rockets and rails dig through bricks, so this is
        // how a bot opens a path toward you.
        // Never dig into a wall that is close enough for the blast to hurt us.
        let aim_dir = vec2(input.aim.cos(), input.aim.sin());
        let wall_is_far = world.map.distance_to_wall(player.pos, aim_dir, 200.0) > 110.0;
        input.fire = if sees { self.rng.f32() < 0.9 } else { wall_is_far && self.rng.f32() < 0.25 };

        input.weapon = self.pick_weapon(world, me, dist, sees);
        input
    }

    /// Prefer the railgun at range, the rocket at mid range, the blaster up
    /// close, but never pick a weapon that is empty or reloading. The bouncer is
    /// what a bot uses against a target it cannot see: bolts that come round
    /// corners. Bots never drop bombs or build blocks: placing one and walking
    /// away (or hiding behind it) is a plan, and bots do not make plans.
    fn pick_weapon(&self, world: &World, me: usize, dist: f32, sees: bool) -> u8 {
        let preferred: &[u8] = if !sees {
            &[ROCKET, BOUNCER, RAILGUN, BLASTER]
        } else if dist > 450.0 {
            &[RAILGUN, ROCKET, BOUNCER, BLASTER]
        } else if dist > 150.0 {
            &[ROCKET, BOUNCER, BLASTER, RAILGUN]
        } else {
            &[BLASTER, BOUNCER, ROCKET, RAILGUN]
        };
        let weapons = &world.players[me].weapons;
        for &id in preferred {
            if let Some(slot) = weapons.iter().position(|w| w.id == id && w.ammo > 0 && w.reload == 0.0) {
                return slot as u8;
            }
        }
        // Everything is reloading; keep whatever is selected.
        world.players[me].current as u8
    }
}

fn rotate(v: Vec2, angle: f32) -> Vec2 {
    let (s, c) = angle.sin_cos();
    vec2(v.x * c - v.y * s, v.x * s + v.y * c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tilemap::{Tilemap, MAP_GRID};

    #[test]
    fn bots_fight_and_score() {
        let mut world = World::new(Tilemap::from_ascii(MAP_GRID), 4, &crate::weapons::ARSENAL, 5);
        let mut bots: Vec<Bot> = (0..4).map(|i| Bot::new(100 + i)).collect();
        for _ in 0..60 * 90 {
            let inputs: Vec<Input> = (0..4).map(|i| bots[i].think(&world, i)).collect();
            world.step(&inputs);
        }
        let deaths: i32 = world.players.iter().map(|p| p.deaths).sum();
        assert!(deaths > 0, "four bots should have killed someone in 90 s");
    }

    /// The same on every map, with the weapons bots really carry. The Anthill is
    /// the hard case: its tunnels hide everyone until someone digs.
    #[test]
    fn bots_fight_on_every_map() {
        for def in &crate::tilemap::MAPS {
            let mut world = World::new(Tilemap::from_ascii(def.art), 4, &crate::weapons::DEFAULT_LOADOUT, 5);
            let mut bots: Vec<Bot> = (0..4).map(|i| Bot::new(100 + i)).collect();
            for _ in 0..60 * 90 {
                let inputs: Vec<Input> = (0..4).map(|i| bots[i].think(&world, i)).collect();
                world.step(&inputs);
            }
            let kills: i32 = world.players.iter().map(|p| p.kills.max(0)).sum();
            assert!(kills >= 3, "{}: bots scored only {kills} kills in 90 s", def.name);
        }
    }
}
