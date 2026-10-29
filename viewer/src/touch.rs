//! Touch controls for phones: a floating move stick, a look drag and a few
//! round buttons, all in the HUD's virtual pixels (720 units tall, see `hud.rs`)
//! so the thing that is drawn is exactly the thing that is hit-tested.
//!
//! This file knows nothing about winit or wgpu: `main.rs` feeds it touch events
//! and reads back the held buttons, and `hud.rs` draws it. Like the sim it can be
//! tested without a window.
//!
//! The design copies what phone shooters do:
//!   * Left thumb: a stick that appears where the thumb lands (a fixed stick is
//!     missed by thumbs that drift).
//!   * Right thumb on empty space: drag to look.
//!   * FIRE and DIG are hold buttons that keep steering the camera while held, so
//!     shooting or tunnelling never costs you your aim. This is why a finger
//!     stays bound to the control it landed on for its whole life (a *role*)
//!     instead of being re-tested every move.
//!   * Climbing stays automatic (walk into a wall), like the keyboard game.

use crate::hud::MARGIN;
use glam::Vec2;

/// How far the thumb has to travel from where it landed for full speed.
const STICK_RADIUS: f32 = 90.0;
/// Small deflections are thumb jitter, not steering.
const DEAD_ZONE: f32 = 0.12;
/// Virtual pixels of finger travel -> the "mouse pixels" `Camera::look` expects.
/// A thumb moves less than a mouse, so it gets a gain. Tune on a real phone.
const LOOK_GAIN: f32 = 2.0;
/// A finger may land a bit outside a button's drawn circle and still press it.
const HIT_SLACK: f32 = 1.15;

/// Where the weapon plate sits on a touch screen (top right, out of the way of
/// the thumbs). Tapping it is the "next weapon" button, so the HUD's own panel
/// doubles as the control. `hud.rs` draws it here.
pub const WEAPON_PLATE_Y: f32 = 70.0;
const WEAPON_PLATE_W: f32 = 332.0;
const WEAPON_PLATE_H: f32 = 142.0;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Button {
    Fire,
    Dig,
    Jump,
    Rope,
    Weapon,
}

/// A round button: centre and radius, in virtual pixels.
#[derive(Clone, Copy)]
pub struct Round {
    pub button: Button,
    pub x: f32,
    pub y: f32,
    pub r: f32,
}

/// The round buttons for a screen `width` virtual pixels wide, anchored to the
/// right edge and laid out in an arc around FIRE, where the right thumb rests.
pub fn buttons(width: f32) -> [Round; 4] {
    let at = |button, dx: f32, y, r| Round { button, x: width - dx, y, r };
    [
        at(Button::Fire, 135.0, 520.0, 95.0),
        at(Button::Jump, 135.0, 330.0, 62.0),
        at(Button::Rope, 300.0, 420.0, 56.0),
        at(Button::Dig, 320.0, 600.0, 66.0),
    ]
}

/// The weapon plate as a tap target: (x, y, w, h), a little padded.
fn weapon_rect(width: f32) -> (f32, f32, f32, f32) {
    let x = width - MARGIN - WEAPON_PLATE_W;
    // Starts at the top of the slot rail, which sits just above the plate.
    (x - 8.0, MARGIN, WEAPON_PLATE_W + 16.0, WEAPON_PLATE_Y + WEAPON_PLATE_H + 8.0 - MARGIN)
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Down,
    Move,
    Up,
}

/// What one touch event asks the game to do right now. Held state is read from
/// the controls; these are the things that happen once.
#[derive(Default, Debug, PartialEq)]
pub struct Reaction {
    /// Camera turn, in the mouse pixels `Camera::look` takes.
    pub look: (f32, f32),
    pub next_weapon: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Stick,
    Look,
    Button(Button),
}

struct Finger {
    id: u64,
    role: Role,
    /// Where it landed (the stick's centre) and where it is now, virtual pixels.
    start: (f32, f32),
    at: (f32, f32),
}

#[derive(Default)]
pub struct TouchControls {
    /// Set by the first touch. A laptop with a touch screen keeps its mouse and
    /// keyboard until someone actually touches it.
    pub active: bool,
    /// Screen width in virtual pixels, from the last event (for drawing).
    pub width: f32,
    fingers: Vec<Finger>,
}

impl TouchControls {
    /// Feed one touch event. `pos` is in physical pixels, `screen` the window's
    /// size in physical pixels.
    pub fn event(&mut self, id: u64, phase: Phase, pos: (f32, f32), screen: (u32, u32)) -> Reaction {
        let unit = 720.0 / screen.1.max(1) as f32;
        let p = (pos.0 * unit, pos.1 * unit);
        self.width = screen.0 as f32 * unit;
        self.active = true;
        let mut reaction = Reaction::default();
        match phase {
            Phase::Down => {
                // A finger the OS never ended (a lost `Up`) must not stay held.
                self.fingers.retain(|f| f.id != id);
                if let Some(role) = self.role_at(p) {
                    reaction.next_weapon = role == Role::Button(Button::Weapon);
                    if !reaction.next_weapon {
                        self.fingers.push(Finger { id, role, start: p, at: p });
                    }
                }
            }
            Phase::Move => {
                if let Some(f) = self.fingers.iter_mut().find(|f| f.id == id) {
                    // Everything but the stick and the look drag is a button, and
                    // only the hold buttons turn the camera too.
                    if matches!(f.role, Role::Look | Role::Button(Button::Fire | Button::Dig)) {
                        reaction.look = ((p.0 - f.at.0) * LOOK_GAIN, (p.1 - f.at.1) * LOOK_GAIN);
                    }
                    f.at = p;
                }
            }
            Phase::Up => self.fingers.retain(|f| f.id != id),
        }
        reaction
    }

    /// What a finger landing at `p` becomes, or `None` if it should be ignored.
    fn role_at(&self, p: (f32, f32)) -> Option<Role> {
        let (wx, wy, ww, wh) = weapon_rect(self.width);
        if p.0 >= wx && p.0 <= wx + ww && p.1 >= wy && p.1 <= wy + wh {
            return Some(Role::Button(Button::Weapon));
        }
        for b in buttons(self.width) {
            if (p.0 - b.x).hypot(p.1 - b.y) <= b.r * HIT_SLACK {
                return Some(Role::Button(b.button));
            }
        }
        if p.0 >= self.width / 2.0 {
            Some(Role::Look)
        } else if !self.fingers.iter().any(|f| f.role == Role::Stick) {
            Some(Role::Stick)
        } else {
            None // a second finger on the left half has nothing to do
        }
    }

    pub fn held(&self, button: Button) -> bool {
        self.fingers.iter().any(|f| f.role == Role::Button(button))
    }

    /// Desired movement for `Input::move_dir`: x = right, y = forward (up the screen).
    pub fn move_dir(&self) -> Vec2 {
        let Some((start, at)) = self.stick() else { return Vec2::ZERO };
        let d = Vec2::new(at.0 - start.0, start.1 - at.1) / STICK_RADIUS;
        if d.length() < DEAD_ZONE {
            Vec2::ZERO
        } else {
            d.clamp_length_max(1.0)
        }
    }

    /// The stick's centre and the thumb's position, for drawing.
    pub fn stick(&self) -> Option<((f32, f32), (f32, f32))> {
        self.fingers.iter().find(|f| f.role == Role::Stick).map(|f| (f.start, f.at))
    }

    pub fn stick_radius() -> f32 {
        STICK_RADIUS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // A 1560x720 screen: one physical pixel is one virtual pixel.
    const SCREEN: (u32, u32) = (1560, 720);

    fn fire() -> (f32, f32) {
        let b = buttons(1560.0)[0];
        (b.x, b.y)
    }

    #[test]
    fn the_stick_floats_where_the_thumb_lands_and_clamps() {
        let mut t = TouchControls::default();
        t.event(1, Phase::Down, (300.0, 400.0), SCREEN);
        assert_eq!(t.move_dir(), Vec2::ZERO);
        // Up the screen is forward, to the right is right.
        t.event(1, Phase::Move, (300.0 + 45.0, 400.0 - 45.0), SCREEN);
        let d = t.move_dir();
        assert!((d.x - 0.5).abs() < 1e-4 && (d.y - 0.5).abs() < 1e-4);
        t.event(1, Phase::Move, (900.0, 400.0), SCREEN);
        assert!((t.move_dir().length() - 1.0).abs() < 1e-4);
        t.event(1, Phase::Up, (900.0, 400.0), SCREEN);
        assert_eq!(t.move_dir(), Vec2::ZERO);
    }

    #[test]
    fn jitter_inside_the_dead_zone_does_not_move() {
        let mut t = TouchControls::default();
        t.event(1, Phase::Down, (300.0, 400.0), SCREEN);
        t.event(1, Phase::Move, (305.0, 396.0), SCREEN);
        assert_eq!(t.move_dir(), Vec2::ZERO);
    }

    #[test]
    fn empty_space_on_the_right_looks_and_the_left_does_not() {
        let mut t = TouchControls::default();
        t.event(1, Phase::Down, (1000.0, 300.0), SCREEN);
        let r = t.event(1, Phase::Move, (1010.0, 295.0), SCREEN);
        assert_eq!(r.look, (10.0 * LOOK_GAIN, -5.0 * LOOK_GAIN));
        // The left thumb (the stick) never turns the camera.
        t.event(2, Phase::Down, (200.0, 300.0), SCREEN);
        let r = t.event(2, Phase::Move, (260.0, 300.0), SCREEN);
        assert_eq!(r.look, (0.0, 0.0));
    }

    #[test]
    fn a_finger_on_fire_holds_it_and_still_looks() {
        let mut t = TouchControls::default();
        let (x, y) = fire();
        t.event(1, Phase::Down, (x, y), SCREEN);
        assert!(t.held(Button::Fire));
        let r = t.event(1, Phase::Move, (x - 20.0, y), SCREEN);
        assert_eq!(r.look, (-20.0 * LOOK_GAIN, 0.0));
        assert!(t.held(Button::Fire), "dragging off the button must not release it");
        t.event(1, Phase::Up, (x, y), SCREEN);
        assert!(!t.held(Button::Fire));
    }

    #[test]
    fn jump_does_not_turn_the_camera() {
        let mut t = TouchControls::default();
        let b = buttons(1560.0)[1];
        t.event(1, Phase::Down, (b.x, b.y), SCREEN);
        assert!(t.held(Button::Jump));
        let r = t.event(1, Phase::Move, (b.x + 30.0, b.y), SCREEN);
        assert_eq!(r.look, (0.0, 0.0));
    }

    #[test]
    fn fingers_are_independent() {
        let mut t = TouchControls::default();
        let (x, y) = fire();
        t.event(1, Phase::Down, (300.0, 400.0), SCREEN); // stick
        t.event(2, Phase::Down, (x, y), SCREEN); // fire
        t.event(1, Phase::Up, (300.0, 400.0), SCREEN);
        assert!(t.held(Button::Fire), "lifting the stick finger must not release fire");
        assert_eq!(t.move_dir(), Vec2::ZERO);
    }

    #[test]
    fn a_second_finger_on_the_left_is_ignored() {
        let mut t = TouchControls::default();
        t.event(1, Phase::Down, (300.0, 400.0), SCREEN);
        t.event(2, Phase::Down, (200.0, 200.0), SCREEN);
        t.event(2, Phase::Move, (400.0, 200.0), SCREEN);
        assert_eq!(t.stick().unwrap().0, (300.0, 400.0));
    }

    #[test]
    fn tapping_the_weapon_plate_asks_for_the_next_weapon_once() {
        let mut t = TouchControls::default();
        let (x, _, _, _) = weapon_rect(1560.0);
        let r = t.event(1, Phase::Down, (x + 50.0, WEAPON_PLATE_Y + 40.0), SCREEN);
        assert!(r.next_weapon);
        assert!(!t.held(Button::Weapon));
        let r = t.event(1, Phase::Move, (x + 60.0, WEAPON_PLATE_Y + 40.0), SCREEN);
        assert_eq!(r, Reaction::default());
    }

    #[test]
    fn positions_scale_with_the_screen_height() {
        // A 2x denser screen (1440 tall): the same virtual spot is twice the pixels.
        let mut t = TouchControls::default();
        let screen = (3120, 1440);
        let (x, y) = fire();
        t.event(1, Phase::Down, (x * 2.0, y * 2.0), screen);
        assert!(t.held(Button::Fire));
    }

    #[test]
    fn the_buttons_do_not_overlap_each_other_or_the_weapon_plate() {
        let b = buttons(1560.0);
        for (i, a) in b.iter().enumerate() {
            for c in &b[i + 1..] {
                assert!((a.x - c.x).hypot(a.y - c.y) > a.r + c.r, "{:?} overlaps {:?}", a.button, c.button);
            }
            let (wx, wy, ww, wh) = weapon_rect(1560.0);
            let near_x = a.x.clamp(wx, wx + ww);
            let near_y = a.y.clamp(wy, wy + wh);
            assert!((a.x - near_x).hypot(a.y - near_y) > a.r, "{:?} overlaps the weapon plate", a.button);
        }
    }
}
