// A free-flying camera. Plain data plus the three vectors the raymarcher needs.
// Y is up, the world is right-handed, yaw 0 looks along +Z.

use glam::Vec3;
use sdf::Grid;

pub struct Camera {
    pub pos: Vec3,
    /// Radians, turning left is positive.
    pub yaw: f32,
    /// Radians, looking up is positive. Kept short of straight up/down so
    /// `right` never degenerates.
    pub pitch: f32,
    /// Vertical field of view in radians.
    pub fov_y: f32,
}

impl Camera {
    pub fn forward(&self) -> Vec3 {
        let (sy, cy) = self.yaw.sin_cos();
        let (sp, cp) = self.pitch.sin_cos();
        Vec3::new(sy * cp, sp, cy * cp)
    }

    /// Unit vectors (right, up). With y-up and right-handed axes, looking down
    /// -Z puts +X on the right, which is what `forward x Y` gives.
    pub fn right_up(&self) -> (Vec3, Vec3) {
        let f = self.forward();
        let right = f.cross(Vec3::Y).normalize();
        (right, right.cross(f))
    }

    /// Turn by mouse movement in pixels. Mouse right = look right = yaw down.
    pub fn look(&mut self, dx: f32, dy: f32) {
        const SENSITIVITY: f32 = 0.0022;
        self.yaw -= dx * SENSITIVITY;
        let limit = 89f32.to_radians();
        self.pitch = (self.pitch - dy * SENSITIVITY).clamp(-limit, limit);
    }

    /// Third-person view: sit `dist` metres behind `target`, looking along the
    /// current yaw and pitch. Pulled in when rock is in the way, so the camera
    /// never ends up inside a wall (where the raymarcher would paint a solid
    /// screen) and a worm in a tunnel is seen from just behind.
    pub fn follow(&mut self, target: Vec3, dist: f32, grid: &Grid) {
        const MARGIN: f32 = 0.4;
        let back = -self.forward();
        let d = grid.raycast(target, back, dist + MARGIN).map_or(dist, |t| (t - MARGIN).clamp(0.0, dist));
        self.pos = target + back * d;
        // Keep below the top of the arena, like the flying camera does.
        self.pos.y = self.pos.y.min(63.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cam(yaw: f32, pitch: f32) -> Camera {
        Camera { pos: Vec3::ZERO, yaw, pitch, fov_y: 1.0 }
    }

    #[test]
    fn yaw_zero_looks_down_plus_z_with_x_on_the_left() {
        let c = cam(0.0, 0.0);
        assert!((c.forward() - Vec3::Z).length() < 1e-6);
        // Looking toward +Z in a right-handed y-up world, +X is on the left.
        assert!((c.right_up().0 + Vec3::X).length() < 1e-6);
    }

    #[test]
    fn mouse_right_turns_right_and_mouse_down_looks_down() {
        let mut c = cam(0.0, 0.0);
        c.look(100.0, 0.0);
        assert!(c.forward().x < 0.0, "turned toward -X, which is to the right of +Z");
        c.look(0.0, 100.0);
        assert!(c.forward().y < 0.0);
    }

    #[test]
    fn basis_is_orthonormal_even_when_looking_nearly_straight_up() {
        let mut c = cam(0.7, 0.0);
        c.look(0.0, -1.0e6); // clamped
        let (r, u) = c.right_up();
        let f = c.forward();
        assert!(r.dot(f).abs() < 1e-5 && u.dot(f).abs() < 1e-5 && r.dot(u).abs() < 1e-5);
        assert!((u.length() - 1.0).abs() < 1e-5);
    }

    #[test]
    fn follow_sits_behind_the_target_and_is_pulled_in_by_rock() {
        use glam::IVec3;
        use sdf::{Op, Shape};
        let mut grid = Grid::new(IVec3::new(4, 4, 4)); // 16 m cube of rock
        // A cave 8 m wide, so there is room to stand and the wall to the +z side is 4 m away.
        let c = Vec3::splat(8.0);
        grid.apply(Shape::Sphere { center: c, radius: 4.0 }, Op::Carve);
        let mut cam = cam(0.0, 0.0); // looks along +z, so "behind" is -z
        cam.follow(c, 2.0, &grid);
        assert!((cam.pos - (c - Vec3::Z * 2.0)).length() < 1e-4, "free view: {:?}", cam.pos);
        cam.follow(c, 10.0, &grid); // the wall behind is only 4 m away
        let d = (cam.pos - c).length();
        assert!(d > 3.0 && d < 4.0, "pulled in to just before the wall: {d}");
        // Looking at the far wall from right next to the near one.
        cam.follow(c - Vec3::Z * 3.5, 10.0, &grid);
        assert!(grid.sample(cam.pos) > 0.0, "the camera is in air, not in rock");
    }
}
