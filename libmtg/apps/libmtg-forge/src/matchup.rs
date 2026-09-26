use std::sync::Arc;

use libmtg_decklist::Decklist;
use libmtg_engine::{
    build_catalog, classify_unimplemented_cards, run_game, CardDef, GameEvent, Objective,
    PlayerId, Scenario, SimState, UnimplementedCard,
};
use rand::{rngs::SmallRng, SeedableRng};
use serde::Serialize;

use crate::strategy::{BaselineOpponentStrategy, ForgeStrategy};

#[derive(Default)]
struct FullGameObjective;
impl Objective for FullGameObjective {
    fn observe(&mut self, _event: &GameEvent, _state: &mut SimState) -> bool { false }
}

#[derive(Debug, Clone, Serialize)]
pub struct PartialSupport {
    pub name: String,
    pub board: String,
    pub reason: String,
}

#[derive(Debug, Serialize)]
pub struct DeckAudit {
    pub main_count: u32,
    pub side_count: u32,
    pub unsupported: Vec<UnimplementedCard>,
    /// Cards that pass the engine's coarse implementation check but still have
    /// known rules gaps large enough to invalidate Forge matchup results.
    pub known_partial: Vec<PartialSupport>,
    pub simulation_ready: bool,
}

pub fn audit(deck: &Decklist) -> DeckAudit {
    let catalog = build_catalog();
    let cards = deck.to_engine_deck();
    let unsupported = classify_unimplemented_cards(&cards, &catalog);
    let known_partial = known_partial_support(deck);
    // Karn can access the wishboard (materialized as exile in the Forge
    // simulator), so sideboard support is required just like maindeck support.
    let simulation_ready = unsupported.is_empty() && known_partial.is_empty();
    DeckAudit {
        main_count: deck.main_count(),
        side_count: deck.side_count(),
        unsupported,
        known_partial,
        simulation_ready,
    }
}

#[derive(Debug, Serialize)]
pub struct MatchupStats {
    pub games: u64,
    pub forge_wins: u64,
    pub opponent_wins: u64,
    pub unresolved: u64,
    pub forge_win_pct_of_decided: f64,
    pub on_play_games: u64,
    pub on_draw_games: u64,
}

/// Full-engine runner. We intentionally require 100% implementation coverage;
/// otherwise unknown, inert, or partially-modeled cards would bias win rates.
pub fn run_matchup(
    forge: &Decklist,
    opponent: &Decklist,
    games: u64,
    seed: u64,
    max_turns: u8,
) -> Result<MatchupStats, String> {
    let catalog = build_catalog();
    ensure_supported("Forge", forge, &catalog)?;
    ensure_supported("Opponent", opponent, &catalog)?;

    let mut stats = MatchupStats {
        games,
        forge_wins: 0,
        opponent_wins: 0,
        unresolved: 0,
        forge_win_pct_of_decided: 0.0,
        on_play_games: 0,
        on_draw_games: 0,
    };

    for i in 0..games {
        let game_seed = splitmix64(seed.wrapping_add(i));
        let on_play = i % 2 == 0; // exact 50/50 balance, reproducible
        if on_play {
            stats.on_play_games += 1;
        } else {
            stats.on_draw_games += 1;
        }

        let state = simulate_one(forge, opponent, &catalog, game_seed, on_play, max_turns);
        match state.winner {
            Some(PlayerId::Us) => stats.forge_wins += 1,
            Some(PlayerId::Opp) => stats.opponent_wins += 1,
            None => stats.unresolved += 1,
        }
    }

    let decided = stats.forge_wins + stats.opponent_wins;
    if decided > 0 {
        stats.forge_win_pct_of_decided = 100.0 * stats.forge_wins as f64 / decided as f64;
    }
    Ok(stats)
}

#[derive(Debug, Serialize)]
pub struct PairedMatchupStats {
    pub games_per_build: u64,
    pub build_a_wins: u64,
    pub build_b_wins: u64,
    pub build_a_win_pct_all_games: f64,
    pub build_b_win_pct_all_games: f64,
    pub delta_b_minus_a_pp: f64,
    pub a_only_wins: u64,
    pub b_only_wins: u64,
    pub both_win: u64,
    pub neither_wins: u64,
}

/// Paired A/B comparison against the same opponent. Each build receives the
/// same per-trial seed and the same forced play/draw assignment. This is the
/// full-game equivalent of the opening-hand common-random-number comparison.
pub fn run_paired_matchup(
    build_a: &Decklist,
    build_b: &Decklist,
    opponent: &Decklist,
    games: u64,
    seed: u64,
    max_turns: u8,
) -> Result<PairedMatchupStats, String> {
    let catalog = build_catalog();
    ensure_supported("Forge build A", build_a, &catalog)?;
    ensure_supported("Forge build B", build_b, &catalog)?;
    ensure_supported("Opponent", opponent, &catalog)?;

    let mut out = PairedMatchupStats {
        games_per_build: games,
        build_a_wins: 0,
        build_b_wins: 0,
        build_a_win_pct_all_games: 0.0,
        build_b_win_pct_all_games: 0.0,
        delta_b_minus_a_pp: 0.0,
        a_only_wins: 0,
        b_only_wins: 0,
        both_win: 0,
        neither_wins: 0,
    };

    for i in 0..games {
        let game_seed = splitmix64(seed.wrapping_add(i));
        let on_play = i % 2 == 0;
        let a = simulate_one(build_a, opponent, &catalog, game_seed, on_play, max_turns);
        let b = simulate_one(build_b, opponent, &catalog, game_seed, on_play, max_turns);
        let aw = a.winner == Some(PlayerId::Us);
        let bw = b.winner == Some(PlayerId::Us);
        out.build_a_wins += aw as u64;
        out.build_b_wins += bw as u64;
        match (aw, bw) {
            (true, true) => out.both_win += 1,
            (true, false) => out.a_only_wins += 1,
            (false, true) => out.b_only_wins += 1,
            (false, false) => out.neither_wins += 1,
        }
    }

    if games > 0 {
        out.build_a_win_pct_all_games = 100.0 * out.build_a_wins as f64 / games as f64;
        out.build_b_win_pct_all_games = 100.0 * out.build_b_wins as f64 / games as f64;
        out.delta_b_minus_a_pp = out.build_b_win_pct_all_games - out.build_a_win_pct_all_games;
    }
    Ok(out)
}

fn simulate_one(
    forge: &Decklist,
    opponent: &Decklist,
    catalog: &std::collections::HashMap<String, CardDef>,
    game_seed: u64,
    on_play: bool,
    max_turns: u8,
) -> SimState {
    let mut rng = SmallRng::seed_from_u64(game_seed);
    run_game(
        Scenario {
            us_label: "Mystic Forge".to_string(),
            opp_label: "Opponent".to_string(),
            catalog: catalog.clone(),
            us_deck: forge.to_engine_deck(),
            opp_deck: opponent.to_engine_deck(),
            us_strategy: Box::new(ForgeStrategy::new(PlayerId::Us)),
            opp_strategy: Box::new(BaselineOpponentStrategy::new(PlayerId::Opp)),
            evaluate_card: Arc::new(|_, _, _| 0.5),
            objective: Box::<FullGameObjective>::default(),
            max_turns,
            on_play: Some(on_play),
            fixed_us_hand: None,
        },
        &mut rng,
    )
}

fn ensure_supported(
    label: &str,
    deck: &Decklist,
    catalog: &std::collections::HashMap<String, CardDef>,
) -> Result<(), String> {
    let unsupported = classify_unimplemented_cards(&deck.to_engine_deck(), catalog);
    let missing = unsupported
        .iter()
        .map(|c| format!("{}x {} [{}] ({:?})", c.qty, c.name, c.board, c.kind))
        .collect::<Vec<_>>();
    let partial = known_partial_support(deck)
        .into_iter()
        .map(|c| format!("{} [{}] ({})", c.name, c.board, c.reason))
        .collect::<Vec<_>>();
    if missing.is_empty() && partial.is_empty() {
        return Ok(());
    }

    let mut pieces = Vec::new();
    if !missing.is_empty() {
        pieces.push(format!("unsupported: {}", missing.join(", ")));
    }
    if !partial.is_empty() {
        pieces.push(format!("known partial implementations: {}", partial.join(", ")));
    }
    Err(format!(
        "{label} deck is not simulation-ready; refusing to report a win rate. {}",
        pieces.join("; ")
    ))
}

fn known_partial_support(_deck: &Decklist) -> Vec<PartialSupport> {
    // Keep this function as the single place to re-introduce explicit fidelity
    // warnings if a materially partial implementation is added later. The
    // current Forge card set has no entries that need to block matchup runs.
    Vec::new()
}

fn splitmix64(mut x: u64) -> u64 {
    x = x.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = x;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}
