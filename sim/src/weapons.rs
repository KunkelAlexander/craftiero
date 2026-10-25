// Pattern: Type Object. A "weapon type" is a row of data in a table, not a
// Rust type. The weapons below behave differently mostly because their
// numbers differ; the few genuinely new mechanics (a swarm's several missiles,
// a ricochet) are one number each, read in `combat.rs`. More weapons later
// means more rows, and a little code only for mechanics that are more than a
// number (`Special`: homing, a cluster burst, a teleport).
//
// Pattern: Flyweight. A projectile in flight stores just a weapon id and its
// own position and velocity. The shared, unchanging stats live once in `WEAPONS`.
//
// Ported from the 2D game's table. Units are now metres and seconds (the 2D
// numbers were pixels; 1 px of the old worm was about 4.5 cm of this one), and
// the speeds were raised: the arena is 128 m wide, and a bolt that takes ten
// seconds to cross it is no weapon.

use glam::Vec3;

/// The weapon id is its index in `WEAPONS`, and also its slot in a worm's
/// inventory (every worm carries the whole arsenal), so `Input::weapon` is
/// just an id.
pub const BLASTER: u8 = 0;
pub const ROCKET: u8 = 1;
pub const RAILGUN: u8 = 2;
pub const BOUNCER: u8 = 3;
pub const BOMB: u8 = 4;
pub const CLUSTER: u8 = 5;
pub const SWARM: u8 = 6;
pub const TELEPORT: u8 = 7;
pub const NOVA: u8 = 8;
/// Throws up a rectangular wall of rock in front of the worm ("build = add").
pub const BUILD: u8 = 9;
/// The forming tool: a stream of small lumps, so you can draw a bridge or a
/// wall by sweeping the aim ("tractor beam", PLAN.md). Not a new mechanic, only
/// the Build weapon with a short cooldown, a small lump and a big magazine.
pub const TRACTOR: u8 = 10;
/// A long plasma lash: a left-to-right sweep, or an overhead smash when it is
/// used during a double jump.
pub const WHIP: u8 = 11;
/// What a Cluster shell splits into. Not in the arsenal: only the shell makes it.
pub const BOMBLET: u8 = 12;

/// Every weapon a player carries, in slot order. (The 2D game let you pick five
/// of them on a loadout screen; that comes back with the menus, M6.)
pub const ARSENAL: [u8; 12] = [BLASTER, ROCKET, RAILGUN, BOUNCER, BOMB, CLUSTER, SWARM, TELEPORT, NOVA, BUILD, TRACTOR, WHIP];

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    /// Flies through the world as a `Projectile`.
    Projectile,
    /// Instant line; resolved the moment you pull the trigger.
    Beam,
    /// Adds a lump of terrain at the aim point, within `range`. The magazine
    /// is the number of lumps you carry.
    Build,
    /// Adds one rectangular wall of terrain just ahead of the worm.
    Wall,
    /// A melee lash: a left-to-right sweep that damages what it touches and
    /// carves a scar along its path. Fired after a double jump, it becomes an
    /// overhead smash instead.
    Whip,
}

/// The few behaviours that are more than a number. A weapon has at most one, so
/// this is an enum and not another pile of flags.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Special {
    None,
    /// Steers towards the nearest enemy, turning at most this many radians a second.
    Homing(f32),
    /// Where it detonates it splits into this many projectiles of the given weapon.
    Cluster(u8, u8),
    /// Where it lands, its owner appears (or swaps places with the worm it hit).
    Teleport,
}

pub struct WeaponDef {
    pub name: &'static str,
    pub kind: Kind,
    /// Damage on a direct hit (or to each worm a beam passes through).
    pub damage: f32,
    /// Projectile speed in m/s. Unused by beams. A speed of 0 means the
    /// projectile is dropped at the worm's feet, and `lifetime` is its fuse.
    pub speed: f32,
    /// Seconds before an unspent projectile disappears (or explodes).
    pub lifetime: f32,
    /// Seconds between shots.
    pub cooldown: f32,
    /// Seconds from the trigger pull to the shot. Once it starts, a wind-up
    /// runs to completion: letting go does not cancel it, but switching weapons
    /// does. A heavy weapon's cost and its tell are the same number.
    pub windup: f32,
    /// Extra elevation added to the shot, in radians: a mortar's arc. Zero for
    /// a gun that fires straight down the reticle. The launch direction is
    /// clamped so a lobbed weapon always arcs upward, even aiming down.
    pub loft: f32,
    /// Rounds per magazine, and seconds to reload once it is empty.
    pub mag: u16,
    pub reload: f32,
    /// Random aim error in radians (a cone around the aim).
    pub spread: f32,
    /// Beam length, or how far away you can build, in metres.
    pub range: f32,
    /// Explosion size in metres; 0 means the weapon does not explode.
    pub blast_radius: f32,
    pub blast_damage: f32,
    /// Speed change (m/s) given to a worm at the centre of a blast / on a hit.
    pub knockback: f32,
    /// Radius in metres of the terrain edit: the crater an explosion carves, or
    /// the lump a Build weapon adds. 0 leaves the terrain alone.
    pub terrain: f32,
    /// Projectiles per shot (the swarm fires several, each with its own spread).
    pub pellets: u8,
    /// Downward acceleration of the projectile in m/s^2.
    pub gravity: f32,
    /// 0 = ends at the first wall. Above 0 = ricochets off walls, keeping this
    /// fraction of its speed (1.0 loses none).
    pub bounce: f32,
    pub special: Special,
}

/// One weapon in a worm's hands: the table row's id and the changing numbers.
#[derive(Clone, Debug)]
pub struct WeaponState {
    /// Index into `WEAPONS`.
    pub id: u8,
    pub ammo: u16,
    pub cooldown: f32,
    /// Seconds of reload left; > 0 means "reloading".
    pub reload: f32,
    /// Seconds of wind-up built up for this weapon. Kept per weapon so a
    /// charge cannot be carried across a swap; switching cancels it.
    pub charge: f32,
}

impl WeaponState {
    pub fn full(id: u8) -> WeaponState {
        WeaponState { id, ammo: WEAPONS[id as usize].mag, cooldown: 0.0, reload: 0.0, charge: 0.0 }
    }
}

impl WeaponDef {
    /// The direction a shot really leaves the barrel: the aim raised by the
    /// weapon's loft. A lobbed weapon keeps a minimum elevation, so a level or
    /// downward crosshair still sends the shell up and over, and a maximum, so
    /// aiming at the sky cannot turn the arc back through the worm.
    pub fn launch_dir(&self, dir: Vec3) -> Vec3 {
        if self.loft == 0.0 {
            return dir;
        }
        let dir = if dir.length_squared() > 1.0e-8 { dir.normalize() } else { Vec3::Z };
        // Keep the yaw the player aimed and bend the pitch. For a straight-up
        // or straight-down aim the horizontal part has no direction; fall back
        // to the +Z convention the rest of the aim code uses.
        let flat = Vec3::new(dir.x, 0.0, dir.z);
        let fwd = if flat.length_squared() > 1.0e-6 { flat.normalize() } else { Vec3::Z };
        let pitch = (dir.y.clamp(-1.0, 1.0).asin() + self.loft).clamp(0.18, 1.30);
        let (sp, cp) = pitch.sin_cos();
        Vec3::new(fwd.x * cp, sp, fwd.z * cp)
    }
}

pub static WEAPONS: [WeaponDef; 13] = [
    WeaponDef {
        name: "Neon Blaster",
        kind: Kind::Projectile,
        // Balance (from 2D): the close-range gun; the long range is the railgun's.
        damage: 12.0,
        speed: 85.0,
        lifetime: 1.2,
        cooldown: 0.085,
        mag: 24,
        reload: 1.1,
        spread: 0.02,
        range: 0.0,
        blast_radius: 0.0,
        blast_damage: 0.0,
        knockback: 3.2,
        // A blaster bolt bites a visible crater out of rock. It is the gun you
        // always have, so it should be able to dig and shape, not just tickle.
        // At 0.28 m the hole was barely wider than one voxel (0.25 m) and the
        // rock looked untouched; 0.55 m leaves a hole you can actually see and
        // widen with a burst.
        terrain: 0.55,
        pellets: 1,
        gravity: 0.0,
        bounce: 0.0,
        special: Special::None,
        windup: 0.0,
        loft: 0.0,
    },
    WeaponDef {
        name: "Rocket",
        kind: Kind::Projectile,
        damage: 0.0,
        speed: 35.0,
        lifetime: 3.0,
        cooldown: 0.7,
        mag: 4,
        reload: 2.2,
        spread: 0.0,
        range: 0.0,
        blast_radius: 3.0,
        blast_damage: 60.0,
        knockback: 26.0,
        terrain: 2.5,
        pellets: 1,
        gravity: 0.0,
        bounce: 0.0,
        special: Special::None,
        windup: 0.0,
        loft: 0.0,
    },
    WeaponDef {
        name: "Railgun",
        kind: Kind::Beam,
        damage: 70.0,
        speed: 0.0,
        lifetime: 0.0,
        cooldown: 0.5,
        mag: 1,
        reload: 2.5,
        spread: 0.0,
        range: 120.0,
        blast_radius: 0.0,
        blast_damage: 0.0,
        knockback: 13.0,
        // It stops at the first rock and leaves it whole: digging is the
        // explosives' job (2D balance: a wall was no cover from a beam you
        // cannot see coming).
        terrain: 0.0,
        pellets: 1,
        gravity: 0.0,
        bounce: 0.0,
        special: Special::None,
        windup: 0.0,
        loft: 0.0,
    },
    WeaponDef {
        name: "Bouncer",
        kind: Kind::Projectile,
        damage: 25.0, // four hits kill
        speed: 45.0,
        lifetime: 3.0,
        cooldown: 0.35,
        mag: 8,
        reload: 1.6,
        spread: 0.015,
        range: 0.0,
        blast_radius: 0.0,
        blast_damage: 0.0,
        knockback: 5.0,
        terrain: 0.0,
        pellets: 1,
        // A little gravity: in 2D a bolt that bounces for three seconds filled
        // the room, but in a 3D cave it would simply never end.
        gravity: 10.0,
        bounce: 0.9,
        special: Special::None,
        windup: 0.0,
        loft: 0.0,
    },
    WeaponDef {
        name: "Bomb",
        kind: Kind::Projectile,
        damage: 0.0,
        // Dropped at your feet. It also goes off the moment an enemy touches
        // it, so it doubles as a mine. It falls, bounces once or twice and
        // comes to rest (see `bounce` and `gravity`), then waits for its fuse.
        speed: 0.0,
        lifetime: 1.2,
        cooldown: 0.8,
        mag: 2,
        reload: 3.0,
        spread: 0.0,
        range: 0.0,
        blast_radius: 5.5,
        blast_damage: 170.0,
        knockback: 26.0,
        terrain: 5.0,
        pellets: 1,
        gravity: 25.0,
        bounce: 0.3,
        special: Special::None,
        windup: 0.0,
        loft: 0.0,
    },
    WeaponDef {
        name: "Cluster",
        kind: Kind::Projectile,
        damage: 0.0,
        // A mortar, not a throw: slow enough and heavy enough that the arc
        // reads, and it is *launched* with a built-in loft so even a level
        // crosshair sends the shell up and over. At the default loft it peaks
        // around 8 m and lands about 23 m away; aiming higher trades range for
        // a taller curve. The fuse outlasts a steep lob so the shell comes back
        // down instead of bursting in the air halfway through its arc.
        speed: 22.0,
        lifetime: 2.6,
        cooldown: 0.9,
        mag: 3,
        reload: 3.0,
        spread: 0.02,
        range: 0.0,
        blast_radius: 0.0,
        blast_damage: 0.0,
        knockback: 0.0,
        terrain: 0.0,
        pellets: 1,
        gravity: 18.0, // a high lob: point up to arc it over cover
        bounce: 0.0,
        special: Special::Cluster(12, BOMBLET),
        windup: 0.0,
        loft: 0.55,
    },
    WeaponDef {
        name: "Swarm",
        kind: Kind::Projectile,
        damage: 0.0,
        speed: 22.0,
        lifetime: 2.2,
        cooldown: 1.0,
        mag: 2,
        reload: 3.2,
        spread: 0.5, // a wide fan, which then curls in on the target
        range: 0.0,
        blast_radius: 1.5,
        blast_damage: 14.0,
        knockback: 6.0,
        terrain: 1.0,
        pellets: 6,
        gravity: 0.0,
        bounce: 0.0,
        special: Special::Homing(3.0),
        windup: 0.0,
        loft: 0.0,
    },
    WeaponDef {
        name: "Teleport",
        kind: Kind::Projectile,
        damage: 0.0,
        speed: 40.0,
        lifetime: 1.2,
        cooldown: 0.6,
        mag: 3,
        reload: 2.5,
        spread: 0.0,
        range: 0.0,
        blast_radius: 0.0,
        blast_damage: 0.0,
        knockback: 0.0,
        terrain: 0.0,
        pellets: 1,
        gravity: 0.0,
        bounce: 0.0,
        special: Special::Teleport,
        windup: 0.0,
        loft: 0.0,
    },
    WeaponDef {
        name: "Nova",
        kind: Kind::Projectile,
        damage: 0.0,
        speed: 18.0,
        lifetime: 5.0,
        cooldown: 1.0,
        mag: 1,
        reload: 8.0,
        spread: 0.0,
        range: 0.0,
        blast_radius: 8.0, // it clears a cavern
        blast_damage: 140.0,
        knockback: 36.0,
        terrain: 7.0,
        pellets: 1,
        gravity: 0.0,
        bounce: 0.0,
        special: Special::None,
        // The one weapon that does not fire on the click: a 1.2 s charge-up.
        // That is the difference between "a bigger rocket" and "a decision".
        windup: 1.2,
        loft: 0.0,
    },
    WeaponDef {
        name: "Block",
        kind: Kind::Wall,
        damage: 0.0,
        speed: 0.0,
        lifetime: 0.0,
        // One deliberate shield per press. The old fast cooldown let the aim
        // sweep out a whole fortress of lumps; a wall that covers the worm
        // should cost a beat and be worth placing.
        cooldown: 0.50,
        mag: 4,
        reload: 2.0,
        spread: 0.0,
        range: 0.0,
        blast_radius: 0.0,
        blast_damage: 0.0,
        knockback: 0.0,
        terrain: 0.0,
        pellets: 1,
        gravity: 0.0,
        bounce: 0.0,
        special: Special::None,
        windup: 0.0,
        loft: 0.0,
    },
    WeaponDef {
        name: "Tractor",
        kind: Kind::Build,
        damage: 0.0,
        speed: 0.0,
        lifetime: 0.0,
        // A lump every fourth tick while the button is held. Every tick was
        // too fast: the beam extrudes rock towards you along the aim (each lump
        // is placed on the rock the last one made), so it filled the gap to
        // the surface in under a second. At 16 lumps a second it grows at about
        // 8 m/s, and stops short of your face (see `build_spot`).
        cooldown: 0.06,
        mag: 60, // nearly four seconds of beam
        reload: 3.0,
        spread: 0.0,
        range: 12.0,
        blast_radius: 0.0,
        blast_damage: 0.0,
        knockback: 0.0,
        terrain: 0.8,
        pellets: 1,
        gravity: 0.0,
        bounce: 0.0,
        special: Special::None,
        windup: 0.0,
        loft: 0.0,
    },
    WeaponDef {
        name: "Plasma Whip",
        kind: Kind::Whip,
        // A long reach and a slow cadence, but a hard hit: this is the
        // close-quarters answer to a rocket in the face.
        damage: 44.0,
        speed: 0.0,
        lifetime: 0.0,
        // A lash is a whole-body swing, not a flick of the wrist. The lash is
        // always carried, so the wind-up only coils it: a short tell, then the
        // crack, then a beat before the next swing. Holding the trigger keeps
        // the whip swinging; it is melee, not a magazine.
        cooldown: 0.55,
        mag: 0,
        reload: 0.0,
        spread: 0.0,
        // The carried lash is about six metres; a strike extends it to
        // fourteen. `terrain` is the radius of the shallow groove it burns
        // across whatever rock the sweep crosses.
        range: 14.0,
        blast_radius: 0.0,
        blast_damage: 0.0,
        knockback: 18.0,
        terrain: 0.30,
        pellets: 1,
        gravity: 0.0,
        bounce: 0.0,
        special: Special::None,
        // A short coil, not the old raised-over-the-shoulder draw. The lash is
        // never put away, so the tell has to live in its shape, not in hiding it.
        windup: 0.14,
        loft: 0.0,
    },
    // Not in anyone's arsenal: what the Cluster shell splits into.
    WeaponDef {
        name: "Bomblet",
        kind: Kind::Projectile,
        damage: 0.0,
        speed: 12.0,
        lifetime: 0.55,
        cooldown: 0.0,
        mag: 0,
        reload: 0.0,
        spread: 0.0,
        range: 0.0,
        blast_radius: 2.8,
        blast_damage: 42.0,
        knockback: 20.0,
        terrain: 2.4,
        pellets: 1,
        gravity: 10.0,
        bounce: 0.0,
        special: Special::None,
        windup: 0.0,
        loft: 0.0,
    },
];
