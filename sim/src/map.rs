// The demo map: rolling hills, a big cavern with pillars, tunnels to far
// chambers and up to the surface, floating rocks to grapple, and some grey
// bedrock that nothing can dig. Everything is built from signed distances
// (negative = rock) combined with min (add rock) and max(-x) (carve), the same
// two moves the player makes with the brush.
//
// `Grid::from_fn` skips chunks that are far from the surface, which is only
// valid if this function never OVER-estimates the distance. So the height
// field uses a slope-corrected estimate and the shapes are lower bounds.
//
// Every client and the server run this same generator and get the same map
// (it is pure integer-hash noise: no clock, no thread timing). That is why a
// `Welcome` carries only the terrain EDITS and not the map. It also means a
// change here is a protocol change: bump `PROTOCOL_VERSION`.

use glam::{IVec3, Vec3};
use sdf::{Grid, Material};

use crate::rng::Rng;

/// 128 x 64 x 128 m: 32 x 16 x 32 chunks of 16 voxels of 0.25 m.
pub const WORLD_CHUNKS: IVec3 = IVec3::new(32, 16, 32);
/// The same size in metres.
pub const WORLD_METRES: Vec3 = Vec3::new(
    WORLD_CHUNKS.x as f32 * sdf::CHUNK as f32 * sdf::VOX,
    WORLD_CHUNKS.y as f32 * sdf::CHUNK as f32 * sdf::VOX,
    WORLD_CHUNKS.z as f32 * sdf::CHUNK as f32 * sdf::VOX,
);

/// The most worms a match holds (humans and NPCs together).
pub const MAX_PLAYERS: usize = 4;

/// A map to choose from. All of them come out of the same generator with
/// different numbers, so there is one set of code to keep right (and nothing to
/// send over the wire but the index: a `Welcome` carries `map: u8`).
pub struct MapDef {
    pub name: &'static str,
    /// One line for the card on the setup page.
    pub blurb: &'static str,
    /// The height of the ground before the hills are added, in metres.
    pub base: f32,
    /// How tall the hills are, as a multiple of the valley's.
    pub hills: f32,
    /// Height of the mountains round the edge, in metres.
    pub rim: f32,
    /// A cavern with tunnels, chambers and shafts under the hills?
    pub caves: bool,
    /// Rock all the way up with only a network of chambers and tunnels inside:
    /// there is no outdoors, and the worms start in the chambers.
    pub maze: bool,
    /// How fast the distance field grows with depth far from the surface (see
    /// `ground`): 1 / sqrt(1 + the steepest slope squared), so a map with
    /// steeper hills needs a smaller number.
    pub far: f32,
}

pub const MAPS: [MapDef; 3] = [
    MapDef { name: "Valley", blurb: "hills with a cavern and tunnels", base: 10.0, hills: 1.0, rim: 56.0, caves: true, maze: false, far: 0.2 },
    MapDef { name: "Plains", blurb: "open and flat with no caves", base: 10.0, hills: 0.35, rim: 22.0, caves: false, maze: false, far: 0.2 },
    // A flat roof 45 m up, with the chambers and tunnels carved below it.
    MapDef { name: "Tunnels", blurb: "no outdoors - only tunnels", base: 46.0, hills: 0.0, rim: 0.0, caves: false, maze: true, far: 0.5 },
];

/// The tunnel map's chambers: a 3 x 3 grid across the arena, each at one of
/// three heights so the tunnels between them climb and drop.
fn maze_chamber(i: usize, j: usize) -> (Vec3, Vec3) {
    let at = |n: usize| [26.0, 64.0, 102.0][n];
    let y = 14.0 + ((i + j) % 3) as f32 * 8.0;
    (Vec3::new(at(i), y, at(j)), Vec3::new(9.0, 5.0, 9.0))
}

/// The tunnel map's air: every chamber, and a tunnel from each to the next one
/// east and south, so there are loops and no dead ends.
fn maze_cave(p: Vec3) -> f32 {
    let mut cave = f32::MAX;
    for i in 0..3 {
        for j in 0..3 {
            let (c, radii) = maze_chamber(i, j);
            cave = smin(cave, ellipsoid(p, c, radii), 3.0);
            for (ni, nj) in [(i + 1, j), (i, j + 1)] {
                if ni < 3 && nj < 3 {
                    cave = smin(cave, capsule(p, c, maze_chamber(ni, nj).0, 2.8), 3.0);
                }
            }
        }
    }
    cave
}

/// Where worms start and respawn, as (x, z) columns: they drop from the sky onto
/// the hills. `count` well-separated spots in the valley, picked from `seed`, so
/// every match starts somewhere new and nobody spawns on top of anybody.
///
/// Pure and deterministic like the rest of the sim: the host calls it once per
/// match and the clients never need to (the worms' places come in the snapshot).
/// Columns that would drop a worm onto a floating rock, the sky bridge or down a
/// shaft into the cavern are skipped.
pub fn spawn_points(map: u8, seed: u32, count: usize) -> Vec<Vec3> {
    // The valley floor: inside the rim, where the hills are gentle.
    const LO: f32 = 32.0;
    const HI: f32 = 96.0;
    // Worms drop from here on the open maps (what `World::new` does too).
    const TOP: f32 = WORLD_METRES.y - 0.5;
    let m = (map as usize).min(MAPS.len() - 1);
    let mut rng = Rng::new(seed.wrapping_mul(0x9E37_79B1) ^ 0x5bd1_e995);
    let mut points: Vec<Vec3> = Vec::with_capacity(count);
    // Ask for this much room between spawns, and ask for less after every 50
    // failed tries: a crowded valley still ends with `count` points.
    let mut apart = 20.0_f32;
    let mut failures = 0;
    while points.len() < count {
        let from = if MAPS[m].maze {
            // A random chamber, and a spot above its floor.
            let (c, radii) = maze_chamber(rng.next_u32() as usize % 3, rng.next_u32() as usize % 3);
            Vec3::new(c.x + rng.range(-radii.x, radii.x) * 0.6, c.y, c.z + rng.range(-radii.z, radii.z) * 0.6)
        } else {
            Vec3::new(rng.range(LO, HI), TOP, rng.range(LO, HI))
        };
        let spaced = points.iter().all(|q| (q.x - from.x).hypot(q.z - from.z) >= apart);
        if spaced && open_ground(m, from) {
            points.push(from);
        } else {
            failures += 1;
            if failures % 50 == 0 {
                apart = (apart - 3.0).max(0.0);
            }
        }
    }
    points
}

/// Is a worm dropped from `from` going to land somewhere good? On the open maps
/// the first rock met from the sky must be the height field itself: a floating
/// rock, the sky bridge or the mouth of a shaft puts it somewhere else. On the
/// tunnel map the start must be in the air of a chamber, with floor below.
fn open_ground(map: usize, from: Vec3) -> bool {
    if MAPS[map].maze {
        return terrain_in(map, from) > 1.0 && terrain_in(map, from - Vec3::Y * 12.0) < 0.0;
    }
    let (x, z) = (from.x, from.z);
    let mut y = SKY_CEILING;
    while y > 0.0 {
        if terrain_in(map, Vec3::new(x, y, z)) < 0.0 {
            return (y - height(map, x, z)).abs() < 1.5;
        }
        y -= 0.5;
    }
    false
}

/// Thickness of the indestructible rock around the arena: the four walls and
/// the floor. Players can throw anything at it; it does not move.
const SOLID_THICKNESS: f32 = 3.0;

/// Air between the tallest terrain and the world's top face. A surface that
/// reached the boundary would be clipped there ("an open SDF"), so nothing is
/// built above this line.
const SKY_MARGIN: f32 = 8.0;
const SKY_CEILING: f32 = WORLD_METRES.y - SKY_MARGIN;

/// The rim peaks stay below the solid wall, so a worm cannot climb a mountain
/// and jump over the arena's frame.
const RIM_CEILING: f32 = SKY_CEILING - 8.0;

/// The valley, the map everything was first made for.
pub fn build() -> Grid {
    build_map(0)
}

/// Map number `map` (an index into `MAPS`; anything else is the valley).
///
/// `from_fn_material` takes plain fn pointers (so its worker threads can share
/// them), which cannot carry the map number: hence one small fn per map.
pub fn build_map(map: u8) -> Grid {
    let terrain: fn(Vec3) -> f32 = match map {
        1 => |p| terrain_in(1, p),
        2 => |p| terrain_in(2, p),
        _ => |p| terrain_in(0, p),
    };
    Grid::from_fn_material(WORLD_CHUNKS, terrain, material)
}

/// The map's heights seen from above, `n` x `n` values in 0..1 (row by row from
/// the north-west corner), for the little picture on the setup page. It reads
/// the height function itself, not the grid, so no map has to be built to show it.
pub fn preview(map: u8, n: usize) -> Vec<f32> {
    let m = (map as usize).min(MAPS.len() - 1);
    (0..n * n)
        .map(|i| {
            let (x, z) = (((i % n) as f32 + 0.5) / n as f32 * 128.0, ((i / n) as f32 + 0.5) / n as f32 * 128.0);
            if MAPS[m].maze {
                // There is no outside to look down on: show where the air is,
                // at the heights of the chambers (low = air, high = rock).
                let open = [14.0, 22.0, 30.0].iter().any(|&y| terrain_in(m, Vec3::new(x, y, z)) > 0.0);
                return if open { 0.2 } else { 0.9 };
            }
            (height_raw(m, x, z) / RIM_CEILING).clamp(0.0, 1.0)
        })
        .collect()
}

fn hash2(x: i32, z: i32) -> f32 {
    // Integer hash (PCG-style mixing), mapped to 0..1.
    let mut h = (x as u32).wrapping_mul(0x9E37_79B1) ^ (z as u32).wrapping_mul(0x85EB_CA77);
    h ^= h >> 15;
    h = h.wrapping_mul(0x2C1B_3C6D);
    h ^= h >> 12;
    h = h.wrapping_mul(0x297A_2D39);
    h ^= h >> 15;
    (h >> 8) as f32 / (1u32 << 24) as f32
}

/// Smooth value noise in 0..1.
fn noise2(x: f32, z: f32) -> f32 {
    let (xi, zi) = (x.floor(), z.floor());
    let (fx, fz) = (x - xi, z - zi);
    let (sx, sz) = (fx * fx * (3.0 - 2.0 * fx), fz * fz * (3.0 - 2.0 * fz));
    let (xi, zi) = (xi as i32, zi as i32);
    let a = hash2(xi, zi);
    let b = hash2(xi + 1, zi);
    let c = hash2(xi, zi + 1);
    let d = hash2(xi + 1, zi + 1);
    let top = a + (b - a) * sx;
    let bottom = c + (d - c) * sx;
    top + (bottom - top) * sz
}

/// The height field as a lookup table, filled once. `ground` needs the height
/// five times per voxel and the noise behind it is not cheap: evaluating it
/// directly made map generation take 19 s. Spacing is one voxel, so reading the
/// table with bilinear interpolation loses nothing the grid could show.
fn height(map: usize, x: f32, z: f32) -> f32 {
    use std::sync::OnceLock;
    const STEP: f32 = 0.25;
    const N: usize = 513; // 0..=128 m
    // One table per map, each filled the first time that map is asked for.
    static TABLES: [OnceLock<Vec<f32>>; MAPS.len()] = [OnceLock::new(), OnceLock::new(), OnceLock::new()];
    let table = TABLES[map].get_or_init(|| {
        (0..N * N).map(|i| height_raw(map, (i % N) as f32 * STEP, (i / N) as f32 * STEP)).collect()
    });
    let (gx, gz) = ((x / STEP).clamp(0.0, (N - 1) as f32 - 0.001), (z / STEP).clamp(0.0, (N - 1) as f32 - 0.001));
    let (ix, iz) = (gx as usize, gz as usize);
    let (fx, fz) = (gx - ix as f32, gz - iz as f32);
    let at = |x: usize, z: usize| table[z * N + x];
    let a = at(ix, iz) + (at(ix + 1, iz) - at(ix, iz)) * fx;
    let b = at(ix, iz + 1) + (at(ix + 1, iz + 1) - at(ix, iz + 1)) * fx;
    a + (b - a) * fz
}

/// Rolling hills, with mountains rising around the edge so the arena reads as
/// a valley. The mountains stop below the solid frame, which stands above them
/// at the very edge of the world.
fn height_raw(map: usize, x: f32, z: f32) -> f32 {
    let def = &MAPS[map];
    let hills = (noise2(x * 0.02, z * 0.02) * 16.0 + noise2(x * 0.045, z * 0.045) * 8.0 + noise2(x * 0.1, z * 0.1) * 2.5)
        * def.hills;
    // Distance to the arena edge. A max-norm would be exact but has creases
    // along the diagonals, where the height field would fold and the distance
    // estimate with it. The 8-norm is a smooth, squared-off stand-in.
    let r = ((x - 64.0).abs().powi(8) + (z - 64.0).abs().powi(8)).powf(0.125);
    let t = (1.0 - (64.0 - r) / 30.0).clamp(0.0, 1.0);
    let rim = t * t * (3.0 - 2.0 * t) * def.rim;
    // The peaks stop well below the grid's top face. A soft min rounds the
    // plateau over, and every surface (mountain, wall, floating rock) closes
    // inside the world instead of being cut open by the boundary.
    smin(def.base + hills + rim, RIM_CEILING, 6.0)
}

/// A lower bound of the distance to the height field (`Grid::from_fn` needs
/// one: an over-estimate could drop a chunk that holds surface).
///
/// Within 2 m of the surface: height difference divided by sqrt(1 + slope^2),
/// the first-order estimate, which is accurate there. Beyond that the estimate
/// drifts, because it ignores how the flank curves away, and the error grows
/// with depth. Far from the surface only the steepest possible slope is safe:
/// moving 1 m further away increases the distance by at least
/// 1 / sqrt(1 + max_slope^2), about 0.2 here (mountain flanks reach slope ~4.8).
fn ground(map: usize, p: Vec3) -> f32 {
    let e = 0.5;
    let gx = (height(map, p.x + e, p.z) - height(map, p.x - e, p.z)) / (2.0 * e);
    let gz = (height(map, p.x, p.z + e) - height(map, p.x, p.z - e)) / (2.0 * e);
    let local = 0.9 / (1.0 + gx * gx + gz * gz).sqrt();
    let u = p.y - height(map, p.x, p.z);
    let near = u.clamp(-2.0, 2.0);
    local * near + (u - near) * MAPS[map].far
}

/// Polynomial smooth minimum: min(a, b) with the crease rounded over width k.
/// Hard min/max leave razor-sharp creases that 0.25 m voxels cannot hold (they
/// come out as sawteeth along every cave lip), and caves look better rounded.
fn smin(a: f32, b: f32, k: f32) -> f32 {
    let h = (k - (a - b).abs()).max(0.0) / k;
    a.min(b) - h * h * k * 0.25
}

fn smax(a: f32, b: f32, k: f32) -> f32 {
    -smin(-a, -b, k)
}

fn sphere(p: Vec3, c: Vec3, r: f32) -> f32 {
    (p - c).length() - r
}

/// Lower bound of the distance to an ellipsoid.
fn ellipsoid(p: Vec3, c: Vec3, radii: Vec3) -> f32 {
    (((p - c) / radii).length() - 1.0) * radii.min_element()
}

fn capsule(p: Vec3, a: Vec3, b: Vec3, r: f32) -> f32 {
    let ab = b - a;
    let t = ((p - a).dot(ab) / ab.length_squared()).clamp(0.0, 1.0);
    (p - (a + ab * t)).length() - r
}

/// Exact distance to a box with rounded edges (sharp corners would come out as
/// sawteeth on 0.25 m voxels). `half` is the half-size including the rounding.
fn slab(p: Vec3, c: Vec3, half: Vec3, round: f32) -> f32 {
    let q = (p - c).abs() - (half - Vec3::splat(round));
    q.max(Vec3::ZERO).length() + q.max_element().min(0.0) - round
}

/// Pillars that hold up the cavern roof: (x, z) of each column.
const PILLARS: [(f32, f32); 4] = [(52.0, 52.0), (78.0, 56.0), (60.0, 78.0), (82.0, 78.0)];

/// Round tunnels from the cavern out to chambers under the foothills:
/// (from, to, radius). Each starts inside the cavern ellipsoid.
const TUNNELS: [(Vec3, Vec3, f32); 5] = [
    (Vec3::new(80.0, 9.0, 50.0), Vec3::new(106.0, 10.0, 24.0), 2.5),
    (Vec3::new(84.0, 9.0, 76.0), Vec3::new(106.0, 11.0, 100.0), 2.5),
    (Vec3::new(48.0, 9.0, 78.0), Vec3::new(22.0, 10.0, 102.0), 2.5),
    // The southern corridor links the two far chambers without the cavern.
    (Vec3::new(22.0, 10.0, 102.0), Vec3::new(64.0, 7.0, 114.0), 2.2),
    (Vec3::new(64.0, 7.0, 114.0), Vec3::new(106.0, 11.0, 100.0), 2.2),
];

/// Chambers at the tunnels' far ends: (centre, radii).
const CHAMBERS: [(Vec3, Vec3); 3] = [
    (Vec3::new(106.0, 10.0, 24.0), Vec3::new(7.0, 6.0, 7.0)),
    (Vec3::new(106.0, 11.0, 100.0), Vec3::new(8.0, 6.0, 8.0)),
    (Vec3::new(22.0, 10.0, 102.0), Vec3::new(7.0, 6.0, 7.0)),
];

/// Shafts from each chamber up to the surface: (from, to, radius).
const SHAFTS: [(Vec3, Vec3, f32); 3] = [
    (Vec3::new(106.0, 10.0, 24.0), Vec3::new(92.0, 44.0, 38.0), 2.5),
    (Vec3::new(106.0, 11.0, 100.0), Vec3::new(100.0, 44.0, 70.0), 2.5),
    (Vec3::new(22.0, 10.0, 102.0), Vec3::new(34.0, 44.0, 92.0), 2.5),
];

/// Indestructible chokepoints: (index into TUNNELS, position along it 0..1).
/// A grey disc of bedrock around the tunnel, so the passage through it cannot
/// be widened and the rock around it is thick enough that going round is work.
const GATES: [(usize, f32); 3] = [(0, 0.55), (1, 0.5), (2, 0.5)];
const GATE_RADIUS: f32 = 7.5;
const GATE_HALF_THICKNESS: f32 = 1.0;

/// Free-floating rock spheres, grapple hook anchors: (centre, radius). Spaced
/// so every one has another within the rope's 30 m, which lets a player swing
/// across the valley from one to the next.
const FLOATERS: [(Vec3, f32); 11] = [
    (Vec3::new(30.0, 48.0, 80.0), 6.0),
    (Vec3::new(64.0, 44.0, 18.0), 4.0),
    (Vec3::new(44.0, 41.0, 30.0), 3.0),
    (Vec3::new(80.0, 42.0, 52.0), 3.5),
    (Vec3::new(50.0, 47.0, 58.0), 2.5),
    (Vec3::new(28.0, 44.0, 56.0), 3.0),
    (Vec3::new(96.0, 45.0, 68.0), 3.0),
    (Vec3::new(78.0, 50.0, 92.0), 3.5),
    (Vec3::new(58.0, 46.0, 90.0), 3.0),
    (Vec3::new(104.0, 46.0, 60.0), 3.0),
    (Vec3::new(14.0, 40.0, 40.0), 3.0),
];

/// Everything indestructible except the frame: pillars, a bridge across the
/// cavern, a sky platform and a tall spire. The same function builds the
/// shape and decides the material, so they cannot drift apart.
fn bedrock(p: Vec3) -> f32 {
    let mut d = f32::MAX;
    for (x, z) in PILLARS {
        d = d.min(capsule(p, Vec3::new(x, 0.0, z), Vec3::new(x, 18.0, z), 2.2));
    }
    // Walkway between two of the pillars, with a gap above it to the roof.
    d = d.min(slab(p, Vec3::new(65.0, 11.0, 54.0), Vec3::new(13.0, 0.8, 1.8), 0.6));
    // A bridge in the sky over the arena's middle, out of reach of the ground.
    d = d.min(slab(p, Vec3::new(64.0, 42.0, 64.0), Vec3::new(10.0, 1.2, 4.0), 0.8));
    // A landmark spire in the south-west, rising out of the frame's floor.
    d = d.min(slab(p, Vec3::new(46.0, 25.0, 100.0), Vec3::new(2.5, 26.0, 2.5), 0.8));
    d
}

/// True inside one of the gates' discs (the tunnel's own air is carved
/// separately, so only the rock around it turns grey).
fn in_gate(p: Vec3) -> bool {
    GATES.iter().any(|&(i, t)| {
        let (a, b, _) = TUNNELS[i];
        let dir = (b - a).normalize();
        let q = p - (a + (b - a) * t);
        let along = q.dot(dir);
        along.abs() < GATE_HALF_THICKNESS && (q - dir * along).length() < GATE_RADIUS
    })
}

/// Signed distance to the indestructible frame: four walls and the floor,
/// stopped `SKY_MARGIN` below the world's top face so the frame's own top face
/// closes inside the grid. Negative inside the solid rock. The pieces are half-spaces, so
/// `min` for their union and `max` for the intersection stay exact distances.
fn frame(p: Vec3) -> f32 {
    let wall = (p.x - SOLID_THICKNESS)
        .min(WORLD_METRES.x - SOLID_THICKNESS - p.x)
        .min(p.z - SOLID_THICKNESS)
        .min(WORLD_METRES.z - SOLID_THICKNESS - p.z);
    let floor = p.y - SOLID_THICKNESS;
    wall.min(floor).max(p.y - SKY_CEILING)
}

/// The material of the rock at `p`: the frame, the bedrock shapes and the
/// tunnel gates are `Solid` and everything else is ordinary, destructible
/// `Rock`. Air gets whatever falls out of this; the grid only stores it where
/// there is rock.
fn material(p: Vec3) -> Material {
    if frame(p) < 0.0 || bedrock(p) < 0.0 || in_gate(p) {
        Material::Solid
    } else {
        Material::Rock
    }
}

/// The valley's distance field (what the tests below look at).
pub fn terrain(p: Vec3) -> f32 {
    terrain_in(0, p)
}

fn terrain_in(map: usize, p: Vec3) -> f32 {
    // Rock below the height field.
    let mut d = ground(map, p);

    if MAPS[map].caves {
        // Carve a cavern and tunnels out of it.
        let cavern_c = Vec3::new(64.0, 9.0, 64.0);
        let mut cave = ellipsoid(p, cavern_c, Vec3::new(26.0, 8.0, 22.0));
        for (top, r) in [
            (Vec3::new(34.0, 40.0, 40.0), 3.5),
            (Vec3::new(96.0, 40.0, 90.0), 4.5),
            (Vec3::new(70.0, 40.0, 112.0), 3.0),
        ] {
            cave = smin(cave, capsule(p, cavern_c, top, r), 3.0);
        }
        // A long side tunnel, so there is something to fly through.
        cave = smin(cave, capsule(p, Vec3::new(40.0, 9.0, 64.0), Vec3::new(8.0, 16.0, 20.0), 3.0), 3.0);
        // The far chambers with their tunnels and the shafts up to the surface.
        for (c, radii) in CHAMBERS {
            cave = smin(cave, ellipsoid(p, c, radii), 3.0);
        }
        for (a, b, r) in TUNNELS.into_iter().chain(SHAFTS) {
            cave = smin(cave, capsule(p, a, b, r), 3.0);
        }
        d = smax(d, -cave, 1.5);
    }

    if MAPS[map].maze {
        // Only the tunnels: no bedrock pillar may block one, and nothing floats
        // in solid rock.
        d = smax(d, -maze_cave(p), 1.5);
        return d.min(frame(p));
    }

    // Bedrock goes in after the carving, so no tunnel cuts through it. The
    // soft min blends its feet into the surrounding rock.
    d = smin(d, bedrock(p), 1.0);

    // Floating rocks above the hills. Every one stays below the sky ceiling so
    // its whole surface closes inside the grid.
    d = d.min(ellipsoid(p, Vec3::new(98.0, 50.0, 34.0), Vec3::new(9.0, 3.5, 7.0)) * 0.8);
    for (c, r) in FLOATERS {
        d = d.min(sphere(p, c, r));
    }

    // The frame goes last: no cavern or tunnel carves it, and it reaches the
    // world edge so a worm can never leave the arena.
    d.min(frame(p))
}

#[cfg(test)]
mod tests {
    use super::*;

    // These call `terrain` directly: building the whole grid in a debug-mode
    // test takes over a minute.
    #[test]
    fn map_has_air_above_rock_below_and_an_open_cavern() {
        assert!(terrain(Vec3::new(64.0, 62.0, 30.0)) > 0.0, "sky is air");
        assert!(terrain(Vec3::new(64.0, 2.0, 30.0)) < 0.0, "deep ground is rock");
        assert!(terrain(Vec3::new(64.0, 9.0, 64.0)) > 0.0, "cavern centre is air");
        assert!(terrain(Vec3::new(52.0, 9.0, 52.0)) < 0.0, "pillar is rock");
        assert!(terrain(Vec3::new(2.0, 30.0, 2.0)) < 0.0, "the solid frame closes the edge");
        assert_eq!(material(Vec3::new(2.0, 30.0, 2.0)), Material::Solid);
        assert!(terrain(Vec3::new(2.0, 60.0, 2.0)) > 0.0, "and stops below the sky ceiling");
    }

    #[test]
    fn spawn_points_are_random_per_seed_spread_out_and_on_the_hills() {
        let a = spawn_points(0, 1, 8);
        assert_eq!(a, spawn_points(0, 1, 8), "the same seed gives the same spawns");
        assert_ne!(a, spawn_points(0, 2, 8), "another seed gives others");
        assert_eq!(a.len(), 8);
        for (i, &p) in a.iter().enumerate() {
            assert!(open_ground(0, p), "{p} is not open ground");
            for q in &a[i + 1..] {
                assert!((p.x - q.x).hypot(p.z - q.z) > 5.0, "two spawns on top of each other");
            }
        }
        // Four of them are not all in one corner, as the old fixed spots were.
        let first_four = spawn_points(0, 3, MAX_PLAYERS);
        let far = first_four.iter().any(|p| (p.x - first_four[0].x).hypot(p.z - first_four[0].z) > 20.0);
        assert!(far, "{first_four:?}");
    }

    #[test]
    fn nothing_reaches_the_sky_ceiling() {
        // Every column is air at the world's top face: no surface can be cut
        // open by the boundary.
        for x in [1.0, 32.0, 64.0, 96.0, 126.0] {
            for z in [1.0, 32.0, 64.0, 96.0, 126.0] {
                assert!(terrain(Vec3::new(x, 60.0, z)) > 0.0, "({x}, 60, {z}) is air");
                assert!(terrain(Vec3::new(x, SKY_CEILING + 0.5, z)) > 0.0, "({x}, top, {z}) is air");
            }
        }
    }

    #[test]
    fn the_frame_is_solid_and_the_rock_inside_is_not() {
        assert_eq!(material(Vec3::new(1.5, 20.0, 64.0)), Material::Solid, "wall");
        assert_eq!(material(Vec3::new(64.0, 1.5, 64.0)), Material::Solid, "floor");
        assert_eq!(material(Vec3::new(64.0, 20.0, 64.0)), Material::Rock, "the arena rock");
    }

    #[test]
    fn bedrock_and_gates_are_solid_and_the_tunnels_are_open() {
        // The pillar, the cavern walkway, the sky bridge and the spire.
        assert_eq!(material(Vec3::new(52.0, 9.0, 52.0)), Material::Solid, "pillar");
        assert!(terrain(Vec3::new(52.0, 9.0, 52.0)) < 0.0);
        assert_eq!(material(Vec3::new(65.0, 11.0, 54.0)), Material::Solid, "walkway");
        assert!(terrain(Vec3::new(65.0, 11.0, 54.0)) < 0.0);
        assert_eq!(material(Vec3::new(64.0, 42.0, 64.0)), Material::Solid, "sky bridge");
        assert!(terrain(Vec3::new(64.0, 42.0, 64.0)) < 0.0);
        assert_eq!(material(Vec3::new(46.0, 40.0, 100.0)), Material::Solid, "spire");
        assert!(terrain(Vec3::new(46.0, 40.0, 100.0)) < 0.0);
        // A gate's rim is solid; its axis is open air; rock beyond it is not.
        let (a, b, _) = TUNNELS[0];
        let (_, t) = GATES[0];
        let centre = a + (b - a) * t;
        assert!(terrain(centre) > 0.0, "the tunnel passes through the gate");
        assert!(in_gate(centre + Vec3::Y * 5.0));
        assert!(!in_gate(centre + Vec3::Y * 9.0), "the gate is a disc, not the whole mountain");
        assert!(!in_gate(centre + (b - a).normalize() * 4.0));
        // Every tunnel starts in the cavern and every chamber is open air.
        for (a, _, _) in TUNNELS.iter().take(3) {
            assert!(terrain(*a) > 0.0);
        }
        for (c, _) in CHAMBERS {
            assert!(terrain(c) > 0.0, "chamber {c} is open");
        }
        // Floaters are rock and below the ceiling.
        for (c, r) in FLOATERS {
            assert!(terrain(c) < 0.0, "floater at {c} is rock");
            assert!(c.y + r < SKY_CEILING, "floater at {c} closes inside the grid");
        }
    }

    #[test]
    fn distance_is_never_overestimated() {
        // Grid::from_fn's shortcut relies on this: the field may not change
        // faster than one metre per metre walked. Check lines at several
        // heights through hills, rim, cavern, pillars and tunnels.
        let step = 0.1;
        for map in 0..MAPS.len() {
            for (y, z) in [(9.0, 52.0), (30.0, 20.0), (6.0, 100.0), (48.0, 64.0), (20.0, 4.0), (10.0, 102.0), (10.0, 24.0), (42.0, 100.0), (11.0, 54.0)] {
                let mut prev = terrain_in(map, Vec3::new(1.0, y, z));
                let mut x = 1.0 + step;
                while x < 127.0 {
                    let d = terrain_in(map, Vec3::new(x, y, z));
                    assert!((d - prev).abs() <= step * 1.001, "{}: Lipschitz violated at ({x}, {y}, {z}): {prev} -> {d}", MAPS[map].name);
                    prev = d;
                    x += step;
                }
            }
        }
    }

    #[test]
    fn every_map_closes_inside_the_world_and_has_ground_to_stand_on() {
        for map in 0..MAPS.len() {
            for x in [1.0, 32.0, 64.0, 96.0, 126.0] {
                for z in [1.0, 32.0, 64.0, 96.0, 126.0] {
                    assert!(terrain_in(map, Vec3::new(x, SKY_CEILING + 0.5, z)) > 0.0, "{} is open at the top", MAPS[map].name);
                    assert!(terrain_in(map, Vec3::new(x, 2.0, z)) < 0.0, "{} has no floor", MAPS[map].name);
                }
            }
            let points = spawn_points(map as u8, 5, MAX_PLAYERS + 4);
            assert_eq!(points.len(), MAX_PLAYERS + 4);
            assert!(points.iter().all(|&p| open_ground(map, p)), "{} spawns on open ground", MAPS[map].name);
        }
    }

    #[test]
    fn the_maps_differ_and_the_plains_have_no_cavern() {
        assert!(terrain_in(0, Vec3::new(64.0, 9.0, 64.0)) > 0.0, "the valley's cavern");
        assert!(terrain_in(1, Vec3::new(64.0, 9.0, 64.0)) < 0.0, "the plains are solid under the surface");
        let valley = preview(0, 8);
        let flat = preview(1, 8);
        assert_ne!(valley, flat);
        let mean = |v: &[f32]| v.iter().sum::<f32>() / v.len() as f32;
        assert!(mean(&valley) > mean(&flat), "the valley is higher than the plains");
        let maze = preview(2, 8);
        assert!(maze.iter().chain(&valley).chain(&flat).all(|h| (0.0..=1.0).contains(h)));
        assert!(maze.iter().any(|&h| h < 0.5) && maze.iter().any(|&h| h > 0.5), "air and rock");
    }

    #[test]
    fn the_tunnel_map_is_rock_with_connected_chambers_inside() {
        let m = 2;
        // Solid from the roof down to the floor except where the air is.
        assert!(terrain_in(m, Vec3::new(64.0, 40.0, 40.0)) < 0.0, "no outdoors: rock up to the roof");
        assert!(terrain_in(m, Vec3::new(64.0, 50.0, 64.0)) > 0.0, "sky above the roof");
        // Every chamber is open air, and so is the middle of every tunnel to the next.
        for i in 0..3 {
            for j in 0..3 {
                let (c, _) = maze_chamber(i, j);
                assert!(terrain_in(m, c) > 0.0, "chamber {i},{j} is open");
                for (ni, nj) in [(i + 1, j), (i, j + 1)] {
                    if ni < 3 && nj < 3 {
                        let mid = (c + maze_chamber(ni, nj).0) * 0.5;
                        assert!(terrain_in(m, mid) > 0.0, "tunnel {i},{j} -> {ni},{nj} is open");
                    }
                }
            }
        }
        // Worms start inside, on the chambers' floors.
        for p in spawn_points(2, 4, MAX_PLAYERS + 4) {
            assert!(terrain_in(m, p) > 0.0, "{p} starts in the air");
        }
    }
}

