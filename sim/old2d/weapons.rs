// Pattern: Type Object. A "weapon type" is a row of data in a table, not a
// Rust type. The weapons below behave differently mostly because their
// numbers differ; the few genuinely new mechanics (a swarm's several missiles,
// a ricochet) are one flag or number each, read in
// `world.rs`. More weapons later means more rows, and a little code only for
// mechanics that are more than a number (`Special`: homing, a cluster burst, a
// teleport, and later things like gravity wells).
//
// Pattern: Flyweight. A projectile in flight stores just a weapon id and its
// own position/velocity. The shared, unchanging stats live once in `WEAPONS`.

pub const BLASTER: u8 = 0;
pub const ROCKET: u8 = 1;
pub const RAILGUN: u8 = 2;
/// Not a gun: places a brick. It took the shotgun's slot (the Scatter was the
/// least interesting weapon), so building costs a loadout pick like any weapon.
pub const BUILD: u8 = 3;
pub const BOUNCER: u8 = 4;
pub const BOMB: u8 = 5;
pub const CLUSTER: u8 = 6;
pub const SWARM: u8 = 7;
pub const TELEPORT: u8 = 8;
pub const NOVA: u8 = 9;
/// What a Cluster shell splits into. Not in the loadout: only the shell makes it.
pub const BOMBLET: u8 = 10;

/// Every weapon a player can choose from, in the order the loadout screen lists
/// them. (The menu's shooting range hands out all of them.)
pub const ARSENAL: [u8; 10] = [BLASTER, BUILD, ROCKET, BOUNCER, BOMB, RAILGUN, CLUSTER, SWARM, TELEPORT, NOVA];

/// How many weapons a player carries into a match, picked from the arsenal
/// before it starts (like Liero's weapon menu). Keys 1-5 select them.
pub const LOADOUT_SIZE: usize = 5;

/// What bots carry, and what the picker starts with.
pub const DEFAULT_LOADOUT: [u8; LOADOUT_SIZE] = [BLASTER, BOUNCER, ROCKET, BOMB, RAILGUN];

/// Is this an acceptable choice: the right number of different arsenal weapons?
/// A loadout arrives from another player's machine, and nothing that comes from
/// outside is trusted, so the host checks it before it touches the world.
pub fn valid_loadout(ids: &[u8]) -> bool {
    ids.len() == LOADOUT_SIZE
        && ids.iter().all(|id| ARSENAL.contains(id))
        && ids.iter().enumerate().all(|(i, id)| !ids[..i].contains(id))
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Kind {
    /// Flies through the world as a `Projectile`.
    Projectile,
    /// Instant line; resolved the moment you pull the trigger.
    Beam,
    /// Puts a brick on the first free tile along the aim, within `range`.
    /// The magazine is the number of blocks you carry.
    Build,
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
    /// Where it lands, its owner appears (or swaps places with the player it hit).
    Teleport,
}

pub struct WeaponDef {
    pub name: &'static str,
    pub kind: Kind,
    /// Damage on a direct hit (or to each player a beam passes through).
    pub damage: f32,
    /// Projectile speed in px/s. Unused by beams. A speed of 0 means the
    /// projectile is dropped at the player's feet, and `lifetime` is its fuse.
    pub speed: f32,
    /// Seconds before an unspent projectile disappears (or explodes).
    pub lifetime: f32,
    /// Seconds between shots.
    pub cooldown: f32,
    /// Rounds per magazine, and seconds to reload once it is empty.
    pub mag: u8,
    pub reload: f32,
    /// Random aim error in radians (each side).
    pub spread: f32,
    /// Beam length (or how far away you can build) in px. Unused by projectiles.
    pub range: f32,
    /// Explosion size in px; 0 means the weapon does not explode.
    pub blast_radius: f32,
    pub blast_damage: f32,
    /// Speed change (px/s) given to a player at the centre of a blast / on a hit.
    pub knockback: f32,
    /// How much a brick is hurt by this weapon (bricks have 3 hp). Beams
    /// ignore it: they stop at the first brick (see `fire_beam`).
    pub tile_damage: u8,
    /// Projectiles per shot (the swarm fires several, each with its own spread).
    pub pellets: u8,
    /// Does the projectile ricochet off walls instead of stopping at them?
    pub bounce: bool,
    pub special: Special,
}

pub static WEAPONS: [WeaponDef; 11] = [
    WeaponDef {
        name: "Neon Blaster",
        kind: Kind::Projectile,
        // Balance: was 12 damage and 1.2 s (840 px), the best gun at every
        // range. Now it is the close-range gun, and the long range is the
        // railgun's.
        damage: 10.0,
        speed: 700.0,
        lifetime: 0.9,
        cooldown: 0.12,
        mag: 20,
        reload: 1.2,
        spread: 0.04,
        range: 0.0,
        blast_radius: 0.0,
        blast_damage: 0.0,
        knockback: 60.0,
        tile_damage: 0,
        pellets: 1,
        bounce: false,
        special: Special::None,
    },
    WeaponDef {
        name: "Rocket",
        kind: Kind::Projectile,
        damage: 0.0,
        speed: 420.0,
        lifetime: 3.0,
        cooldown: 0.7,
        mag: 4,
        reload: 2.2,
        spread: 0.0,
        range: 0.0,
        blast_radius: 72.0,
        blast_damage: 60.0,
        knockback: 650.0,
        tile_damage: 3,
        pellets: 1,
        bounce: false,
        special: Special::None,
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
        range: 1500.0,
        blast_radius: 0.0,
        blast_damage: 0.0,
        knockback: 350.0,
        // Balance: the rail used to cut two bricks, so a brick was no cover
        // from an instant, map-long beam you cannot see coming. Now bricks
        // stop it and digging is the explosives' job.
        tile_damage: 0,
        pellets: 1,
        bounce: false,
        special: Special::None,
    },
    WeaponDef {
        name: "Block",
        kind: Kind::Build,
        damage: 0.0,
        speed: 0.0,
        lifetime: 0.0,
        cooldown: 0.15, // fast enough to throw up a wall in one sweep of the aim
        mag: 6,
        reload: 2.0,
        spread: 0.0,
        range: 96.0, // three tiles
        blast_radius: 0.0,
        blast_damage: 0.0,
        knockback: 0.0,
        tile_damage: 0,
        pellets: 1,
        bounce: false,
        special: Special::None,
    },
    WeaponDef {
        name: "Bouncer",
        kind: Kind::Projectile,
        damage: 25.0, // four hits kill (22 needed five, and lost to the blaster)
        speed: 520.0,
        lifetime: 3.0,
        cooldown: 0.35,
        mag: 8,
        reload: 1.6,
        spread: 0.02,
        range: 0.0,
        blast_radius: 0.0,
        blast_damage: 0.0,
        knockback: 120.0,
        tile_damage: 0,
        pellets: 1,
        bounce: true,
        special: Special::None,
    },
    WeaponDef {
        name: "Bomb",
        kind: Kind::Projectile,
        damage: 0.0,
        // Dropped at your feet. It also goes off the moment an enemy touches
        // it, so it doubles as a mine.
        // Balance: it used to be a Bomberman plus, 75 damage, 1.8 s fuse:
        // standing diagonally from it was perfectly safe, and anyone who walked
        // off within 1.4 s took nothing. Now it is round, with the same reach as
        // the plus had (3.5 tiles) in every direction, and a shorter fuse: you
        // must react within about half a second.
        speed: 0.0,
        lifetime: 1.2,
        cooldown: 0.8,
        mag: 2,
        reload: 3.0,
        spread: 0.0,
        range: 0.0,
        blast_radius: 112.0,
        blast_damage: 120.0,
        knockback: 450.0,
        tile_damage: 3,
        pellets: 1,
        bounce: false,
        special: Special::None,
    },
    // ---- the second batch ---------------------------------------------------
    WeaponDef {
        name: "Cluster",
        kind: Kind::Projectile,
        damage: 0.0,
        speed: 360.0,
        lifetime: 0.8, // bursts about 290 px out if nothing stops it first
        cooldown: 0.9,
        mag: 3,
        reload: 3.0,
        spread: 0.02,
        range: 0.0,
        blast_radius: 0.0,
        blast_damage: 0.0,
        knockback: 0.0,
        tile_damage: 0,
        pellets: 1,
        bounce: false,
        special: Special::Cluster(7, BOMBLET),
    },
    WeaponDef {
        name: "Swarm",
        kind: Kind::Projectile,
        damage: 0.0,
        speed: 300.0,
        lifetime: 2.2,
        cooldown: 1.0,
        mag: 2,
        reload: 3.2,
        spread: 0.6, // a wide fan, which then curls in on the target
        range: 0.0,
        blast_radius: 30.0,
        blast_damage: 14.0,
        knockback: 150.0,
        tile_damage: 1,
        pellets: 6,
        bounce: false,
        special: Special::Homing(3.0),
    },
    WeaponDef {
        name: "Teleport",
        kind: Kind::Projectile,
        damage: 0.0,
        speed: 520.0,
        lifetime: 1.2,
        cooldown: 0.6,
        mag: 3,
        reload: 2.5,
        spread: 0.0,
        range: 0.0,
        blast_radius: 0.0,
        blast_damage: 0.0,
        knockback: 0.0,
        tile_damage: 0,
        pellets: 1,
        bounce: false,
        special: Special::Teleport,
    },
    WeaponDef {
        name: "Nova",
        kind: Kind::Projectile,
        damage: 0.0,
        speed: 240.0,
        lifetime: 5.0,
        cooldown: 1.0,
        mag: 1,
        reload: 8.0,
        spread: 0.0,
        range: 0.0,
        blast_radius: 230.0, // seven tiles: it clears a room
        blast_damage: 140.0,
        knockback: 1000.0,
        tile_damage: 3,
        pellets: 1,
        bounce: false,
        special: Special::None,
    },
    // Not in anyone's loadout: what the Cluster shell splits into.
    WeaponDef {
        name: "Bomblet",
        kind: Kind::Projectile,
        damage: 0.0,
        speed: 240.0,
        lifetime: 0.55,
        cooldown: 0.0,
        mag: 0,
        reload: 0.0,
        spread: 0.0,
        range: 0.0,
        blast_radius: 40.0,
        blast_damage: 24.0,
        knockback: 300.0,
        tile_damage: 2,
        pellets: 1,
        bounce: false,
        special: Special::None,
    },
];
