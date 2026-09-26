//! wasm-bindgen frontend for the Doomsday web apps.
//!
//! A thin application layer over the engine, compiled to wasm via `wasm-pack`
//! (the `libmtg_doomsday.js` / `libmtg_doomsday_bg.wasm` the web pages load). One
//! module serves both frontends:
//! - **pile builder** (`pilegen.html`) — pick a matchup, run a scenario to the
//!   point Doomsday resolves, and encode/decode pile snapshots for sharing
//!   (`run_scenario` / `encode_snapshot` / `decode_snapshot`).
//! - **goldfish** (`dd-goldfish.html`) — Monte-Carlo a pasted decklist
//!   (`run_goldfish_web` / `run_goldfish_asap_web` / `missing_cards_web`).
//!
//! The whole module is gated to `target_arch = "wasm32"`; it is absent on native
//! targets, where the `dd-goldfish` CLI bin is the entry point instead.

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::prelude::*;
#[cfg(target_arch = "wasm32")]
use libmtg_engine::{
    build_catalog, classify_unimplemented_cards, BoardSnapshot, CardRegistry,
    ScenarioResult, PlayerId, to_url_token, from_url_token,
};
#[cfg(target_arch = "wasm32")]
use libmtg_decklist::Decklist;
#[cfg(target_arch = "wasm32")]
use crate::{generate_scenario, run_goldfish_asap_mode, run_goldfish_send, MullMode, DEFAULT_PROTECTION};
#[cfg(target_arch = "wasm32")]
use crate::goldfish::{
    deal_opening_hands,
    learned_mull::{hand_estimates, keep_suggestion},
    run_goldfish_fixed_hand_report,
};

#[cfg(target_arch = "wasm32")]
fn cards(list: &[(&str, i32)]) -> Vec<(String, i32, String)> {
    list.iter().map(|(n, q)| (n.to_string(), *q, "main".to_string())).collect()
}

#[cfg(target_arch = "wasm32")]
fn dd_deck() -> Vec<(String, i32, String)> {
    // tempo-doomsday-wasteland-1.4
    cards(&[
        ("Underground Sea", 3), ("Polluted Delta", 4), ("Flooded Strand", 1),
        ("Misty Rainforest", 1), ("Scalding Tarn", 1), ("Marsh Flats", 1),
        ("Island", 1), ("Swamp", 1), ("Undercity Sewers", 2), ("Wasteland", 3),
        ("Cavern of Souls", 1),
        ("Lotus Petal", 2), ("Lion's Eye Diamond", 1),
        ("Dark Ritual", 4), ("Doomsday", 4), ("Brainstorm", 4),
        ("Ponder", 4), ("Consider", 1), ("Edge of Autumn", 1),
        ("Force of Will", 4), ("Daze", 3), ("Thoughtseize", 2),
        ("Street Wraith", 1), ("Thassa's Oracle", 1), ("Unearth", 1),
        ("Tamiyo, Inquisitive Student", 4), ("Orcish Bowmasters", 2),
        ("Murktide Regent", 2),
    ])
}

#[cfg(target_arch = "wasm32")]
fn izzet_delver_deck() -> Vec<(String, i32, String)> {
    // izzet-delver-ethanr-mar26
    cards(&[
        ("Volcanic Island", 4), ("Scalding Tarn", 2), ("Flooded Strand", 2),
        ("Misty Rainforest", 2), ("Polluted Delta", 3), ("Wasteland", 4),
        ("Island", 1), ("Thundering Falls", 1),
        ("Delver of Secrets", 3), ("Dragon's Rage Channeler", 4),
        ("Murktide Regent", 2), ("Brazen Borrower", 1),
        ("Cori-Steel Cutter", 3), ("Mishra's Bauble", 4),
        ("Lightning Bolt", 4), ("Unholy Heat", 1),
        ("Force of Will", 4), ("Force of Negation", 1), ("Daze", 4),
        ("Brainstorm", 4), ("Ponder", 4), ("Preordain", 2),
    ])
}

#[cfg(target_arch = "wasm32")]
fn ub_tempo_deck() -> Vec<(String, i32, String)> {
    // dimir-tempo-1.0
    cards(&[
        ("Underground Sea", 4), ("Polluted Delta", 4), ("Flooded Strand", 2),
        ("Misty Rainforest", 1), ("Scalding Tarn", 1), ("Bloodstained Mire", 1),
        ("Island", 1), ("Swamp", 1), ("Wasteland", 4), ("Undercity Sewers", 1),
        ("Tamiyo, Inquisitive Student", 4), ("Orcish Bowmasters", 4),
        ("Murktide Regent", 3), ("Barrowgoyf", 2), ("Brazen Borrower", 1),
        ("Kaito, Bane of Nightmares", 2),
        ("Brainstorm", 4), ("Ponder", 4), ("Force of Will", 4), ("Daze", 3),
        ("Fatal Push", 4), ("Snuff Out", 1), ("Thoughtseize", 4),
    ])
}

/// Returns JSON list of available matchup names.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn list_matchups() -> String {
    serde_json::to_string(&["Izzet Delver", "UB Tempo"]).unwrap()
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn run_scenario(matchup: &str) -> String {
    let catalog = build_catalog();
    let dd_cards = dd_deck();

    let (opp_name, opp_cards) = match matchup {
        "UB Tempo" => ("UB Tempo", ub_tempo_deck()),
        _ => ("Izzet Delver", izzet_delver_deck()),
    };

    let state = generate_scenario("doomsday", opp_name, &catalog, &dd_cards, &opp_cards);
    serde_json::to_string(&state.to_result()).unwrap()
}

/// Cached card registry for snapshot encode/decode (built once from catalog).
/// Registers both front-face and back-face names so flipped DFCs resolve.
#[cfg(target_arch = "wasm32")]
fn get_registry() -> &'static CardRegistry {
    use std::sync::OnceLock;
    static REGISTRY: OnceLock<CardRegistry> = OnceLock::new();
    REGISTRY.get_or_init(|| {
        let catalog = build_catalog();
        // Front-face names (canonical), sorted for stable ID assignment.
        let mut names: Vec<String> = catalog.keys().cloned().collect();
        names.sort();
        let mut entries: Vec<(&str, &str, u16)> = names.iter()
            .enumerate()
            .map(|(i, name)| (name.as_str(), "DEV", i as u16))
            .collect();
        // Back-face / adventure names get the same ID as their front face.
        let mut back_entries: Vec<(String, &str, u16)> = Vec::new();
        for (i, name) in names.iter().enumerate() {
            if let Some(def) = catalog.get(name) {
                if let Some(back) = def.back_name() {
                    if back != name.as_str() {
                        back_entries.push((back.to_string(), "DEV", i as u16));
                    }
                }
            }
        }
        for (bname, set, id) in &back_entries {
            entries.push((bname.as_str(), set, *id));
        }
        CardRegistry::from_entries(&entries)
    })
}

/// Encode a ScenarioResult + pile selection into a compact URL token.
///
/// `scenario_json`: the JSON from `run_scenario`.
/// `pile_json`: `{ "library": [[idx, slot], ...], "graveyard": [[idx, slot], ...] }`.
/// Slots are 1..=5 (1 = top of pile, 5 = bottom). An index with slot=0 is ignored.
#[cfg(target_arch = "wasm32")]
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct PileSelection {
    #[serde(default)]
    library: Vec<(usize, u8)>,
    #[serde(default)]
    graveyard: Vec<(usize, u8)>,
}

#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn encode_snapshot(scenario_json: &str, pile_json: &str) -> Result<String, JsValue> {
    let result: ScenarioResult = serde_json::from_str(scenario_json)
        .map_err(|e| JsValue::from_str(&format!("bad scenario JSON: {e}")))?;
    let pile: PileSelection = serde_json::from_str(pile_json)
        .map_err(|e| JsValue::from_str(&format!("bad pile JSON: {e}")))?;

    let registry = get_registry();
    let mut snap = BoardSnapshot::from_result(&result, registry)
        .map_err(|e| JsValue::from_str(&format!("snapshot: {e}")))?;

    for &(idx, slot) in &pile.library {
        if let Some(card) = snap.player_mut(PlayerId::Us).library.get_mut(idx) {
            card.pile_slot = slot;
        }
    }
    for &(idx, slot) in &pile.graveyard {
        if let Some(card) = snap.player_mut(PlayerId::Us).graveyard.get_mut(idx) {
            card.pile_slot = slot;
        }
    }

    Ok(to_url_token(&snap))
}

/// Decode a URL token back into ScenarioResult JSON + pile selection.
///
/// Returns JSON: `{ "scenario": ScenarioResult, "pile": { "library": [[idx, slot], ...], "graveyard": [[idx, slot], ...] } }`.
/// Logs/decision_log/text_summary will be empty.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn decode_snapshot(token: &str) -> Result<String, JsValue> {
    let registry = get_registry();
    let snap = from_url_token(token)
        .map_err(|e| JsValue::from_str(&format!("decode: {e}")))?;

    let pile = PileSelection {
        library: snap.player(PlayerId::Us).library.iter()
            .enumerate()
            .filter(|(_, c)| c.pile_slot != 0)
            .map(|(i, c)| (i, c.pile_slot))
            .collect(),
        graveyard: snap.player(PlayerId::Us).graveyard.iter()
            .enumerate()
            .filter(|(_, c)| c.pile_slot != 0)
            .map(|(i, c)| (i, c.pile_slot))
            .collect(),
    };
    let result = snap.to_result(registry);

    #[derive(serde::Serialize)]
    struct Shared { scenario: ScenarioResult, pile: PileSelection }

    serde_json::to_string(&Shared { scenario: result, pile })
        .map_err(|e| JsValue::from_str(&e.to_string()))
}

// ── goldfish frontend (dd-goldfish.html) ────────────────────────────────────

/// Goldfish a pasted text decklist with the cast-ASAP `DDGoldfishStrategy` (follows
/// the recipe solver to combo by `cutoff`) under the selected `mull_mode`
/// (`keep7` / `realistic` / `aggressive`). Returns `GoldfishStats` as JSON; the
/// headline `P(cast by cutoff)` is read off the cast-turn CDF client-side.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn run_goldfish_asap_web(
    deck_text: &str,
    games: u32,
    cutoff: u32,
    mull_mode: &str,
    play_draw: &str,
) -> String {
    let deck = Decklist::parse_text(deck_text).to_engine_deck();
    let mode = MullMode::from_str_or_default(mull_mode);
    // "play" / "draw" force it; anything else (default "random") randomizes 50/50.
    let on_play = match play_draw {
        "play" => Some(true),
        "draw" => Some(false),
        _ => None,
    };
    // Two-wincon when the deck runs The Fantasticar: the pilot also pursues the car pop and
    // a pop counts as a "send", so the headline is P(send by cutoff) = P(Doomsday OR car).
    // A car-less deck runs the Doomsday-only path unchanged.
    let has_car = deck.iter().any(|(name, _, _)| name == "The Fantasticar");
    let stats = if has_car {
        run_goldfish_send(&deck, games, DEFAULT_PROTECTION, cutoff, mode, on_play, true)
    } else {
        run_goldfish_asap_mode(&deck, games, DEFAULT_PROTECTION, cutoff, mode, on_play)
    };
    serde_json::to_string(&stats).unwrap()
}

/// Classify the deck's cards the engine can't simulate (✗ missing / ~ inert).
/// Returns a JSON array of `UnimplementedCard`, for rendering + pre-filled
/// `missing-card` issue links.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn missing_cards_web(deck_text: &str) -> String {
    let deck = Decklist::parse_text(deck_text).to_engine_deck();
    let report = classify_unimplemented_cards(&deck, &build_catalog());
    serde_json::to_string(&report).unwrap()
}

// ── hand explorer (hands.html) ──────────────────────────────────────────────

/// Deal `n` random opening hands from the deck, each with the model's *instant* read
/// (both GBDTs + the two policies' keep/mull verdicts). No simulation. Returns
/// `[{cards:[..], est:HandEstimates}, ..]` as JSON.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn deal_hands_web(deck_text: &str, n: u32, play_draw: &str) -> String {
    let deck = Decklist::parse_text(deck_text).to_engine_deck();
    let on_play = play_draw != "draw";
    let out: Vec<serde_json::Value> = deal_opening_hands(&deck, n as usize)
        .into_iter()
        .map(|cards| {
            let refs: Vec<&str> = cards.iter().map(|s| s.as_str()).collect();
            serde_json::json!({ "cards": cards, "est": hand_estimates(&refs, on_play) })
        })
        .collect();
    serde_json::to_string(&out).unwrap()
}

/// Full report for one hand (`cards` joined by `|`): the model's instant estimate plus the
/// measured sim truth (P(cast), E[TTD], the interaction-conditioned variants, resources at cast).
/// Returns `{cards:[..], est:HandEstimates, sim:HandSimReport}` as JSON.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn hand_report_web(
    deck_text: &str,
    hand_str: &str,
    cutoff: u32,
    horizon: u32,
    games: u32,
    play_draw: &str,
) -> String {
    let deck = Decklist::parse_text(deck_text).to_engine_deck();
    let on_play = play_draw != "draw";
    let hand: Vec<String> = hand_str
        .split('|')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let refs: Vec<&str> = hand.iter().map(|s| s.as_str()).collect();
    let est = hand_estimates(&refs, on_play);
    let sim = run_goldfish_fixed_hand_report(&deck, &hand, cutoff, horizon, games, on_play);
    serde_json::to_string(&serde_json::json!({ "cards": hand, "est": est, "sim": sim })).unwrap()
}

/// Instant model read for one hand (`cards` joined by `|`): both GBDTs + the resource score.
/// No simulation — this is the "known stuff" revealed without running any games.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn hand_estimates_web(hand_str: &str, play_draw: &str) -> String {
    let on_play = play_draw != "draw";
    let hand: Vec<String> = hand_str
        .split('|')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let refs: Vec<&str> = hand.iter().map(|s| s.as_str()).collect();
    serde_json::to_string(&hand_estimates(&refs, on_play)).unwrap()
}

/// Instant keep/bottom suggestion for one hand (`cards` joined by `|`) at a chosen `keep_size`:
/// each policy's best `keep_size`-card subset, the cards to bottom, and the score-vs-bar. No sim.
#[cfg(target_arch = "wasm32")]
#[wasm_bindgen]
pub fn keep_suggestion_web(hand_str: &str, play_draw: &str, keep_size: u32) -> String {
    let on_play = play_draw != "draw";
    let hand: Vec<String> = hand_str
        .split('|')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect();
    let refs: Vec<&str> = hand.iter().map(|s| s.as_str()).collect();
    serde_json::to_string(&keep_suggestion(&refs, on_play, keep_size as usize)).unwrap()
}
