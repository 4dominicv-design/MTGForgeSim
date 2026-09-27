//! Bounded matchup search under an explicit open-decklist information model.
use libmtg_engine::{simulate_priority_action, AnnounceChoice, AnnounceOptions, CounterType,
    LegalAction, ObjId, PhaseKind, PlayerId, SimState, Strategy, TargetGap, TargetSpec,
    TurnPosition, WishOption};
use serde::{Deserialize, Serialize};
use crate::{agent::{build_agent_observation, legal_action_id}, search::evaluate_forge_state,
    strategy::{BaselineOpponentStrategy, ForgeStrategy}};

pub const LOOKAHEAD_VERSION: &str = "forge_matchup_lookahead_v1";

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct LookaheadConfig {
    pub samples: usize,
    pub candidates: usize,
    /// Additional heuristic actions per player in the current priority window.
    pub continuation_actions: usize,
    pub decisions_per_game: usize,
}
impl Default for LookaheadConfig {
    fn default() -> Self { Self { samples: 1, candidates: 4, continuation_actions: 2, decisions_per_game: 8 } }
}
impl LookaheadConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=16).contains(&self.samples) || !(2..=16).contains(&self.candidates)
            || !(0..=8).contains(&self.continuation_actions) || !(1..=1000).contains(&self.decisions_per_game) {
            return Err("search requires samples 1..16, candidates 2..16, depth 0..8, search-decisions 1..1000".into());
        }
        Ok(())
    }
}

pub struct MatchupLookaheadStrategy {
    who: PlayerId,
    cfg: LookaheadConfig,
    seed: u64,
    decision: usize,
    base: ForgeStrategy,
    records: Vec<String>,
}
impl MatchupLookaheadStrategy {
    pub fn new(who: PlayerId, cfg: LookaheadConfig, seed: u64) -> Self {
        Self { who, cfg, seed, decision: 0, base: ForgeStrategy::new(who), records: Vec::new() }
    }

    fn score(state: &SimState, who: PlayerId) -> f64 {
        if state.invalid_actions > 0 || state.player(who).life <= 0 { return -1_000_000.0; }
        if state.winner == Some(who) { return 1_000_000.0; }
        if state.winner == Some(who.opp()) { return -1_000_000.0; }
        let material = |p| state.permanents_of(p).filter_map(|o| state.def_of(o.id)?.as_creature())
            .map(|c| 1.5 * c.power().max(0) as f64 + 0.2 * c.toughness().max(0) as f64).sum::<f64>();
        let incoming: i32 = state.permanents_of(who.opp()).filter_map(|o| state.def_of(o.id)?.as_creature())
            .map(|c| c.power().max(0)).sum();
        let life = state.player(who).life;
        let protected = state.has_player_protection(who);
        let danger = if !protected && incoming >= life { 40.0 } else { 0.0 };
        let burden: u32 = state.permanents_of(who).filter(|o| o.catalog_key == "The One Ring")
            .map(|o| state.counter_count(o.id, CounterType::Burden)).sum();
        evaluate_forge_state(state, who) + material(who) - material(who.opp())
            + life.min(20) as f64 * 0.6 - danger - burden as f64
            + if protected { 10.0 + incoming as f64 } else { 0.0 }
    }
}

impl Strategy for MatchupLookaheadStrategy {
    fn choose_action(&mut self, state: &SimState, ap: PlayerId, legal: &[LegalAction]) -> LegalAction {
        if self.decision >= self.cfg.decisions_per_game || ap != self.who || !state.stack.is_empty()
            || !matches!(state.current_phase, Some(TurnPosition::Phase(PhaseKind::PreCombatMain | PhaseKind::PostCombatMain)))
            || legal.len() <= 1 {
            return self.base.choose_action(state, ap, legal);
        }
        let seed = self.seed.wrapping_add((self.decision as u64).wrapping_mul(0x9e3779b97f4a7c15));
        // Candidate filtering also uses a sampled state, not real hidden cards.
        let view = state.sampled_search_state(self.who, seed);
        let fallback = ForgeStrategy::new(self.who).choose_action(&view, ap, legal);
        let mut candidates = vec![fallback.clone()];
        if fallback != LegalAction::Pass { candidates.push(LegalAction::Pass); }
        // Keep heuristic safeguards: do not force no-progress or lethal actions.
        let mut remaining = legal.to_vec();
        while candidates.len() < self.cfg.candidates {
            remaining.retain(|a| !candidates.contains(a));
            if remaining.is_empty() { break; }
            remaining.push(LegalAction::Pass);
            let next = ForgeStrategy::new(self.who).choose_action(&view, ap, &remaining);
            if next == LegalAction::Pass { break; }
            candidates.push(next);
        }
        if candidates.len() < 2 { return fallback; }
        let mut best = (f64::NEG_INFINITY, fallback.clone());
        let mut scores = Vec::new();
        for action in &candidates {
            let mut total = 0.0;
            let mut valid = true;
            for sample in 0..self.cfg.samples {
                let rollout_seed = seed.wrapping_add(sample as u64);
                let sampled = state.sampled_search_state(self.who, rollout_seed);
                let us: Box<dyn Strategy> = Box::new(ForgeStrategy::bounded(self.who, self.cfg.continuation_actions));
                let opponent: Box<dyn Strategy> = Box::new(BaselineOpponentStrategy::bounded(self.who.opp(), self.cfg.continuation_actions));
                let (us, opp) = if self.who == PlayerId::Us { (us, opponent) } else { (opponent, us) };
                let branch = simulate_priority_action(&sampled, ap, self.who, action.clone(), rollout_seed, us, opp);
                if branch.invalid_actions != state.invalid_actions { valid = false; break; }
                total += Self::score(&branch, self.who);
            }
            if !valid { continue; }
            let score = total / self.cfg.samples as f64;
            scores.push(serde_json::json!({"action_id": legal_action_id(action, state), "score": score}));
            // Prefer the existing survival policy on ties.
            if score > best.0 + 1e-9 { best = (score, action.clone()); }
        }
        self.decision += 1;
        if !best.0.is_finite() { return fallback; }
        self.records.push(format!("MATCHUP_SEARCH\t{}", serde_json::json!({
            "schema": LOOKAHEAD_VERSION, "decision": self.decision, "sampling_seed": seed,
            "config": self.cfg, "information_model": "open_decklists_sampled_hidden_cards",
            "observation": build_agent_observation(state, self.who, legal),
            "candidates": scores, "chosen_action_id": legal_action_id(&best.1, state),
            "label_type": "heuristic_search_score_not_optimal_play"
        })));
        best.1
    }
    fn declare_attackers(&mut self, s: &SimState) -> Vec<(ObjId, Option<ObjId>)> { self.base.declare_attackers(s) }
    fn declare_blockers(&mut self, s: &SimState) -> Vec<(ObjId, ObjId)> { self.base.declare_blockers(s) }
    fn take_mulligan(&mut self, s: &SimState, n: u32) -> bool { self.base.take_mulligan(s, n) }
    fn announce(&mut self, s: &SimState, id: ObjId, o: &AnnounceOptions) -> AnnounceChoice { self.base.announce(s, id, o) }
    fn choose_targets(&mut self, s: &SimState, id: ObjId, choices: &[ObjId], spec: &TargetSpec) -> Vec<ObjId> { self.base.choose_targets(s, id, choices, spec) }
    fn choose_legend_to_keep(&mut self, s: &SimState, choices: &[ObjId]) -> Option<ObjId> { self.base.choose_legend_to_keep(s, choices) }
    fn choose_wish(&mut self, id: ObjId, choices: &[WishOption], s: &SimState) -> Option<WishOption> { self.base.choose_wish(id, choices, s) }
    fn choose_transmute_sacrifice(&mut self, id: ObjId, who: PlayerId, choices: &[ObjId], s: &SimState) -> Option<ObjId> { self.base.choose_transmute_sacrifice(id, who, choices, s) }
    fn choose_transmute_target(&mut self, id: ObjId, choices: &[ObjId], payable: &[ObjId], s: &SimState) -> Option<ObjId> { self.base.choose_transmute_target(id, choices, payable, s) }
    fn player_id(&self) -> PlayerId { self.who }
    fn plan_gap(&self, s: &SimState) -> TargetGap { self.base.plan_gap(s) }
    fn card_fills(&self, id: ObjId, gap: &TargetGap, s: &SimState) -> f64 { self.base.card_fills(id, gap, s) }
    fn drain_decisions(&mut self) -> Vec<String> { std::mem::take(&mut self.records) }
}

#[cfg(test)]
mod tests {
    use super::*;
    use libmtg_engine::{build_catalog, PlayerState, Zone, SpellFace};
    #[test]
    fn hidden_order_does_not_change_choice_and_budget_is_bounded() {
        let mut state = SimState::new(PlayerState::new("us"), PlayerState::new("opp"));
        state.catalog = build_catalog();
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));
        state.player_mut(PlayerId::Us).pool.c = 6;
        state.player_mut(PlayerId::Us).pool.total = 6;
        let forge = state.place_card(PlayerId::Us, "Mystic Forge", Zone::Hand { known: false });
        let grim = state.place_card(PlayerId::Us, "Grim Monolith", Zone::Hand { known: false });
        state.place_card(PlayerId::Us, "Island", Zone::Library);
        state.place_card(PlayerId::Us, "Mountain", Zone::Library);
        state.place_card(PlayerId::Opp, "Lightning Bolt", Zone::Hand { known: false });
        state.place_card(PlayerId::Opp, "Mountain", Zone::Library);
        let mut other = state.fork_for_search(99);
        other.player_mut(PlayerId::Us).library_order.swap(0,1);
        let legal = [LegalAction::Pass, LegalAction::CastSpell { card_id: forge, face: SpellFace::Main }, LegalAction::CastSpell { card_id: grim, face: SpellFace::Main }];
        let cfg = LookaheadConfig { samples: 2, candidates: 3, continuation_actions: 1, decisions_per_game: 1 };
        let mut a = MatchupLookaheadStrategy::new(PlayerId::Us, cfg, 73);
        let mut b = MatchupLookaheadStrategy::new(PlayerId::Us, cfg, 73);
        assert_eq!(a.choose_action(&state, PlayerId::Us, &legal), b.choose_action(&other, PlayerId::Us, &legal));
        let records = a.drain_decisions();
        assert_eq!(records, b.drain_decisions());
        assert_eq!(records.len(),1);
        let record: serde_json::Value = serde_json::from_str(records[0].strip_prefix("MATCHUP_SEARCH\t").unwrap()).unwrap();
        assert!(record["observation"]["visible_top"].is_null());
        assert_eq!(record["observation"]["known_opponent_hand"].as_array().unwrap().len(),0);
        assert_eq!(a.choose_action(&state, PlayerId::Us, &legal), ForgeStrategy::new(PlayerId::Us).choose_action(&state, PlayerId::Us, &legal));
        assert!(a.drain_decisions().is_empty());
    }
}
