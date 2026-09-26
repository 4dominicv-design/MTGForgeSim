//! Deal opening hands and show what the production `Realistic` mulligan rule decides,
//! with the inspectable signals behind each call. A design/validation aid for the
//! goldfish mulligan modes (`libmtg_doomsday::goldfish::mull`).
//!
//!   cargo run -p libmtg-doomsday --example sample_hands -- [count] [seed] [cutoff] [deck]
//!
//! Defaults: 16 hands, seed 20260620, cutoff T4, deck=tempo (also: ritual).
//! Env: RAW=1 deal raw hands (incl. deterministic-line hands); otherwise only the
//! hard middle (no guaranteed line) is shown.

use libmtg_doomsday::goldfish::mull::{hand_signals, realistic_keep};
use libmtg_doomsday::goldfish::recipe::{self, CardRole};
use libmtg_engine::{build_catalog, PlayerId, PlayerState, SimState, Zone};
use rand::rngs::SmallRng;
use rand::seq::SliceRandom;
use rand::SeedableRng;

fn deck_by_name(name: &str) -> Vec<(&'static str, i32)> {
    match name {
        "ritual" => vec![
            ("Underground Sea", 4), ("Polluted Delta", 4), ("Flooded Strand", 2),
            ("Misty Rainforest", 1), ("Scalding Tarn", 1), ("Marsh Flats", 1),
            ("Island", 2), ("Swamp", 2), ("Undercity Sewers", 2),
            ("Lotus Petal", 4), ("Lion's Eye Diamond", 1),
            ("Dark Ritual", 4), ("Doomsday", 4), ("Thassa's Oracle", 1),
            ("Brainstorm", 4), ("Ponder", 4), ("Consider", 2), ("Edge of Autumn", 1),
            ("Street Wraith", 3), ("Unearth", 1),
            ("Force of Will", 4), ("Daze", 4), ("Thoughtseize", 4),
        ],
        _ => vec![ // "tempo"
            ("Underground Sea", 3), ("Polluted Delta", 4), ("Flooded Strand", 1),
            ("Misty Rainforest", 1), ("Scalding Tarn", 1), ("Marsh Flats", 1),
            ("Island", 1), ("Swamp", 1), ("Undercity Sewers", 2), ("Wasteland", 3),
            ("Cavern of Souls", 1), ("Lotus Petal", 2), ("Lion's Eye Diamond", 1),
            ("Dark Ritual", 4), ("Doomsday", 4), ("Brainstorm", 4), ("Ponder", 4),
            ("Consider", 1), ("Edge of Autumn", 1), ("Force of Will", 4), ("Daze", 3),
            ("Thoughtseize", 2), ("Street Wraith", 1), ("Thassa's Oracle", 1),
            ("Unearth", 1), ("Tamiyo, Inquisitive Student", 4), ("Orcish Bowmasters", 2),
            ("Murktide Regent", 2),
        ],
    }
}

/// Short tag for a card (the solver role, with the deck's threats/filler split out).
fn tag(name: &str, role: CardRole) -> &'static str {
    match name {
        "Tamiyo, Inquisitive Student" => "threat:Tamiyo",
        "Murktide Regent" | "Orcish Bowmasters" | "Thassa's Oracle" => "filler",
        "Force of Will" | "Daze" => "interaction",
        "Thoughtseize" => "interaction",
        "Unearth" => "filler",
        "Street Wraith" => "free-cantrip",
        "Wasteland" => "land(colorless)",
        _ => match role {
            CardRole::Payoff => "DOOMSDAY",
            CardRole::PayoffTutor => "tutor:finds-DD",
            CardRole::Ritual => "ritual",
            CardRole::Petal => "petal",
            CardRole::Fetch => "fetch",
            CardRole::BlackLandUntapped => "land(B)",
            CardRole::BlackLandTapped => "land(B,tapped)",
            CardRole::Cantrip => "cantrip",
            CardRole::BlueSource => "land(U)",
            CardRole::Other => "other",
        },
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let count: usize = args.get(1).and_then(|s| s.parse().ok()).unwrap_or(16);
    let seed: u64 = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(20_260_620);
    let cutoff: u32 = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(4);
    let deck = args.get(4).map(String::as_str).unwrap_or("tempo");
    let raw = std::env::var("RAW").is_ok();

    let catalog = build_catalog();
    let mut pool: Vec<String> = Vec::new();
    for (name, qty) in deck_by_name(deck) {
        for _ in 0..qty { pool.push(name.to_string()); }
    }
    let mut rng = SmallRng::seed_from_u64(seed);
    println!("Realistic-mode opening hands  ({deck} list, {} cards, cutoff T{cutoff})\n", pool.len());

    let (mut shown, mut dealt, mut keeps) = (0usize, 0usize, 0u32);
    while shown < count && dealt < count * 500 {
        dealt += 1;
        let mut d = pool.clone();
        d.shuffle(&mut rng);
        let (hand, library) = d.split_at(7);

        let mut state = SimState::new(PlayerState::new("us"), PlayerState::new("opp"));
        state.catalog = catalog.clone();
        for n in hand { state.place_card(PlayerId::Us, n, Zone::Hand { known: false }); }
        for n in library { state.place_card(PlayerId::Us, n, Zone::Library); }

        let s = hand_signals(&state, PlayerId::Us, cutoff);
        if !raw && s.det_line { continue; } // study the middle by default
        shown += 1;
        let keep = realistic_keep(&s, 0);
        if keep { keeps += 1; }

        println!("── hand #{shown} ──");
        for card in state.hand_of(PlayerId::Us) {
            let role = recipe::card_role(&state, PlayerId::Us, card.id);
            println!("    {:<30} [{}]", card.catalog_key, tag(&card.catalog_key, role));
        }
        println!(
            "    sources U-land:{} B-land:{} colored:{} | petals:{} rituals:{} deep:{} marginal:{} support:{} | DD:{} Tamiyo:{} det-line:{}",
            s.blue_lands, s.black_lands, s.colored_lands, s.petals, s.rituals,
            s.deep_looks, s.marginal_looks, s.supporters, yn(s.has_dd), yn(s.has_tamiyo), yn(s.det_line),
        );
        println!("    Realistic → {}\n", if keep { "KEEP" } else { "MULL" });
    }
    println!("Realistic keep-rate: {keeps}/{shown}  (seed {seed}, {dealt} dealt)");
}

fn yn(b: bool) -> &'static str { if b { "y" } else { "n" } }
