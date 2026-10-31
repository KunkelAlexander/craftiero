// The main menu: what is on it, where, and what pressing it does.
//
// Like `touch.rs` this is pure: no window, no GPU, no clock. The viewer feeds
// it presses (mouse, finger or key) and gets back an `Action` to carry out; the
// HUD asks it for the list of buttons and paints them. One list serves both
// jobs, so a button can never be drawn in one place and hit-tested in another.
//
// Layout is in the HUD's "virtual pixels": the screen is 720 units tall and as
// wide as the window needs (see hud.rs). It follows the 2D game's phone menu: a
// title and three big buttons in a row along the bottom, and, for joining, a
// keypad of the room-code letters (nobody types on a phone's real keyboard
// in a game, and the code alphabet has no look-alikes to mistype).
//
//   PLAY   HOST   JOIN          offline / open a room / enter a friend's code
//
// PLAY, HOST and SERVER ROOM first go to a setup page where the player picks
// how many humans and NPCs the match has; JOIN goes to the code page.
//
// Pattern: Immediate Mode UI, again. There are no widget objects: `buttons`
// rebuilds the list from the state every time it is asked.

use protocol::relay::{CODE_ALPHABET, CODE_LEN};
use sim::map::MAX_PLAYERS;

/// Margin from the screen edge, like the HUD's.
const MARGIN: f32 = 28.0;
const GAP: f32 = 14.0;

#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub fn contains(&self, (px, py): (f32, f32)) -> bool {
        px >= self.x && px < self.x + self.w && py >= self.y && py < self.y + self.h
    }
}

/// What a button does when pressed.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Press {
    Play,
    Host,
    /// A match the server runs.
    ServerRoom,
    /// Go to the code-entry page.
    OpenCode,
    Letter(char),
    /// The setup page's steppers.
    /// Choose the map with this index.
    Map(u8),
    PlayersUp,
    PlayersDown,
    /// Start the match the setup page describes.
    Start,
    Delete,
    /// Back to the title page.
    Back,
    Submit,
}

pub struct Button {
    pub rect: Rect,
    pub label: String,
    pub press: Press,
    pub enabled: bool,
    /// The button the player most likely wants: drawn in the accent colour.
    pub primary: bool,
}

/// What the setup page decides: how many players the match has (2 to 4) and
/// which map it is on. Who plays them is the match's business: offline it is
/// you and NPCs, in a room every seat is for a friend and one nobody takes
/// stays an NPC (see `main.rs`).
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Setup {
    pub players: u8,
    /// Index into `sim::map::MAPS`.
    pub map: u8,
}

impl Setup {
    fn more_players(&mut self) {
        self.players = (self.players + 1).min(MAX_PLAYERS as u8);
    }

    /// Never fewer than two: one worm alone has nobody to fight.
    fn fewer_players(&mut self) {
        self.players = (self.players - 1).max(2);
    }

    fn map_before(self) -> u8 {
        (self.map + sim::map::MAPS.len() as u8 - 1) % sim::map::MAPS.len() as u8
    }

    fn map_after(self) -> u8 {
        (self.map + 1) % sim::map::MAPS.len() as u8
    }
}

/// What the viewer must do after a press.
#[derive(Clone, PartialEq, Debug)]
pub enum Action {
    /// Start an offline match: you and the other players as NPCs.
    Play(Setup),
    /// Start a match and open a room on the relay for friends.
    Host(Setup),
    /// Ask the server to run a match (friends join it by code, like any room).
    ServerRoom(Setup),
    /// Join this room.
    Join(String),
}

/// What the setup page is setting up.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Mode {
    Play,
    Host,
    ServerRoom,
}

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Page {
    Title,
    Setup(Mode),
    Code,
}

/// A key the menu understands. The viewer translates winit's keys into these,
/// so this file needs no windowing types.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Key {
    Char(char),
    Backspace,
    Enter,
    Escape,
    /// The arrow keys change the map on the setup page.
    Left,
    Right,
}

pub struct Menu {
    page: Page,
    code: String,
    setup: Setup,
}

impl Default for Menu {
    fn default() -> Menu {
        Menu { page: Page::Title, code: String::new(), setup: Setup { players: 4, map: 0 } }
    }
}

impl Menu {
    pub fn page(&self) -> Page {
        self.page
    }

    /// The letters typed so far.
    pub fn code(&self) -> &str {
        &self.code
    }

    /// What the setup page currently says.
    pub fn setup(&self) -> Setup {
        self.setup
    }

    /// Every button of the current page for a screen `width` units wide.
    pub fn buttons(&self, width: f32) -> Vec<Button> {
        match self.page {
            Page::Title => title_buttons(width),
            Page::Setup(_) => setup_buttons(width, self.setup),
            Page::Code => code_buttons(width, self.code.len() == CODE_LEN),
        }
    }

    /// A press (mouse down or finger down) at `at`, in virtual units.
    pub fn press(&mut self, at: (f32, f32), width: f32) -> Option<Action> {
        if matches!(self.page, Page::Setup(_)) {
            if let Some(i) = map_cards(width).iter().position(|r| r.contains(at)) {
                return self.apply(Press::Map(i as u8));
            }
        }
        let hit = self.buttons(width).into_iter().find(|b| b.enabled && b.rect.contains(at))?;
        self.apply(hit.press)
    }

    /// A key press. `None` means nothing to do (or the viewer's business:
    /// Escape on the title page is "quit" there, not here).
    pub fn key(&mut self, key: Key) -> Option<Action> {
        match (self.page, key) {
            (Page::Title, Key::Char('p' | 'P') | Key::Enter) => self.apply(Press::Play),
            (Page::Title, Key::Char('h' | 'H')) => self.apply(Press::Host),
            (Page::Title, Key::Char('s' | 'S')) => self.apply(Press::ServerRoom),
            (Page::Title, Key::Char('j' | 'J')) => self.apply(Press::OpenCode),
            (Page::Setup(_), Key::Left) => self.apply(Press::Map(self.setup.map_before())),
            (Page::Setup(_), Key::Right) => self.apply(Press::Map(self.setup.map_after())),
            (Page::Setup(_), Key::Enter) => self.apply(Press::Start),
            (Page::Setup(_), Key::Escape) => self.apply(Press::Back),
            (Page::Code, Key::Char(c)) => self.apply(Press::Letter(c.to_ascii_uppercase())),
            (Page::Code, Key::Backspace) => self.apply(Press::Delete),
            (Page::Code, Key::Enter) => self.apply(Press::Submit),
            (Page::Code, Key::Escape) => self.apply(Press::Back),
            _ => None,
        }
    }

    /// Go to the setup page. The map stays what it was: a player who liked the
    /// Plains does not have to pick them again.
    fn open_setup(&mut self, mode: Mode) -> Option<Action> {
        self.page = Page::Setup(mode);
        self.setup.players = 4;
        None
    }

    fn stepper(&mut self, change: fn(&mut Setup)) -> Option<Action> {
        change(&mut self.setup);
        None
    }

    fn apply(&mut self, press: Press) -> Option<Action> {
        match press {
            Press::Play => self.open_setup(Mode::Play),
            Press::Host => self.open_setup(Mode::Host),
            Press::ServerRoom => self.open_setup(Mode::ServerRoom),
            Press::Map(i) => {
                self.setup.map = i;
                None
            }
            Press::PlayersUp => self.stepper(Setup::more_players),
            Press::PlayersDown => self.stepper(Setup::fewer_players),
            Press::Start => {
                let Page::Setup(mode) = self.page else { return None };
                self.page = Page::Title;
                let seats = self.setup;
                Some(match mode {
                    Mode::Play => Action::Play(seats),
                    Mode::Host => Action::Host(seats),
                    Mode::ServerRoom => Action::ServerRoom(seats),
                })
            }
            Press::OpenCode => {
                self.page = Page::Code;
                self.code.clear();
                None
            }
            Press::Letter(c) => {
                // Only characters a code can contain, and no more than it holds.
                if CODE_ALPHABET.contains(c) && self.code.len() < CODE_LEN {
                    self.code.push(c);
                }
                None
            }
            Press::Delete => {
                self.code.pop();
                None
            }
            Press::Back => {
                self.page = Page::Title;
                None
            }
            Press::Submit => {
                // A half-typed code is not worth bothering the relay with.
                (self.code.len() == CODE_LEN).then(|| {
                    self.page = Page::Title;
                    Action::Join(std::mem::take(&mut self.code))
                })
            }
        }
    }
}

/// Where the title goes: centred, in the upper part of the screen.
pub const TITLE_Y: f32 = 150.0;

const BUTTON_H: f32 = 72.0;

fn title_buttons(width: f32) -> Vec<Button> {
    // Three equal buttons in a row along the bottom, never wider than 300.
    let w = ((width - 2.0 * MARGIN - 2.0 * GAP) / 3.0).min(300.0);
    let total = 3.0 * w + 2.0 * GAP;
    let (x0, y) = ((width - total) / 2.0, 720.0 - MARGIN - BUTTON_H - 40.0);
    let mut buttons: Vec<Button> =
        [("PLAY", Press::Play, true), ("HOST", Press::Host, false), ("JOIN", Press::OpenCode, false)]
            .into_iter()
            .enumerate()
            .map(|(i, (label, press, primary))| Button {
                rect: Rect { x: x0 + i as f32 * (w + GAP), y, w, h: BUTTON_H },
                label: label.into(),
                press,
                enabled: true,
                primary,
            })
            .collect();
    // The server's own match gets a wide button above the row: HOST puts the
    // match on your computer, this one puts it on the server.
    buttons.push(Button {
        rect: Rect { x: x0, y: y - GAP - 56.0, w: total, h: 56.0 },
        label: "SERVER ROOM".into(),
        press: Press::ServerRoom,
        enabled: true,
        primary: false,
    });
    buttons
}

/// The one row of the setup page.
pub const PLAYERS_LABEL: &str = "PLAYERS";

const STEP: f32 = 72.0;

/// The box that shows a row's number.
pub fn setup_value(width: f32) -> Rect {
    let w = 110.0;
    Rect { x: (width - w) / 2.0, y: 330.0, w, h: STEP }
}

/// Where the label of a row goes (above its stepper).
pub fn setup_label_y() -> f32 {
    setup_value(0.0).y - 28.0
}

/// One card per map, side by side across the top of the setup page.
pub fn map_cards(width: f32) -> Vec<Rect> {
    let n = sim::map::MAPS.len() as f32;
    let w = ((width - 2.0 * MARGIN - (n - 1.0) * GAP) / n).min(210.0);
    let x0 = (width - (n * w + (n - 1.0) * GAP)) / 2.0;
    (0..sim::map::MAPS.len()).map(|i| Rect { x: x0 + i as f32 * (w + GAP), y: 84.0, w, h: 150.0 }).collect()
}

/// The little picture of the map on its card: a square, centred near the top.
pub fn map_preview(card: Rect) -> Rect {
    let side = (card.w - 24.0).min(96.0);
    Rect { x: card.x + (card.w - side) / 2.0, y: card.y + 12.0, w: side, h: side }
}

fn setup_buttons(width: f32, setup: Setup) -> Vec<Button> {
    let mut buttons = Vec::new();
    let value = setup_value(width);
    for (label, press, enabled, x) in [
        ("-", Press::PlayersDown, setup.players > 2, value.x - GAP - STEP),
        ("+", Press::PlayersUp, setup.players < MAX_PLAYERS as u8, value.x + value.w + GAP),
    ] {
        buttons.push(Button { rect: Rect { x, y: value.y, w: STEP, h: STEP }, label: label.into(), press, enabled, primary: false });
    }
    // BACK and START share the bottom row.
    let w = ((width - 2.0 * MARGIN - GAP) / 2.0).min(300.0);
    let x0 = (width - (2.0 * w + GAP)) / 2.0;
    let y = 720.0 - MARGIN - BUTTON_H - 40.0;
    for (i, (label, press, primary)) in [("BACK", Press::Back, false), ("START", Press::Start, true)].into_iter().enumerate() {
        buttons.push(Button {
            rect: Rect { x: x0 + i as f32 * (w + GAP), y, w, h: BUTTON_H },
            label: label.into(),
            press,
            enabled: true,
            primary,
        });
    }
    buttons
}

/// Columns of the code keypad: the alphabet is 32 letters, so 8 x 4.
const KEY_COLS: usize = 8;

/// Where the code slots (the four boxes showing what was typed) are.
pub fn code_slots(width: f32) -> Vec<Rect> {
    let (w, h) = (74.0, 92.0);
    let total = CODE_LEN as f32 * w + (CODE_LEN as f32 - 1.0) * GAP;
    (0..CODE_LEN).map(|i| Rect { x: (width - total) / 2.0 + i as f32 * (w + GAP), y: 84.0, w, h }).collect()
}

fn code_buttons(width: f32, complete: bool) -> Vec<Button> {
    let key = ((width - 2.0 * MARGIN - (KEY_COLS as f32 - 1.0) * GAP) / KEY_COLS as f32).min(56.0);
    let total = KEY_COLS as f32 * key + (KEY_COLS as f32 - 1.0) * GAP;
    let (x0, y0) = ((width - total) / 2.0, 216.0);
    let mut buttons: Vec<Button> = CODE_ALPHABET
        .chars()
        .enumerate()
        .map(|(i, c)| Button {
            rect: Rect {
                x: x0 + (i % KEY_COLS) as f32 * (key + GAP),
                y: y0 + (i / KEY_COLS) as f32 * (key + GAP),
                w: key,
                h: key,
            },
            label: c.to_string(),
            press: Press::Letter(c),
            enabled: true,
            primary: false,
        })
        .collect();

    // BACK, DEL and JOIN share the row under the keypad.
    let rows = CODE_ALPHABET.len().div_ceil(KEY_COLS) as f32;
    let y = y0 + rows * (key + GAP) + 8.0;
    let w = (total - 2.0 * GAP) / 3.0;
    for (i, (label, press, enabled, primary)) in
        [("BACK", Press::Back, true, false), ("DEL", Press::Delete, true, false), ("JOIN", Press::Submit, complete, true)]
            .into_iter()
            .enumerate()
    {
        buttons.push(Button {
            rect: Rect { x: x0 + i as f32 * (w + GAP), y, w, h: 60.0 },
            label: label.into(),
            press,
            enabled,
            primary,
        });
    }
    buttons
}

/// The "BACK TO THE MENU" button on a notice screen (joining, or a failure).
pub fn notice_button(width: f32) -> Rect {
    let w = 260.0;
    Rect { x: (width - w) / 2.0, y: 470.0, w, h: BUTTON_H }
}

/// The "NEW MATCH" button under the result of a finished match. Only drawn for
/// touch: a keyboard has R.
pub fn rematch_button(width: f32) -> Rect {
    let w = 300.0;
    Rect { x: (width - w) / 2.0, y: 470.0, w, h: BUTTON_H }
}

/// The small MENU button in a corner while playing, so a touch screen (which
/// has no Esc) can leave the match.
pub fn menu_button(width: f32) -> Rect {
    Rect { x: width - MARGIN - 130.0, y: MARGIN, w: 130.0, h: 44.0 }
}

/// The small RESTART button under it: any player may start the match over for
/// everybody (the keyboard has R).
pub fn restart_button(width: f32) -> Rect {
    let menu = menu_button(width);
    Rect { y: menu.y + menu.h + GAP, ..menu }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDE: f32 = 1280.0;

    fn centre(r: Rect) -> (f32, f32) {
        (r.x + r.w / 2.0, r.y + r.h / 2.0)
    }

    fn button(menu: &Menu, width: f32, label: &str) -> Rect {
        menu.buttons(width).into_iter().find(|b| b.label == label).unwrap_or_else(|| panic!("no {label} button")).rect
    }

    fn tap(menu: &mut Menu, label: &str) -> Option<Action> {
        let at = centre(button(menu, WIDE, label));
        menu.press(at, WIDE)
    }

    #[test]
    fn the_title_has_play_host_and_join() {
        let menu = Menu::default();
        let labels: Vec<String> = menu.buttons(WIDE).into_iter().map(|b| b.label).collect();
        assert_eq!(labels, ["PLAY", "HOST", "JOIN", "SERVER ROOM"]);
    }

    #[test]
    fn play_host_and_server_room_go_through_the_setup_page() {
        let mut menu = Menu::default();
        assert_eq!(tap(&mut menu, "PLAY"), None);
        assert_eq!(menu.page(), Page::Setup(Mode::Play));
        assert_eq!(tap(&mut menu, "START"), Some(Action::Play(Setup { players: 4, map: 0 })));
        assert_eq!(menu.page(), Page::Title, "back on the title once the match starts");

        tap(&mut menu, "HOST");
        assert_eq!(tap(&mut menu, "START"), Some(Action::Host(Setup { players: 4, map: 0 })));
        tap(&mut menu, "SERVER ROOM");
        assert_eq!(tap(&mut menu, "START"), Some(Action::ServerRoom(Setup { players: 4, map: 0 })));
    }

    #[test]
    fn the_setup_page_has_one_stepper_for_two_to_four_players() {
        let mut menu = Menu::default();
        tap(&mut menu, "PLAY");
        let labels: Vec<String> = menu.buttons(WIDE).into_iter().map(|b| b.label).collect();
        assert_eq!(labels, ["-", "+", "BACK", "START"]);
        let press = |menu: &mut Menu, label: &str| {
            let b = menu.buttons(WIDE).into_iter().find(|b| b.label == label).unwrap();
            let enabled = b.enabled;
            menu.press(centre(b.rect), WIDE);
            enabled
        };
        assert!(!press(&mut menu, "+"), "four is the most");
        assert_eq!(menu.setup().players, 4);
        for _ in 0..5 {
            press(&mut menu, "-");
        }
        assert_eq!(menu.setup().players, 2, "two is the fewest");
        assert!(!press(&mut menu, "-"), "and the button says so");
        press(&mut menu, "+");
        assert_eq!(menu.key(Key::Enter), Some(Action::Play(Setup { players: 3, map: 0 })));
    }

    #[test]
    fn the_setup_page_picks_a_map_by_card_or_arrow_and_remembers_it() {
        let mut menu = Menu::default();
        tap(&mut menu, "PLAY");
        assert_eq!(menu.setup().map, 0);
        let second = map_cards(WIDE)[1];
        menu.press(centre(second), WIDE);
        assert_eq!(menu.setup().map, 1);
        menu.key(Key::Right);
        menu.key(Key::Right);
        assert_eq!(menu.setup().map, 0, "wraps round");
        menu.key(Key::Left);
        assert_eq!(menu.setup().map, sim::map::MAPS.len() as u8 - 1);
        let last = menu.setup().map;
        assert_eq!(menu.key(Key::Enter), Some(Action::Play(Setup { players: 4, map: last })));
        tap(&mut menu, "HOST");
        assert_eq!(menu.setup().map, last, "the next setup starts on the same map");
        assert_eq!(menu.key(Key::Enter), Some(Action::Host(Setup { players: 4, map: last })));
    }

    #[test]
    fn the_map_cards_fit_the_screen_and_clear_the_steppers() {
        for width in [640.0, 820.0, 1280.0, 1700.0] {
            let cards = map_cards(width);
            for (i, c) in cards.iter().enumerate() {
                assert!(c.x >= 0.0 && c.x + c.w <= width, "card {i} on screen at {width}");
                let p = map_preview(*c);
                assert!(p.x >= c.x && p.x + p.w <= c.x + c.w && p.y + p.h <= c.y + c.h, "the picture is inside its card");
                for d in &cards[i + 1..] {
                    assert!(c.x + c.w <= d.x, "cards do not overlap");
                }
            }
            let bottom = cards[0].y + cards[0].h;
            assert!(bottom + 30.0 < setup_label_y(), "the blurb fits between the cards and the first row");
        }
    }

    #[test]
    fn back_and_escape_leave_the_setup_page() {
        let mut menu = Menu::default();
        tap(&mut menu, "PLAY");
        tap(&mut menu, "BACK");
        assert_eq!(menu.page(), Page::Title);
        menu.key(Key::Char('h'));
        assert_eq!(menu.page(), Page::Setup(Mode::Host));
        assert_eq!(menu.key(Key::Escape), None);
        assert_eq!(menu.page(), Page::Title);
    }

    #[test]
    fn a_press_between_buttons_does_nothing() {
        let mut menu = Menu::default();
        assert_eq!(menu.press((5.0, 5.0), WIDE), None);
        assert_eq!(menu.page(), Page::Title);
    }

    #[test]
    fn joining_by_tapping_the_keypad() {
        let mut menu = Menu::default();
        assert_eq!(tap(&mut menu, "JOIN"), None);
        assert_eq!(menu.page(), Page::Code);
        for c in ['K', '7', 'Q', 'P'] {
            tap(&mut menu, &c.to_string());
        }
        assert_eq!(menu.code(), "K7QP");
        assert_eq!(tap(&mut menu, "JOIN"), Some(Action::Join("K7QP".into())));
        assert_eq!(menu.page(), Page::Title, "back on the title once the join starts");
        assert_eq!(menu.code(), "");
    }

    #[test]
    fn join_waits_for_a_whole_code() {
        let mut menu = Menu::default();
        tap(&mut menu, "JOIN");
        tap(&mut menu, "A");
        tap(&mut menu, "B");
        let join = menu.buttons(WIDE).into_iter().find(|b| b.label == "JOIN").unwrap();
        assert!(!join.enabled);
        assert_eq!(menu.press(centre(join.rect), WIDE), None);
        assert_eq!(menu.key(Key::Enter), None, "Enter does not skip the rule either");
        assert_eq!(menu.page(), Page::Code);
    }

    #[test]
    fn typing_accepts_only_what_a_code_can_hold() {
        let mut menu = Menu::default();
        menu.key(Key::Char('j'));
        for c in "k0o1i7zzzz".chars() {
            // 0, O, 1 and I are not in the alphabet; the code stops at four.
            menu.key(Key::Char(c));
        }
        assert_eq!(menu.code(), "K7ZZ");
        menu.key(Key::Backspace);
        assert_eq!(menu.code(), "K7Z");
        assert_eq!(menu.key(Key::Escape), None);
        assert_eq!(menu.page(), Page::Title);
    }

    #[test]
    fn keyboard_shortcuts_on_the_title() {
        let mut menu = Menu::default();
        assert_eq!(menu.key(Key::Char('p')), None);
        assert_eq!(menu.key(Key::Enter), Some(Action::Play(Setup { players: 4, map: 0 })));
        menu.key(Key::Char('H'));
        assert_eq!(menu.key(Key::Enter), Some(Action::Host(Setup { players: 4, map: 0 })));
        menu.key(Key::Char('s'));
        assert_eq!(menu.key(Key::Enter), Some(Action::ServerRoom(Setup { players: 4, map: 0 })));
        assert_eq!(menu.key(Key::Char('j')), None);
        assert_eq!(menu.page(), Page::Code);
        for c in "abcd".chars() {
            menu.key(Key::Char(c));
        }
        assert_eq!(menu.key(Key::Enter), Some(Action::Join("ABCD".into())));
    }

    #[test]
    fn opening_the_code_page_starts_with_an_empty_code() {
        let mut menu = Menu::default();
        tap(&mut menu, "JOIN");
        tap(&mut menu, "A");
        tap(&mut menu, "BACK");
        tap(&mut menu, "JOIN");
        assert_eq!(menu.code(), "");
    }

    #[test]
    fn delete_removes_the_last_letter() {
        let mut menu = Menu::default();
        tap(&mut menu, "JOIN");
        tap(&mut menu, "A");
        tap(&mut menu, "B");
        tap(&mut menu, "DEL");
        assert_eq!(menu.code(), "A");
        tap(&mut menu, "DEL");
        tap(&mut menu, "DEL"); // nothing left: harmless
        assert_eq!(menu.code(), "");
    }

    #[test]
    fn every_code_letter_has_a_key() {
        let mut menu = Menu::default();
        tap(&mut menu, "JOIN");
        let labels: Vec<String> = menu.buttons(WIDE).into_iter().map(|b| b.label).collect();
        for c in CODE_ALPHABET.chars() {
            assert!(labels.contains(&c.to_string()), "no key for {c}");
        }
    }

    /// Whatever the window's shape, no button may leave the screen or overlap
    /// another: a phone in landscape is the narrowest we care about.
    #[test]
    fn buttons_fit_on_every_screen_shape_without_overlapping() {
        for width in [640.0, 820.0, 960.0, 1280.0, 1700.0] {
            for page in [
                Page::Title,
                Page::Code,
                Page::Setup(Mode::Play),
                Page::Setup(Mode::Host),
                Page::Setup(Mode::ServerRoom),
            ] {
                let menu = Menu { page, ..Menu::default() };
                let buttons = menu.buttons(width);
                for (i, a) in buttons.iter().enumerate() {
                    let r = a.rect;
                    assert!(r.x >= 0.0 && r.x + r.w <= width && r.y >= 0.0 && r.y + r.h <= 720.0, "{} is off screen at {width}", a.label);
                    for b in &buttons[i + 1..] {
                        let s = b.rect;
                        let apart = r.x + r.w <= s.x || s.x + s.w <= r.x || r.y + r.h <= s.y || s.y + s.h <= r.y;
                        assert!(apart, "{} overlaps {} at width {width}", a.label, b.label);
                    }
                }
            }
        }
    }

    #[test]
    fn the_code_slots_do_not_run_into_the_keypad() {
        let slots_bottom = code_slots(WIDE).iter().map(|r| r.y + r.h).fold(0.0, f32::max);
        let keys_top = Menu { page: Page::Code, ..Menu::default() }
            .buttons(WIDE)
            .iter()
            .map(|b| b.rect.y)
            .fold(f32::MAX, f32::min);
        assert!(slots_bottom < keys_top);
    }

    #[test]
    fn the_corner_button_stays_out_of_the_way_of_the_notice_button() {
        assert!(!menu_button(WIDE).contains(centre(notice_button(WIDE))));
    }

    #[test]
    fn the_restart_button_sits_under_the_menu_button_on_screen() {
        for width in [WIDE, 960.0] {
            let (menu, restart) = (menu_button(width), restart_button(width));
            assert!(restart.y >= menu.y + menu.h, "no overlap");
            assert!(restart.x >= 0.0 && restart.x + restart.w <= width);
        }
    }

    #[test]
    fn the_rematch_button_is_in_reach_and_clear_of_the_corner_button() {
        for width in [WIDE, 960.0, 1480.0] {
            let b = rematch_button(width);
            assert!(!menu_button(width).contains(centre(b)));
            assert!(b.x >= 0.0 && b.x + b.w <= width, "on screen at {width}");
            assert!(b.y + b.h <= 720.0);
        }
    }
}
