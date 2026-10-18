// The demo map: rolling hills, a big cavern with pillars, tunnels up to the
// surface and a few floating rocks. Everything is built from signed distances
// (negative = rock) combined with min (add rock) and max(-x) (carve), the same
// two moves the player makes with the brush.
//
// `Grid::from_fn` skips chunks that are far from the surface, which is only
// valid if this function never OVER-estimates the distance. So the height
// field uses a slope-corrected estimate and the shapes are lower bounds.

use glam::{IVec3, Vec3};
use sdf::Grid;

/// 128 x 64 x 128 m: 32 x 16 x 32 chunks of 16 voxels of 0.25 m.
pub const WORLD_CHUNKS: IVec3 = IVec3::new(32, 16, 32);

pub fn build() -> Grid {
    Grid::from_fn(WORLD_CHUNKS, terrain)
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
fn height(x: f32, z: f32) -> f32 {
    use std::sync::OnceLock;
    const STEP: f32 = 0.25;
    const N: usize = 513; // 0..=128 m
    static TABLE: OnceLock<Vec<f32>> = OnceLock::new();
    let table = TABLE.get_or_init(|| {
        (0..N * N).map(|i| height_raw((i % N) as f32 * STEP, (i / N) as f32 * STEP)).collect()
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
/// a valley and the box walls are never seen as flat slabs.
fn height_raw(x: f32, z: f32) -> f32 {
    let hills = noise2(x * 0.02, z * 0.02) * 16.0 + noise2(x * 0.045, z * 0.045) * 8.0 + noise2(x * 0.1, z * 0.1) * 2.5;
    // Distance to the arena edge. A max-norm would be exact but has creases
    // along the diagonals, where the height field would fold and the distance
    // estimate with it. The 8-norm is a smooth, squared-off stand-in.
    let r = ((x - 64.0).abs().powi(8) + (z - 64.0).abs().powi(8)).powf(0.125);
    let t = (1.0 - (64.0 - r) / 30.0).clamp(0.0, 1.0);
    let rim = t * t * (3.0 - 2.0 * t) * 56.0;
    10.0 + hills + rim
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
fn ground(p: Vec3) -> f32 {
    let e = 0.5;
    let gx = (height(p.x + e, p.z) - height(p.x - e, p.z)) / (2.0 * e);
    let gz = (height(p.x, p.z + e) - height(p.x, p.z - e)) / (2.0 * e);
    let local = 0.9 / (1.0 + gx * gx + gz * gz).sqrt();
    let u = p.y - height(p.x, p.z);
    let near = u.clamp(-2.0, 2.0);
    local * near + (u - near) * 0.2
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

pub fn terrain(p: Vec3) -> f32 {
    // Rock below the height field.
    let mut d = ground(p);

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
    d = smax(d, -cave, 1.5);

    // Pillars hold up the cavern roof.
    for (x, z) in [(52.0, 52.0), (78.0, 56.0), (60.0, 78.0), (82.0, 78.0)] {
        d = smin(d, capsule(p, Vec3::new(x, 0.0, z), Vec3::new(x, 18.0, z), 2.2), 1.5);
    }

    // Floating rocks above the hills.
    d = d.min(sphere(p, Vec3::new(30.0, 48.0, 80.0), 6.0) * 0.8);
    d = d.min(ellipsoid(p, Vec3::new(98.0, 52.0, 34.0), Vec3::new(9.0, 4.0, 7.0)) * 0.8);
    d = d.min(sphere(p, Vec3::new(64.0, 44.0, 18.0), 4.0) * 0.8);
    d
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
        assert!(terrain(Vec3::new(2.0, 60.0, 2.0)) < 0.0, "the rim mountains reach the top at the edge");
    }

    #[test]
    fn distance_is_never_overestimated() {
        // Grid::from_fn's shortcut relies on this: the field may not change
        // faster than one metre per metre walked. Check lines at several
        // heights through hills, rim, cavern, pillars and tunnels.
        let step = 0.1;
        for (y, z) in [(9.0, 52.0), (30.0, 20.0), (6.0, 100.0), (48.0, 64.0), (20.0, 4.0)] {
            let mut prev = terrain(Vec3::new(1.0, y, z));
            let mut x = 1.0 + step;
            while x < 127.0 {
                let d = terrain(Vec3::new(x, y, z));
                assert!((d - prev).abs() <= step * 1.001, "Lipschitz violated at ({x}, {y}, {z}): {prev} -> {d}");
                prev = d;
                x += step;
            }
        }
    }
}

