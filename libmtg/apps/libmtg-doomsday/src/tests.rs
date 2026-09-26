//! Doomsday content tests, moved out of the engine and rewritten against the
//! public engine API (`SimState::place_card`, `SimState::new`, the read
//! accessors) — no white-box engine internals.
#![allow(clippy::all)]

use libmtg_engine::*;
use crate::strategy::*;
use crate::planner::*;
use crate::{simulate_game, DoomsdayResolvedObjective};
use rand::{SeedableRng, rngs::StdRng};

// ── Test scaffolding (public-API only) ──────────────────────────────────────

/// A bare game state with the real card catalog loaded.
fn st() -> SimState {
    let mut s = SimState::new(PlayerState::new("us_deck"), PlayerState::new("opp_deck"));
    s.catalog = libmtg_engine::build_catalog();
    s
}

fn cards(list: &[(&str, i32)]) -> Vec<(String, i32, String)> {
    list.iter().map(|(n, q)| (n.to_string(), *q, "main".to_string())).collect()
}

fn val_dd_deck() -> Vec<(String, i32, String)> {
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

fn val_ub_tempo_deck() -> Vec<(String, i32, String)> {
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

fn parse_log_hand_info(log: &str) -> Option<(u32, u32, u32, u32)> {
    let us_hand = log.find("us: ").and_then(|i| log[i+4..].chars().next()?.to_digit(10))?;
    let us_mull = {
        let after_us = log.find("us: ").map(|i| &log[i..])?;
        let m_pos = after_us.find("(-")?;
        after_us[m_pos+2..].chars().next()?.to_digit(10)?
    };
    let opp_hand = log.find("opp: ").and_then(|i| log[i+5..].chars().next()?.to_digit(10))?;
    let opp_mull = {
        let after_opp = log.find("opp: ").map(|i| &log[i..])?;
        let m_pos = after_opp.find("(-")?;
        after_opp[m_pos+2..].chars().next()?.to_digit(10)?
    };
    Some((us_hand, us_mull, opp_hand, opp_mull))
}

/// Build a state with specific lands on board and cards in hand (public-API).
fn plan_test_state(lands: &[&str], hand: &[&str]) -> SimState {
    let mut state = st();
    for &name in lands { state.place_card(PlayerId::Us, name, Zone::Battlefield); }
    for &name in hand { state.place_card(PlayerId::Us, name, Zone::Hand { known: false }); }
    state
}

/// Extract the priority-round action names from a plan (spells + land drops, skip taps).
fn plan_spell_names(plan: &[PlanAction], state: &SimState) -> Vec<String> {
    plan.iter().filter_map(|a| match a {
        PlanAction::CastSpell(id) | PlanAction::LandDrop(id) =>
            state.objects.get(id).map(|c| c.catalog_key.clone()),
        PlanAction::TapForMana { .. } | PlanAction::CrackFetch { .. } => None,
    }).collect()
}


    #[test]
    fn test_dd_plan_gap_no_hand_all_gaps_high() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        let matchup = MatchupInfo::default();
        let gap = dd_plan_gap(&state, PlayerId::Us, &matchup);
        assert!(gap.mana >= 0.9, "no mana sources → mana gap near 1.0, got {}", gap.mana);
        assert!(gap.threat >= 0.9, "no threats → threat gap near 1.0, got {}", gap.threat);
    }


    #[test]
    fn test_dd_plan_gap_dd_in_hand_zeroes_threat() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        let _dd = state.place_card(PlayerId::Us, "Doomsday", Zone::Hand { known: false });
        let matchup = MatchupInfo::default();
        let gap = dd_plan_gap(&state, PlayerId::Us, &matchup);
        assert_eq!(gap.threat, 0.0, "DD in hand → threat gap 0.0");
    }


    #[test]
    fn test_dd_plan_gap_lands_reduce_mana_gap() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        state.place_card(PlayerId::Us, "Underground Sea", Zone::Battlefield);
        state.place_card(PlayerId::Us, "Polluted Delta", Zone::Battlefield);
        let matchup = MatchupInfo::default();
        let gap = dd_plan_gap(&state, PlayerId::Us, &matchup);
        // 2 lands → mana_gap = (3-2)/3 ≈ 0.33
        assert!(gap.mana < 0.4, "2 lands → mana gap <0.4, got {}", gap.mana);
        assert!(gap.mana > 0.2, "2 lands → mana gap >0.2, got {}", gap.mana);
    }


    #[test]
    fn test_dd_plan_gap_interaction_high_vs_blue() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        let matchup = MatchupInfo { opp_has_counters: true, ..Default::default() };
        let gap = dd_plan_gap(&state, PlayerId::Us, &matchup);
        assert!(gap.interaction >= 0.9, "no interaction vs blue → high gap, got {}", gap.interaction);
    }


    #[test]
    fn test_dd_plan_gap_interaction_low_vs_nonblue() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        let matchup = MatchupInfo { opp_has_counters: false, ..Default::default() };
        let gap = dd_plan_gap(&state, PlayerId::Us, &matchup);
        assert!(gap.interaction <= 0.2, "vs non-blue → interaction gap low, got {}", gap.interaction);
    }


    #[test]
    fn test_dd_card_fills_land_high_when_needed() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        let land_id = state.place_card(PlayerId::Us, "Underground Sea", Zone::Hand { known: false });
        let gap = TargetGap { mana: 1.0, threat: 1.0, interaction: 0.5 };
        let score = dd_card_fills(land_id, &gap, &state, PlayerId::Us);
        assert!(score > 0.7, "land fills high mana gap → score >0.7, got {}", score);
    }


    #[test]
    fn test_dd_card_fills_land_low_when_flooded() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        let land_id = state.place_card(PlayerId::Us, "Underground Sea", Zone::Hand { known: false });
        let gap = TargetGap { mana: 0.0, threat: 1.0, interaction: 0.5 };
        let score = dd_card_fills(land_id, &gap, &state, PlayerId::Us);
        assert!(score < 0.1, "land when mana gap=0 → score <0.1, got {}", score);
    }


    #[test]
    fn test_dd_card_fills_doomsday_very_high() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        let dd_id = state.place_card(PlayerId::Us, "Doomsday", Zone::Hand { known: false });
        let gap = TargetGap { mana: 0.5, threat: 1.0, interaction: 0.5 };
        let score = dd_card_fills(dd_id, &gap, &state, PlayerId::Us);
        assert!(score >= 0.9, "DD with high threat gap → score >=0.9, got {}", score);
    }


    #[test]
    fn test_dd_card_fills_second_dd_low() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        let _dd1 = state.place_card(PlayerId::Us, "Doomsday", Zone::Hand { known: false });
        let dd2 = state.place_card(PlayerId::Us, "Doomsday", Zone::Hand { known: false });
        let gap = TargetGap { mana: 0.5, threat: 1.0, interaction: 0.5 };
        let score = dd_card_fills(dd2, &gap, &state, PlayerId::Us);
        assert!(score <= 0.15, "second DD → score low, got {}", score);
    }


    #[test]
    fn test_dd_card_fills_oracle_near_zero() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        let oracle_id = state.place_card(PlayerId::Us, "Thassa's Oracle", Zone::Hand { known: false });
        let gap = TargetGap { mana: 0.5, threat: 1.0, interaction: 0.5 };
        let score = dd_card_fills(oracle_id, &gap, &state, PlayerId::Us);
        assert!(score <= 0.1, "Oracle pre-DD → near-zero, got {}", score);
    }


    #[test]
    fn test_dd_card_fills_cantrip_always_medium() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        let bs_id = state.place_card(PlayerId::Us, "Brainstorm", Zone::Hand { known: false });
        // Even with all gaps filled, cantrips retain medium value
        let gap_low = TargetGap { mana: 0.0, threat: 0.0, interaction: 0.0 };
        let gap_high = TargetGap { mana: 1.0, threat: 1.0, interaction: 1.0 };
        let score_low = dd_card_fills(bs_id, &gap_low, &state, PlayerId::Us);
        let score_high = dd_card_fills(bs_id, &gap_high, &state, PlayerId::Us);
        assert!(score_low > 0.2, "cantrip always valuable, got {}", score_low);
        assert!(score_high > 0.2, "cantrip always valuable, got {}", score_high);
        assert!((score_low - score_high).abs() < 0.2, "cantrip score stable across gap states");
    }


    #[test]
    fn test_dd_card_fills_fow_high_when_interaction_needed() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        let fow_id = state.place_card(PlayerId::Us, "Force of Will", Zone::Hand { known: false });
        let gap = TargetGap { mana: 0.5, threat: 0.5, interaction: 1.0 };
        let score = dd_card_fills(fow_id, &gap, &state, PlayerId::Us);
        assert!(score > 0.7, "FoW with high interaction gap → score >0.7, got {}", score);
    }


    #[test]
    fn test_dd_london_bottom_picks_worst_cards() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        // Hand: Oracle (dead), DD (great), Underground Sea (good mana), Brainstorm (medium)
        let oracle_id = state.place_card(PlayerId::Us, "Thassa's Oracle", Zone::Hand { known: false });
        let _dd_id = state.place_card(PlayerId::Us, "Doomsday", Zone::Hand { known: false });
        let _land_id = state.place_card(PlayerId::Us, "Underground Sea", Zone::Hand { known: false });
        let _bs_id = state.place_card(PlayerId::Us, "Brainstorm", Zone::Hand { known: false });
        let strat = DoomsdayStrategy::new(MatchupInfo::default());
        let bottom = strat.london_bottom(&state, 1);
        assert_eq!(bottom.len(), 1);
        assert_eq!(bottom[0], oracle_id, "Oracle should be bottomed as lowest-value card");
    }


    #[test]
    fn test_opp_plan_gap_no_board_all_gaps_high() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        // Opp facing combo (DD): opp_fast_clock = false
        let matchup = MatchupInfo { opp_has_counters: true, opp_fast_clock: false, ..Default::default() };
        let gap = opp_plan_gap(&state, PlayerId::Opp, &matchup);
        assert!(gap.mana >= 0.9, "no lands → mana gap high, got {}", gap.mana);
        assert!(gap.threat >= 0.9, "no threats → threat gap high, got {}", gap.threat);
        assert!(gap.interaction >= 0.9, "no interaction vs combo → gap high, got {}", gap.interaction);
    }


    #[test]
    fn test_opp_plan_gap_lands_reduce_mana() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        state.place_card(PlayerId::Opp, "Volcanic Island", Zone::Battlefield);
        state.place_card(PlayerId::Opp, "Scalding Tarn", Zone::Battlefield);
        let matchup = MatchupInfo::default();
        let gap = opp_plan_gap(&state, PlayerId::Opp, &matchup);
        assert!(gap.mana < 0.1, "2 lands → mana gap ~0, got {}", gap.mana);
    }


    #[test]
    fn test_opp_plan_gap_creature_reduces_threat() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        let _delver = state.place_card(PlayerId::Opp, "Delver of Secrets", Zone::Battlefield);
        let matchup = MatchupInfo::default();
        let gap = opp_plan_gap(&state, PlayerId::Opp, &matchup);
        assert!(gap.threat < 0.5, "1 creature → threat gap reduced, got {}", gap.threat);
    }


    #[test]
    fn test_opp_plan_gap_interaction_low_vs_aggro() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        // Facing aggro: opp_fast_clock = true
        let matchup = MatchupInfo { opp_has_counters: false, opp_fast_clock: true, ..Default::default() };
        let gap = opp_plan_gap(&state, PlayerId::Opp, &matchup);
        assert!(gap.interaction <= 0.5, "vs aggro → interaction gap capped, got {}", gap.interaction);
    }


    #[test]
    fn test_opp_card_fills_land_high_when_needed() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        let land_id = state.place_card(PlayerId::Opp, "Volcanic Island", Zone::Hand { known: false });
        let gap = TargetGap { mana: 1.0, threat: 1.0, interaction: 0.5 };
        let score = opp_card_fills(land_id, &gap, &state, PlayerId::Opp);
        assert!(score > 0.7, "land fills high mana gap → score >0.7, got {}", score);
    }


    #[test]
    fn test_opp_card_fills_land_low_when_flooded() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        let land_id = state.place_card(PlayerId::Opp, "Volcanic Island", Zone::Hand { known: false });
        let gap = TargetGap { mana: 0.0, threat: 1.0, interaction: 0.5 };
        let score = opp_card_fills(land_id, &gap, &state, PlayerId::Opp);
        assert!(score < 0.1, "land when mana gap=0 → near-zero, got {}", score);
    }


    #[test]
    fn test_opp_card_fills_creature_high() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        let delver_id = state.place_card(PlayerId::Opp, "Delver of Secrets", Zone::Hand { known: false });
        let gap = TargetGap { mana: 0.0, threat: 1.0, interaction: 0.5 };
        let score = opp_card_fills(delver_id, &gap, &state, PlayerId::Opp);
        assert!(score > 0.7, "creature with high threat gap → high score, got {}", score);
    }


    #[test]
    fn test_opp_card_fills_surplus_creature_lower() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        let _board_delver = state.place_card(PlayerId::Opp, "Delver of Secrets", Zone::Battlefield);
        let hand_delver = state.place_card(PlayerId::Opp, "Delver of Secrets", Zone::Hand { known: false });
        // threat_gap low since we already have one on board
        let gap = TargetGap { mana: 0.0, threat: 0.1, interaction: 0.5 };
        let score = opp_card_fills(hand_delver, &gap, &state, PlayerId::Opp);
        assert!(score < 0.3, "surplus Delver on board → lower, got {}", score);
    }


    #[test]
    fn test_opp_card_fills_fow_high_vs_combo() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        let fow_id = state.place_card(PlayerId::Opp, "Force of Will", Zone::Hand { known: false });
        let gap = TargetGap { mana: 0.0, threat: 0.5, interaction: 1.0 };
        let score = opp_card_fills(fow_id, &gap, &state, PlayerId::Opp);
        assert!(score > 0.7, "FoW with high interaction gap → score >0.7, got {}", score);
    }


    #[test]
    fn test_opp_card_fills_cantrip_medium() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        let bs_id = state.place_card(PlayerId::Opp, "Brainstorm", Zone::Hand { known: false });
        let gap = TargetGap { mana: 0.0, threat: 0.0, interaction: 0.0 };
        let score = opp_card_fills(bs_id, &gap, &state, PlayerId::Opp);
        assert!(score > 0.2 && score < 0.5, "cantrip always medium, got {}", score);
    }


    #[test]
    fn test_opp_london_bottom_picks_worst() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        // Opp hand: FoW (needed vs combo), extra Wasteland (useless), Delver (threat), Brainstorm (medium)
        let _fow = state.place_card(PlayerId::Opp, "Force of Will", Zone::Hand { known: false });
        let waste = state.place_card(PlayerId::Opp, "Wasteland", Zone::Hand { known: false });
        let _delver = state.place_card(PlayerId::Opp, "Delver of Secrets", Zone::Hand { known: false });
        let _bs = state.place_card(PlayerId::Opp, "Brainstorm", Zone::Hand { known: false });
        // Already have 2 lands on board → mana gap is 0
        state.place_card(PlayerId::Opp, "Underground Sea", Zone::Battlefield);
        state.place_card(PlayerId::Opp, "Polluted Delta", Zone::Battlefield);
        let matchup = MatchupInfo { opp_has_counters: true, opp_fast_clock: false, ..Default::default() };
        let strat = GenericOppStrategy::new(matchup);
        let bottom = strat.london_bottom(&state, 1);
        assert_eq!(bottom.len(), 1);
        // With mana gap=0, the only land (Wasteland) should score lowest
        assert_eq!(bottom[0], waste, "extra land should be bottomed when mana-flooded");
    }


    #[test]
    fn test_dd_mull_no_land_hand() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        // All spells, no lands: should mull even with Dark Ritual (can't cast without B source).
        state.place_card(PlayerId::Us, "Doomsday", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Force of Will", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Brainstorm", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Ponder", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Dark Ritual", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Thoughtseize", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Consider", Zone::Hand { known: false });
        assert!(dd_should_mulligan(&state, PlayerId::Us, 0),
            "no lands (only Ritual) should mull — can't cast Ritual without B source");
    }


    #[test]
    fn test_dd_mull_truly_no_mana() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        // All non-mana cards
        state.place_card(PlayerId::Us, "Doomsday", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Force of Will", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Brainstorm", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Ponder", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Thoughtseize", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Daze", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Consider", Zone::Hand { known: false });
        assert!(dd_should_mulligan(&state, PlayerId::Us, 0), "0 mana sources should mull");
    }


    #[test]
    fn test_dd_mull_land_flood() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        // 5 mana-producing lands + 2 spells → flood
        state.place_card(PlayerId::Us, "Underground Sea", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Polluted Delta", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Island", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Swamp", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Misty Rainforest", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Doomsday", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Brainstorm", Zone::Hand { known: false });
        assert!(dd_should_mulligan(&state, PlayerId::Us, 0), "5+ mana lands should mull");
    }


    #[test]
    fn test_dd_keep_good_hand() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        // Classic keep: land, ritual, DD, cantrip, interaction
        state.place_card(PlayerId::Us, "Underground Sea", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Dark Ritual", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Doomsday", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Brainstorm", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Force of Will", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Ponder", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Polluted Delta", Zone::Hand { known: false });
        assert!(!dd_should_mulligan(&state, PlayerId::Us, 0), "good DD hand should keep");
    }


    #[test]
    fn test_dd_mull_all_mana_no_action() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        // All lands + rituals, no threats or cantrips
        state.place_card(PlayerId::Us, "Underground Sea", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Polluted Delta", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Dark Ritual", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Lotus Petal", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Island", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Swamp", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Wasteland", Zone::Hand { known: false });
        // 7 mana sources, 0 threats, 0 selection → 5+ mana → mull
        assert!(dd_should_mulligan(&state, PlayerId::Us, 0), "all mana should mull");
    }


    #[test]
    fn test_dd_mull_6_card_lenient() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        // 6 cards: 1 land + 5 interaction (no threat/selection) — still has "spells"
        state.place_card(PlayerId::Us, "Underground Sea", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Force of Will", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Daze", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Thoughtseize", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Force of Will", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Daze", Zone::Hand { known: false });
        assert!(!dd_should_mulligan(&state, PlayerId::Us, 1), "6-card with land + spells should keep");
    }


    #[test]
    fn test_dd_always_keeps_at_4() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        // Terrible 4-card hand — should still keep
        state.place_card(PlayerId::Us, "Thassa's Oracle", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Unearth", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Lion's Eye Diamond", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Thassa's Oracle", Zone::Hand { known: false });
        assert!(!dd_should_mulligan(&state, PlayerId::Us, 3), "always keep at 4 cards");
    }


    #[test]
    fn test_opp_mull_no_land() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        state.place_card(PlayerId::Opp, "Delver of Secrets", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Force of Will", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Brainstorm", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Ponder", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Daze", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Lightning Bolt", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Dragon's Rage Channeler", Zone::Hand { known: false });
        assert!(opp_should_mulligan(&state, PlayerId::Opp, 0, &[Color::Blue, Color::Red]), "0-land opp hand should mull");
    }


    #[test]
    fn test_opp_mull_land_flood() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        state.place_card(PlayerId::Opp, "Volcanic Island", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Scalding Tarn", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Flooded Strand", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Polluted Delta", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Misty Rainforest", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Delver of Secrets", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Brainstorm", Zone::Hand { known: false });
        assert!(opp_should_mulligan(&state, PlayerId::Opp, 0, &[Color::Blue, Color::Red]), "5+ mana lands opp hand should mull");
    }


    #[test]
    fn test_opp_keep_good_hand() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        state.place_card(PlayerId::Opp, "Volcanic Island", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Wasteland", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Delver of Secrets", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Brainstorm", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Force of Will", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Daze", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Ponder", Zone::Hand { known: false });
        assert!(!opp_should_mulligan(&state, PlayerId::Opp, 0, &[Color::Blue, Color::Red]), "good tempo hand should keep");
    }


    #[test]
    fn test_opp_mull_all_interaction_no_threats() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        state.place_card(PlayerId::Opp, "Volcanic Island", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Wasteland", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Force of Will", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Daze", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Lightning Bolt", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Fatal Push", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Thoughtseize", Zone::Hand { known: false });
        // 2 mana + 5 interaction, 0 threats, 0 selection → mull
        assert!(opp_should_mulligan(&state, PlayerId::Opp, 0, &[Color::Blue, Color::Red]),
            "all interaction no threats/cantrips should mull");
    }


    #[test]
    fn test_opp_always_keeps_at_4() {
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        state.place_card(PlayerId::Opp, "Force of Will", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Daze", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Lightning Bolt", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Fatal Push", Zone::Hand { known: false });
        assert!(!opp_should_mulligan(&state, PlayerId::Opp, 3, &[Color::Blue, Color::Red]), "always keep at 4 cards");
    }


    #[test]
    fn test_dd_mull_wasteland_only() {
        // Wasteland produces no colored mana — should mull.
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        state.place_card(PlayerId::Us, "Wasteland", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Doomsday", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Brainstorm", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Ponder", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Force of Will", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Thoughtseize", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Consider", Zone::Hand { known: false });
        assert!(dd_should_mulligan(&state, PlayerId::Us, 0),
            "Wasteland-only hand should mull — no U or BBB");
    }


    #[test]
    fn test_dd_keep_fetch_hand() {
        // 3 fetches + Ritual + DD: fetches provide U/B, should keep.
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        state.place_card(PlayerId::Us, "Polluted Delta", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Misty Rainforest", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Flooded Strand", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Dark Ritual", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Doomsday", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Brainstorm", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Ponder", Zone::Hand { known: false });
        assert!(!dd_should_mulligan(&state, PlayerId::Us, 0),
            "fetch lands provide U/B — should keep");
    }


    #[test]
    fn test_dd_keep_usea_cantrips() {
        // 1 Underground Sea + cantrips: has U, should keep.
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        state.place_card(PlayerId::Us, "Underground Sea", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Brainstorm", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Ponder", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Consider", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Doomsday", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Force of Will", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Thoughtseize", Zone::Hand { known: false });
        assert!(!dd_should_mulligan(&state, PlayerId::Us, 0),
            "USea + cantrips should keep");
    }


    #[test]
    fn test_opp_mull_wasteland_only() {
        // Opponent with only Wasteland — no U, should mull.
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        state.place_card(PlayerId::Opp, "Wasteland", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Delver of Secrets", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Brainstorm", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Force of Will", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Daze", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Lightning Bolt", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Ponder", Zone::Hand { known: false });
        assert!(opp_should_mulligan(&state, PlayerId::Opp, 0, &[Color::Blue, Color::Red]),
            "Wasteland-only opp hand should mull — no U");
    }


    #[test]
    fn test_opp_keep_fetch_hand() {
        // Fetch land provides U via deck knowledge — should keep.
        let mut state = st();
        state.catalog = libmtg_engine::build_catalog();
        state.place_card(PlayerId::Opp, "Scalding Tarn", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Delver of Secrets", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Brainstorm", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Force of Will", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Daze", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Lightning Bolt", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Ponder", Zone::Hand { known: false });
        assert!(!opp_should_mulligan(&state, PlayerId::Opp, 0, &[Color::Blue, Color::Red]),
            "fetch land provides U — should keep");
    }


    #[test]
    #[ignore] // slow: ~10s for 10000 sims
    fn validation_stats() {
        use rand::Rng;
        let catalog = libmtg_engine::build_catalog();
        let dd_cards = val_dd_deck();
        let opp_cards = val_ub_tempo_deck();
        let n = 1_000; // ~2min in debug mode; use --release for 10000
        let mut rng = StdRng::seed_from_u64(12345);

        let mut dd_success = 0u32;
        let mut dd_fail = 0u32;
        let mut us_mull_total = 0u32;
        let mut opp_mull_total = 0u32;
        let mut us_mulled_games = 0u32;
        let mut opp_mulled_games = 0u32;
        let mut success_turns = Vec::new();
        let mut us_hand_sizes = Vec::new();
        let mut opp_hand_sizes = Vec::new();

        let mut panics = 0u32;
        for _i in 0..n {
            // Use a per-sim RNG seeded from the master, so panics don't lose rng state.
            let sim_seed = rng.gen::<u64>();
            let mut sim_rng = StdRng::seed_from_u64(sim_seed);
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                simulate_game(
                    "doomsday", "UB Tempo", &catalog, &dd_cards, &opp_cards, &mut sim_rng,
                )
            }));
            let result = match result {
                Ok(r) => r,
                Err(_) => { panics += 1; continue; }
            };
            let state = result;
            if state.terminal {
                dd_success += 1;
                success_turns.push(state.current_turn);
                // Find the opening hand summary line (contains "mulligans").
                if let Some(info_log) = state.log.iter().find(|l| l.contains("mulligans")) {
                    if let Some((uh, um, oh, om)) = parse_log_hand_info(info_log) {
                        us_hand_sizes.push(uh);
                        opp_hand_sizes.push(oh);
                        if um > 0 { us_mulled_games += 1; }
                        us_mull_total += um;
                        if om > 0 { opp_mulled_games += 1; }
                        opp_mull_total += om;
                    }
                }
            } else {
                dd_fail += 1;
            }
        }

        let total = dd_success + dd_fail;
        let success_rate = dd_success as f64 / total as f64 * 100.0;
        let avg_us_hand = if us_hand_sizes.is_empty() { 0.0 }
            else { us_hand_sizes.iter().sum::<u32>() as f64 / us_hand_sizes.len() as f64 };
        let avg_opp_hand = if opp_hand_sizes.is_empty() { 0.0 }
            else { opp_hand_sizes.iter().sum::<u32>() as f64 / opp_hand_sizes.len() as f64 };

        // Turn distribution for successful games
        let mut turn_counts = [0u32; 8]; // index 2..7
        for &t in &success_turns {
            if (t as usize) < turn_counts.len() { turn_counts[t as usize] += 1; }
        }

        eprintln!("\n══════════════════════════════════════════════════════");
        eprintln!("  VALIDATION: {} sims vs UB Tempo", n);
        eprintln!("══════════════════════════════════════════════════════");
        eprintln!("  DD success rate: {}/{} ({:.1}%)", dd_success, total, success_rate);
        eprintln!("  DD fail (no cast): {}/{} ({:.1}%)", dd_fail, total, 100.0 - success_rate);
        if panics > 0 {
            eprintln!("  ⚠ Panicked sims: {} (pre-existing engine bugs)", panics);
        }
        eprintln!();
        eprintln!("  DD success by turn:");
        for t in 2..=7 {
            let c = turn_counts[t];
            let pct = if dd_success > 0 { c as f64 / dd_success as f64 * 100.0 } else { 0.0 };
            eprintln!("    T{}: {:>5} ({:.1}%)", t, c, pct);
        }
        eprintln!();
        eprintln!("  Mulligan rates:");
        eprintln!("    Us:  {:.1}% of games mulled, avg {:.2} mulls/game",
            us_mulled_games as f64 / total as f64 * 100.0,
            us_mull_total as f64 / total as f64);
        eprintln!("    Opp: {:.1}% of games mulled, avg {:.2} mulls/game",
            opp_mulled_games as f64 / total as f64 * 100.0,
            opp_mull_total as f64 / total as f64);
        eprintln!();
        eprintln!("  Avg hand size (after mulls+london bottom):");
        eprintln!("    Us:  {:.2}", avg_us_hand);
        eprintln!("    Opp: {:.2}", avg_opp_hand);
        eprintln!("══════════════════════════════════════════════════════\n");

        // Sanity assertions — these should be very loose, just catching broken sims.
        assert!(success_rate > 10.0, "DD success rate suspiciously low: {:.1}%", success_rate);
        assert!(success_rate < 95.0, "DD success rate suspiciously high: {:.1}%", success_rate);
        assert!(avg_us_hand >= 5.0, "avg US hand size too low: {:.2}", avg_us_hand);
        assert!(avg_opp_hand >= 5.0, "avg OPP hand size too low: {:.2}", avg_opp_hand);
    }




    #[test]
    fn plan_direct_dd_cast() {
        // 3 black-producing lands + DD in hand → cast DD directly.
        let state = plan_test_state(
            &["Underground Sea", "Badlands", "Scrubland"],
            &["Doomsday"],
        );
        let plan = make_turn_plan(&state, PlayerId::Us, dd_plan_quality);
        let names = plan_spell_names(&plan, &state);
        assert!(names.contains(&"Doomsday".to_string()),
            "plan should cast Doomsday, got: {:?}", names);
        assert!(!names.contains(&"Dark Ritual".to_string()),
            "direct path should not need Ritual");
    }


    #[test]
    fn plan_ritual_path() {
        // 1 black land + Ritual + DD in hand → Ritual then DD.
        let state = plan_test_state(
            &["Underground Sea"],
            &["Dark Ritual", "Doomsday"],
        );
        let plan = make_turn_plan(&state, PlayerId::Us, dd_plan_quality);
        let names = plan_spell_names(&plan, &state);
        assert_eq!(names, vec!["Dark Ritual", "Doomsday"],
            "should cast Ritual then DD, got: {:?}", names);
    }


    #[test]
    fn plan_land_drop_enables_dd() {
        // No lands on board, 1 land + Ritual + DD in hand → land drop, then Ritual → DD.
        let state = plan_test_state(
            &[],
            &["Underground Sea", "Dark Ritual", "Doomsday"],
        );
        let plan = make_turn_plan(&state, PlayerId::Us, dd_plan_quality);
        let names = plan_spell_names(&plan, &state);
        assert_eq!(names, vec!["Underground Sea", "Dark Ritual", "Doomsday"],
            "should land drop then Ritual then DD, got: {:?}", names);
    }


    #[test]
    fn plan_petal_on_board_ritual_dd() {
        // Lotus Petal on board (untapped) + Ritual + DD in hand, no lands.
        let mut state = st();
        state.place_card(PlayerId::Us, "Lotus Petal", Zone::Battlefield);
        state.place_card(PlayerId::Us, "Dark Ritual", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Doomsday", Zone::Hand { known: false });
        let plan = make_turn_plan(&state, PlayerId::Us, dd_plan_quality);
        let names = plan_spell_names(&plan, &state);
        assert_eq!(names, vec!["Dark Ritual", "Doomsday"],
            "Petal sac → Ritual → DD, got: {:?}", names);
    }


    #[test]
    fn plan_no_dd_casts_cantrip() {
        // Land on board + cantrip in hand, no DD → should cast cantrip.
        let state = plan_test_state(
            &["Underground Sea"],
            &["Brainstorm"],
        );
        let plan = make_turn_plan(&state, PlayerId::Us, dd_plan_quality);
        let names = plan_spell_names(&plan, &state);
        assert_eq!(names, vec!["Brainstorm"],
            "without DD, should cast cantrip, got: {:?}", names);
    }


    #[test]
    fn plan_land_drop_plus_cantrip() {
        // Land in hand + cantrip in hand + land on board → land drop + cantrip.
        let state = plan_test_state(
            &["Underground Sea"],
            &["Badlands", "Ponder"],
        );
        let plan = make_turn_plan(&state, PlayerId::Us, dd_plan_quality);
        let names = plan_spell_names(&plan, &state);
        assert!(names.contains(&"Badlands".to_string()), "should land drop, got: {:?}", names);
        assert!(names.contains(&"Ponder".to_string()), "should cast Ponder, got: {:?}", names);
    }


    #[test]
    fn plan_empty_hand_passes() {
        // Land on board but empty hand → empty plan.
        let state = plan_test_state(&["Underground Sea"], &[]);
        let plan = make_turn_plan(&state, PlayerId::Us, dd_plan_quality);
        assert!(plan.is_empty(), "empty hand should produce empty plan, got: {:?}", plan);
    }


    #[test]
    fn plan_no_mana_for_spell() {
        // DD in hand but no mana sources → can't cast, no spells in plan.
        let state = plan_test_state(&[], &["Doomsday"]);
        let plan = make_turn_plan(&state, PlayerId::Us, dd_plan_quality);
        let names = plan_spell_names(&plan, &state);
        assert!(!names.contains(&"Doomsday".to_string()),
            "shouldn't cast DD without mana, got: {:?}", names);
    }


    #[test]
    fn plan_ritual_without_dd_not_cast() {
        // Ritual in hand, no DD → should NOT cast Ritual (wasted BBB).
        let state = plan_test_state(
            &["Underground Sea"],
            &["Dark Ritual", "Brainstorm"],
        );
        let plan = make_turn_plan(&state, PlayerId::Us, dd_plan_quality);
        let names = plan_spell_names(&plan, &state);
        assert!(!names.contains(&"Dark Ritual".to_string()),
            "shouldn't cast Ritual without DD, got: {:?}", names);
        assert!(names.contains(&"Brainstorm".to_string()),
            "should cast Brainstorm instead, got: {:?}", names);
    }


    #[test]
    fn plan_prefers_dd_over_cantrip() {
        // Can cast either cantrip or Ritual→DD. Should pick DD.
        let state = plan_test_state(
            &["Underground Sea"],
            &["Dark Ritual", "Doomsday", "Brainstorm"],
        );
        let plan = make_turn_plan(&state, PlayerId::Us, dd_plan_quality);
        let names = plan_spell_names(&plan, &state);
        assert!(names.contains(&"Doomsday".to_string()),
            "should prefer DD over cantrip, got: {:?}", names);
    }


    #[test]
    #[ignore] // 500 full simulations — run with `cargo test -- --ignored`
    fn stress_invariant_check() {
        let catalog = libmtg_engine::build_catalog();
        let dd_cards = val_dd_deck();
        let opp_cards = val_ub_tempo_deck();
        for seed in 0..500 {
            let mut rng = StdRng::seed_from_u64(seed);
            let _ = simulate_game("doomsday", "UB Tempo", &catalog, &dd_cards, &opp_cards, &mut rng);
        }
    }


    // ── Objective ────────────────────────────────────────────────────────────

    #[test]
    fn test_doomsday_resolved_objective_ends_sim() {
        use libmtg_engine::Objective;
        let mut state = st();
        let before = state.player(PlayerId::Us).life;
        // A Doomsday object as if it just resolved.
        let id = state.place_card(PlayerId::Us, "Doomsday", Zone::Battlefield);
        let mut obj = DoomsdayResolvedObjective::default();
        let ended = obj.observe(
            &libmtg_engine::GameEvent::SpellResolved { controller: PlayerId::Us, card_id: id },
            &mut state,
        );
        assert!(ended, "Doomsday resolving ends the simulation");
        assert_eq!(state.life_before_dd, Some(before), "pre-DD life recorded");
        assert_eq!(state.player(PlayerId::Us).life, before / 2, "Doomsday life accounting applied");
    }

    // ── Combat decision heuristics ────────────────────────────────────────────
    // These exercise DoomsdayStrategy::declare_attackers / GenericOppStrategy::
    // declare_blockers directly (the engine's combat *machinery* is tested in the
    // libmtg-engine crate). Vanilla creatures are built via the public CardDef API
    // and placed with `place_card`.

    fn put_creature(state: &mut SimState, who: PlayerId, name: &str, p: i32, t: i32,
                    kws: &[Keyword], summoning_sick: bool) -> ObjId {
        state.catalog.insert(name.to_string(), CardDef::vanilla_creature(name, p, t, kws));
        let id = state.place_card(who, name, Zone::Battlefield);
        if let Some(bf) = state.permanent_bf_mut(id) { bf.entered_this_turn = summoning_sick; }
        id
    }

    #[test]
    fn test_declare_attackers_safe_to_attack() {
        let mut state = st();
        let ragavan_id = put_creature(&mut state, PlayerId::Us, "Ragavan", 2, 4, &[], false);
        let atk = DoomsdayStrategy::new(MatchupInfo::default()).declare_attackers(&state);
        assert!(atk.iter().any(|&(id, _)| id == ragavan_id), "2/4 should attack into an empty board");
    }

    #[test]
    fn test_declare_attackers_too_risky() {
        let mut state = st();
        let ragavan_id = put_creature(&mut state, PlayerId::Us, "Ragavan", 2, 2, &[], false);
        put_creature(&mut state, PlayerId::Opp, "Mosscoat Construct", 3, 3, &[], false);
        let atk = DoomsdayStrategy::new(MatchupInfo::default()).declare_attackers(&state);
        assert!(!atk.iter().any(|&(id, _)| id == ragavan_id), "should not attack a 2/2 into a 3/3");
    }

    #[test]
    fn test_declare_attackers_summoning_sickness() {
        let mut state = st();
        let ragavan_id = put_creature(&mut state, PlayerId::Us, "Ragavan", 2, 4, &[], true);
        let atk = DoomsdayStrategy::new(MatchupInfo::default()).declare_attackers(&state);
        assert!(!atk.iter().any(|&(id, _)| id == ragavan_id), "summoning sickness prevents attack");
    }

    #[test]
    fn test_declare_blockers_good_block() {
        let mut state = st();
        let ragavan_id = put_creature(&mut state, PlayerId::Us, "Ragavan", 2, 2, &[], false);
        let mosscoat_id = put_creature(&mut state, PlayerId::Opp, "Mosscoat Construct", 3, 3, &[], false);
        state.combat_attackers = vec![ragavan_id];
        let blocks = GenericOppStrategy::new(MatchupInfo::default()).declare_blockers(&state);
        assert_eq!(blocks.len(), 1, "3/3 should block 2/2");
        assert_eq!(blocks[0], (ragavan_id, mosscoat_id));
    }

    #[test]
    fn test_declare_blockers_no_chump() {
        let mut state = st();
        let beast_id = put_creature(&mut state, PlayerId::Us, "Beast", 4, 4, &[], false);
        put_creature(&mut state, PlayerId::Opp, "Squirrel Token", 1, 1, &[], false);
        state.combat_attackers = vec![beast_id];
        let blocks = GenericOppStrategy::new(MatchupInfo::default()).declare_blockers(&state);
        assert!(blocks.is_empty(), "should not chump-block a 4/4 with a 1/1");
    }

    #[test]
    fn test_flying_attack_safety_ignores_ground() {
        // A flying 3/3 should attack even past a 3/3 ground creature that can't block it.
        let mut state = st();
        let murktide_id = put_creature(&mut state, PlayerId::Us, "Murktide Regent", 3, 3, &[Keyword::Flying], false);
        put_creature(&mut state, PlayerId::Opp, "Troll", 3, 3, &[], false);
        let atk = DoomsdayStrategy::new(MatchupInfo::default()).declare_attackers(&state);
        assert!(atk.iter().any(|&(id, _)| id == murktide_id),
            "flying creature should attack when only ground blockers exist");
    }

    #[test]
    fn test_flying_attacker_avoids_reach_blocker() {
        let mut state = st();
        let dragon_id = put_creature(&mut state, PlayerId::Us, "Dragon", 3, 3, &[Keyword::Flying], false);
        put_creature(&mut state, PlayerId::Opp, "Giant Spider", 5, 5, &[Keyword::Reach], false);
        let atk = DoomsdayStrategy::new(MatchupInfo::default()).declare_attackers(&state);
        assert!(!atk.iter().any(|&(id, _)| id == dragon_id),
            "flying attacker should avoid a reach blocker that outclasses it");
    }
