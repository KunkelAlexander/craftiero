// The per-weapon signature: what each gun looks and feels like when it fires
// and when it lands. This is feel, not rules, so it lives in the viewer and not
// in `WeaponDef`; `sim` only says which weapon caused an event.
//
// The palette lives here too. A shot, its blast and the sparks off a body it
// hit should all be the same colour, and one table is the only way to keep them
// that way. `effects.rs` draws from it; `main.rs` takes the camera response.

use crate::camera::Kick;
use glam::Vec3;
use sim::weapons::{
    BLASTER, BOMB, BOMBLET, BOUNCER, BUILD, CLUSTER, NOVA, RAILGUN, ROCKET, SWARM, TELEPORT, TRACTOR,
    WHIP,
};

// Colours shared with the weapons in `worms.rs`, so a shot matches its gun.
pub const CYAN: Vec3 = Vec3::new(0.35, 0.92, 1.0);
pub const EMBER: Vec3 = Vec3::new(1.0, 0.55, 0.16);
pub const LIME: Vec3 = Vec3::new(0.55, 0.95, 0.30);
pub const MAGENTA: Vec3 = Vec3::new(0.90, 0.35, 0.85);
pub const VIOLET: Vec3 = Vec3::new(0.55, 0.42, 1.0);
pub const ICE: Vec3 = Vec3::new(0.80, 0.92, 1.0);
pub const BRASS: Vec3 = Vec3::new(0.80, 0.62, 0.26);
pub const BEAM: Vec3 = Vec3::new(0.55, 0.92, 1.0);
pub const PLASMA: Vec3 = Vec3::new(0.55, 0.28, 1.0);
pub const PLASMA_HOT: Vec3 = Vec3::new(0.92, 0.86, 1.0);
pub const DUST: Vec3 = Vec3::new(0.60, 0.55, 0.46);
pub const SMOKE: Vec3 = Vec3::new(0.38, 0.36, 0.34);
pub const SPARK: Vec3 = Vec3::new(1.0, 0.72, 0.28);
pub const HOT: Vec3 = Vec3::new(1.0, 0.90, 0.60);

/// How a blast of this weapon looks. All sizes are multiples of the sim's blast
/// radius (the damage reach), so a weapon's fireball stays proportional to what
/// it can hurt.
pub struct Blast {
    /// The white-hot core at the centre.
    pub core: Vec3,
    pub core_size: f32,
    /// Three fire layers: colour, size, and how far each is thrown from the
    /// centre. The outside layer is the one that reads as smoke.
    pub fire: [Vec3; 3],
    pub fire_size: [f32; 3],
    pub fire_reach: [f32; 3],
    /// Multiplier on the shared fireball lifetime.
    pub fire_life: f32,
    /// Sparks or dirt thrown out of the crater.
    pub debris: usize,
    pub debris_colour: Vec3,
    pub debris_speed: f32,
    pub debris_up: f32,
    pub debris_hot: bool,
    /// The slow cloud that stays after the fire is out.
    pub smoke: Vec3,
    pub smoke_size: f32,
    pub smoke_life: f32,
    pub smoke_rise: f32,
    /// Strength of the ground dust ring; 0 leaves the floor alone.
    pub dust: f32,
    /// How many expanding shock rings, and how long the first one takes.
    pub rings: u8,
    pub ring_life: f32,
    /// Shake at the centre, in metres.
    pub shake: f32,
    /// A lens punch in degrees at the centre. Widening the view cannot move the
    /// ray through the centre of the screen, so it is free impact.
    pub fov: f32,
    /// A physical shove of the view, in camera space: metres up and back, and
    /// degrees of muzzle climb. The shake is the noise; this is the direction.
    pub push_up: f32,
    pub push_back: f32,
    pub push_pitch: f32,
}

/// How a hit on a body looks. `sparks` is the count, `size` the flash radius and
/// `speed` the sparks' initial speed.
pub struct Hit {
    pub flash: Vec3,
    pub spark: Vec3,
    pub sparks: usize,
    pub size: f32,
    pub speed: f32,
    /// Shake at the point of impact, in metres.
    pub shake: f32,
}

/// The muzzle flash: a hot ball whose colour and size match the weapon, so a
/// rocket booms orange and a blaster snaps cyan.
pub fn flash(weapon: u8) -> (Vec3, f32) {
    match weapon {
        BLASTER => (CYAN, 0.20),
        ROCKET => (EMBER, 0.34),
        BOUNCER => (LIME, 0.24),
        BOMB => (EMBER, 0.26),
        CLUSTER => (BRASS, 0.28),
        SWARM => (MAGENTA, 0.18),
        TELEPORT => (VIOLET, 0.26),
        NOVA => (ICE, 0.38),
        WHIP => (PLASMA, 0.26),
        _ => (HOT, 0.20),
    }
}

/// The signature of a weapon's blast. Every caller gets a row, including the
/// cluster's bomblet, which is not in anyone's arsenal.
pub fn blast(weapon: u8) -> Blast {
    match weapon {
        ROCKET => Blast {
            core: Vec3::new(1.0, 0.98, 0.92),
            core_size: 0.26,
            fire: [Vec3::new(1.0, 0.60, 0.20), Vec3::new(1.0, 0.80, 0.28), Vec3::new(1.0, 0.96, 0.78)],
            fire_size: [0.38, 0.28, 0.18],
            fire_reach: [0.16, 0.24, 0.32],
            fire_life: 1.0,
            debris: 8,
            debris_colour: SPARK,
            debris_speed: 4.8,
            debris_up: 0.55,
            debris_hot: true,
            smoke: SMOKE,
            smoke_size: 0.20,
            smoke_life: 1.6,
            smoke_rise: 1.2,
            dust: 10.0,
            rings: 1,
            ring_life: 0.32,
            shake: 0.060,
            fov: 1.6,
            push_up: 0.025,
            push_back: 0.060,
            push_pitch: 0.5,
        },
        // More ground than fire: a dirty, low blast that throws dirt and leaves
        // a long dark cloud instead of a bright flower.
        BOMB => Blast {
            core: Vec3::new(1.0, 0.90, 0.68),
            core_size: 0.28,
            fire: [Vec3::new(0.92, 0.45, 0.12), Vec3::new(1.0, 0.66, 0.20), Vec3::new(0.82, 0.36, 0.14)],
            fire_size: [0.42, 0.32, 0.20],
            fire_reach: [0.14, 0.22, 0.30],
            fire_life: 1.3,
            debris: 7,
            debris_colour: Vec3::new(0.48, 0.38, 0.27),
            debris_speed: 4.2,
            debris_up: 0.45,
            debris_hot: false,
            smoke: Vec3::new(0.30, 0.28, 0.26),
            smoke_size: 0.30,
            smoke_life: 2.2,
            smoke_rise: 0.9,
            dust: 14.0,
            rings: 1,
            ring_life: 0.42,
            shake: 0.075,
            fov: 2.2,
            push_up: 0.035,
            push_back: 0.075,
            push_pitch: 0.7,
        },
        NOVA => Blast {
            core: Vec3::ONE,
            core_size: 0.20,
            fire: [Vec3::new(0.45, 0.75, 1.0), Vec3::new(0.82, 0.95, 1.0), Vec3::new(1.0, 1.0, 1.0)],
            fire_size: [0.34, 0.26, 0.18],
            fire_reach: [0.16, 0.24, 0.32],
            fire_life: 1.6,
            debris: 18,
            debris_colour: ICE,
            debris_speed: 5.5,
            debris_up: 0.55,
            debris_hot: true,
            smoke: Vec3::new(0.68, 0.78, 0.88),
            smoke_size: 0.24,
            smoke_life: 2.6,
            smoke_rise: 1.4,
            dust: 20.0,
            // Two rings: a fast white shock and a slower, wider second one.
            rings: 2,
            ring_life: 0.55,
            shake: 0.20,
            fov: 4.5,
            push_up: 0.060,
            push_back: 0.120,
            push_pitch: 1.2,
        },
        SWARM => Blast {
            core: Vec3::new(1.0, 0.88, 0.96),
            core_size: 0.20,
            fire: [MAGENTA, Vec3::new(0.96, 0.60, 0.88), Vec3::new(1.0, 0.92, 0.98)],
            fire_size: [0.34, 0.26, 0.18],
            fire_reach: [0.12, 0.16, 0.18],
            fire_life: 0.7,
            debris: 4,
            debris_colour: SPARK,
            debris_speed: 3.5,
            debris_up: 0.5,
            debris_hot: true,
            smoke: Vec3::new(0.46, 0.40, 0.46),
            smoke_size: 0.14,
            smoke_life: 0.7,
            smoke_rise: 1.0,
            dust: 4.0,
            rings: 0,
            ring_life: 0.0,
            shake: 0.012,
            fov: 0.3,
            push_up: 0.004,
            push_back: 0.010,
            push_pitch: 0.1,
        },
        BOMBLET => Blast {
            core: Vec3::new(1.0, 0.92, 0.72),
            core_size: 0.18,
            fire: [Vec3::new(1.0, 0.55, 0.16), Vec3::new(1.0, 0.75, 0.25), Vec3::new(1.0, 0.95, 0.80)],
            fire_size: [0.34, 0.26, 0.18],
            fire_reach: [0.10, 0.14, 0.18],
            fire_life: 0.6,
            debris: 3,
            debris_colour: SPARK,
            debris_speed: 3.5,
            debris_up: 0.5,
            debris_hot: true,
            smoke: SMOKE,
            smoke_size: 0.12,
            smoke_life: 0.55,
            smoke_rise: 0.8,
            dust: 3.0,
            rings: 0,
            ring_life: 0.0,
            shake: 0.014,
            fov: 0.2,
            push_up: 0.004,
            push_back: 0.010,
            push_pitch: 0.1,
        },
        // Nothing else explodes today. A small, plain row keeps a future weapon
        // visible without making it look like something it is not.
        _ => Blast {
            core: Vec3::new(1.0, 0.98, 0.92),
            core_size: 0.22,
            fire: [EMBER, Vec3::new(1.0, 0.75, 0.28), Vec3::new(1.0, 0.94, 0.76)],
            fire_size: [0.36, 0.28, 0.20],
            fire_reach: [0.12, 0.16, 0.20],
            fire_life: 0.8,
            debris: 4,
            debris_colour: SPARK,
            debris_speed: 3.8,
            debris_up: 0.5,
            debris_hot: true,
            smoke: SMOKE,
            smoke_size: 0.18,
            smoke_life: 1.0,
            smoke_rise: 1.0,
            dust: 6.0,
            rings: 0,
            ring_life: 0.0,
            shake: 0.020,
            fov: 0.4,
            push_up: 0.008,
            push_back: 0.020,
            push_pitch: 0.2,
        },
    }
}

/// The signature of a hit on a body. The same event carries the damage, which
/// `effects.rs` uses to scale the flash; this row is the weapon's character.
pub fn hit(weapon: u8) -> Hit {
    match weapon {
        BLASTER => Hit { flash: HOT, spark: CYAN, sparks: 4, size: 0.14, speed: 3.5, shake: 0.010 },
        RAILGUN => Hit { flash: ICE, spark: BEAM, sparks: 7, size: 0.22, speed: 5.5, shake: 0.030 },
        BOUNCER => Hit { flash: LIME, spark: LIME, sparks: 5, size: 0.16, speed: 4.5, shake: 0.020 },
        WHIP => Hit { flash: PLASMA_HOT, spark: PLASMA, sparks: 8, size: 0.24, speed: 5.0, shake: 0.028 },
        // Explosions also emit a `Hit` per victim: the body catches the blast.
        NOVA => Hit { flash: Vec3::ONE, spark: ICE, sparks: 8, size: 0.30, speed: 5.5, shake: 0.040 },
        ROCKET | BOMB | CLUSTER | SWARM | BOMBLET => {
            Hit { flash: HOT, spark: SPARK, sparks: 5, size: 0.18, speed: 4.0, shake: 0.022 }
        }
        _ => Hit { flash: HOT, spark: SPARK, sparks: 4, size: 0.15, speed: 3.5, shake: 0.012 },
    }
}

/// How a shot pushes the view, per weapon. This is feel data, not rules, so it
/// lives beside the blast signatures rather than in `WeaponDef`.
///
/// The old kick only slid the camera backwards along its own view direction:
/// a dolly, which the eye reads as nothing. A shot now also climbs (pitch) and
/// widens the lens, and the lens is the aim-safe part because the ray through
/// the centre of the screen does not move. Fast weapons get less of everything
/// than their single shot suggests, because their next shot arrives before the
/// view has settled.
pub fn shot_kick(weapon: u8) -> Kick {
    let (up, back, pitch, fov) = match weapon {
        NOVA => (0.100, 0.30, 3.0, 8.0),
        ROCKET => (0.045, 0.16, 1.6, 4.5),
        RAILGUN => (0.035, 0.13, 1.3, 3.5),
        CLUSTER => (0.035, 0.11, 1.1, 3.0),
        SWARM => (0.020, 0.065, 0.6, 1.8),
        BOUNCER => (0.014, 0.050, 0.4, 1.2),
        WHIP => (0.020, 0.035, 0.8, 2.0),
        TELEPORT => (0.010, 0.030, 0.3, 0.6),
        BLASTER => (0.008, 0.030, 0.25, 0.8),
        BOMB => (0.008, 0.020, 0.2, 0.5),
        BUILD => (0.010, 0.020, 0.15, 0.3),
        TRACTOR => (0.004, 0.008, 0.05, 0.1),
        _ => (0.006, 0.015, 0.2, 0.4),
    };
    // A lash hits across the body rather than away from the shoulder.
    let side = if weapon == WHIP { 0.10 } else { 0.0 };
    Kick { up, back, side, pitch, fov }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn heavy_weapons_kick_harder_and_nothing_kicks_the_blaster() {
        let (n, r, b) = (shot_kick(NOVA), shot_kick(ROCKET), shot_kick(BLASTER));
        assert!(n.pitch > r.pitch && r.pitch > b.pitch, "climb: {} {} {}", n.pitch, r.pitch, b.pitch);
        assert!(n.fov > r.fov && r.fov > b.fov, "lens: {} {} {}", n.fov, r.fov, b.fov);
        assert!(
            n.up + n.back > r.up + r.back && r.up + r.back > b.up + b.back,
            "shove: {} {} {}",
            n.up + n.back,
            r.up + r.back,
            b.up + b.back
        );
        // A blaster fired eight times a second must not visibly move the view,
        // or the aim contract (the crosshair is where the shot goes) breaks.
        assert!(b.pitch < 0.5 && b.fov < 1.0, "the metronome stays quiet");
        // The whip is the one weapon that hits across the body.
        assert!(shot_kick(WHIP).side.abs() > 0.0, "the lash shoves sideways");
    }

    #[test]
    fn a_nova_is_unmistakably_bigger_than_a_bomblet() {
        let nova = blast(NOVA);
        let bomblet = blast(BOMBLET);
        assert!(nova.shake > bomblet.shake * 5.0, "shake: {} {}", nova.shake, bomblet.shake);
        assert!(nova.fov > bomblet.fov, "lens: {} {}", nova.fov, bomblet.fov);
        assert!(nova.fire_size[0] * 8.0 > bomblet.fire_size[0] * 1.8 * 2.0, "the fireball is much larger in metres");
        assert!(nova.debris > bomblet.debris);
        assert!(nova.smoke_life > bomblet.smoke_life * 3.0, "the cloud stays: {} {}", nova.smoke_life, bomblet.smoke_life);
        assert!(nova.push_up > blast(ROCKET).push_up, "the view is shoved, not just wobbled");
        assert!(blast(ROCKET).push_up > bomblet.push_up);
        assert_eq!(nova.rings, 2, "the Nova answers twice");
        assert_eq!(bomblet.rings, 0);
        // Ice, not fire.
        assert!(nova.fire[0].z > nova.fire[0].x, "the Nova is blue: {:?}", nova.fire[0]);
    }

    #[test]
    fn a_bomb_is_dirt_and_smoke_where_the_nova_is_light() {
        let bomb = blast(BOMB);
        assert!(!bomb.debris_hot, "a bomb throws dirt, not sparks");
        assert!(bomb.smoke_life > blast(ROCKET).smoke_life, "and leaves a longer cloud");
        assert!(bomb.debris_speed < blast(ROCKET).debris_speed, "the blast is heavier, not faster");
    }

    #[test]
    fn a_railgun_hit_is_a_thin_blue_strike_and_a_blaster_is_a_tick() {
        let rail = hit(RAILGUN);
        let blaster = hit(BLASTER);
        assert!(rail.sparks > blaster.sparks);
        assert!(rail.shake > blaster.shake);
        assert!(rail.spark.z > rail.spark.x, "blue-white, not gold");
    }
}
