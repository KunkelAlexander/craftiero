// A free-flying camera. Plain data plus the three vectors the raymarcher needs.
// Y is up, the world is right-handed, yaw 0 looks along +Z.

use glam::Vec3;
use sdf::Grid;

#[derive(Clone, Copy)]
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

    /// Third-person view: sit a little right of and above `target`, `dist`
    /// metres back along the current yaw and pitch. The offset is what keeps the
    /// worm out of the middle of the screen: without it the camera, the eye and
    /// the crosshair are collinear, so the head always covers the reticle, at
    /// any distance. The view still looks along the aim; `main.rs` aims the shot
    /// at the point under the crosshair so the offset does not move the impact.
    ///
    /// Pulled in when rock is in the way, along the whole camera-to-target
    /// vector, so the shoulder offset cannot poke through a wall in a tunnel.
    pub fn follow(&mut self, target: Vec3, dist: f32, grid: &Grid) {
        const MARGIN: f32 = 0.4;
        // How far right of the aim line the camera sits, in metres.
        const SHOULDER: f32 = 0.75;
        // And how far above it.
        const SHOULDER_UP: f32 = 0.35;
        let (right, _) = self.right_up();
        let offset = right * SHOULDER + Vec3::Y * SHOULDER_UP - self.forward() * dist;
        let len = offset.length();
        let dir = offset / len;
        let d = grid.raycast(target, dir, len + MARGIN).map_or(len, |t| (t - MARGIN).clamp(0.0, len));
        self.pos = target + dir * d;
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
    fn follow_sits_over_the_shoulder_and_is_pulled_in_by_rock() {
        use glam::IVec3;
        use sdf::{Op, Shape};
        let mut grid = Grid::new(IVec3::new(4, 4, 4)); // 16 m cube of rock
        // A cave 8 m wide, so there is room to stand and the wall to the +z side is 4 m away.
        let c = Vec3::splat(8.0);
        grid.apply(Shape::Sphere { center: c, radius: 4.0 }, Op::Carve);
        let mut cam = cam(0.0, 0.0); // looks along +z, so "behind" is -z
        cam.follow(c, 2.0, &grid);
        let rel = cam.pos - c;
        assert!(rel.dot(Vec3::Z) < -1.9, "behind the target: {rel:?}");
        let (right, _) = cam.right_up();
        assert!(rel.dot(right) > 0.5, "over the right shoulder: {rel:?}");
        assert!(rel.y > 0.25, "and a little above it: {rel:?}");
        // The whole point: the target is no longer on the centre ray, so the
        // worm cannot sit on the crosshair.
        let to_target = c - cam.pos;
        let along = to_target.dot(cam.forward());
        let lateral = (to_target - cam.forward() * along).length();
        assert!(lateral > 0.5, "the target is off the crosshair ray: {lateral}");
        cam.follow(c, 10.0, &grid); // the wall behind is only 4 m away
        let d = (cam.pos - c).length();
        assert!(d > 3.0 && d < 4.2, "pulled in to just before the wall: {d}");
        // Looking at the far wall from right next to the near one.
        cam.follow(c - Vec3::Z * 3.5, 10.0, &grid);
        assert!(grid.sample(cam.pos) > 0.0, "the camera is in air, not in rock");
    }
}
