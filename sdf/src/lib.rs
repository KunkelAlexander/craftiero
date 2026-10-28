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

/// What a voxel of rock is made of. Air has no material of its own: an edit
/// that turns air into rock always makes `Rock`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[repr(u8)]
pub enum Material {
    /// Ordinary terrain. Digging, explosions and the brush change it.
    #[default]
    Rock,
    /// The arena's frame. `Grid::apply` refuses to change it, so the world
    /// always keeps a closed border.
    Solid,
}

impl Material {
    fn is_solid(self) -> bool {
        matches!(self, Material::Solid)
    }
}

/// The per-voxel data of a chunk that is neither all air nor one uniform kind
/// of rock. Distances and materials are separate arrays: the material is only
/// read when the distance says "rock", and keeping it out of `i8` leaves the
/// distance encoding alone.
#[derive(Clone)]
struct ChunkData {
    dist: Box<[i8; CHUNK_VOL]>,
    /// `None` means every voxel here is ordinary `Rock`. Only chunks that hold
    /// a solid voxel pay the second array, and most of the world never does.
    material: Option<Box<[Material; CHUNK_VOL]>>,
}

/// Pattern: Flyweight. A chunk that is all air or all rock stores nothing, and
/// all of them share the one meaning. Most of a cave map is exactly that, and
/// they cost no memory and no meshing time.
#[derive(Clone)]
enum Chunk {
    Air,
    Rock,
    Solid,
    Data(Box<ChunkData>),
}

/// `Clone` so a client that already holds the pristine map can start from a
/// copy instead of generating it again.
#[derive(Clone)]
pub struct Grid {
    /// World size in chunks.
    size: IVec3,
    chunks: Vec<Chunk>,
    /// Chunks that contain at least one solid voxel. Solid never changes, so
    /// this is filled once when the map is generated; `apply` uses it to skip
    /// the distance-to-solid work for edits far from the arena's frame.
    solid: Vec<bool>,

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
            solid: vec![false; n],
            dirty: vec![false; n],
            dirty_list: Vec::new(),
        }
    }

    /// Fill the world from a distance function (map generation). Chunks whose
    /// centre is farther from the surface than the chunk is wide are stored as
    /// uniform. That shortcut is only valid if `f` never over-estimates the
    /// distance, so write `f` conservatively.
    pub fn from_fn(size_chunks: IVec3, f: fn(Vec3) -> f32) -> Grid {
        Self::from_fn_material(size_chunks, f, |_| Material::Rock)
    }

    /// `from_fn`, plus a material for the rock at each point. `material` is
    /// only asked about voxels that end up inside rock; an all-rock chunk that
    /// is entirely `Solid` collapses to `Chunk::Solid` just like an all-`Rock`
    /// one collapses to `Chunk::Rock`.
    pub fn from_fn_material(
        size_chunks: IVec3,
        f: fn(Vec3) -> f32,
        material: fn(Vec3) -> Material,
    ) -> Grid {
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
                        undecided.push(c);
                    } else {
                        // Deep inside rock: the distance is uniform, but the
                        // material is not necessarily. A chunk that straddles a
                        // material boundary has to be evaluated voxel by voxel.
                        match uniform_material(c * CHUNK, CHUNK, material) {
                            Some(Material::Solid) => {
                                let idx = g.chunk_index(c);
                                g.chunks[idx] = Chunk::Solid;
                            }
                            Some(Material::Rock) => {} // stays Rock
                            None => undecided.push(c),
                        }
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
                .map(|part| scope.spawn(move || part.iter().map(|&c| (c, eval_chunk(c, f, material))).collect::<Vec<_>>()))
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
        g.solid = g.chunks.iter().map(contains_solid).collect();
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
            Chunk::Rock | Chunk::Solid => -127,
            Chunk::Data(data) => data.dist[local_index(v & MASK)],
        }
    }

    /// Material of the voxel at integer voxel coordinates `v`. Outside the
    /// grid counts as solid, matching `voxel`'s "outside is rock".
    fn material_voxel(&self, v: IVec3) -> Material {
        let d = self.voxel_dims();
        if v.x as u32 >= d.x as u32 || v.y as u32 >= d.y as u32 || v.z as u32 >= d.z as u32 {
            return Material::Solid;
        }
        let c = IVec3::new(v.x >> SHIFT, v.y >> SHIFT, v.z >> SHIFT);
        match &self.chunks[self.chunk_index(c)] {
            Chunk::Air | Chunk::Rock => Material::Rock,
            Chunk::Solid => Material::Solid,
            Chunk::Data(data) => data
                .material
                .as_ref()
                .map_or(Material::Rock, |m| m[local_index(v & MASK)]),
        }
    }

    /// Material of the voxel containing `p`.
    pub fn material(&self, p: Vec3) -> Material {
        self.material_voxel((p / VOX).floor().as_ivec3())
    }

    fn set_voxel(&mut self, v: IVec3, value: i8) {
        let c = IVec3::new(v.x >> SHIFT, v.y >> SHIFT, v.z >> SHIFT);
        let idx = self.chunk_index(c);
        // A uniform chunk becomes real data the first time something is cut
        // into it. (Chunks never turn back into uniform: a cheap omission that
        // costs memory only, never correctness.) Solid chunks never get here:
        // `apply` skips their voxels, so a chunk that turns to data is always
        // rock. The material fill is therefore Rock.
        if !matches!(self.chunks[idx], Chunk::Data(_)) {
            let fill = if matches!(self.chunks[idx], Chunk::Air) { 127 } else { -127 };
            self.chunks[idx] = Chunk::Data(Box::new(ChunkData {
                dist: Box::new([fill; CHUNK_VOL]),
                material: None,
            }));
        }
        if let Chunk::Data(data) = &mut self.chunks[idx] {
            data.dist[local_index(v & MASK)] = value;
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
            Chunk::Rock | Chunk::Solid => out.fill(-127),
            Chunk::Data(data) => out.copy_from_slice(&data.dist[..]),
        }
    }

    /// True if the chunk stores voxels, i.e. the surface may pass through it.
    /// The renderer builds its empty-space skipping map from this.
    pub fn chunk_has_surface(&self, c: IVec3) -> bool {
        matches!(self.chunks[self.chunk_index(c)], Chunk::Data(_))
    }

    /// How far voxel `v` is from the nearest solid rock, up to `reach_metres`.
    /// `None` means no solid voxel was found close enough to clamp against.
    ///
    /// Carving air normally raises its distance to the shape's boundary. If an
    /// indestructible voxel is closer than that boundary it cannot have been
    /// removed, so the true distance stays the smaller one. Without this clamp
    /// the band in front of the solid frame reads up to 1 m too far and the
    /// raymarcher overshoots it: the speckled, hatched surfaces seen where
    /// digging reaches the floor.
    ///
    /// 26 rays of at most five voxels are enough for a solid mass the size of
    /// the frame, and cost a fraction of a sphere search per voxel. One voxel
    /// is subtracted because the last solid voxel's centre is one voxel inside
    /// the field's zero, so the result never over-estimates the distance.
    fn solid_distance(&self, v: IVec3, reach_metres: f32) -> Option<f32> {
        let steps = ((reach_metres + VOX) / VOX).ceil() as i32;
        let mut best = f32::INFINITY;
        for dz in -1..=1 {
            for dy in -1..=1 {
                for dx in -1..=1 {
                    if dx == 0 && dy == 0 && dz == 0 {
                        continue;
                    }
                    for n in 1..=steps {
                        let s = v + IVec3::new(dx * n, dy * n, dz * n);
                        if !self.material_voxel(s).is_solid() {
                            continue;
                        }
                        let d = (((v - s).as_vec3() * VOX).length() - VOX).max(0.0);
                        best = best.min(d);
                    }
                }
            }
        }
        best.is_finite().then_some(best)
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
                    // Only a carve near solid rock can raise a distance past
                    // the nearest surface, so only those chunks need the probe.
                    let near_solid = (0..3).any(|dz| {
                        (0..3).any(|dy| {
                            (0..3).any(|dx| {
                                let n = IVec3::new(cx - 1 + dx, cy - 1 + dy, cz - 1 + dz);
                                n.cmpge(IVec3::ZERO).all()
                                    && n.cmplt(self.size).all()
                                    && self.solid[self.chunk_index(n)]
                            })
                        })
                    });
                    let clamp_solid = near_solid && matches!(op, Op::Carve);
                    let mut changed_lo = IVec3::splat(i32::MAX);
                    let mut changed_hi = IVec3::splat(i32::MIN);
                    for z in v_lo.z..=v_hi.z {
                        for y in v_lo.y..=v_hi.y {
                            for x in v_lo.x..=v_hi.x {
                                let v = IVec3::new(x, y, z);
                                let old = self.voxel(v);
                                let d_shape = shape.distance(v.as_vec3() * VOX);
                                let mut new = quantize(op.combine(dequantize(old), d_shape));
                                if new == old {
                                    continue;
                                }
                                // Solid terrain is the arena's frame: the shaped
                                // hole stops at its face instead of cutting
                                // through. Checked after `new == old` so chunks
                                // of solid rock that the shape does not reach
                                // cost only the distance test.
                                if self.material_voxel(v).is_solid() {
                                    continue;
                                }
                                // Carving air raises the stored distance to the
                                // shape's own boundary. A solid voxel closer than
                                // that boundary is still there, so keep the
                                // smaller distance (see `solid_distance`).
                                if clamp_solid && new > old {
                                    let want = dequantize(new);
                                    if want > 0.0 {
                                        if let Some(d) = self.solid_distance(v, want) {
                                            if d < want {
                                                new = quantize(d);
                                                if new == old {
                                                    continue;
                                                }
                                            }
                                        }
                                    }
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
                Chunk::Solid => 3u8,
                Chunk::Data(_) => 4u8,
            };
            h = (h ^ tag as u64).wrapping_mul(0x100000001b3);
            if let Chunk::Data(data) = c {
                for b in data.dist.iter() {
                    h = (h ^ (*b as u8) as u64).wrapping_mul(0x100000001b3);
                }
                if let Some(material) = &data.material {
                    for m in material.iter() {
                        h = (h ^ (*m as u8) as u64).wrapping_mul(0x100000001b3);
                    }
                }
            }
        }
        h
    }
}

fn contains_solid(chunk: &Chunk) -> bool {
    match chunk {
        Chunk::Solid => true,
        Chunk::Data(data) => data.material.as_ref().is_some_and(|m| m.iter().any(|&v| v.is_solid())),
        _ => false,
    }
}

/// The material of a whole `size` voxel cube, if it is the same at the centre
/// and all eight corners. `None` means a material boundary runs through it and
/// it has to be evaluated per voxel. Used for whole chunks in the cheap map pass
/// and for 4^3 blocks inside `eval_chunk`.
fn uniform_material(origin: IVec3, size: i32, material: fn(Vec3) -> Material) -> Option<Material> {
    let centre = (origin.as_vec3() + Vec3::splat(size as f32 * 0.5 - 0.5)) * VOX;
    let m = material(centre);
    let last = size - 1;
    for i in 0..8 {
        let corner = IVec3::new((i & 1) * last, ((i >> 1) & 1) * last, ((i >> 2) & 1) * last);
        if material((origin + corner).as_vec3() * VOX) != m {
            return None;
        }
    }
    Some(m)
}

/// Sample `f` over one chunk, in blocks of 4 x 4 x 4 voxels. The same "far from
/// the surface" shortcut that skips whole chunks is applied per block: one
/// evaluation at the block centre decides it, and only blocks near the surface
/// pay for all 64 voxels. Most of an undecided chunk is empty air or solid rock
/// next to a thin surface, so this cuts the evaluations of a typical map ~3x.
fn eval_chunk(c: IVec3, f: fn(Vec3) -> f32, material: fn(Vec3) -> Material) -> Chunk {
    const BLOCK: i32 = 4;
    let block_half_diagonal = BLOCK as f32 * 0.5 * 3f32.sqrt() * VOX;
    let origin = c * CHUNK;
    let mut dist = Box::new([0i8; CHUNK_VOL]);
    let mut mats = Box::new([Material::Rock; CHUNK_VOL]);
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
                // Deep inside rock the material can be settled for the whole
                // block in nine calls. Near the surface every rock voxel is
                // asked, because a solid border that cuts through a block must
                // not launder the destructive voxels beside it.
                let block_mat = match fill {
                    Some(-127) => uniform_material(origin + block, BLOCK, material),
                    _ => None,
                };
                for lz in bz..bz + BLOCK {
                    for ly in by..by + BLOCK {
                        for lx in bx..bx + BLOCK {
                            let l = IVec3::new(lx, ly, lz);
                            let p = (origin + l).as_vec3() * VOX;
                            let i = local_index(l);
                            let v = match fill {
                                Some(v) => v,
                                None => quantize(f(p)),
                            };
                            dist[i] = v;
                            if v < 0 {
                                mats[i] = block_mat.unwrap_or_else(|| material(p));
                            }
                        }
                    }
                }
            }
        }
    }
    // A chunk the centre test could not rule out may still quantize to all air
    // (the band clamps at 1 m) or all rock. Store it as uniform: it costs
    // nothing, and a renderer can skip it.
    let any_solid = mats.iter().any(|&m| m == Material::Solid);
    if dist.iter().all(|&v| v == 127) {
        Chunk::Air
    } else if dist.iter().all(|&v| v == -127) {
        if !any_solid {
            Chunk::Rock
        } else if mats.iter().all(|&m| m == Material::Solid) {
            Chunk::Solid
        } else {
            Chunk::Data(Box::new(ChunkData { dist, material: Some(mats) }))
        }
    } else {
        Chunk::Data(Box::new(ChunkData { dist, material: any_solid.then_some(mats) }))
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
    fn a_box_adds_a_flat_wall() {
        let mut g = Grid::from_fn(IVec3::new(4, 4, 4), |p| p.y - 4.0);
        let boxy = Shape::Box {
            centre: Vec3::new(8.0, 6.0, 8.0),
            half: Vec3::new(2.0, 1.5, 0.3),
            yaw: 0.0,
        };
        g.apply(boxy, Op::Add);
        assert!(g.sample(Vec3::new(8.0, 6.0, 8.0)) < -0.2, "inside the wall");
        assert!(g.sample(Vec3::new(8.0, 6.0, 8.6)) > 0.0, "past its thickness");
        assert!(g.sample(Vec3::new(8.0, 6.0, 7.4)) > 0.0, "in front of it");
        assert!(g.sample(Vec3::new(11.0, 6.0, 8.0)) > 0.0, "past its width");
        assert!(g.sample(Vec3::new(8.0, 8.0, 8.0)) > 0.0, "above it");
    }

    #[test]
    fn a_box_distance_is_rotated_with_its_yaw() {
        let boxy = Shape::Box {
            centre: Vec3::ZERO,
            half: Vec3::new(2.0, 1.0, 0.3),
            yaw: std::f32::consts::FRAC_PI_2,
        };
        // A box turned 90 degrees is thin along x and wide along z.
        assert!(boxy.distance(Vec3::new(0.0, 0.0, 1.5)) < 0.0);
        assert!(boxy.distance(Vec3::new(1.5, 0.0, 0.0)) > 0.0);
    }

    #[test]
    fn a_blade_cuts_a_thin_slit_into_a_wall() {
        // A wall of rock at z >= 8, cut along x at height 6, 1.5 m deep.
        let mut g = Grid::from_fn(IVec3::new(4, 4, 4), |p| 8.0 - p.z);
        let blade = Shape::Blade {
            a: Vec3::new(4.0, 6.0, 8.0),
            b: Vec3::new(10.0, 6.0, 8.0),
            dir: Vec3::Z,
            depth: 1.5,
            half_thickness: 0.25,
        };
        g.apply(blade, Op::Carve);
        assert!(g.sample(Vec3::new(7.0, 6.0, 8.8)) > 0.0, "the slit is open");
        assert!(g.sample(Vec3::new(7.0, 6.9, 8.8)) < 0.0, "rock above the slit stays");
        assert!(g.sample(Vec3::new(7.0, 5.1, 8.8)) < 0.0, "rock below the slit stays");
        assert!(g.sample(Vec3::new(7.0, 6.0, 10.2)) < 0.0, "the slit stops at its depth");
        assert!(g.sample(Vec3::new(12.0, 6.0, 8.8)) < 0.0, "and at its length");
        // The bounds cover every voxel the shape can change.
        let (lo, hi) = blade.bounds();
        assert!(lo.x <= 4.0 && hi.x >= 10.0 && hi.z >= 9.5 && lo.y <= 5.75 && hi.y >= 6.25);
    }

    #[test]
    fn a_blade_has_rounded_edges_but_keeps_its_extents() {
        let blade = Shape::Blade {
            a: Vec3::new(4.0, 6.0, 8.0),
            b: Vec3::new(10.0, 6.0, 8.0),
            dir: Vec3::Z,
            depth: 1.5,
            half_thickness: 0.25,
        };
        // The very corner of the box is rounded away...
        assert!(blade.distance(Vec3::new(4.02, 6.23, 9.48)) > 0.0, "the corner is rounded");
        // ...but the flat middle of each face is still where it was.
        assert!(blade.distance(Vec3::new(7.0, 6.2, 8.8)) < 0.0, "inside near the wall");
        assert!(blade.distance(Vec3::new(7.0, 6.3, 8.8)) > 0.0, "outside past the face");
        assert!(blade.distance(Vec3::new(7.0, 6.0, 9.45)) < 0.0, "inside near the end");
        assert!(blade.distance(Vec3::new(7.0, 6.0, 9.55)) > 0.0, "outside past the end");
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
    fn solid_terrain_cannot_be_carved() {
        // Rock everywhere, but the bottom half is the arena's solid frame.
        let mut g = Grid::from_fn_material(
            IVec3::new(4, 4, 4),
            |p| p.y - 8.0,
            |p| if p.y < 4.0 { Material::Solid } else { Material::Rock },
        );
        assert!(g.sample(Vec3::new(8.0, 2.0, 8.0)) < 0.0);
        assert_eq!(g.material(Vec3::new(8.0, 2.0, 8.0)), Material::Solid);
        g.apply(Shape::Sphere { center: Vec3::new(8.0, 4.0, 8.0), radius: 3.0 }, Op::Carve);
        assert!(g.sample(Vec3::new(8.0, 2.0, 8.0)) < 0.0, "solid stays rock");
        assert_eq!(g.material(Vec3::new(8.0, 2.0, 8.0)), Material::Solid);
        assert!(g.sample(Vec3::new(8.0, 6.5, 8.0)) > 0.0, "the rock above is carved away");
    }

    #[test]
    fn a_carve_does_not_inflate_air_in_front_of_solid_rock() {
        // Air above a solid floor at y = 4 (destructible rock would be above,
        // but there is none here: the floor is the nearest surface).
        let mut g = Grid::from_fn_material(
            IVec3::new(4, 4, 4),
            |p| p.y - 4.0,
            |p| if p.y < 4.0 { Material::Solid } else { Material::Rock },
        );
        let probe = Vec3::new(8.0, 4.2, 8.0);
        let before = g.sample(probe);
        assert!((before - 0.2).abs() < 0.12, "air is 0.2 m above the floor: {before}");

        // A crater centred on the floor cannot remove the solid rock. Without
        // the clamp this raised the air's distance to the 1 m band and made the
        // raymarcher overshoot the floor (the hatching seen when digging down).
        g.apply(Shape::Sphere { center: Vec3::new(8.0, 4.0, 8.0), radius: 3.0 }, Op::Carve);
        let after = g.sample(probe);
        assert!(after < before + 0.15, "the floor is still the nearest surface: {before} -> {after}");
        assert!(g.sample(Vec3::new(8.0, 3.9, 8.0)) < 0.0, "the floor is still there");
    }

    #[test]
    fn checksum_notices_a_material_change() {
        let rock = Grid::from_fn(IVec3::new(2, 2, 2), |p| p.y - 4.0);
        let solid = Grid::from_fn_material(IVec3::new(2, 2, 2), |p| p.y - 4.0, |_| Material::Solid);
        assert_ne!(rock.checksum(), solid.checksum(), "material is part of the terrain");
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
