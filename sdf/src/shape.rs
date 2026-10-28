// Edits: a shape plus an operation. Pattern: Command. An edit is a small value,
// so the host can apply it, log it, and send it to clients (~20 bytes).

use glam::Vec3;

#[derive(Clone, Copy, Debug)]
pub enum Shape {
    Sphere { center: Vec3, radius: f32 },
    /// A swept sphere: the beam of a tractor beam, or a tunnel.
    Capsule { a: Vec3, b: Vec3, radius: f32 },
    /// An upright box, rotated about the world Y axis by `yaw`. `half` is in
    /// the box's own frame: x is its width, y its height, z its thickness. The
    /// Block weapon uses this to throw up a rectangular wall; a box keeps the
    /// faces flat instead of looking like a row of lumps.
    Box { centre: Vec3, half: Vec3, yaw: f32 },
    /// A thin slab: the cut of a whip. It spans the segment `a..b` along one
    /// axis and reaches `depth` metres into the rock along `dir` (the part of
    /// `dir` along `a..b` is dropped, so the faces stay flat), and is
    /// `2 * half_thickness` thick across the sheet. Where a capsule leaves a
    /// round tunnel, this leaves a flat-walled slit, like a wire through cheese.
    Blade { a: Vec3, b: Vec3, dir: Vec3, depth: f32, half_thickness: f32 },
    /// A flat cylinder (disc) around `axis` (a unit vector), `half_height`
    /// either side of its centre, dragged from centre `a` to centre `b`. With
    /// `a == b` it is a plain disc pressed into the ground; the whip drags it
    /// along its path, so one swing is one smooth flat-bottomed groove instead
    /// of a row of separate holes. `axis` is the surface normal.
    Cylinder { a: Vec3, b: Vec3, axis: Vec3, radius: f32, half_height: f32 },
}

/// How much a cylinder's rim is rounded, in metres. Same reason as the blade:
/// a sharp thin disc aliases on the stored grid.
const CYLINDER_ROUND: f32 = 0.2;

/// The blade's own frame: its centre, its three unit axes (along the cut,
/// into the rock, across the sheet) and its half extents in those axes.
fn blade_frame(a: Vec3, b: Vec3, dir: Vec3, depth: f32, half_thickness: f32) -> (Vec3, [Vec3; 3], Vec3) {
    let ab = b - a;
    let len = ab.length();
    let u = if len > 1e-4 { ab / len } else { dir.any_orthonormal_vector() };
    // Into the rock, made perpendicular to the cut so the slab is a true box.
    let v = (dir - u * dir.dot(u)).try_normalize().unwrap_or_else(|| u.any_orthonormal_vector());
    let w = u.cross(v);
    let centre = (a + b) * 0.5 + v * (depth * 0.5);
    (centre, [u, v, w], Vec3::new(len * 0.5, depth * 0.5, half_thickness))
}

impl Shape {
    /// Exact signed distance to the shape (negative inside it), in metres.
    pub fn distance(&self, p: Vec3) -> f32 {
        match *self {
            Shape::Sphere { center, radius } => (p - center).length() - radius,
            Shape::Capsule { a, b, radius } => {
                let ab = b - a;
                let t = ((p - a).dot(ab) / ab.length_squared().max(1e-6)).clamp(0.0, 1.0);
                (p - (a + ab * t)).length() - radius
            }
            Shape::Box { centre, half, yaw } => {
                // Rotate the point into the box's frame (the inverse of `yaw`
                // about Y), then use the standard exact box distance.
                let d = p - centre;
                let (s, c) = yaw.sin_cos();
                let local = Vec3::new(c * d.x - s * d.z, d.y, s * d.x + c * d.z);
                let q = local.abs() - half;
                q.max(Vec3::ZERO).length() + q.x.max(q.y).max(q.z).min(0.0)
            }
            Shape::Blade { a, b, dir, depth, half_thickness } => {
                let (centre, axes, half) = blade_frame(a, b, dir, depth, half_thickness);
                let d = p - centre;
                let local = Vec3::new(d.dot(axes[0]), d.dot(axes[1]), d.dot(axes[2]));
                // A rounded box: same extents, edges rounded by the sheet's
                // half thickness. A sharp-cornered 0.5 m slab is only two
                // voxels wide, and its corners alias into a staircase on the
                // stored grid; the rounded one has no crease to alias, and
                // consecutive blades along a cut join without notches.
                let r = half_thickness.min(half.x).min(half.y);
                let q = local.abs() - (half - Vec3::splat(r));
                q.max(Vec3::ZERO).length() + q.x.max(q.y).max(q.z).min(0.0) - r
            }
            Shape::Cylinder { a, b, axis, radius, half_height } => {
                // Closest point on the centre path, found in the disc's plane
                // (the path's part along the axis does not move the footprint).
                let ab = b - a;
                let ab_flat = ab - axis * ab.dot(axis);
                let t = ((p - a).dot(ab_flat) / ab_flat.length_squared().max(1e-6)).clamp(0.0, 1.0);
                // Then 2D: distance along the axis and distance from it, and
                // the exact rounded-rectangle distance in that plane.
                let d = p - (a + ab * t);
                let along = d.dot(axis);
                let across = (d - axis * along).length();
                let r = CYLINDER_ROUND.min(radius).min(half_height);
                let q = glam::Vec2::new(across - (radius - r), along.abs() - (half_height - r));
                q.max(glam::Vec2::ZERO).length() + q.x.max(q.y).min(0.0) - r
            }
        }
    }

    pub fn bounds(&self) -> (Vec3, Vec3) {
        match *self {
            Shape::Sphere { center, radius } => (center - Vec3::splat(radius), center + Vec3::splat(radius)),
            Shape::Capsule { a, b, radius } => (a.min(b) - Vec3::splat(radius), a.max(b) + Vec3::splat(radius)),
            Shape::Box { centre, half, yaw } => {
                // A yawed box's world AABB is wider than its own `half`: the
                // width and thickness both contribute to x and z.
                let (s, c) = yaw.sin_cos();
                let ex = half.x * c.abs() + half.z * s.abs();
                let ez = half.x * s.abs() + half.z * c.abs();
                (centre - Vec3::new(ex, half.y, ez), centre + Vec3::new(ex, half.y, ez))
            }
            Shape::Blade { a, b, dir, depth, half_thickness } => {
                // The AABB of an oriented box: each world axis gets the sum of
                // the half extents projected onto it.
                let (centre, axes, half) = blade_frame(a, b, dir, depth, half_thickness);
                let e = axes[0].abs() * half.x + axes[1].abs() * half.y + axes[2].abs() * half.z;
                (centre - e, centre + e)
            }
            Shape::Cylinder { a, b, axis, radius, half_height } => {
                // Along each world axis: the height's projection plus the
                // disc's rim, which reaches radius * sin(angle to the axis).
                let rim = (Vec3::ONE - axis * axis).max(Vec3::ZERO);
                let e = axis.abs() * half_height + Vec3::new(rim.x.sqrt(), rim.y.sqrt(), rim.z.sqrt()) * radius;
                (a.min(b) - e, a.max(b) + e)
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub enum Op {
    /// Dig: terrain minus shape.
    Carve,
    /// Build: terrain plus shape.
    Add,
    /// Build with a rounded join of width `k` metres, so new material flows
    /// into the old surface instead of meeting it at a crease. This is what
    /// makes "forming" terrain look like clay.
    Smooth(f32),
}

impl Op {
    /// How far beyond the stored band a shape can still change values.
    pub fn extra_reach(self) -> f32 {
        match self {
            Op::Smooth(k) => k,
            _ => 0.0,
        }
    }

    /// New terrain distance from the old one and the shape's distance.
    pub fn combine(self, old: f32, shape: f32) -> f32 {
        match self {
            Op::Carve => old.max(-shape),
            Op::Add => old.min(shape),
            Op::Smooth(k) => {
                // Polynomial smooth minimum.
                let h = (k - (old - shape).abs()).max(0.0) / k;
                old.min(shape) - h * h * k * 0.25
            }
        }
    }
}
