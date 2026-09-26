use libmtg_engine::{
    AnnounceChoice, AnnounceOptions, LegalAction, ObjId, PlayerId, SimState, Strategy,
    TargetGap, WishOption,
};

use crate::profile::{keep_v0, role_for, ForgeRole};

pub(crate) fn forge_announce_choice(
    state: &SimState,
    card_id: ObjId,
    options: &AnnounceOptions,
) -> AnnounceChoice {
    let name = state.objects.get(&card_id).map(|o| o.catalog_key.as_str()).unwrap_or("");
    let chosen_x = if options.has_x_cost {
        // In goldfish/search mode, Command wants the largest legal X by default.
        // Other X cards keep the engine's conservative X<=3 bootstrap behavior.
        if name == "Kozilek's Command" { options.max_x } else { options.max_x.min(3) }
    } else {
        0
    };
    AnnounceChoice { chosen_mode: 0, alt_cost_index: None, chosen_x }
}

/// Baseline Forge policy used as scaffolding for the eventual search agent.
/// It only chooses among actions the rules engine says are legal; card-specific
/// tactical search will replace this priority table incrementally.
pub struct ForgeStrategy {
    who: PlayerId,
}

impl ForgeStrategy {
    pub fn new(who: PlayerId) -> Self { Self { who } }

    fn card_priority(name: &str) -> i32 {
        match name {
            "Mystic Forge" => 100,
            "Karn, the Great Creator" => 95,
            "The One Ring" => 92,
            "Tezzeret, Cruel Captain" => 88,
            "Trinisphere" => 86,
            "Grim Monolith" => 84,
            "Basalt Monolith" => 83,
            "Manifold Key" => 80,
            "Transmute Artifact" => 78,
            "Relic of Sauron" => 72,
            "Paradox Engine" => 70,
            "Kozilek's Command" => 60,
            "Giant's Boulder" => 58,
            _ => 40,
        }
    }
}

impl Strategy for ForgeStrategy {
    fn declare_attackers(&mut self, _state: &SimState) -> Vec<(ObjId, Option<ObjId>)> { Vec::new() }
    fn declare_blockers(&mut self, _state: &SimState) -> Vec<(ObjId, ObjId)> { Vec::new() }

    fn take_mulligan(&mut self, state: &SimState, mulligans_taken: u32) -> bool {
        let hand: Vec<String> = state.hand_of(self.who).map(|c| c.catalog_key.clone()).collect();
        !keep_v0(&hand, mulligans_taken)
    }

    fn choose_action(&mut self, state: &SimState, _ap: PlayerId, legal: &[LegalAction]) -> LegalAction {
        // Make a land drop whenever the engine offers one.
        if let Some(a) = legal.iter().find(|a| matches!(a, LegalAction::LandDrop(_))) {
            return a.clone();
        }

        let mut best: Option<(i32, LegalAction)> = None;
        for action in legal {
            let card_id = match action {
                LegalAction::CastSpell { card_id, .. } => Some(*card_id),
                LegalAction::ActivateAbility { source_id, .. } => Some(*source_id),
                LegalAction::ActivateManaAbility { source_id, .. } => Some(*source_id),
                _ => None,
            };
            let Some(id) = card_id else { continue };
            let name = state.objects.get(&id).map(|o| o.catalog_key.as_str()).unwrap_or("");

            // Basalt Monolith can pay for its own {3}: untap activation by
            // tapping for {C}{C}{C}.  Activating that ability while Basalt is
            // already untapped returns to the exact same game state and a
            // priority-table pilot can loop forever.  Grim can exhibit the
            // same bad pattern when other mana is available.  These actions
            // are legal Magic, so keep them in the rules engine; the pilot
            // simply declines the strategically zero-progress version.
            if matches!(action, LegalAction::ActivateAbility { .. })
                && matches!(name, "Basalt Monolith" | "Grim Monolith")
                && state.objects.get(&id).and_then(|o| o.bf()).map_or(false, |bf| !bf.tapped)
            {
                continue;
            }

            let score = Self::card_priority(name);
            if best.as_ref().map_or(true, |(s, _)| score > *s) {
                best = Some((score, action.clone()));
            }
        }
        best.map(|(_, a)| a).unwrap_or(LegalAction::Pass)
    }

    fn announce(&mut self, state: &SimState, card_id: ObjId, options: &AnnounceOptions) -> AnnounceChoice {
        forge_announce_choice(state, card_id, options)
    }

    fn choose_wish(&mut self, _effect_id: ObjId, choices: &[WishOption], _state: &SimState) -> Option<WishOption> {
        choices.iter().max_by_key(|choice| {
            let name = match choice {
                WishOption::Sideboard { name, .. } | WishOption::Exile { name, .. } => name.as_str(),
            };
            Self::card_priority(name)
        }).cloned()
    }

    fn player_id(&self) -> PlayerId { self.who }

    fn plan_gap(&self, state: &SimState) -> TargetGap {
        let has_engine = state.permanents_of(self.who).any(|c| role_for(&c.catalog_key) == ForgeRole::Engine)
            || state.hand_of(self.who).any(|c| role_for(&c.catalog_key) == ForgeRole::Engine);
        TargetGap {
            mana: if state.permanents_of(self.who).any(|c| matches!(role_for(&c.catalog_key), ForgeRole::Land | ForgeRole::FastMana)) { 0.2 } else { 1.0 },
            threat: if has_engine { 0.0 } else { 1.0 },
            interaction: 0.5,
        }
    }

    fn card_fills(&self, card_id: ObjId, gap: &TargetGap, state: &SimState) -> f64 {
        let Some(obj) = state.objects.get(&card_id) else { return 0.0 };
        match role_for(&obj.catalog_key) {
            ForgeRole::Land | ForgeRole::FastMana => 1.0 * gap.mana,
            ForgeRole::Engine | ForgeRole::Tutor => 1.0 * gap.threat,
            ForgeRole::Lock => 0.8 * gap.interaction,
            ForgeRole::Payoff => 0.45,
            ForgeRole::Utility => 0.25,
        }
    }
}

/// Generic non-Forge baseline opponent. This is intentionally not presented as
/// a matchup-quality Legacy pilot; it exists so the full-game harness has a
/// strategy seam ready for archetype-specific policies.
pub struct BaselineOpponentStrategy {
    who: PlayerId,
}

impl BaselineOpponentStrategy {
    pub fn new(who: PlayerId) -> Self { Self { who } }
}

impl Strategy for BaselineOpponentStrategy {
    fn declare_attackers(&mut self, state: &SimState) -> Vec<(ObjId, Option<ObjId>)> {
        state.permanents_of(self.who)
            .filter_map(|c| state.def_of(c.id).and_then(|d| if d.is_creature() { Some((c.id, None)) } else { None }))
            .collect()
    }
    fn declare_blockers(&mut self, _state: &SimState) -> Vec<(ObjId, ObjId)> { Vec::new() }
    fn take_mulligan(&mut self, _state: &SimState, mulligans_taken: u32) -> bool { mulligans_taken == 0 }

    fn choose_action(&mut self, _state: &SimState, _ap: PlayerId, legal: &[LegalAction]) -> LegalAction {
        legal.iter().find(|a| matches!(a, LegalAction::LandDrop(_))).cloned()
            .or_else(|| legal.iter().find(|a| matches!(a, LegalAction::CastSpell { .. })).cloned())
            .or_else(|| legal.iter().find(|a| matches!(a, LegalAction::ActivateAbility { .. })).cloned())
            .unwrap_or(LegalAction::Pass)
    }

    fn player_id(&self) -> PlayerId { self.who }
    fn plan_gap(&self, _state: &SimState) -> TargetGap { TargetGap { mana: 0.5, threat: 0.5, interaction: 0.5 } }
    fn card_fills(&self, _card_id: ObjId, _gap: &TargetGap, _state: &SimState) -> f64 { 0.5 }
}
