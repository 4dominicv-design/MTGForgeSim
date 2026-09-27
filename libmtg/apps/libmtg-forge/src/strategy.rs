use libmtg_engine::{
    AnnounceChoice, AnnounceOptions, LegalAction, ObjId, PlayerId, SimState, Strategy,
    TargetGap, WishOption, TargetSpec, PhaseKind, TurnPosition, StepKind, pick_targets,
    simulate_priority_action, AlwaysPass, parse_mana_cost, CounterType, Keyword, creature_has_keyword, can_block_pair, can_attack_now, life_after_priority_costs,
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

    fn costs_are_survivable(&self, state: &SimState, ap: PlayerId, action: &LegalAction) -> bool {
        let pilot: Box<dyn Strategy> = Box::new(Self { who: self.who, resolution_only: true });
        let other: Box<dyn Strategy> = Box::new(AlwaysPass::new(self.who.opp()));
        let (us, opp) = if self.who == PlayerId::Us { (pilot, other) } else { (other, pilot) };
        life_after_priority_costs(state, ap, self.who, action.clone(), us, opp).map_or(false, |life| life > 0)
    }

    fn power(state: &SimState, id: ObjId) -> i32 {
        state.def_of(id).and_then(|d| d.as_creature()).map_or(0, |c| c.power().max(0))
    }

    fn toughness(state: &SimState, id: ObjId) -> i32 {
        state.def_of(id).and_then(|d| d.as_creature()).map_or(0, |c| c.toughness().max(0))
    }

    fn incoming_damage(&self, state: &SimState) -> i32 {
        state.permanents_of(self.who.opp()).map(|o| Self::combat_power(state, o.id)).sum()
    }

    fn combat_power(state: &SimState, id: ObjId) -> i32 {
        Self::power(state, id) * if creature_has_keyword(id, Keyword::DoubleStrike, state) { 2 } else { 1 }
    }

    fn defensive_blocks(&self, state: &SimState, attackers: &[ObjId]) -> Vec<(ObjId, ObjId)> {
        let mut attackers = attackers.to_vec();
        attackers.sort_by_key(|&id| std::cmp::Reverse(Self::combat_power(state, id)));
        let mut incoming: i32 = attackers.iter().map(|&a| Self::combat_power(state, a)).sum();
        let mut used = Vec::new();
        let mut blocks = Vec::new();
        for a in attackers {
            let candidate = state.permanents_of(self.who)
                .filter(|b| !used.contains(&b.id) && can_block_pair(state, a, b.id))
                .filter_map(|b| {
                    let damage = Self::combat_power(state, a);
                    let survives = Self::toughness(state, b.id) > damage
                        && !(Self::power(state, a) > 0 && creature_has_keyword(a, Keyword::Deathtouch, state));
                    let first_strike = creature_has_keyword(a, Keyword::FirstStrike, state)
                        || creature_has_keyword(a, Keyword::DoubleStrike, state);
                    let blocker_first = creature_has_keyword(b.id, Keyword::FirstStrike, state)
                        || creature_has_keyword(b.id, Keyword::DoubleStrike, state);
                    let kills = (survives || !first_strike || blocker_first)
                        && (Self::combat_power(state, b.id) >= Self::toughness(state, a)
                            || (Self::power(state, b.id) > 0 && creature_has_keyword(b.id, Keyword::Deathtouch, state)));
                    let prevented = if creature_has_keyword(a, Keyword::Trample, state) {
                        let absorb = if creature_has_keyword(a, Keyword::Deathtouch, state) { 1 } else { Self::toughness(state, b.id) };
                        damage.min(absorb)
                    } else { damage };
                    if !survives && !kills && incoming < state.player(self.who).life { return None; }
                    Some(((if survives { 1000 } else { 0 }) + if kills { 500 } else { 0 }
                        + prevented * 20 - Self::power(state, b.id), b.id, prevented))
                }).max_by_key(|&(score, _, _)| score);
            if let Some((_, b, prevented)) = candidate {
                used.push(b);
                blocks.push((a, b));
                incoming -= prevented;
            }
        }
        blocks
    }

    fn ring_draw_is_useful(&self, state: &SimState, ring: ObjId) -> bool {
        let draw = state.counter_count(ring, CounterType::Burden) as usize + 1;
        // Leave a card for the next draw step, and avoid adding burden when
        // already stocked with cards or near lethal upkeep life loss.
        state.library_size(self.who) > draw
            && state.hand_size(self.who) < 7
            && state.player(self.who).life > draw as i32 + 2
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
            "The One Ring" if self.ring_draw_is_useful(state, target) => Some(80),
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
    fn choose_legend_to_keep(&mut self, state: &SimState, choices: &[ObjId]) -> Option<ObjId> {
        choices.iter().copied().max_by_key(|&id| {
            let obj = &state.objects[&id];
            let untapped = obj.bf().map_or(false, |bf| !bf.tapped);
            let value = if obj.catalog_key == "The One Ring" {
                -(state.counter_count(id, CounterType::Burden) as i64)
            } else {
                state.counter_count(id, CounterType::Loyalty) as i64
                    + state.counter_count(id, CounterType::PlusOnePlusOne) as i64
            };
            (value, untapped)
        })
    }

    fn declare_attackers(&mut self, state: &SimState) -> Vec<(ObjId, Option<ObjId>)> {
        let mut attackers: Vec<_> = state.permanents_of(self.who)
            .filter(|o| can_attack_now(state, o.id) && Self::power(state, o.id) > 0)
            .map(|o| (o.id, None)).collect();
        // Only assume lethal when it is available without relying on bad blocks.
        let unblocked: i32 = attackers.iter().filter(|(a, _)| !state.permanents_of(self.who.opp())
            .any(|b| can_block_pair(state, *a, b.id))).map(|(a, _)| Self::combat_power(state, *a)).sum();
        if unblocked < state.player(self.who.opp()).life
            && self.incoming_damage(state) >= state.player(self.who).life {
            let enemies: Vec<_> = state.permanents_of(self.who.opp()).filter(|o| Self::power(state, o.id) > 0).map(|o| o.id).collect();
            let defense = self.defensive_blocks(state, &enemies);
            attackers.retain(|(id, _)| creature_has_keyword(*id, Keyword::Vigilance, state)
                || !defense.iter().any(|&(_, b)| b == *id));
        }
        attackers
    }

    fn declare_blockers(&mut self, state: &SimState) -> Vec<(ObjId, ObjId)> {
        self.defensive_blocks(state, &state.combat_attackers)
    }

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

            if matches!(action, LegalAction::ActivateAbility { .. }) {
                if matches!(name, "The One Ring" | "Mystic Forge" | "Relic of Sauron") {
                    // Develop during our main phase, after upkeep triggers resolve.
                    if ap != self.who || !state.stack.is_empty()
                        || !matches!(state.current_phase, Some(TurnPosition::Phase(
                            PhaseKind::PreCombatMain | PhaseKind::PostCombatMain))) { continue; }
                }
                if name == "The One Ring" && !self.ring_draw_is_useful(state, id) { continue; }
                if name == "Relic of Sauron" && (state.library_size(self.who) <= 2
                    || state.hand_size(self.who) >= 7) { continue; }
                if name == "Mystic Forge" {
                    if state.player(self.who).life <= 2 || state.library_size(self.who) <= 1 { continue; }
                    let Some(top) = state.library_of(self.who).next() else { continue; };
                    let Some(def) = state.def_of(top.id).or_else(|| state.catalog.get(&top.catalog_key)) else { continue; };
                    // Preserve a castable top card, including one we need more mana
                    // for. Only clear lands or colored nonartifact blockers.
                    let colored = def.mana_cost().chars().any(|c| "WUBRG".contains(c));
                    if !def.is_land() && (def.is_artifact() || !colored) { continue; }
                }
            }
            if name == "Kozilek's Command" && matches!(action, LegalAction::CastSpell { .. }) {
                // Our current Command mode draws one card. Keep setup mana for main.
                if state.library_size(self.who) <= 1 || ap != self.who
                    || !matches!(state.current_phase, Some(TurnPosition::Phase(
                        PhaseKind::PreCombatMain | PhaseKind::PostCombatMain))) { continue; }
            }

            // Legal mana payments can still kill us. Preview only dangerous
            // positions, stopping before any spell or ability resolves.
            let tombs = state.permanents_of(self.who).filter(|o| o.catalog_key == "Ancient Tomb").count() as i32;
            if tombs > 0 && state.player(self.who).life <= tombs * 2 {
                if !self.costs_are_survivable(state, ap, action) { continue; }
            }

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

            let mut score = if let LegalAction::ActivateAbility { ability_index, .. } = action {
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
            if name == "The One Ring" && matches!(action, LegalAction::CastSpell { .. })
                && (state.player(self.who).life <= 4 || self.incoming_damage(state) >= state.player(self.who).life) {
                score = 200; // Buy a turn before investing in another engine.
            }
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

    #[test]
    fn ring_waits_for_main_and_keeps_a_draw_step_reserve() {
        let (mut state, _, _, _, _) = key_position();
        let ring = state.place_card(PlayerId::Us, "The One Ring", Zone::Battlefield);
        let action = LegalAction::ActivateAbility { source_id: ring, ability_index: 0 };
        let legal = [LegalAction::Pass, action.clone()];
        let mut pilot = ForgeStrategy::new(PlayerId::Us);
        state.place_card(PlayerId::Us, "Island", Zone::Library);
        assert_eq!(pilot.choose_action(&state, PlayerId::Us, &legal), LegalAction::Pass);
        state.place_card(PlayerId::Us, "Island", Zone::Library);
        assert_eq!(pilot.choose_action(&state, PlayerId::Us, &legal), action);
        state.current_phase = Some(TurnPosition::Step(StepKind::Upkeep));
        assert_eq!(pilot.choose_action(&state, PlayerId::Us, &legal), LegalAction::Pass);
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));
        state.player_mut(PlayerId::Us).life = 3;
        assert_eq!(pilot.choose_action(&state, PlayerId::Us, &legal), LegalAction::Pass);
        state.player_mut(PlayerId::Us).life = 20;
        for _ in 0..7 { state.place_card(PlayerId::Us, "Island", Zone::Hand { known: false }); }
        assert_eq!(pilot.choose_action(&state, PlayerId::Us, &legal), LegalAction::Pass);
    }

    #[test]
    fn ring_replacement_keeps_fresh_copy_through_state_based_actions() {
        let (mut state, _, _, _, _) = key_position();
        let old = state.place_card(PlayerId::Us, "The One Ring", Zone::Battlefield);
        for _ in 0..10 { state.place_card(PlayerId::Us, "Island", Zone::Library); }
        let pilot = ForgeStrategy::new(PlayerId::Us);
        let mut state = pilot.preview_action(&state, PlayerId::Us,
            LegalAction::ActivateAbility { source_id: old, ability_index: 0 });
        assert_eq!(state.counter_count(old, CounterType::Burden), 1);
        let fresh = state.place_card(PlayerId::Us, "The One Ring", Zone::Battlefield);
        let state = pilot.preview_action(&state, PlayerId::Us, LegalAction::Pass);
        assert!(state.permanent_bf(fresh).is_some());
        assert!(state.permanent_bf(old).is_none());
        assert_eq!(state.counter_count(fresh, CounterType::Burden), 0);
    }

    #[test]
    fn mystic_forge_preserves_spells_and_clears_blockers() {
        for (top, should_exile) in [("Mox Opal", false), ("Paradox Engine", false),
            ("Kozilek's Command", false), ("Island", true), ("Transmute Artifact", true)] {
            let (mut state, _, _, _, _) = key_position();
            let forge = state.place_card(PlayerId::Us, "Mystic Forge", Zone::Battlefield);
            state.place_card(PlayerId::Us, top, Zone::Library);
            state.place_card(PlayerId::Us, "Island", Zone::Library);
            let action = LegalAction::ActivateAbility { source_id: forge, ability_index: 0 };
            let mut pilot = ForgeStrategy::new(PlayerId::Us);
            assert_eq!(pilot.choose_action(&state, PlayerId::Us, &[LegalAction::Pass, action.clone()]),
                if should_exile { action } else { LegalAction::Pass }, "{top}");
        }
    }

    fn body(state: &mut SimState, who: PlayerId, name: &str, power: i32, toughness: i32, keywords: &[Keyword]) -> ObjId {
        state.catalog.insert(name.into(), libmtg_engine::CardDef::vanilla_creature(name, power, toughness, keywords));
        let id = state.place_card(who, name, Zone::Battlefield);
        state.permanent_bf_mut(id).unwrap().entered_this_turn = false;
        id
    }

    #[test]
    fn refuses_lethal_tomb_payment_without_changing_live_state() {
        let mut state = SimState::new(PlayerState::new("forge"), PlayerState::new("red"));
        state.catalog = build_catalog();
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));
        state.player_mut(PlayerId::Us).life = 2;
        let tomb = state.place_card(PlayerId::Us, "Ancient Tomb", Zone::Battlefield);
        state.place_card(PlayerId::Us, "Ancient Tomb", Zone::Battlefield);
        let forge = state.place_card(PlayerId::Us, "Mystic Forge", Zone::Hand { known: false });
        let cast = LegalAction::CastSpell { card_id: forge, face: libmtg_engine::SpellFace::Main };
        let mut pilot = ForgeStrategy::new(PlayerId::Us);
        assert_eq!(pilot.choose_action(&state, PlayerId::Us, &[cast.clone(), LegalAction::Pass]), LegalAction::Pass);
        assert_eq!(state.player(PlayerId::Us).life, 2);
        assert!(!state.permanent_bf(tomb).unwrap().tapped);
        state.player_mut(PlayerId::Us).pool.c = 4;
        state.player_mut(PlayerId::Us).pool.total = 4;
        assert_eq!(pilot.choose_action(&state, PlayerId::Us, &[cast.clone(), LegalAction::Pass]), cast);
    }

    #[test]
    fn ring_over_engine_when_facing_lethal() {
        let (mut state, _, _, _, _) = key_position();
        state.player_mut(PlayerId::Us).life = 1;
        let forge = state.place_card(PlayerId::Us, "Mystic Forge", Zone::Hand { known: false });
        let ring = state.place_card(PlayerId::Us, "The One Ring", Zone::Hand { known: false });
        let cast = |card_id| LegalAction::CastSpell { card_id, face: libmtg_engine::SpellFace::Main };
        let legal = [cast(forge), cast(ring)];
        let mut pilot = ForgeStrategy::new(PlayerId::Us);
        assert_eq!(pilot.choose_action(&state, PlayerId::Us, &legal), cast(ring));
        state.player_mut(PlayerId::Us).life = 20;
        assert_eq!(pilot.choose_action(&state, PlayerId::Us, &legal), cast(forge));
    }

    #[test]
    fn blocks_to_survive_but_respects_flying_and_tapped_blockers() {
        let (mut state, _, _, _, _) = key_position();
        state.player_mut(PlayerId::Us).life = 2;
        let a = body(&mut state, PlayerId::Opp, "Flyer", 3, 3, &[Keyword::Flying]);
        let ground = body(&mut state, PlayerId::Us, "Ground", 4, 4, &[]);
        let reach = body(&mut state, PlayerId::Us, "Reach", 0, 1, &[Keyword::Reach]);
        state.combat_attackers = vec![a];
        let mut pilot = ForgeStrategy::new(PlayerId::Us);
        assert_eq!(pilot.declare_blockers(&state), vec![(a, reach)]);
        state.permanent_bf_mut(reach).unwrap().tapped = true;
        assert!(pilot.declare_blockers(&state).is_empty());
        assert!(!can_block_pair(&state, a, ground));
    }

    #[test]
    fn preserves_defender_unless_attack_is_unblocked_lethal() {
        let (mut state, _, _, _, _) = key_position();
        state.player_mut(PlayerId::Us).life = 2;
        let guard = body(&mut state, PlayerId::Us, "Guard", 4, 4, &[]);
        let enemy = body(&mut state, PlayerId::Opp, "Enemy", 2, 2, &[]);
        let mut pilot = ForgeStrategy::new(PlayerId::Us);
        assert!(pilot.declare_attackers(&state).is_empty());
        state.permanent_bf_mut(enemy).unwrap().tapped = true;
        state.player_mut(PlayerId::Opp).life = 4;
        assert_eq!(pilot.declare_attackers(&state), vec![(guard, None)]);
    }

}
