// sdf: a destructible terrain stored as a signed distance field (SDF).
//
// Convention: the value at a point is the distance to the nearest surface in
// metres, POSITIVE in air and NEGATIVE inside rock. Zero is the surface.
//
// Why a grid of distances and not "a list of shapes" (the overlay in the video):
// a list gets slower with every explosion, a grid costs the same after a
// thousand. The edit (a shape plus an operation) is still what we would send
// over the network, like `TileChanged` in the 2D game.
//
// This crate knows nothing about windows, GPUs, networking or time, so the sim
// can use it for collision and the renderer can use it for meshes.

mod mesh;
mod shape;

pub use mesh::Mesh;
pub use shape::{Op, Shape};

use glam::{IVec3, Vec3};

/// Edge of one voxel in metres. 0.25 m gives a 3 m crater about 24 voxels wide.
pub const VOX: f32 = 0.25;
/// Voxels per chunk edge. A chunk is the unit of storage, of remeshing and of
/// "dirty" tracking. 16^3 = 4096 voxels = 4 KB.
pub const CHUNK: i32 = 1 << 4;
const SHIFT: i32 = 4;
const MASK: i32 = CHUNK - 1;
pub const CHUNK_VOL: usize = (CHUNK * CHUNK * CHUNK) as usize;

// Distances are stored in one signed byte, clamped to +-4 voxels. Far from the
// surface the exact distance is never needed (collision and meshing only look
// near it), and 1 byte instead of 4 keeps 512x256x512 voxels at 67 MB.
const BAND_VOX: f32 = 4.0;
const SCALE: f32 = 127.0 / BAND_VOX;
/// Metres beyond which a shape cannot change any stored value.
pub const BAND: f32 = BAND_VOX * VOX;

fn quantize(metres: f32) -> i8 {
    (metres / VOX * SCALE).round().clamp(-127.0, 127.0) as i8
}

fn dequantize(v: i8) -> f32 {
    v as f32 / SCALE * VOX
}

/// Pattern: Flyweight. A chunk that is all air or all rock stores nothing, and
/// all of them share the one meaning. Most of a cave map is exactly that, and
/// they cost no memory and no meshing time.
enum Chunk {
    Air,
    Rock,
    Data(Box<[i8; CHUNK_VOL]>),
}

pub struct Grid {
    /// World size in chunks.
    size: IVec3,
    chunks: Vec<Chunk>,
    /// Pattern: Dirty Flag. Chunks whose samples changed since the renderer
    /// last asked. The flag array stops a chunk being queued twice.
    dirty: Vec<bool>,
    dirty_list: Vec<IVec3>,
}

impl Grid {
    /// A world full of rock.
    pub fn new(size_chunks: IVec3) -> Grid {
        let n = (size_chunks.x * size_chunks.y * size_chunks.z) as usize;
        Grid {
            size: size_chunks,
            chunks: (0..n).map(|_| Chunk::Rock).collect(),
            dirty: vec![false; n],
            dirty_list: Vec::new(),
        }
    }

    /// Fill the world from a distance function (map generation). Chunks whose
    /// centre is farther from the surface than the chunk is wide are stored as
    /// uniform. That shortcut is only valid if `f` never over-estimates the
    /// distance, so write `f` conservatively.
    pub fn from_fn(size_chunks: IVec3, f: fn(Vec3) -> f32) -> Grid {
        let mut g = Grid::new(size_chunks);
        let half_diagonal = CHUNK as f32 * 0.5 * 3f32.sqrt() * VOX;
        // Cheap serial pass: one evaluation per chunk settles the easy ones.
        let mut undecided = Vec::new();
        for cz in 0..size_chunks.z {
            for cy in 0..size_chunks.y {
                for cx in 0..size_chunks.x {
                    let c = IVec3::new(cx, cy, cz);
                    let centre = ((c * CHUNK).as_vec3() + Vec3::splat(CHUNK as f32 * 0.5)) * VOX;
                    let d = f(centre);
                    if d > half_diagonal + BAND {
                        let idx = g.chunk_index(c);
                        g.chunks[idx] = Chunk::Air;
                    } else if d >= -(half_diagonal + BAND) {
                        undecided.push(c); // (the rest stay Rock)
                    }
                }
            }
        }
        // Expensive pass: 4096 evaluations per undecided chunk. Chunks do not
        // depend on each other, so spread them over all cores. `f` is a plain
        // fn pointer, which is safe to share between threads.
        let threads = std::thread::available_parallelism().map_or(1, |n| n.get());
        let per_thread = undecided.len().div_ceil(threads).max(1);
        let results: Vec<(IVec3, Chunk)> = std::thread::scope(|scope| {
            let workers: Vec<_> = undecided
                .chunks(per_thread)
                .map(|part| scope.spawn(move || part.iter().map(|&c| (c, eval_chunk(c, f))).collect::<Vec<_>>()))
                .collect();
            // Joined in order, so the result does not depend on thread timing.
            workers.into_iter().flat_map(|w| w.join().expect("map worker panicked")).collect()
        });
        for (c, chunk) in results {
            let idx = g.chunk_index(c);
            if matches!(chunk, Chunk::Data(_)) {
                g.mark_dirty_range(c * CHUNK, c * CHUNK);
            }
            g.chunks[idx] = chunk;
        }
        g
    }

    pub fn size_chunks(&self) -> IVec3 {
        self.size
    }

    /// World size in metres.
    pub fn size_metres(&self) -> Vec3 {
        (self.size * CHUNK).as_vec3() * VOX
    }

    fn voxel_dims(&self) -> IVec3 {
        self.size * CHUNK
    }

    fn chunk_index(&self, c: IVec3) -> usize {
        ((c.z * self.size.y + c.y) * self.size.x + c.x) as usize
    }

    /// Raw stored value. Outside the world counts as rock, so the arena is
    /// closed and the mesher produces a wall at the edge.
    fn voxel(&self, v: IVec3) -> i8 {
        // Casting to u32 folds the "negative or too big" test into one compare.
        let d = self.voxel_dims();
        if v.x as u32 >= d.x as u32 || v.y as u32 >= d.y as u32 || v.z as u32 >= d.z as u32 {
            return -127;
        }
        // CHUNK is 16, so divide and remainder are a shift and a mask. This is
        // the hottest line in the crate (collision, meshing, edits all call it).
        let c = IVec3::new(v.x >> SHIFT, v.y >> SHIFT, v.z >> SHIFT);
        match &self.chunks[self.chunk_index(c)] {
            Chunk::Air => 127,
            Chunk::Rock => -127,
            Chunk::Data(data) => data[local_index(v & MASK)],
        }
    }

    fn set_voxel(&mut self, v: IVec3, value: i8) {
        let c = IVec3::new(v.x >> SHIFT, v.y >> SHIFT, v.z >> SHIFT);
        let idx = self.chunk_index(c);
        // A uniform chunk becomes real data the first time something is cut
        // into it. (Chunks never turn back into uniform: a cheap omission that
        // costs memory only, never correctness.)
        if !matches!(self.chunks[idx], Chunk::Data(_)) {
            let fill = if matches!(self.chunks[idx], Chunk::Air) { 127 } else { -127 };
            self.chunks[idx] = Chunk::Data(Box::new([fill; CHUNK_VOL]));
        }
        if let Chunk::Data(data) = &mut self.chunks[idx] {
            data[local_index(v & MASK)] = value;
        }
    }

    /// Mark every chunk whose 18^3 mesh input (its voxels plus a 1-voxel
    /// border, see mesh.rs) contains a voxel in `lo..=hi`.
    fn mark_dirty_range(&mut self, lo: IVec3, hi: IVec3) {
        let l = lo - IVec3::ONE;
        let h = hi + IVec3::ONE;
        let c_lo = IVec3::new(l.x >> SHIFT, l.y >> SHIFT, l.z >> SHIFT).max(IVec3::ZERO);
        let c_hi = IVec3::new(h.x >> SHIFT, h.y >> SHIFT, h.z >> SHIFT).min(self.size - IVec3::ONE);
        for cz in c_lo.z..=c_hi.z {
            for cy in c_lo.y..=c_hi.y {
                for cx in c_lo.x..=c_hi.x {
                    let c = IVec3::new(cx, cy, cz);
                    let idx = self.chunk_index(c);
                    if !self.dirty[idx] {
                        self.dirty[idx] = true;
                        self.dirty_list.push(c);
                    }
                }
            }
        }
    }

    /// Chunks changed since the last call. The renderer remeshes exactly these.
    pub fn take_dirty(&mut self) -> Vec<IVec3> {
        for c in &self.dirty_list {
            let idx = self.chunk_index(*c);
            self.dirty[idx] = false;
        }
        std::mem::take(&mut self.dirty_list)
    }

    /// Copy one chunk's raw voxels (x fastest, then y, then z) for the GPU
    /// upload. Uniform chunks are expanded: the renderer's 3D texture has no
    /// "air" or "rock" shortcut, so every voxel needs a value there.
    pub fn copy_chunk(&self, c: IVec3, out: &mut [i8; CHUNK_VOL]) {
        match &self.chunks[self.chunk_index(c)] {
            Chunk::Air => out.fill(127),
            Chunk::Rock => out.fill(-127),
            Chunk::Data(data) => out.copy_from_slice(&data[..]),
        }
    }

    /// True if the chunk stores voxels, i.e. the surface may pass through it.
    /// The renderer builds its empty-space skipping map from this.
    pub fn chunk_has_surface(&self, c: IVec3) -> bool {
        matches!(self.chunks[self.chunk_index(c)], Chunk::Data(_))
    }

    /// Cut, add or blend a shape into the terrain. Deterministic: only
    /// +, -, *, min, max, sqrt on f32, so the host and a client applying the
    /// same edit get the same bytes (checked by a test).
    pub fn apply(&mut self, shape: Shape, op: Op) {
        let reach = BAND + op.extra_reach();
        let (min, max) = shape.bounds();
        let lo = ((min - Vec3::splat(reach)) / VOX).floor().as_ivec3().max(IVec3::ZERO);
        let hi = ((max + Vec3::splat(reach)) / VOX).ceil().as_ivec3().min(self.voxel_dims() - IVec3::ONE);
        let half_diagonal = CHUNK as f32 * 0.5 * 3f32.sqrt() * VOX;
        // Work chunk by chunk: a shape's bounding box is mostly empty corners,
        // and a whole chunk farther than `reach` from the shape cannot change.
        for cz in (lo.z >> SHIFT)..=(hi.z >> SHIFT) {
            for cy in (lo.y >> SHIFT)..=(hi.y >> SHIFT) {
                for cx in (lo.x >> SHIFT)..=(hi.x >> SHIFT) {
                    let c = IVec3::new(cx, cy, cz);
                    let origin = c * CHUNK;
                    let centre = (origin.as_vec3() + Vec3::splat(CHUNK as f32 * 0.5)) * VOX;
                    if shape.distance(centre) > half_diagonal + reach {
                        continue;
                    }
                    let v_lo = origin.max(lo);
                    let v_hi = (origin + IVec3::splat(CHUNK - 1)).min(hi);
                    let mut changed_lo = IVec3::splat(i32::MAX);
                    let mut changed_hi = IVec3::splat(i32::MIN);
                    for z in v_lo.z..=v_hi.z {
                        for y in v_lo.y..=v_hi.y {
                            for x in v_lo.x..=v_hi.x {
                                let v = IVec3::new(x, y, z);
                                let old = self.voxel(v);
                                let d_shape = shape.distance(v.as_vec3() * VOX);
                                let new = quantize(op.combine(dequantize(old), d_shape));
                                if new == old {
                                    continue;
                                }
                                self.set_voxel(v, new);
                                // Every change counts, even far from the surface:
                                // the raymarcher steps by the stored distance, so a
                                // stale value deep in air would make rays overshoot.
                                // (The Surface Nets mesh alone only needs voxels within
                                // ~2 of the surface.)
                                changed_lo = changed_lo.min(v);
                                changed_hi = changed_hi.max(v);
                            }
                        }
                    }
                    if changed_lo.x != i32::MAX {
                        self.mark_dirty_range(changed_lo, changed_hi);
                    }
                }
            }
        }
    }

    /// Distance to the surface at `p` in metres (trilinear between voxels, so
    /// it is smooth: no stair-steps when a worm slides along a wall).
    pub fn sample(&self, p: Vec3) -> f32 {
        let g = p / VOX;
        let base = g.floor();
        let f = g - base;
        let b = base.as_ivec3();
        let s = |dx, dy, dz| dequantize(self.voxel(b + IVec3::new(dx, dy, dz)));
        let x00 = s(0, 0, 0) + (s(1, 0, 0) - s(0, 0, 0)) * f.x;
        let x10 = s(0, 1, 0) + (s(1, 1, 0) - s(0, 1, 0)) * f.x;
        let x01 = s(0, 0, 1) + (s(1, 0, 1) - s(0, 0, 1)) * f.x;
        let x11 = s(0, 1, 1) + (s(1, 1, 1) - s(0, 1, 1)) * f.x;
        let y0 = x00 + (x10 - x00) * f.y;
        let y1 = x01 + (x11 - x01) * f.y;
        y0 + (y1 - y0) * f.z
    }

    /// Surface normal at `p` (direction of increasing distance, pointing out of
    /// the rock). Collision uses it to push a worm out along the wall.
    pub fn gradient(&self, p: Vec3) -> Vec3 {
        let e = VOX * 0.5;
        Vec3::new(
            self.sample(p + Vec3::X * e) - self.sample(p - Vec3::X * e),
            self.sample(p + Vec3::Y * e) - self.sample(p - Vec3::Y * e),
            self.sample(p + Vec3::Z * e) - self.sample(p - Vec3::Z * e),
        )
        .normalize_or_zero()
    }

    /// Sphere tracing: the distance field says how far we can safely step
    /// without crossing the surface. Returns the distance along `dir` (unit
    /// length) to the first rock, if within `max_t`. Used for bullets, rope and
    /// line of sight.
    pub fn raycast(&self, origin: Vec3, dir: Vec3, max_t: f32) -> Option<f32> {
        let mut t = 0.0;
        while t < max_t {
            let d = self.sample(origin + dir * t);
            if d < VOX * 0.1 {
                return Some(t);
            }
            // Never step less than half a voxel: near grazing surfaces the
            // distance shrinks towards zero and we would crawl.
            t += d.max(VOX * 0.5);
        }
        None
    }

    /// Checksum of all stored voxels, for desync detection online and tests.
    pub fn checksum(&self) -> u64 {
        let mut h: u64 = 0xcbf29ce484222325;
        for c in &self.chunks {
            let tag = match c {
                Chunk::Air => 1u8,
                Chunk::Rock => 2u8,
                Chunk::Data(_) => 3u8,
            };
            h = (h ^ tag as u64).wrapping_mul(0x100000001b3);
            if let Chunk::Data(data) = c {
                for b in data.iter() {
                    h = (h ^ (*b as u8) as u64).wrapping_mul(0x100000001b3);
                }
            }
        }
        h
    }
}

/// Sample `f` over one chunk, in blocks of 4 x 4 x 4 voxels. The same "far from
/// the surface" shortcut that skips whole chunks is applied per block: one
/// evaluation at the block centre decides it, and only blocks near the surface
/// pay for all 64 voxels. Most of an undecided chunk is empty air or solid rock
/// next to a thin surface, so this cuts the evaluations of a typical map ~3x.
fn eval_chunk(c: IVec3, f: fn(Vec3) -> f32) -> Chunk {
    const BLOCK: i32 = 4;
    let block_half_diagonal = BLOCK as f32 * 0.5 * 3f32.sqrt() * VOX;
    let origin = c * CHUNK;
    let mut data = Box::new([0i8; CHUNK_VOL]);
    for bz in (0..CHUNK).step_by(BLOCK as usize) {
        for by in (0..CHUNK).step_by(BLOCK as usize) {
            for bx in (0..CHUNK).step_by(BLOCK as usize) {
                let block = IVec3::new(bx, by, bz);
                let centre = ((origin + block).as_vec3() + Vec3::splat(BLOCK as f32 * 0.5 - 0.5)) * VOX;
                let d = f(centre);
                let fill = if d > block_half_diagonal + BAND {
                    Some(127)
                } else if d < -(block_half_diagonal + BAND) {
                    Some(-127)
                } else {
                    None
                };
                for lz in bz..bz + BLOCK {
                    for ly in by..by + BLOCK {
                        for lx in bx..bx + BLOCK {
                            let l = IVec3::new(lx, ly, lz);
                            data[local_index(l)] = match fill {
                                Some(v) => v,
                                None => quantize(f((origin + l).as_vec3() * VOX)),
                            };
                        }
                    }
                }
            }
        }
    }
    // A chunk the centre test could not rule out may still quantize to all air
    // (the band clamps at 1 m) or all rock. Store it as uniform: it costs
    // nothing, and a renderer can skip it.
    if data.iter().all(|&v| v == 127) {
        Chunk::Air
    } else if data.iter().all(|&v| v == -127) {
        Chunk::Rock
    } else {
        Chunk::Data(data)
    }
}

fn local_index(l: IVec3) -> usize {
    ((l.z * CHUNK + l.y) * CHUNK + l.x) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world() -> Grid {
        Grid::new(IVec3::new(4, 4, 4)) // 16 m cube of rock
    }

    #[test]
    fn rock_is_solid_and_carving_makes_air() {
        let mut g = world();
        let c = Vec3::splat(8.0);
        assert!(g.sample(c) < 0.0);
        g.apply(Shape::Sphere { center: c, radius: 3.0 }, Op::Carve);
        // Deep in air the value is clamped to the band (1 m), not the true 3 m.
        assert!(g.sample(c) > BAND * 0.9, "centre of crater is air: {}", g.sample(c));
        assert!((g.sample(c + Vec3::X * 3.0)).abs() < 0.1, "surface sits at the radius");
        assert!(g.sample(c + Vec3::X * 3.5) < 0.0, "outside the crater is still rock");
    }

    #[test]
    fn add_fills_air_back_in() {
        let mut g = world();
        let c = Vec3::splat(8.0);
        g.apply(Shape::Sphere { center: c, radius: 4.0 }, Op::Carve);
        g.apply(Shape::Sphere { center: c, radius: 2.0 }, Op::Add);
        assert!(g.sample(c) < -0.5);
        assert!(g.sample(c + Vec3::X * 3.0) > 0.0);
    }

    #[test]
    fn gradient_points_out_of_rock() {
        let mut g = world();
        let c = Vec3::splat(8.0);
        g.apply(Shape::Sphere { center: c, radius: 3.0 }, Op::Carve);
        // Standing on the crater wall at +x: rock is further +x, air towards the centre.
        let n = g.gradient(c + Vec3::X * 3.0);
        assert!(n.x < -0.9, "{n:?}");
    }

    #[test]
    fn raycast_hits_the_crater_wall() {
        let mut g = world();
        let c = Vec3::splat(8.0);
        g.apply(Shape::Sphere { center: c, radius: 3.0 }, Op::Carve);
        let t = g.raycast(c, Vec3::X, 20.0).expect("hits wall");
        assert!((t - 3.0).abs() < 0.3, "t = {t}");
        assert!(g.raycast(c, Vec3::X, 1.0).is_none(), "ray too short to reach it");
    }

    #[test]
    fn edit_dirties_only_nearby_chunks_once() {
        let mut g = world();
        g.apply(Shape::Sphere { center: Vec3::splat(2.0), radius: 1.0 }, Op::Carve);
        let d = g.take_dirty();
        assert!(!d.is_empty() && d.len() <= 8, "{}", d.len());
        assert!(g.take_dirty().is_empty(), "taking clears the flags");
        // Carving air again changes nothing, so nothing is dirty.
        g.apply(Shape::Sphere { center: Vec3::splat(2.0), radius: 0.5 }, Op::Carve);
        assert!(g.take_dirty().is_empty());
    }

    #[test]
    fn same_edits_give_same_bytes() {
        let edit = |g: &mut Grid| {
            g.apply(Shape::Sphere { center: Vec3::new(5.3, 7.7, 6.1), radius: 2.7 }, Op::Carve);
            g.apply(
                Shape::Capsule { a: Vec3::new(3.0, 8.0, 8.0), b: Vec3::new(12.0, 9.0, 7.0), radius: 1.0 },
                Op::Carve,
            );
            g.apply(Shape::Sphere { center: Vec3::new(8.0, 8.0, 8.0), radius: 1.5 }, Op::Smooth(0.5));
        };
        let (mut a, mut b) = (world(), world());
        edit(&mut a);
        edit(&mut b);
        assert_eq!(a.checksum(), b.checksum());
    }

    #[test]
    fn from_fn_shortcuts_change_no_voxel() {
        // The chunk and block shortcuts must give exactly the voxels that
        // evaluating the function everywhere would.
        fn ball(p: Vec3) -> f32 {
            (p - Vec3::new(8.0, 7.0, 9.0)).length() - 5.0
        }
        let g = Grid::from_fn(IVec3::new(4, 4, 4), ball);
        for z in 0..64 {
            for y in 0..64 {
                for x in 0..64 {
                    let v = IVec3::new(x, y, z);
                    assert_eq!(g.voxel(v), quantize(ball(v.as_vec3() * VOX)), "voxel {v:?}");
                }
            }
        }
    }

    #[test]
    fn from_fn_matches_apply() {
        fn ball(p: Vec3) -> f32 {
            (p - Vec3::splat(8.0)).length() - 5.0 // a ball of rock... negative inside
        }
        let g = Grid::from_fn(IVec3::new(4, 4, 4), ball);
        assert!(g.sample(Vec3::splat(8.0)) < 0.0);
        assert!(g.sample(Vec3::new(1.0, 1.0, 1.0)) > 0.0);
    }
}
