// A free-flying camera. Plain data plus the three vectors the raymarcher needs.
// Y is up, the world is right-handed, yaw 0 looks along +Z.

use glam::Vec3;

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
}
