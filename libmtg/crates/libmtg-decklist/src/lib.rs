//! Portable Magic decklists: parse from the standard `[qty] [name]` text format
//! or — with the native-only `fetch` feature — straight from a Moxfield /
//! MTGGoldfish URL (cached on disk).
//!
//! A [`Decklist`] is deliberately decoupled from the engine and the SQLite DB:
//! it's just shareable text, so the simulator can take a deck from a file, a
//! pasted list, or a URL without touching a database. Hand it to the engine
//! with [`Decklist::to_engine_deck`], which yields the `(name, qty, board)`
//! tuples a `Scenario` expects.
//!
//! Text format (a superset of the MTGO / Arena / MTGGoldfish exports):
//! - `4 Brainstorm` or `4x Brainstorm` — quantity then name.
//! - A trailing ` (SET) 123` set/collector annotation is stripped.
//! - `#` and `//` start comment lines.
//! - Sections: an explicit `Sideboard` / `Deck` header switches section;
//!   absent headers, the first blank line after the mainboard starts the
//!   sideboard (MTGO convention).

use serde::{Deserialize, Serialize};

#[cfg(feature = "fetch")]
mod fetch;
#[cfg(feature = "fetch")]
pub use fetch::{from_url, from_url_cached, FetchError};

/// One `quantity × card` line of a decklist.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeckEntry {
    pub name: String,
    pub qty: u32,
}

/// A parsed decklist, split into mainboard and sideboard. Order is preserved as
/// parsed; the engine shuffles, so order is cosmetic.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Decklist {
    pub main: Vec<DeckEntry>,
    pub side: Vec<DeckEntry>,
}

/// Which board a card line belongs to while parsing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Main,
    Side,
    /// Maybeboard / unknown section — entries are dropped.
    Skip,
}

impl Decklist {
    /// Parse the standard `[qty] [name]` text format. Never fails: unparseable
    /// lines (junk, headers we don't recognize) are skipped rather than
    /// rejected, matching how players paste real-world exports.
    pub fn parse_text(content: &str) -> Decklist {
        let mut deck = Decklist::default();
        let mut section = Section::Main;
        // Once an explicit `Deck`/`Sideboard` header appears, blank lines stop
        // implying a section switch (Arena-style exports use headers, not
        // blank-line separation).
        let mut explicit_headers = false;

        for raw in content.lines() {
            let line = raw.trim();

            if line.is_empty() {
                if !explicit_headers
                    && section == Section::Main
                    && !deck.main.is_empty()
                {
                    section = Section::Side;
                }
                continue;
            }
            if line.starts_with('#') || line.starts_with("//") {
                continue;
            }

            if let Some(header) = section_header(line) {
                section = header;
                explicit_headers = true;
                continue;
            }

            if let Some(entry) = parse_entry(line) {
                match section {
                    Section::Main => deck.main.push(entry),
                    Section::Side => deck.side.push(entry),
                    Section::Skip => {}
                }
            }
            // Non-card, non-header lines (e.g. Arena's `About` / `Name ...`) are
            // ignored.
        }
        deck
    }

    /// Flatten to the engine's deck representation: `(card_name, qty, board)`
    /// tuples, where `board` is `"main"` or `"side"`.
    pub fn to_engine_deck(&self) -> Vec<(String, i32, String)> {
        let main = self
            .main
            .iter()
            .map(|e| (e.name.clone(), e.qty as i32, "main".to_string()));
        let side = self
            .side
            .iter()
            .map(|e| (e.name.clone(), e.qty as i32, "side".to_string()));
        main.chain(side).collect()
    }

    /// Total mainboard card count (sum of quantities).
    pub fn main_count(&self) -> u32 {
        self.main.iter().map(|e| e.qty).sum()
    }

    /// Total sideboard card count (sum of quantities).
    pub fn side_count(&self) -> u32 {
        self.side.iter().map(|e| e.qty).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.main.is_empty() && self.side.is_empty()
    }
}

/// Recognize a section-header line (case-insensitive, optional trailing `:`).
/// Returns `None` for anything that isn't a known header so the caller can fall
/// through to entry parsing.
fn section_header(line: &str) -> Option<Section> {
    let key = line.trim_end_matches(':').trim().to_ascii_lowercase();
    match key.as_str() {
        "deck" | "mainboard" | "main" | "commander" | "companion" => Some(Section::Main),
        "sideboard" | "side" | "sb" => Some(Section::Side),
        "maybeboard" | "maybe" => Some(Section::Skip),
        _ => None,
    }
}

/// Parse one `[qty][x] <name> [(SET) 123]` card line. Returns `None` when the
/// first token isn't a positive quantity (so headers/junk are skipped).
fn parse_entry(line: &str) -> Option<DeckEntry> {
    let (qty_tok, rest) = line.split_once(char::is_whitespace)?;
    // Accept both `4` and `4x` / `4X`.
    let qty: u32 = qty_tok.trim_end_matches(['x', 'X']).parse().ok()?;
    if qty == 0 {
        return None;
    }
    let name = clean_card_name(rest.trim());
    if name.is_empty() {
        return None;
    }
    Some(DeckEntry { name, qty })
}

/// Normalize a decklist line's name to its catalog key. Strips:
/// - a DFC / split back face, keeping the front face (which is written first):
///   Moxfield uses a single slash — `Tamiyo, Inquisitive Student / Tamiyo,
///   Seasoned Scholar` — while Scryfall/Arena/MTGO use `//`. Splitting on a single
///   `/` handles both. Result: `Tamiyo, Inquisitive Student`.
/// - a trailing set/collector annotation: `Brainstorm (MH2) 217` -> `Brainstorm`.
///
/// Commas and other in-name punctuation are preserved (`Jace, Wielder of
/// Mysteries` stays intact) — only the two suffixes above are removed. No MTG card
/// name contains a `/` outside the DFC/split separator, so this is safe.
fn clean_card_name(name: &str) -> String {
    // Front face is first; split on a single '/' (covers both " / " and " // ").
    let name = name.split('/').next().unwrap_or(name).trim();
    match name.find(" (") {
        Some(pos) => name[..pos].trim().to_string(),
        None => name.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_qty_and_name() {
        let dl = Decklist::parse_text("4 Brainstorm\n1 Doomsday\n");
        assert_eq!(
            dl.main,
            vec![
                DeckEntry { name: "Brainstorm".into(), qty: 4 },
                DeckEntry { name: "Doomsday".into(), qty: 1 },
            ]
        );
        assert!(dl.side.is_empty());
        assert_eq!(dl.main_count(), 5);
    }

    #[test]
    fn dfc_reduces_to_front_face_and_keeps_commas() {
        let dl = Decklist::parse_text(
            // The exact Moxfield export line (single slash + set/collector suffix).
            "4 Tamiyo, Inquisitive Student / Tamiyo, Seasoned Scholar (MH3) 242\n\
             4 Tamiyo, Inquisitive Student // Tamiyo, Seasoned Scholar\n\
             1 Jace, Wielder of Mysteries\n\
             1 Fire // Ice (APC) 128\n",
        );
        assert_eq!(dl.main[0].name, "Tamiyo, Inquisitive Student"); // Moxfield single slash + set
        assert_eq!(dl.main[1].name, "Tamiyo, Inquisitive Student"); // double slash
        assert_eq!(dl.main[2].name, "Jace, Wielder of Mysteries");  // comma preserved
        assert_eq!(dl.main[3].name, "Fire");                        // split front + set suffix
    }

    #[test]
    fn blank_line_starts_sideboard() {
        let dl = Decklist::parse_text("4 Brainstorm\n3 Daze\n\n2 Thoughtseize\n1 Surgical Extraction\n");
        assert_eq!(dl.main_count(), 7);
        assert_eq!(dl.side_count(), 3);
        assert_eq!(dl.side[0].name, "Thoughtseize");
    }

    #[test]
    fn explicit_headers_override_blank_lines() {
        // Arena style: headers present, blank lines must NOT split the main.
        let dl = Decklist::parse_text(
            "Deck\n4 Brainstorm\n\n4 Ponder\n\nSideboard\n2 Thoughtseize\n",
        );
        assert_eq!(dl.main_count(), 8, "blank lines inside Deck shouldn't spill to side");
        assert_eq!(dl.side_count(), 2);
    }

    #[test]
    fn strips_set_codes_and_x_quantities() {
        let dl = Decklist::parse_text("4x Brainstorm (MH2) 217\n1x Doomsday (DMR) 95\n");
        assert_eq!(dl.main[0], DeckEntry { name: "Brainstorm".into(), qty: 4 });
        assert_eq!(dl.main[1], DeckEntry { name: "Doomsday".into(), qty: 1 });
    }

    #[test]
    fn skips_comments_and_junk() {
        let dl = Decklist::parse_text("# my deck\n// notes\nAbout\nName Doomsday\n4 Brainstorm\n");
        // `About` / `Name Doomsday` aren't `<qty> <name>` lines -> skipped.
        assert_eq!(dl.main, vec![DeckEntry { name: "Brainstorm".into(), qty: 4 }]);
    }

    #[test]
    fn maybeboard_dropped() {
        let dl = Decklist::parse_text("Deck\n4 Brainstorm\nMaybeboard\n9 Black Lotus\n");
        assert_eq!(dl.main_count(), 4);
        assert!(dl.side.is_empty());
    }

    #[test]
    fn round_trips_to_engine_deck() {
        let dl = Decklist::parse_text("4 Brainstorm\n\n2 Thoughtseize\n");
        let engine = dl.to_engine_deck();
        assert_eq!(
            engine,
            vec![
                ("Brainstorm".to_string(), 4, "main".to_string()),
                ("Thoughtseize".to_string(), 2, "side".to_string()),
            ]
        );
    }
}
