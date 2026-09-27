use libmtg_engine::{
    AnnounceChoice, AnnounceOptions, LegalAction, ObjId, PlayerId, SimState, Strategy,
    TargetGap, WishOption, TargetSpec, PhaseKind, TurnPosition, StepKind, pick_targets,
    simulate_priority_action, AlwaysPass, parse_mana_cost,
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
    resolution_only: bool,
}

impl ForgeStrategy {
    pub fn new(who: PlayerId) -> Self { Self { who, resolution_only: false } }

    fn preview_action(&self, state: &SimState, ap: PlayerId, action: LegalAction) -> SimState {
        let pilot = Box::new(Self { who: self.who, resolution_only: true });
        let other = Box::new(AlwaysPass::new(self.who.opp()));
        let (us, opp): (Box<dyn Strategy>, Box<dyn Strategy>) = if self.who == PlayerId::Us {
            (pilot, other)
        } else { (other, pilot) };
        simulate_priority_action(state, ap, self.who, action, 0, us, opp)
    }

    fn untapped_monoliths(&self, state: &SimState) -> usize {
        state.permanents_of(self.who).filter(|o|
            matches!(o.catalog_key.as_str(), "Grim Monolith" | "Basalt Monolith")
                && o.bf().map_or(false, |bf| !bf.tapped)).count()
    }

    fn transmute_improves_board(&self, state: &SimState, ap: PlayerId, action: &LegalAction) -> bool {
        let branch = self.preview_action(state, ap, action.clone());
        if branch.invalid_actions != state.invalid_actions || branch.player(self.who).life <= 0 {
            return false;
        }
        let new_value = branch.permanents_of(self.who)
            .filter(|o| state.library_of(self.who).any(|old| old.id == o.id))
            .map(|o| Self::card_priority(&o.catalog_key)).max();
        let lost_value = state.permanents_of(self.who)
            .filter(|o| branch.permanent_bf(o.id).is_none())
            .map(|o| Self::card_priority(&o.catalog_key)
                - if o.bf().map_or(false, |bf| bf.tapped) { 30 } else { 0 }).max().unwrap_or(0);
        new_value.map_or(false, |value| value > lost_value)
    }

    fn tez_target_score(&self, state: &SimState, target: ObjId) -> Option<i32> {
        let obj = state.objects.get(&target)?;
        if obj.controller != self.who { return None; }
        let bf = obj.bf()?;
        let def = state.def_of(target)?;
        if bf.tapped {
            match obj.catalog_key.as_str() {
                "Grim Monolith" | "Basalt Monolith" => return Some(110),
                "Relic of Sauron" => return Some(100),
                "Manifold Key" => return Some(90),
                _ => {}
            }
        }
        if def.is_artifact() && def.is_creature() { return Some(70); }
        None
    }

    // Key should untap a productive permanent we control. Prefer the Monoliths
    // and Relic (net mana gain after Key's {1}), then another Ring activation.
    fn key_target_score(&self, state: &SimState, source: ObjId, target: ObjId) -> Option<i32> {
        let obj = state.objects.get(&target)?;
        if target == source || obj.controller != self.who || !obj.bf()?.tapped {
            return None;
        }
        if !state.def_of(target)?.is_artifact() { return None; }
        match obj.catalog_key.as_str() {
            "Grim Monolith" | "Basalt Monolith" => Some(100),
            "Relic of Sauron" => Some(90),
            "The One Ring" => Some(80),
            _ => None,
        }
    }

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
    fn declare_attackers(&mut self, state: &SimState) -> Vec<(ObjId, Option<ObjId>)> {
        state.permanents_of(self.who)
            .filter_map(|c| state.def_of(c.id)
                .and_then(|d| d.is_creature().then_some((c.id, None))))
            .collect()
    }
    fn declare_blockers(&mut self, _state: &SimState) -> Vec<(ObjId, ObjId)> { Vec::new() }

    fn take_mulligan(&mut self, state: &SimState, mulligans_taken: u32) -> bool {
        let hand: Vec<String> = state.hand_of(self.who).map(|c| c.catalog_key.clone()).collect();
        !keep_v0(&hand, mulligans_taken)
    }

    fn choose_action(&mut self, state: &SimState, ap: PlayerId, legal: &[LegalAction]) -> LegalAction {
        if self.resolution_only { return LegalAction::Pass; }
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

            let monolith_untap = matches!(action, LegalAction::ActivateAbility { .. })
                && matches!(name, "Basalt Monolith" | "Grim Monolith");
            if monolith_untap {
                let own_main = ap == self.who && matches!(state.current_phase,
                    Some(TurnPosition::Phase(PhaseKind::PreCombatMain | PhaseKind::PostCombatMain)));
                let opponent_end = ap != self.who && matches!(state.current_phase,
                    Some(TurnPosition::Step(StepKind::End)));
                if !state.stack.is_empty() || !(own_main || opponent_end)
                    || !state.permanent_bf(id).map_or(false, |bf| bf.tapped) { continue; }
                let branch = self.preview_action(state, ap, action.clone());
                // Don't tap one Monolith just to untap another, or pay life for
                // speculative setup. Spells and Key take precedence below.
                if branch.invalid_actions != state.invalid_actions
                    || branch.player(self.who).life < state.player(self.who).life
                    || self.untapped_monoliths(&branch) <= self.untapped_monoliths(state) { continue; }
            }
            if name == "Transmute Artifact" && matches!(action, LegalAction::CastSpell { .. })
                && !self.transmute_improves_board(state, ap, action) { continue; }

            // Keep setup mana available through upkeep/draw and only spend it
            // on a useful untap in our main phase with an empty stack.
            if let LegalAction::ActivateAbility { ability_index, .. } = action {
                if name == "Manifold Key" {
                    if *ability_index != 0 || ap != self.who || !state.stack.is_empty()
                        || !matches!(state.current_phase, Some(TurnPosition::Phase(
                            PhaseKind::PreCombatMain | PhaseKind::PostCombatMain)))
                    {
                        continue;
                    }
                    if !state.permanents_of(self.who)
                        .any(|perm| self.key_target_score(state, id, perm.id).is_some())
                    {
                        continue;
                    }
                }
            }

            let score = if let LegalAction::ActivateAbility { ability_index, .. } = action {
                if name == "Tezzeret, Cruel Captain" {
                    match *ability_index {
                        2 => 120, // Establish the recurring combat payoff.
                        0 => {
                            let Some(value) = state.permanents_of(self.who)
                                .filter_map(|o| self.tez_target_score(state, o.id)).max() else { continue; };
                            value
                        }
                        1 => 85,
                        _ => continue,
                    }
                } else if monolith_untap { 10 } else { Self::card_priority(name) }
            } else { Self::card_priority(name) };
            if best.as_ref().map_or(true, |(s, _)| score > *s) {
                best = Some((score, action.clone()));
            }
        }
        best.map(|(_, a)| a).unwrap_or(LegalAction::Pass)
    }

    fn choose_transmute_sacrifice(&mut self, _source: ObjId, _who: PlayerId,
        choices: &[ObjId], state: &SimState) -> Option<ObjId> {
        choices.iter().copied().max_by_key(|id| {
            let obj = &state.objects[id];
            let mv = state.def_of(*id).map(|d| parse_mana_cost(d.mana_cost()).mana_value()).unwrap_or(0);
            let spent = obj.bf().map_or(false, |bf| bf.tapped);
            let duplicate = state.permanents_of(self.who)
                .filter(|o| o.catalog_key == obj.catalog_key).count() > 1;
            mv * 15 + if spent { 40 } else { 0 } + if duplicate { 30 } else { 0 }
                - Self::card_priority(&obj.catalog_key)
        })
    }

    fn choose_transmute_target(&mut self, _source: ObjId, _choices: &[ObjId],
        payable: &[ObjId], state: &SimState) -> Option<ObjId> {
        payable.iter().copied().max_by_key(|id| {
            let name = &state.objects[id].catalog_key;
            let duplicate = state.permanents_of(self.who).any(|o| &o.catalog_key == name);
            Self::card_priority(name) - if duplicate { 50 } else { 0 }
        })
    }

    fn choose_targets(&mut self, state: &SimState, card_id: ObjId,
                      legal: &[ObjId], spec: &TargetSpec) -> Vec<ObjId> {
        if state.objects.get(&card_id).map(|o| o.catalog_key.as_str()) == Some("Manifold Key") {
            return legal.iter()
                .filter_map(|&id| self.key_target_score(state, card_id, id).map(|score| (score, id)))
                .max_by_key(|(score, _)| *score)
                .map(|(_, id)| vec![id]).unwrap_or_default();
        }
        if state.objects.get(&card_id).map(|o| o.catalog_key.as_str()) == Some("Tezzeret, Cruel Captain") {
            return legal.iter().filter_map(|&id| self.tez_target_score(state, id).map(|score| (score, id)))
                .max_by_key(|(score, _)| *score).map(|(_, id)| vec![id]).unwrap_or_default();
        }
        pick_targets(spec, legal, state)
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


#[cfg(test)]
mod tests {
    use super::*;
    use libmtg_engine::{build_catalog, PlayerState, Zone, StepKind};

    fn key_position() -> (SimState, ObjId, ObjId, ObjId, ObjId) {
        let mut state = SimState::new(PlayerState::new("forge"), PlayerState::new("red"));
        state.catalog = build_catalog();
        let key = state.place_card(PlayerId::Us, "Manifold Key", Zone::Battlefield);
        let grim = state.place_card(PlayerId::Us, "Grim Monolith", Zone::Battlefield);
        let sphere = state.place_card(PlayerId::Us, "Trinisphere", Zone::Battlefield);
        let opposing = state.place_card(PlayerId::Opp, "Grim Monolith", Zone::Battlefield);
        state.permanent_bf_mut(grim).unwrap().tapped = true;
        state.permanent_bf_mut(opposing).unwrap().tapped = true;
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));
        (state, key, grim, sphere, opposing)
    }

    #[test]
    fn key_targets_our_tapped_mana_artifact() {
        let (state, key, grim, sphere, opposing) = key_position();
        let mut pilot = ForgeStrategy::new(PlayerId::Us);
        assert_eq!(pilot.choose_targets(&state, key,
            &[sphere, opposing, grim], &TargetSpec::None), vec![grim]);
    }

    #[test]
    fn key_waits_for_own_main_phase_and_a_productive_target() {
        let (mut state, key, grim, _, _) = key_position();
        let mut pilot = ForgeStrategy::new(PlayerId::Us);
        let action = LegalAction::ActivateAbility { source_id: key, ability_index: 0 };
        let legal = vec![LegalAction::Pass, action.clone()];
        assert_eq!(pilot.choose_action(&state, PlayerId::Us, &legal), action);
        assert_eq!(pilot.choose_action(&state, PlayerId::Opp, &legal), LegalAction::Pass);
        state.current_phase = Some(TurnPosition::Step(StepKind::Upkeep));
        assert_eq!(pilot.choose_action(&state, PlayerId::Us, &legal), LegalAction::Pass);
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));
        state.stack.push(key); // a nonempty stack is sufficient for this timing check
        assert_eq!(pilot.choose_action(&state, PlayerId::Us, &legal), LegalAction::Pass);
        state.stack.clear();
        state.permanent_bf_mut(grim).unwrap().tapped = false;
        assert_eq!(pilot.choose_action(&state, PlayerId::Us, &legal), LegalAction::Pass);
    }
    #[test]
    fn monolith_waits_until_main_phase() {
        let (mut state, _, grim, _, _) = key_position();
        state.player_mut(PlayerId::Us).pool.c = 4;
        state.player_mut(PlayerId::Us).pool.total = 4;
        let mut pilot = ForgeStrategy::new(PlayerId::Us);
        let action = LegalAction::ActivateAbility { source_id: grim, ability_index: 0 };
        let legal = vec![LegalAction::Pass, action.clone()];
        state.current_phase = Some(TurnPosition::Step(StepKind::Upkeep));
        assert_eq!(pilot.choose_action(&state, PlayerId::Us, &legal), LegalAction::Pass);
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));
        assert_eq!(pilot.choose_action(&state, PlayerId::Us, &legal), action);
        assert!(state.permanent_bf(grim).unwrap().tapped, "preview must not change live state");
    }

    #[test]
    fn transmute_requires_an_affordable_upgrade() {
        for extra_mana in [0, 2] {
            let mut state = SimState::new(PlayerState::new("forge"), PlayerState::new("red"));
            state.catalog = build_catalog();
            state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));
            let grim = state.place_card(PlayerId::Us, "Grim Monolith", Zone::Battlefield);
            state.permanent_bf_mut(grim).unwrap().tapped = true;
            state.place_card(PlayerId::Us, "Mystic Forge", Zone::Library);
            let spell = state.place_card(PlayerId::Us, "Transmute Artifact", Zone::Hand { known: false });
            state.player_mut(PlayerId::Us).pool.u = 2;
            state.player_mut(PlayerId::Us).pool.c = extra_mana;
            state.player_mut(PlayerId::Us).pool.total = 2 + extra_mana;
            let action = LegalAction::CastSpell { card_id: spell, face: libmtg_engine::SpellFace::Main };
            let mut pilot = ForgeStrategy::new(PlayerId::Us);
            let chosen = pilot.choose_action(&state, PlayerId::Us, &[LegalAction::Pass, action.clone()]);
            assert_eq!(chosen, if extra_mana == 2 { action } else { LegalAction::Pass });
            assert!(state.permanent_bf(grim).is_some(), "preview must not sacrifice live cards");
        }
    }

    #[test]
    fn tez_uses_ultimate_and_productive_untaps() {
        let (mut state, _, grim, sphere, opposing) = key_position();
        let tez = state.place_card(PlayerId::Us, "Tezzeret, Cruel Captain", Zone::Battlefield);
        let zero = LegalAction::ActivateAbility { source_id: tez, ability_index: 0 };
        let tutor = LegalAction::ActivateAbility { source_id: tez, ability_index: 1 };
        let ultimate = LegalAction::ActivateAbility { source_id: tez, ability_index: 2 };
        let mut pilot = ForgeStrategy::new(PlayerId::Us);
        assert_eq!(pilot.choose_action(&state, PlayerId::Us, &[zero.clone(), tutor.clone(), ultimate.clone()]), ultimate);
        assert_eq!(pilot.choose_action(&state, PlayerId::Us, &[zero.clone(), tutor.clone()]), zero);
        assert_eq!(pilot.choose_targets(&state, tez, &[sphere, opposing, grim], &TargetSpec::None), vec![grim]);
        state.permanent_bf_mut(grim).unwrap().tapped = false;
        assert_eq!(pilot.choose_action(&state, PlayerId::Us, &[zero, tutor.clone()]), tutor);
    }

}
