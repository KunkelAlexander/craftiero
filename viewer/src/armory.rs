// The weapon picker: choose the weapons you carry into the match, like Liero's
// weapon menu. It is shown at the start of every match, and again each time
// the match is restarted, so a player can change their mind.
//
// Why a loadout at all: with fifteen weapons a fixed set would be a list of
// everything, and the choice is half the game. Picking five means a player can
// be the one with the Nova and the Railgun, and the others have to play around
// it. The weapons stand in columns by kind and you may take two of each kind at
// most, so nobody carries only guns. And for anyone who would rather not choose
// (and so not choose "the best"): RANDOM.
//
// Like `menu.rs` this is pure: no window, no GPU, no network. It only collects a
// choice; the viewer sends it to the host as a `ClientMsg::Loadout`, and the
// host decides whether it is allowed (`sim::weapons::valid_loadout`).

use sim::rng::Rng;
use sim::weapons::{can_add, class_of, random_loadout, valid_loadout, Class, ARSENAL, CLASSES, DEFAULT_LOADOUT, LOADOUT_SIZE};

use crate::hud::SLOT_KEYS;
use crate::menu::Rect;

const MARGIN: f32 = 28.0;
const GAP: f32 = 14.0;
const CARD_H: f32 = 64.0;
const CARD_GAP: f32 = 10.0;
/// Where the first row of cards starts (the class headings sit above it).
pub const CARDS_Y: f32 = 150.0;
const BUTTON_H: f32 = 64.0;

pub struct Armory {
    /// The picked weapon ids in the order they were picked: that order is the
    /// order of keys 1-5 in the match.
    picked: Vec<u8>,
    /// For RANDOM.
    rng: Rng,
}

impl Armory {
    /// Starts on the default set, so a player who is happy with it just
    /// presses GO. `seed` is for RANDOM.
    pub fn new(seed: u32) -> Armory {
        Armory { picked: DEFAULT_LOADOUT.to_vec(), rng: Rng::new(seed) }
    }

    pub fn picked(&self) -> &[u8] {
        &self.picked
    }

    pub fn ready(&self) -> bool {
        valid_loadout(&self.picked)
    }

    /// Pick a weapon, or put it back. A full set refuses a sixth, and so does
    /// a kind that already has its two (`can_add` is the host's rule too).
    pub fn toggle(&mut self, id: u8) {
        if let Some(at) = self.picked.iter().position(|&p| p == id) {
            self.picked.remove(at);
        } else if can_add(&self.picked, id) {
            self.picked.push(id);
        }
    }

    /// The weapon on key number `n` of the number row (0 is the key `1`).
    pub fn toggle_key(&mut self, n: usize) {
        if let Some(&id) = ARSENAL.get(n) {
            self.toggle(id);
        }
    }

    pub fn clear(&mut self) {
        self.picked.clear();
    }

    pub fn randomize(&mut self) {
        self.picked = random_loadout(&mut self.rng);
    }

    /// GO: the loadout, if there is a whole one.
    pub fn go(&self) -> Option<Vec<u8>> {
        self.ready().then(|| self.picked.clone())
    }

    /// A press (mouse down or finger down) at `at`, in the HUD's virtual units
    /// and for a screen `width` of them. Returns the loadout when GO was pressed.
    pub fn press(&mut self, at: (f32, f32), width: f32) -> Option<Vec<u8>> {
        for card in cards(width) {
            if card.rect.contains(at) {
                self.toggle(card.id);
                return None;
            }
        }
        let [clear, random, go] = footer(width);
        if clear.contains(at) {
            self.clear();
        } else if random.contains(at) {
            self.randomize();
        } else if go.contains(at) {
            return self.go();
        }
        None
    }
}

/// One card on the screen: which weapon, and where.
pub struct Card {
    pub id: u8,
    /// Its index in the arsenal: the number-row key that picks it.
    pub key: usize,
    pub rect: Rect,
}

/// The cards, one column per kind of weapon (the column's heading is
/// `column_x`), five rows at most.
pub fn cards(width: f32) -> Vec<Card> {
    let w = column_width(width);
    let mut out = Vec::new();
    for (col, class) in CLASSES.iter().enumerate() {
        let members = ARSENAL.iter().enumerate().filter(|&(_, &id)| class_of(id) == *class);
        for (row, (key, &id)) in members.enumerate() {
            out.push(Card {
                id,
                key,
                rect: Rect { x: column_x(width, col), y: CARDS_Y + row as f32 * (CARD_H + CARD_GAP), w, h: CARD_H },
            });
        }
    }
    out
}

fn column_width(width: f32) -> f32 {
    let n = CLASSES.len() as f32;
    ((width - 2.0 * MARGIN - (n - 1.0) * GAP) / n).min(280.0)
}

/// Left edge of column `col`, the whole row of columns centred.
pub fn column_x(width: f32, col: usize) -> f32 {
    let n = CLASSES.len() as f32;
    let w = column_width(width);
    (width - (n * w + (n - 1.0) * GAP)) / 2.0 + col as f32 * (w + GAP)
}

/// The kind each column holds, and how many of it `picked` has.
pub fn columns(picked: &[u8]) -> Vec<(Class, usize)> {
    CLASSES.iter().map(|&c| (c, picked.iter().filter(|&&id| class_of(id) == c).count())).collect()
}

/// CLEAR, RANDOM and GO, side by side under the cards.
pub fn footer(width: f32) -> [Rect; 3] {
    let usable = width - 2.0 * MARGIN - 2.0 * GAP;
    let w = [(0.22, 160.0), (0.30, 220.0), (0.48, 300.0)].map(|(share, max): (f32, f32)| (usable * share).min(max));
    let left = (width - w.iter().sum::<f32>() - 2.0 * GAP) / 2.0;
    let y = 720.0 - MARGIN - BUTTON_H - 30.0;
    let mut x = left;
    w.map(|w| {
        let r = Rect { x, y, w, h: BUTTON_H };
        x += w + GAP;
        r
    })
}

/// What GO says: the way to continue, or how many weapons are still missing.
pub fn go_label(picked: &[u8]) -> String {
    if valid_loadout(picked) {
        "GO".into()
    } else {
        format!("PICK {} MORE", LOADOUT_SIZE.saturating_sub(picked.len()).max(1))
    }
}

/// The key to press for the weapon at arsenal index `key`, as a one-character string.
pub fn key_label(key: usize) -> &'static str {
    &SLOT_KEYS[key..key + 1]
}

#[cfg(test)]
mod tests {
    use super::*;
    use sim::weapons::{BLASTER, BOMB, BUILD, CLUSTER, LAVA, NOVA, RAILGUN, ROCKET, TELEPORT};

    const WIDE: f32 = 1280.0;

    fn centre(r: Rect) -> (f32, f32) {
        (r.x + r.w / 2.0, r.y + r.h / 2.0)
    }

    fn tap_weapon(armory: &mut Armory, id: u8, width: f32) {
        let card = cards(width).into_iter().find(|c| c.id == id).unwrap();
        armory.press(centre(card.rect), width);
    }

    #[test]
    fn it_starts_on_the_default_set_and_go_hands_it_over() {
        let mut armory = Armory::new(1);
        assert_eq!(armory.picked(), DEFAULT_LOADOUT);
        assert!(armory.ready());
        assert_eq!(armory.press(centre(footer(WIDE)[2]), WIDE), Some(DEFAULT_LOADOUT.to_vec()));
    }

    #[test]
    fn taking_a_weapon_again_puts_it_back_and_the_order_is_the_pick_order() {
        let mut armory = Armory::new(1);
        armory.clear();
        for id in [NOVA, BLASTER, TELEPORT] {
            tap_weapon(&mut armory, id, WIDE);
        }
        assert_eq!(armory.picked(), [NOVA, BLASTER, TELEPORT]);
        tap_weapon(&mut armory, BLASTER, WIDE);
        assert_eq!(armory.picked(), [NOVA, TELEPORT]);
        assert_eq!(armory.go(), None, "two of five is not a loadout");
        assert_eq!(go_label(armory.picked()), "PICK 3 MORE");
    }

    #[test]
    fn a_full_set_and_a_full_class_refuse_more() {
        let mut armory = Armory::new(1);
        armory.toggle(NOVA); // the default set is already five
        assert_eq!(armory.picked(), DEFAULT_LOADOUT, "a sixth is refused");
        armory.clear();
        for id in [ROCKET, BOMB, CLUSTER, NOVA] {
            armory.toggle(id);
        }
        assert_eq!(armory.picked(), [ROCKET, BOMB], "two explosives at most");
        armory.toggle(LAVA);
        armory.toggle(BUILD);
        armory.toggle(RAILGUN);
        assert_eq!(armory.picked().len(), 5);
        assert!(armory.ready());
    }

    #[test]
    fn random_gives_a_whole_valid_loadout() {
        let mut armory = Armory::new(9);
        armory.clear();
        armory.press(centre(footer(WIDE)[1]), WIDE);
        assert!(armory.ready());
        assert_eq!(armory.press(centre(footer(WIDE)[0]), WIDE), None);
        assert!(armory.picked().is_empty(), "CLEAR empties it");
    }

    #[test]
    fn number_keys_pick_by_arsenal_position() {
        let mut armory = Armory::new(1);
        armory.clear();
        armory.toggle_key(0);
        armory.toggle_key(1);
        armory.toggle_key(99); // no such key: nothing
        assert_eq!(armory.picked(), [ARSENAL[0], ARSENAL[1]]);
    }

    #[test]
    fn every_weapon_has_a_card_a_key_and_the_cards_fit_every_screen() {
        for width in [640.0, 820.0, 960.0, 1280.0, 1700.0] {
            let all = cards(width);
            assert_eq!(all.len(), ARSENAL.len());
            for (i, a) in all.iter().enumerate() {
                let r = a.rect;
                assert!(r.x >= 0.0 && r.x + r.w <= width, "card {} off screen at {width}", a.id);
                assert!(!key_label(a.key).is_empty());
                for b in &all[i + 1..] {
                    let s = b.rect;
                    let apart = r.x + r.w <= s.x || s.x + s.w <= r.x || r.y + r.h <= s.y || s.y + s.h <= r.y;
                    assert!(apart, "cards {} and {} overlap at {width}", a.id, b.id);
                }
            }
            let bottom = all.iter().map(|c| c.rect.y + c.rect.h).fold(0.0, f32::max);
            let [clear, random, go] = footer(width);
            assert!(bottom + 20.0 < clear.y, "the footer is clear of the cards at {width}");
            assert!(clear.x >= 0.0 && go.x + go.w <= width);
            assert!(clear.x + clear.w <= random.x && random.x + random.w <= go.x);
        }
    }

    #[test]
    fn the_columns_count_each_kind() {
        let counts = columns(&DEFAULT_LOADOUT);
        assert_eq!(counts.iter().map(|c| c.1).sum::<usize>(), LOADOUT_SIZE);
        assert!(counts.iter().all(|c| c.1 <= sim::weapons::MAX_PER_CLASS));
    }
}
