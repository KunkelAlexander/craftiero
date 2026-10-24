// M0 go/no-go benchmark: is the grid + Surface Nets fast enough?
// Run: cargo run -p sdf --release --example bench
// Budget (PLAN.md): r=3 m crater edit + remesh < 2 ms on one thread, the full
// 128x64x128 m world builds in well under a second, memory < 100 MB.

use glam::{IVec3, Vec3};
use sdf::{Grid, Op, Shape};
use std::time::Instant;

/// Rolling hills with a few caves. Divided by 2 so it never over-estimates the
/// distance (the uniform-chunk shortcut in `Grid::from_fn` relies on that).
fn terrain(p: Vec3) -> f32 {
    let hill = 24.0 + 6.0 * (p.x * 0.1).sin() * (p.z * 0.13).cos();
    let cave = (p - Vec3::new(64.0, 20.0, 64.0)).length() - 14.0;
    (p.y - hill).max(-cave) * 0.5
}

/// Tiny deterministic random numbers, enough for a benchmark.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 40) as f32 / (1u64 << 24) as f32
    }
}

fn ms(t: Instant) -> f64 {
    t.elapsed().as_secs_f64() * 1000.0
}

fn main() {
    // 128 x 64 x 128 m at 0.25 m = 512 x 256 x 512 voxels = 32 x 16 x 32 chunks.
    let size = IVec3::new(32, 16, 32);
    let t = Instant::now();
    let mut grid = Grid::from_fn(size, terrain);
    println!("build world ({} chunks):   {:8.1} ms", size.x * size.y * size.z, ms(t));

    let t = Instant::now();
    let dirty = grid.take_dirty();
    let mut tris = 0;
    for c in &dirty {
        tris += grid.mesh_chunk(*c).indices.len() / 3;
    }
    println!("initial mesh: {} chunks, {} tris: {:8.1} ms total, {:.3} ms/chunk", dirty.len(), tris, ms(t), ms(t) / dirty.len() as f64);

    // Explosions: r=3 m craters on the surface, each edit followed by remeshing
    // the chunks it dirtied (what the renderer would do that frame).
    let mut rng = Rng(0x9e3779b97f4a7c15);
    let n = 200;
    let (mut edit_ms, mut mesh_ms, mut chunks) = (0.0, 0.0, 0);
    let mut worst: f64 = 0.0;
    for _ in 0..n {
        let x = 10.0 + rng.next() * 108.0;
        let z = 10.0 + rng.next() * 108.0;
        let y = 24.0 + 6.0 * (x * 0.1).sin() * (z * 0.13).cos();
        let t = Instant::now();
        grid.apply(Shape::Sphere { center: Vec3::new(x, y, z), radius: 3.0 }, Op::Carve);
        let e = ms(t);
        let t = Instant::now();
        let dirty = grid.take_dirty();
        for c in &dirty {
            std::hint::black_box(grid.mesh_chunk(*c));
        }
        let m = ms(t);
        edit_ms += e;
        mesh_ms += m;
        chunks += dirty.len();
        worst = worst.max(e + m);
    }
    println!(
        "crater r=3m x{n}: edit {:.3} ms, remesh {:.3} ms ({:.1} chunks) => {:.3} ms avg, {:.3} ms worst  (budget 2 ms)",
        edit_ms / n as f64,
        mesh_ms / n as f64,
        chunks as f64 / n as f64,
        (edit_ms + mesh_ms) / n as f64,
        worst
    );

    // Tractor beam: a 4 m capsule smoothly added every tick for 60 ticks.
    let t = Instant::now();
    let mut beam_chunks = 0;
    for i in 0..60 {
        let a = Vec3::new(40.0 + i as f32 * 0.2, 30.0, 40.0);
        grid.apply(Shape::Capsule { a, b: a + Vec3::new(0.0, 0.0, 4.0), radius: 1.0 }, Op::Smooth(0.5));
        let dirty = grid.take_dirty();
        beam_chunks += dirty.len();
        for c in &dirty {
            std::hint::black_box(grid.mesh_chunk(*c));
        }
    }
    println!("tractor beam, 60 ticks: {:.3} ms/tick avg ({:.1} chunks/tick)", ms(t) / 60.0, beam_chunks as f64 / 60.0);

    // The edits alone (no remeshing, which the viewer does not do): what a
    // weapon costs the sim per shot. Big craters are the Nova, the beam is the
    // tractor, which edits every tick while the trigger is held.
    for radius in [1.5, 3.0, 8.0] {
        let n = 100;
        let t = Instant::now();
        for _ in 0..n {
            let x = 10.0 + rng.next() * 108.0;
            let z = 10.0 + rng.next() * 108.0;
            let y = 24.0 + 6.0 * (x * 0.1).sin() * (z * 0.13).cos();
            grid.apply(Shape::Sphere { center: Vec3::new(x, y, z), radius }, Op::Carve);
        }
        println!("edit only, carve r={radius} m: {:.3} ms", ms(t) / n as f64);
        grid.take_dirty();
    }
    let t = Instant::now();
    for i in 0..60 {
        let a = Vec3::new(40.0 + i as f32 * 0.2, 30.0, 70.0);
        grid.apply(Shape::Sphere { center: a, radius: 0.8 }, Op::Smooth(0.4));
    }
    println!("edit only, tractor blob r=0.8 m smooth: {:.3} ms/tick", ms(t) / 60.0);
    grid.take_dirty();

    // A dig: a 2.5 m capsule of radius 1.5 m.
    let n = 100;
    let t = Instant::now();
    for _ in 0..n {
        let a = Vec3::new(10.0 + rng.next() * 108.0, 30.0, 10.0 + rng.next() * 108.0);
        grid.apply(Shape::Capsule { a, b: a + Vec3::X * 2.5, radius: 1.5 }, Op::Carve);
    }
    println!("edit only, dig capsule r=1.5 m x 2.5 m: {:.3} ms", ms(t) / n as f64);
    grid.take_dirty();

    // Collision-style queries.
    let t = Instant::now();
    let mut acc = 0.0;
    let q = 1_000_000;
    for _ in 0..q {
        acc += grid.sample(Vec3::new(rng.next() * 128.0, rng.next() * 64.0, rng.next() * 128.0));
    }
    println!("sample: {:.0} ns each (acc {acc:.1})", ms(t) * 1e6 / q as f64);

    let t = Instant::now();
    let mut hits = 0;
    let r = 10_000;
    for _ in 0..r {
        let o = Vec3::new(rng.next() * 128.0, 30.0, rng.next() * 128.0);
        let d = Vec3::new(rng.next() - 0.5, rng.next() - 0.7, rng.next() - 0.5).normalize();
        hits += grid.raycast(o, d, 100.0).is_some() as u32;
    }
    println!("raycast (100 m): {:.2} us each, {hits}/{r} hit", ms(t) * 1000.0 / r as f64);

}
