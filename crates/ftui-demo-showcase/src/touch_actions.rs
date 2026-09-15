//! Turns a screen's keybinding help into keys a touch host can offer as buttons.
//!
//! A phone has no keyboard until one is raised over the terminal, so every
//! screen whose features live behind letter keys (nearly all of them)
//! is unreachable by touch alone. Every screen already publishes what its
//! keys do through [`Screen::keybindings`], written for a human to read:
//! `"Ctrl+D/U"`, `"↑ / ↓ or j / k"`, `"Space/→"`. This module parses those
//! labels back into the individual presses behind them, so the web host can
//! render one button per action instead of the demo hard-coding a control
//! strip per screen.
//!
//! Entries that describe a pointer rather than a key - `"Click"`, `"Wheel"`,
//! `"Drag divider"` - resolve to nothing and are dropped: the canvas already
//! takes taps and drags directly.
//!
//! [`Screen::keybindings`]: crate::screens::Screen::keybindings

use crate::chrome::HelpEntry;

/// Modifier bits, matching `ftui_core::event::Modifiers` - which is also the
/// encoding `ftui-web`'s input parser expects from a host.
pub const MOD_SHIFT: u8 = 0b0001;
/// Alt/Option.
pub const MOD_ALT: u8 = 0b0010;
/// Control.
pub const MOD_CTRL: u8 = 0b0100;
/// Command/Super/Meta.
pub const MOD_SUPER: u8 = 0b1000;

/// One tappable key press derived from a keybinding label.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TouchAction {
    /// Button face, as the screen wrote it: `"Ctrl+D"`, `"↑"`, `"Space"`.
    pub label: String,
    /// What the key does, in the screen's own words.
    pub action: String,
    /// Key name in the host input encoding (`ftui_web::input_parser`).
    pub key: String,
    /// Modifier bits to send alongside the key.
    pub mods: u8,
}

/// Parse help entries into the distinct key presses a touch host can offer.
///
/// Order follows the screen's own help order, and a key that appears in more
/// than one entry is listed once, under the first entry that named it.
pub fn touch_actions(entries: &[HelpEntry]) -> Vec<TouchAction> {
    let mut out: Vec<TouchAction> = Vec::new();
    for entry in entries {
        for press in parse_label(entry.key) {
            if out
                .iter()
                .any(|existing| existing.key == press.key && existing.mods == press.mods)
            {
                continue;
            }
            out.push(TouchAction {
                label: press.label,
                action: entry.action.to_string(),
                key: press.key,
                mods: press.mods,
            });
        }
    }
    out
}

/// A single resolved press, before it is paired with its description.
struct Press {
    label: String,
    key: String,
    mods: u8,
}

fn parse_label(label: &str) -> Vec<Press> {
    // "Macro: r/p/l +/-" names the mode the keys belong to, not a key.
    let body = label.split_once(": ").map_or(label, |(_, tail)| tail);
    let body = strip_parentheticals(body);
    let mut out = Vec::new();
    for clause in body.split([',', ';']) {
        for group in clause.split(" or ") {
            parse_group(group, &mut out);
        }
    }
    out
}

/// Drop `"(search)"` from `"Enter (search)"`: it qualifies the action, and is
/// never part of the chord.
fn strip_parentheticals(label: &str) -> String {
    let mut out = String::with_capacity(label.len());
    let mut depth = 0usize;
    for ch in label.chars() {
        match ch {
            '(' => depth += 1,
            ')' => depth = depth.saturating_sub(1),
            _ if depth == 0 => out.push(ch),
            _ => {}
        }
    }
    out
}

fn parse_group(group: &str, out: &mut Vec<Press>) {
    let tokens: Vec<&str> = group.split_whitespace().collect();
    let has_siblings = tokens.len() > 1;
    for token in tokens {
        // Alternatives written with spaces - "+ / -" - leave the separator as
        // a token of its own. The search key is a lone slash, so only discard
        // a bare slash when the group held something else too.
        if has_siblings && token == "/" {
            continue;
        }
        parse_token(token, out);
    }
}

fn parse_token(token: &str, out: &mut Vec<Press>) {
    // A lone "/" is the key itself; anything longer splits on it.
    let parts: Vec<&str> = if token.chars().count() > 1 {
        token.split('/').filter(|part| !part.is_empty()).collect()
    } else {
        vec![token]
    };
    let inherited = parts.first().map_or(0, |first| split_modifiers(first).0);
    for (index, part) in parts.iter().enumerate() {
        let (mut mods, base) = split_modifiers(part);
        // "Ctrl+D/U" is two chords sharing a modifier, not a chord followed by
        // a bare letter. A part that spells its own modifier keeps it.
        if index > 0 && mods == 0 && !part.contains('+') {
            mods = inherited;
        }
        expand_base(base, mods, out);
    }
}

/// Modifier spellings that appear in showcase help text, longest first so
/// `"Control+"` is not read as `"C-"` would be.
const MODIFIER_PREFIXES: &[(&str, u8)] = &[
    ("Control+", MOD_CTRL),
    ("Ctrl+", MOD_CTRL),
    ("Shift+", MOD_SHIFT),
    ("Option+", MOD_ALT),
    ("Super+", MOD_SUPER),
    ("Meta+", MOD_SUPER),
    ("Cmd+", MOD_SUPER),
    ("Alt+", MOD_ALT),
    ("C-", MOD_CTRL),
    ("S-", MOD_SHIFT),
    ("M-", MOD_ALT),
];

fn split_modifiers(part: &str) -> (u8, &str) {
    let mut mods = 0u8;
    let mut rest = part;
    'strip: loop {
        for (prefix, bit) in MODIFIER_PREFIXES {
            if let Some(tail) = rest.strip_prefix(prefix) {
                // "Ctrl+" with nothing after it is not a chord; leave the
                // token alone rather than emitting a modifier with no key.
                if tail.is_empty() {
                    break 'strip;
                }
                mods |= bit;
                rest = tail;
                continue 'strip;
            }
        }
        break;
    }
    (mods, rest)
}

fn expand_base(base: &str, mods: u8, out: &mut Vec<Press>) {
    // "0-5" is a run of number keys, not a chord involving minus.
    if let Some((low, high)) = digit_range(base) {
        for digit in low..=high {
            push_press(&digit.to_string(), mods, out);
        }
        return;
    }
    // "WASD" and "↑↓←→" each name several keys in one breath.
    if let Some(expanded) = expand_multi_key(base) {
        for one in expanded {
            push_press(&one, mods, out);
        }
        return;
    }
    push_press(base, mods, out);
}

fn push_press(base: &str, mods: u8, out: &mut Vec<Press>) {
    let Some((face, key)) = resolve_key(base, mods) else {
        return;
    };
    out.push(Press {
        label: format!("{}{face}", modifier_prefix(mods)),
        key,
        mods,
    });
}

/// Resolve one bare key name into `(button face, key name to send)`.
///
/// Returns `None` for anything that is not a key - `"Click"`, `"Wheel"`,
/// `"rail"` - which is how pointer-only help entries fall out of the list.
fn resolve_key(base: &str, mods: u8) -> Option<(String, String)> {
    if let Some((face, key)) = named_key(base) {
        // Shift+Tab is its own key code in the host encoding, not Tab with a
        // modifier bit; `ftui-web` maps "BackTab" and nothing else.
        if key == "Tab" && mods & MOD_SHIFT != 0 {
            return Some((face.to_string(), "BackTab".to_string()));
        }
        return Some((face.to_string(), key.to_string()));
    }
    if is_function_key(base) {
        return Some((base.to_string(), base.to_string()));
    }
    let mut chars = base.chars();
    let first = chars.next()?;
    if chars.next().is_some() {
        // An unrecognised word: "Click", "Mouse", "divider", "rail".
        return None;
    }
    if first.is_whitespace() || first.is_control() {
        return None;
    }
    // Screens read the *character* a chord produces, not the shift bit:
    // `Shift+A` arrives as 'A', and `Ctrl+D` as 'd' because no shift was held.
    let key = if mods & MOD_SHIFT != 0 {
        first.to_uppercase().to_string()
    } else if mods != 0 {
        first.to_lowercase().to_string()
    } else {
        first.to_string()
    };
    Some((base.to_string(), key))
}

fn named_key(base: &str) -> Option<(&'static str, &'static str)> {
    Some(match base {
        "Enter" | "Return" => ("Enter", "Enter"),
        "Esc" | "Escape" => ("Esc", "Escape"),
        "Tab" => ("Tab", "Tab"),
        "BackTab" => ("Shift+Tab", "BackTab"),
        "Space" | "Spacebar" => ("Space", "Space"),
        "Backspace" => ("Backspace", "Backspace"),
        "Delete" | "Del" => ("Del", "Delete"),
        "Insert" | "Ins" => ("Ins", "Insert"),
        "Home" => ("Home", "Home"),
        "End" => ("End", "End"),
        "PgUp" | "PageUp" => ("PgUp", "PageUp"),
        // "Dn" only ever abbreviates the second half of "PgUp/Dn".
        "PgDn" | "PgDown" | "PageDown" | "Dn" => ("PgDn", "PageDown"),
        "Up" | "ArrowUp" | "↑" => ("↑", "ArrowUp"),
        "Down" | "ArrowDown" | "↓" => ("↓", "ArrowDown"),
        "Left" | "ArrowLeft" | "←" => ("←", "ArrowLeft"),
        "Right" | "ArrowRight" | "→" => ("→", "ArrowRight"),
        _ => return None,
    })
}

fn is_function_key(base: &str) -> bool {
    base.strip_prefix('F')
        .and_then(|tail| tail.parse::<u8>().ok())
        .is_some_and(|n| (1..=24).contains(&n))
}

fn digit_range(base: &str) -> Option<(u8, u8)> {
    let bytes = base.as_bytes();
    if bytes.len() != 3 || bytes[1] != b'-' {
        return None;
    }
    let low = (bytes[0] as char).to_digit(10)? as u8;
    let high = (bytes[2] as char).to_digit(10)? as u8;
    (low <= high).then_some((low, high))
}

fn expand_multi_key(base: &str) -> Option<Vec<String>> {
    // Movement clusters are written as one word. Send the lower-case letters:
    // the uppercase spelling is a reading convenience, and the screens that
    // take WASD read 'w', not 'W'.
    let expanded = match base {
        "Arrow" | "Arrows" => ["↑", "↓", "←", "→"].map(String::from).to_vec(),
        "WASD" | "wasd" => ["w", "a", "s", "d"].map(String::from).to_vec(),
        "HJKL" | "hjkl" => ["h", "j", "k", "l"].map(String::from).to_vec(),
        _ => {
            // "↑↓←→" - a run of arrow glyphs with nothing between them.
            if base.chars().count() > 1 && base.chars().all(is_arrow_glyph) {
                base.chars().map(|ch| ch.to_string()).collect()
            } else {
                return None;
            }
        }
    };
    Some(expanded)
}

fn is_arrow_glyph(ch: char) -> bool {
    matches!(ch, '↑' | '↓' | '←' | '→')
}

fn modifier_prefix(mods: u8) -> String {
    let mut out = String::new();
    if mods & MOD_CTRL != 0 {
        out.push_str("Ctrl+");
    }
    if mods & MOD_ALT != 0 {
        out.push_str("Alt+");
    }
    if mods & MOD_SHIFT != 0 {
        out.push_str("Shift+");
    }
    if mods & MOD_SUPER != 0 {
        out.push_str("Cmd+");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(label: &'static str) -> Vec<(String, String, u8)> {
        touch_actions(&[HelpEntry {
            key: label,
            action: "whatever",
        }])
        .into_iter()
        .map(|a| (a.label, a.key, a.mods))
        .collect()
    }

    fn keys(label: &'static str) -> Vec<String> {
        parse(label).into_iter().map(|(_, key, _)| key).collect()
    }

    #[test]
    fn a_plain_letter_is_sent_as_written() {
        assert_eq!(parse("r"), vec![("r".into(), "r".into(), 0)]);
        // An uppercase letter is the character the screen matches on, so it
        // travels as-is rather than as shift plus its lower case.
        assert_eq!(parse("R"), vec![("R".into(), "R".into(), 0)]);
    }

    #[test]
    fn alternatives_split_on_slashes_and_on_the_word_or() {
        assert_eq!(keys("j/k"), ["j", "k"]);
        assert_eq!(keys("g / G"), ["g", "G"]);
        assert_eq!(keys("↑ / ↓ or j / k"), ["ArrowUp", "ArrowDown", "j", "k"]);
        assert_eq!(keys("←/→ or h/l"), ["ArrowLeft", "ArrowRight", "h", "l"]);
        assert_eq!(keys("Home/End or g/G"), ["Home", "End", "g", "G"]);
    }

    #[test]
    fn a_shared_modifier_carries_across_a_tight_slash() {
        // "Ctrl+D/U" is Ctrl+D and Ctrl+U, and the screens read the unshifted
        // character because no shift was held to make it.
        assert_eq!(
            parse("Ctrl+D/U"),
            vec![
                ("Ctrl+D".into(), "d".into(), MOD_CTRL),
                ("Ctrl+U".into(), "u".into(), MOD_CTRL),
            ]
        );
        assert_eq!(
            parse("Ctrl+Shift+Up/Down"),
            vec![
                (
                    "Ctrl+Shift+↑".into(),
                    "ArrowUp".into(),
                    MOD_CTRL | MOD_SHIFT
                ),
                (
                    "Ctrl+Shift+↓".into(),
                    "ArrowDown".into(),
                    MOD_CTRL | MOD_SHIFT
                ),
            ]
        );
    }

    #[test]
    fn a_part_that_spells_its_own_modifier_does_not_inherit_one() {
        assert_eq!(
            parse("Tab/Shift+Tab"),
            vec![
                ("Tab".into(), "Tab".into(), 0),
                ("Shift+Tab".into(), "BackTab".into(), MOD_SHIFT),
            ]
        );
        // Spaced alternatives are independent chords, so F3 stays unmodified.
        assert_eq!(
            parse("Ctrl+G / F3"),
            vec![
                ("Ctrl+G".into(), "g".into(), MOD_CTRL),
                ("F3".into(), "F3".into(), 0),
            ]
        );
    }

    #[test]
    fn shift_with_a_letter_sends_the_letter_that_shift_makes() {
        // The app matches (Char('A'), SHIFT) for the a11y overlay, so both the
        // upper case and the shift bit have to arrive.
        assert_eq!(parse("Shift+A"), vec![("Shift+A".into(), "A".into(), 1)]);
    }

    #[test]
    fn shift_with_a_named_key_keeps_the_modifier() {
        assert_eq!(
            parse("Shift+Enter"),
            vec![("Shift+Enter".into(), "Enter".into(), MOD_SHIFT)]
        );
        assert_eq!(
            parse("Shift+Left/Right"),
            vec![
                ("Shift+←".into(), "ArrowLeft".into(), MOD_SHIFT),
                ("Shift+→".into(), "ArrowRight".into(), MOD_SHIFT),
            ]
        );
    }

    #[test]
    fn shift_tab_becomes_the_back_tab_key_however_it_is_spelled() {
        assert_eq!(keys("S-Tab"), ["BackTab"]);
        assert_eq!(keys("Tab / S-Tab"), ["Tab", "BackTab"]);
        assert_eq!(
            keys("Ctrl+Left/Right, Tab/Shift+Tab"),
            ["ArrowLeft", "ArrowRight", "Tab", "BackTab"]
        );
    }

    #[test]
    fn punctuation_keys_survive_the_slash_splitting() {
        // The search key is a slash on its own and must not be split away.
        assert_eq!(keys("/"), ["/"]);
        assert_eq!(keys("[/]"), ["[", "]"]);
        assert_eq!(keys("] / ["), ["]", "["]);
        assert_eq!(keys("}/{"), ["}", "{"]);
        assert_eq!(keys("+ / -"), ["+", "-"]);
        assert_eq!(keys("+/-"), ["+", "-"]);
        assert_eq!(keys("?"), ["?"]);
        assert_eq!(keys("."), ["."]);
    }

    #[test]
    fn digit_runs_expand_to_one_key_each() {
        assert_eq!(keys("1-4"), ["1", "2", "3", "4"]);
        assert_eq!(keys("0-5"), ["0", "1", "2", "3", "4", "5"]);
        assert_eq!(keys("1/2/3"), ["1", "2", "3"]);
    }

    #[test]
    fn movement_clusters_expand_to_the_keys_they_abbreviate() {
        assert_eq!(keys("WASD"), ["w", "a", "s", "d"]);
        assert_eq!(
            keys("Arrows"),
            ["ArrowUp", "ArrowDown", "ArrowLeft", "ArrowRight"]
        );
        assert_eq!(
            keys("↑↓←→/hjkl"),
            [
                "ArrowUp",
                "ArrowDown",
                "ArrowLeft",
                "ArrowRight",
                "h",
                "j",
                "k",
                "l"
            ]
        );
        assert_eq!(keys("W/A/S/D"), ["W", "A", "S", "D"]);
    }

    #[test]
    fn pointer_only_entries_offer_no_buttons() {
        for label in [
            "Click",
            "Scroll",
            "Wheel",
            "Mouse",
            "mouse",
            "Right-click",
            "Shift+Click",
            "Pane rail",
            "Drag divider",
            "Drag timeline",
            "Click info",
            "Click/Scroll",
            "TextFX hint row",
        ] {
            assert!(keys(label).is_empty(), "{label} should offer no key");
        }
    }

    #[test]
    fn a_mixed_entry_keeps_the_keys_and_drops_the_pointer() {
        assert_eq!(keys("Click / Enter / Space"), ["Enter", "Space"]);
    }

    #[test]
    fn a_parenthetical_qualifies_the_action_not_the_chord() {
        assert_eq!(keys("Enter (search)"), ["Enter"]);
        assert_eq!(keys("Ctrl+R / Enter (replace)"), ["r", "Enter"]);
    }

    #[test]
    fn a_mode_prefix_is_not_mistaken_for_a_key() {
        assert_eq!(keys("Macro: r/p/l +/-"), ["r", "p", "l", "+", "-"]);
    }

    #[test]
    fn page_keys_are_spelled_out_however_they_were_abbreviated() {
        assert_eq!(keys("PgUp/Dn"), ["PageUp", "PageDown"]);
        assert_eq!(keys("PgUp/PgDn"), ["PageUp", "PageDown"]);
    }

    #[test]
    fn the_same_key_is_only_offered_once() {
        let actions = touch_actions(&[
            HelpEntry {
                key: "j/k",
                action: "Move",
            },
            HelpEntry {
                key: "j",
                action: "Also move",
            },
        ]);
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0].action, "Move");
    }

    #[test]
    fn every_action_carries_its_own_description() {
        let actions = touch_actions(&[HelpEntry {
            key: "↑/↓",
            action: "Scroll",
        }]);
        assert!(actions.iter().all(|a| a.action == "Scroll"));
    }
}
