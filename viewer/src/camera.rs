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

/// One shot's punch, in camera space. `up`, `back` and `side` are metres along
/// the drawn camera's axes; `pitch` is degrees of muzzle climb; `fov` widens the
/// lens by degrees. The numbers live in `feel.rs`, beside the blast signature,
/// because they are feel, not rules.
#[derive(Clone, Copy, Default)]
pub struct Kick {
    pub up: f32,
    pub back: f32,
    pub side: f32,
    pub pitch: f32,
    pub fov: f32,
}

/// The camera's motion: the part of "feel" that is not aim.
///
/// PS2-era third-person games keep the camera a beat behind the character. It
/// eases after the body, gets pulled in by rock, dips when the worm lands and
/// shakes a little when something explodes. That lag is what makes the camera
/// feel like it has weight instead of being welded to the worm.
///
/// Only the *drawn* copy is animated: its position is shoved and its pitch
/// climbs, while the clean camera keeps the player's aim. A cosmetic kick can
/// therefore never feed back into the next shot.
pub struct CameraFeel {
    /// Smoothed point the camera orbits (just above the worm's eye).
    focus: Vec3,
    /// Distance along the shoulder-offset ray. Rock pulls it in at once;
    /// clear air eases it back out, so a tunnel does not pop the view.
    dist: f32,
    /// Landing dip in metres (negative is down) and its speed.
    dip: f32,
    dip_vel: f32,
    /// Shake amplitude in metres, and a clock for the wobble.
    shake: f32,
    clock: f32,
    /// Cosmetic shot shove, in world space (up and back along the shot), and
    /// its spring speed.
    recoil: Vec3,
    recoil_vel: Vec3,
    /// Muzzle climb in radians, and its spring. This is the part of recoil the
    /// eye reads first: a gun that only slides backwards looks like a dolly,
    /// not a shot.
    climb: f32,
    climb_vel: f32,
    /// Field-of-view punch in radians. Unlike a shove, a widening view cannot
    /// move the ray through the centre of the screen, so it is pure impact with
    /// no cost to the aim contract.
    fov_kick: f32,
    /// 0..1 of a weapon wind-up in progress. A full charge narrows the view a
    /// little, so the release has something to open from.
    charge: f32,
    /// Screen flash left this frame, 0..1. Set by blasts and hits, decayed by
    /// `tick`; the post pass turns it into a white frame and a lens split.
    flash: f32,
    /// The first update snaps instead of easing from nowhere.
    started: bool,
}

/// How far behind the worm the camera sits. 5 m is the M2 debug view; much
/// larger starts to read as a spectator camera rather than Ratchet's.
pub const FOLLOW_DIST: f32 = 5.0;
/// 70 ms of lag: enough to see the camera react, not enough to trail the
/// crosshair. The old code recomputed the camera every frame, which is why
/// running felt like the view was glued to the worm.
const FOLLOW_TAU: f32 = 0.07;
/// Easing back out after a wall is slower than the lag, so a camera pulled
/// in by a tunnel does not rush out again the moment the wall ends.
const DIST_OUT_TAU: f32 = 0.18;
/// A landing dip is a spring, not a slide: it goes down and comes back with
/// a little overshoot. About 7 cm on a full jump, capped at 12 cm.
const DIP_PER_SPEED: f32 = 0.006;
const DIP_MAX: f32 = 0.12;
const DIP_SPRING: f32 = 90.0;
const DIP_DAMP: f32 = 16.0;
/// Shake decays fast; a rocket at your feet is over before the next shot.
const SHAKE_DECAY: f32 = 6.0;
/// The cap has to leave room for the top end: a Nova is a 0.20 m shove, and the
/// old 0.12 m ceiling made it a nudge.
const SHAKE_MAX: f32 = 0.22;
/// The shot shove is a stiff spring so it is gone between blaster rounds.
const RECOIL_SPRING: f32 = 170.0;
const RECOIL_DAMP: f32 = 21.0;
const RECOIL_MAX: f32 = 0.35;
/// Muzzle climb is stiffer than the shove: it is gone in about a fifth of a
/// second, well before a heavy weapon can fire again.
const CLIMB_SPRING: f32 = 230.0;
const CLIMB_DAMP: f32 = 25.0;
const CLIMB_MAX: f32 = 0.09;
/// The view widens on a shot and narrows again in a few frames.
const FOV_TAU: f32 = 0.07;
/// A full wind-up narrows the view by about 6 degrees.
const CHARGE_SQUEEZE: f32 = 0.10;
/// Screen flash: a short fuse, because a flash that lingers reads as fog. At
/// 75 ms the frame is white for two or three frames at 60 Hz, then gone.
const FLASH_TAU: f32 = 0.075;

impl Default for CameraFeel {
    fn default() -> CameraFeel {
        CameraFeel {
            focus: Vec3::ZERO,
            dist: 0.0,
            dip: 0.0,
            dip_vel: 0.0,
            shake: 0.0,
            clock: 0.0,
            recoil: Vec3::ZERO,
            recoil_vel: Vec3::ZERO,
            climb: 0.0,
            climb_vel: 0.0,
            fov_kick: 0.0,
            charge: 0.0,
            flash: 0.0,
            started: false,
        }
    }
}

impl CameraFeel {
    /// Move the camera one frame. `target` is the point to orbit (the worm's
    /// eye); `dist` is the unobstructed follow distance; `cam.yaw/pitch` are
    /// already the player's aim.
    pub fn update(&mut self, dt: f32, cam: &mut Camera, target: Vec3, dist: f32, grid: &Grid) {
        let dt = dt.clamp(0.0, 0.1);
        if !self.started {
            self.focus = target;
            self.started = true;
        }
        // Frame-rate independent exponential smoothing, so 60 and 144 Hz
        // players get the same lag in seconds.
        self.focus = smooth(self.focus, target, FOLLOW_TAU, dt);
        // A lagging focus can cut a corner through a thin wall. Never let the
        // camera orbit a point inside rock: jump the focus to the worm and let
        // the distance limit handle the wall. This is rare (the lag is 0.8 m
        // at full speed) and much better than one frame of flat rock.
        if grid.sample(self.focus) < 0.2 {
            self.focus = target;
        }

        // Where the camera would sit with nothing in the way. `follow` also
        // does the rock raycast, so the collision test and the desired
        // position cannot disagree.
        let mut probe = *cam;
        probe.follow(self.focus, dist, grid);
        let offset = probe.pos - self.focus;
        let free = offset.length();
        if free > 1.0e-4 {
            let direction = offset / free;
            // A wall is an immediate fact: snap in this frame, or the camera
            // spends a few frames inside the rock. Easing out is safe.
            self.dist = if free < self.dist || self.dist <= 0.0 {
                free
            } else {
                smooth_scalar(self.dist, free, DIST_OUT_TAU, dt)
            };
            cam.pos = self.focus + direction * self.dist;
        } else {
            cam.pos = self.focus;
        }

        // Landing dip: a damped spring. Negative `dip` lowers the eye, then
        // the camera comes back up and overshoots by a hair.
        self.dip_vel += (-DIP_SPRING * self.dip - DIP_DAMP * self.dip_vel) * dt;
        self.dip += self.dip_vel * dt;
        cam.pos.y += self.dip;

        self.clock += dt;
        self.shake *= (-SHAKE_DECAY * dt).exp();
        if self.shake < 1.0e-4 {
            self.shake = 0.0;
        }
        self.recoil_vel += (-RECOIL_SPRING * self.recoil - RECOIL_DAMP * self.recoil_vel) * dt;
        self.recoil += self.recoil_vel * dt;
        self.climb_vel += (-CLIMB_SPRING * self.climb - CLIMB_DAMP * self.climb_vel) * dt;
        self.climb += self.climb_vel * dt;
        self.fov_kick *= (-dt / FOV_TAU).exp();
        if self.fov_kick < 1.0e-4 {
            self.fov_kick = 0.0;
        }

        // Keep the view inside the arena, like the fly camera does.
        cam.pos.y = cam.pos.y.min(63.0);
    }

    /// The camera to draw with: the clean gameplay camera plus cosmetic shake,
    /// shot shove, muzzle climb, lens punch and the charge squeeze. Aim keeps
    /// using the clean copy, so none of it ever steers a shot.
    pub fn render(&self, cam: Camera) -> Camera {
        let t = self.clock;
        // Three incommensurate sines make a cheap, non-repeating wobble.
        let wobble = Vec3::new(
            (t * 37.0).sin() + 0.5 * (t * 61.0).sin(),
            (t * 29.0 + 1.7).sin() + 0.5 * (t * 53.0 + 0.4).sin(),
            (t * 41.0 + 3.1).sin(),
        );
        let mut out = cam;
        // Shake in screen space, so the wobble reads the same whichever way
        // the camera faces. A little depth too, like a hand-held camera.
        let (right, up) = cam.right_up();
        let offset = right * wobble.x + up * wobble.y + cam.forward() * (wobble.z * 0.5);
        out.pos += offset * (self.shake * 0.66) + self.recoil;
        // The climb and the view punch happen after the directions above have
        // been worked out from the clean camera, so they only tilt the picture.
        let limit = 89f32.to_radians();
        out.pitch = (out.pitch + self.climb).clamp(-limit, limit);
        out.fov_y = (out.fov_y + self.fov_kick - CHARGE_SQUEEZE * self.charge).max(0.25);
        out
    }

    /// The worm touched down at `impact` m/s downward. A bigger drop gives a
    /// deeper dip, but even a jump only moves the view a few centimetres.
    pub fn land(&mut self, impact: f32) {
        if impact <= 3.0 {
            return;
        }
        let dip = -((impact - 3.0) * DIP_PER_SPEED).min(DIP_MAX);
        // A deeper dip already in progress wins; a second landing cannot
        // cancel it by asking for a shallower one.
        self.dip = self.dip.min(dip);
    }

    /// Add a shake, in metres. The strongest recent shake wins, so a blaster
    /// a frame later cannot drown out the rocket that caused it.
    pub fn shake(&mut self, metres: f32) {
        self.shake = self.shake.max(metres.clamp(0.0, SHAKE_MAX));
    }

    /// Fire the screen flash. The strongest recent event wins, so a Nova's
    /// white-out is not cancelled by the blaster tick that lands next frame.
    pub fn burst(&mut self, amount: f32) {
        self.flash = self.flash.max(amount.clamp(0.0, 1.0));
    }

    /// The flash left this frame, 0..1.
    pub fn flash(&self) -> f32 {
        self.flash
    }

    /// Fade the flash. This is called once per frame even while flying, so a
    /// flash left over from combat cannot stick to the screen.
    pub fn tick(&mut self, dt: f32) {
        self.flash *= (-dt.max(0.0) / FLASH_TAU).exp();
        if self.flash < 1.0e-3 {
            self.flash = 0.0;
        }
    }

    /// Add one shot's punch. `cam` is the clean camera, used only for its axes.
    /// This is applied only when drawing, so holding the trigger cannot walk
    /// the aim.
    pub fn kick(&mut self, cam: &Camera, k: Kick) {
        let (right, up) = cam.right_up();
        let shove = up * k.up + right * k.side - cam.forward() * k.back;
        self.recoil = (self.recoil + shove).clamp_length_max(RECOIL_MAX);
        self.climb = (self.climb + k.pitch.to_radians()).clamp(-CLIMB_MAX, CLIMB_MAX);
        self.fov_kick = (self.fov_kick + k.fov.to_radians()).min(0.25);
    }

    /// Add a lens punch without a shove. An explosion uses this: widening the
    /// view cannot move the ray through the centre of the screen, so it is pure
    /// impact with no cost to the aim contract.
    pub fn punch(&mut self, degrees: f32) {
        self.fov_kick = (self.fov_kick + degrees.to_radians()).min(0.25);
    }

    /// How far into a wind-up the held weapon is (0..1). A full charge narrows
    /// the view, so the shot has something to open out from.
    pub fn set_charge(&mut self, t: f32) {
        self.charge = t.clamp(0.0, 1.0);
    }

    /// Put the camera straight at `target` (a respawn or a teleport), so it
    /// does not fly across the map after the worm.
    pub fn snap(&mut self, target: Vec3) {
        self.focus = target;
        self.started = false;
        self.dist = 0.0;
        self.dip = 0.0;
        self.dip_vel = 0.0;
        self.shake = 0.0;
        self.recoil = Vec3::ZERO;
        self.recoil_vel = Vec3::ZERO;
        self.climb = 0.0;
        self.climb_vel = 0.0;
        self.fov_kick = 0.0;
        self.flash = 0.0;
        self.charge = 0.0;
    }
}

fn smooth(current: Vec3, target: Vec3, tau: f32, dt: f32) -> Vec3 {
    if tau <= 0.0 {
        return target;
    }
    current.lerp(target, 1.0 - (-dt / tau).exp())
}

fn smooth_scalar(current: f32, target: f32, tau: f32, dt: f32) -> f32 {
    if tau <= 0.0 {
        return target;
    }
    current + (target - current) * (1.0 - (-dt / tau).exp())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cam(yaw: f32, pitch: f32) -> Camera {
        Camera { pos: Vec3::ZERO, yaw, pitch, fov_y: 1.0 }
    }

    /// A spherical cave in a solid block: enough air for a 5 m camera arm.
    fn cave(chunks: i32, radius: f32) -> (Grid, Vec3) {
        use glam::IVec3;
        use sdf::{Op, Shape};
        let mut grid = Grid::new(IVec3::splat(chunks));
        let c = Vec3::splat(chunks as f32 * 4.0 * 0.5);
        grid.apply(Shape::Sphere { center: c, radius }, Op::Carve);
        (grid, c)
    }

    fn update_many(feel: &mut CameraFeel, cam: &mut Camera, target: Vec3, grid: &Grid, n: usize) {
        for _ in 0..n {
            feel.update(1.0 / 60.0, cam, target, FOLLOW_DIST, grid);
        }
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

    #[test]
    fn the_feel_eases_after_the_worm_and_stays_out_of_rock() {
        let (grid, mut c) = cave(8, 15.0);
        let mut cam = Camera { pos: c + Vec3::Z * 12.0, yaw: 0.0, pitch: 0.0, fov_y: 1.0 };
        let mut feel = CameraFeel::default();
        update_many(&mut feel, &mut cam, c, &grid, 120);
        assert!(grid.sample(cam.pos) > 0.01, "the camera is in air");
        assert!(cam.pos.z < c.z - 2.0, "behind the worm: {:?}", cam.pos - c);
        assert!((cam.pos - c).length() > 4.0, "at the follow distance: {:?}", cam.pos - c);

        // Move the target a metre sideways: the camera lags, then catches up.
        let before = cam.pos;
        c += Vec3::X;
        feel.update(1.0 / 60.0, &mut cam, c, FOLLOW_DIST, &grid);
        let moved = (cam.pos - before).length();
        assert!(moved > 0.0 && moved < 0.5, "one frame of lag, not a snap: {moved}");
        update_many(&mut feel, &mut cam, c, &grid, 120);
        let d = (cam.pos - c).length();
        assert!(d > 4.0 && d < 6.5, "caught up to the follow arm: {d}");
    }

    #[test]
    fn rock_pulls_the_camera_in_at_once_and_it_eases_back_out() {
        let (tight, c) = cave(4, 2.0);
        let mut cam = Camera { pos: c, yaw: 0.0, pitch: 0.0, fov_y: 1.0 };
        let mut feel = CameraFeel::default();
        feel.update(1.0 / 60.0, &mut cam, c, FOLLOW_DIST, &tight);
        let pulled = (cam.pos - c).length();
        assert!(pulled < 2.5, "the wall pulls it in at once: {pulled}");
        assert!(tight.sample(cam.pos) > 0.0, "never inside the rock");

        let (open, _) = cave(4, 7.5);
        feel.update(1.0 / 60.0, &mut cam, c, FOLLOW_DIST, &open);
        let after = (cam.pos - c).length();
        assert!(after > pulled + 0.01, "eases out, not snaps: {pulled} -> {after}");
        assert!(after < FOLLOW_DIST + 1.0, "still easing: {after}");
        update_many(&mut feel, &mut cam, c, &open, 120);
        assert!((cam.pos - c).length() > 4.0, "back out: {:?}", cam.pos - c);
    }

    #[test]
    fn a_landing_dips_the_view_and_returns() {
        let (grid, c) = cave(8, 15.0);
        let mut cam = Camera { pos: Vec3::ZERO, yaw: 0.0, pitch: 0.0, fov_y: 1.0 };
        let mut feel = CameraFeel::default();
        update_many(&mut feel, &mut cam, c, &grid, 60);
        let level = cam.pos.y;

        feel.land(14.0);
        feel.update(1.0 / 60.0, &mut cam, c, FOLLOW_DIST, &grid);
        assert!(cam.pos.y < level - 0.01, "the view dips: {} vs {level}", cam.pos.y);
        update_many(&mut feel, &mut cam, c, &grid, 120);
        assert!((cam.pos.y - level).abs() < 0.005, "and comes back: {}", cam.pos.y);
    }

    #[test]
    fn a_shot_shoves_the_view_pitches_it_and_widens_the_lens() {
        let (grid, c) = cave(8, 15.0);
        let mut cam = Camera { pos: Vec3::ZERO, yaw: 0.0, pitch: 0.0, fov_y: 1.0 };
        let mut feel = CameraFeel::default();
        update_many(&mut feel, &mut cam, c, &grid, 60);
        let clean = cam;

        assert_eq!(feel.render(clean).pos, clean.pos, "nothing to add at rest");
        feel.shake(0.08);
        assert!((feel.render(clean).pos - clean.pos).length() > 1.0e-3);
        // Let the shake die first, then check the punch on its own.
        update_many(&mut feel, &mut cam, c, &grid, 120);

        feel.kick(&clean, Kick { up: 0.06, back: 0.20, pitch: 2.0, fov: 5.0, ..Kick::default() });
        let kicked = feel.render(clean);
        assert!(kicked.pos.y > clean.pos.y + 0.03, "the view is shoved up: {} vs {}", kicked.pos.y, clean.pos.y);
        assert!(kicked.pitch > clean.pitch + 0.01, "the muzzle climbs: {}", kicked.pitch);
        assert!(kicked.fov_y > clean.fov_y + 0.05, "and the lens widens: {}", kicked.fov_y);
        assert_eq!(clean.pitch, 0.0, "the aim camera is untouched");

        update_many(&mut feel, &mut cam, c, &grid, 180);
        let settled = feel.render(clean);
        assert!((settled.pos - clean.pos).length() < 1.0e-3, "gone again: {:?}", settled.pos - clean.pos);
        assert!((settled.pitch - clean.pitch).abs() < 1.0e-3, "climb is gone: {}", settled.pitch);
        assert!((settled.fov_y - clean.fov_y).abs() < 1.0e-3, "lens is back: {}", settled.fov_y);
    }

    #[test]
    fn a_nova_shove_is_allowed_past_the_old_ceiling() {
        // The old cap was 0.12 m, which turned a Nova into a nudge. The top of
        // the range has to be a real shove, while still being capped.
        let mut feel = CameraFeel::default();
        feel.shake(0.20);
        assert!((feel.shake - 0.20).abs() < 1.0e-6, "a Nova is felt: {}", feel.shake);
        feel.shake(1.0);
        assert!((feel.shake - SHAKE_MAX).abs() < 1.0e-6);
    }

    #[test]
    fn a_blast_flashes_the_screen_then_gets_out_of_the_way() {
        let mut feel = CameraFeel::default();
        feel.burst(0.5);
        assert!((feel.flash() - 0.5).abs() < 1.0e-6);
        // The strongest recent event wins, so a following small hit cannot
        // cancel the Nova's white-out.
        feel.burst(0.1);
        assert!((feel.flash() - 0.5).abs() < 1.0e-6);
        // Three frames at 60 Hz: still bright. A fifth of a second later: gone.
        for _ in 0..3 {
            feel.tick(1.0 / 60.0);
        }
        assert!(feel.flash() > 0.25, "a flash lasts a blink: {}", feel.flash());
        feel.tick(0.2);
        assert!(feel.flash() < 0.02, "but not two: {}", feel.flash());
    }

    #[test]
    fn a_blast_punches_the_lens_without_moving_the_aim() {
        let (grid, c) = cave(8, 15.0);
        let mut cam = Camera { pos: Vec3::ZERO, yaw: 0.0, pitch: 0.0, fov_y: 1.0 };
        let mut feel = CameraFeel::default();
        update_many(&mut feel, &mut cam, c, &grid, 60);
        let clean = cam;

        feel.punch(4.5);
        let punched = feel.render(clean);
        assert!(punched.fov_y > clean.fov_y + 0.05, "the lens widens: {}", punched.fov_y);
        assert!((punched.pos - clean.pos).length() < 1.0e-6, "the eye does not move");
        assert!((punched.pitch - clean.pitch).abs() < 1.0e-6, "and the aim line is untouched");
    }

    #[test]
    fn a_heavy_kick_is_settled_before_that_weapon_can_fire_again() {
        let (grid, c) = cave(8, 15.0);
        let mut cam = Camera { pos: Vec3::ZERO, yaw: 0.0, pitch: 0.0, fov_y: 1.0 };
        let mut feel = CameraFeel::default();
        update_many(&mut feel, &mut cam, c, &grid, 60);
        let clean = cam;

        // A Nova-sized punch, then half a second: the railgun's cooldown is the
        // shortest wait between two aimed shots of a heavy weapon, so by then
        // none of the punch may still be pushing the view off the reticle.
        feel.kick(&clean, Kick { up: 0.10, back: 0.30, pitch: 3.0, fov: 8.0, ..Kick::default() });
        assert!(feel.render(clean).pitch - clean.pitch > 0.04, "the climb is felt");
        update_many(&mut feel, &mut cam, c, &grid, 30);
        let settled = feel.render(clean);
        assert!((settled.pitch - clean.pitch).abs() < 0.001, "climb is gone: {}", settled.pitch - clean.pitch);
        assert!((settled.fov_y - clean.fov_y).abs() < 0.001, "lens is back: {}", settled.fov_y - clean.fov_y);
        assert!((settled.pos - clean.pos).length() < 0.004, "the shove is gone: {:?}", settled.pos - clean.pos);
    }

    #[test]
    fn a_charge_narrows_the_view_and_releasing_it_opens_again() {
        let (grid, c) = cave(8, 15.0);
        let mut cam = Camera { pos: Vec3::ZERO, yaw: 0.0, pitch: 0.0, fov_y: 1.0 };
        let mut feel = CameraFeel::default();
        update_many(&mut feel, &mut cam, c, &grid, 60);
        let clean = cam;

        feel.set_charge(1.0);
        assert!(feel.render(clean).fov_y < clean.fov_y - 0.05, "a full charge narrows the view");
        feel.set_charge(0.0);
        assert!((feel.render(clean).fov_y - clean.fov_y).abs() < 1.0e-6, "and letting it go restores it");
    }

    #[test]
    fn a_snap_puts_the_camera_at_the_new_place_without_a_flight() {
        let (grid, c) = cave(8, 15.0);
        let mut cam = Camera { pos: Vec3::ZERO, yaw: 0.0, pitch: 0.0, fov_y: 1.0 };
        let mut feel = CameraFeel::default();
        update_many(&mut feel, &mut cam, c, &grid, 60);

        let far = c + Vec3::new(8.0, 0.0, 0.0);
        feel.snap(far);
        feel.update(1.0 / 60.0, &mut cam, far, FOLLOW_DIST, &grid);
        assert!((cam.pos - far).length() < FOLLOW_DIST + 1.0, "there in one frame: {:?}", cam.pos - far);
    }
}
