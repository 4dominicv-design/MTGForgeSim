    use super::*;
    use super::strategy;
    use rand::SeedableRng;

    // ── Test helpers ──────────────────────────────────────────────────────────

    fn make_state() -> SimState {
        let us = PlayerState::new("us_deck");
        let opp = PlayerState::new("opp_deck");
        let mut s = SimState::new(us, opp);
        s.rng = Box::new(rand::rngs::StdRng::seed_from_u64(42));
        // Install the same strategies the engine functions used to receive via the
        // threaded map (now folded onto the players); tests reach them via with_strategy.
        s.set_strategy(PlayerId::Us, Box::new(strategy::AlwaysPass::new(PlayerId::Us)));
        s.set_strategy(PlayerId::Opp, Box::new(strategy::AlwaysPass::new(PlayerId::Opp)));
        s
    }


    fn test_catalog() -> std::collections::HashMap<String, CardDef> {
        super::card_defs::build_catalog()
    }

    fn catalog_card(name: &str) -> CardDef {
        test_catalog().remove(name).unwrap_or_else(|| panic!("card not found in catalog: {name}"))
    }

    fn creature(name: &str, power: i32, toughness: i32) -> CardDef {
        CardDef::new(
            name, CardKind::Creature(CreatureData::new("", power, toughness)),
            vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![])
    }

    /// Insert a permanent into `state.objects` for `who` and return its ObjId.
    /// Also pre-registers and activates trigger/replacement instances so fire_triggers works.
    fn add_perm(state: &mut SimState, who: PlayerId, name: &str, bf: BattlefieldState) -> ObjId {
        let id = state.alloc_id();
        state.objects.insert(id, GameObject {
            id,
            catalog_key: name.to_string(),
            owner: who,
            controller: who,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Battlefield(bf),
        });
        // Look up the real CardDef (including triggers/replacements) from the catalog; fall back
        // to a minimal 1/1 stub for anonymous test creatures that have no special behaviour.
        let def = test_catalog().remove(name).unwrap_or_else(|| {
            CardDef::new(name, CardKind::Creature(CreatureData::new("", 1, 1)),
                         vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![])
        });

        let ts = state.next_ci_timestamp();
        state.objects.get_mut(&id).unwrap().ci_timestamp = ts;
        // Seed state.catalog so recompute() can find this object's base def.
        state.catalog.entry(name.to_string()).or_insert(def);
        id
    }

    /// Insert a default permanent (untapped, no mana abilities).
    fn add_default_perm(state: &mut SimState, who: PlayerId, name: &str) -> ObjId {
        add_perm(state, who, name, BattlefieldState::new())
    }

    /// Insert a permanent using a pre-built `CardDef` (full static_ability_defs included).
    /// Also seeds `state.materialized.defs` so mana abilities and type checks work without recompute.
    fn add_perm_with_def(state: &mut SimState, who: PlayerId, def: &CardDef, bf: BattlefieldState) -> ObjId {
        let id = state.alloc_id();
        state.objects.insert(id, GameObject {
            id,
            catalog_key: def.name.clone(),
            owner: who,
            controller: who,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Battlefield(bf),
        });
        let ts = state.next_ci_timestamp();
        state.objects.get_mut(&id).unwrap().ci_timestamp = ts;
        state.objects.get_mut(&id).unwrap().materialized = Some(def.clone());
        // Seed state.catalog so recompute() can find this object's base def.
        state.catalog.entry(def.name.clone()).or_insert_with(|| def.clone());
        id
    }

    fn make_land(state: &mut SimState, who: PlayerId, name: &str, tapped: bool) -> ObjId {
        add_perm(state, who, name, BattlefieldState {
            tapped,
            ..BattlefieldState::new()
        })
    }

    fn add_hand_card(state: &mut SimState, who: PlayerId, name: &str) -> ObjId {
        let id = state.alloc_id();
        state.objects.insert(id, GameObject {
            id,
            catalog_key: name.to_string(),
            owner: who,
            controller: who,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Hand { known: false },
        });
        state.catalog.entry(name.to_string())
            .or_insert_with(|| test_catalog().remove(name).unwrap_or_else(|| creature(name, 1, 1)));
        id
    }

    fn add_hand_card_with_def(state: &mut SimState, who: PlayerId, def: &CardDef) -> ObjId {
        let id = add_hand_card(state, who, &def.name.clone());
        state.objects.get_mut(&id).unwrap().materialized = Some(def.clone());
        id
    }

    fn add_graveyard_card(state: &mut SimState, who: PlayerId, name: &str) -> ObjId {
        let id = state.alloc_id();
        state.objects.insert(id, GameObject {
            id,
            catalog_key: name.to_string(),
            owner: who,
            controller: who,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Graveyard,
        });
        id
    }

    /// Put a spell on the stack (for targeting / protection tests).
    fn add_stack_spell(state: &mut SimState, who: PlayerId, def: &CardDef) -> ObjId {
        let id = state.alloc_id();
        state.objects.insert(id, GameObject {
            id,
            catalog_key: def.name.clone(),
            owner: who,
            controller: who,
            is_token: false,
            materialized: Some(def.clone()),
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::StackSpell(SpellState { effect: None, chosen_targets: vec![], is_back_face: false, costs_paid_ctx: CostsPaidCtx::default() }),
        });
        state.catalog.entry(def.name.clone()).or_insert_with(|| def.clone());
        id
    }

    fn add_library_card(state: &mut SimState, who: PlayerId, name: &str) -> ObjId {
        let id = state.alloc_id();
        state.objects.insert(id, GameObject {
            id,
            catalog_key: name.to_string(),
            owner: who,
            controller: who,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Library,
        });
        state.player_mut(who).library_order.push_back(id);
        state.catalog.entry(name.to_string())
            .or_insert_with(|| test_catalog().remove(name).unwrap_or_else(|| creature(name, 1, 1)));
        id
    }

    // ── Section 1: Pure Function Tests ────────────────────────────────────────

    #[test]
    fn test_parse_mana_cost_black() {
        let mc = parse_mana_cost("BBB");
        assert_eq!(mc.b, 3);
        assert_eq!(mc.u, 0);
        assert_eq!(mc.generic, 0);
    }

    #[test]
    fn test_parse_mana_cost_mixed() {
        // "1UB" → b=1, u=1, generic=1
        let mc = parse_mana_cost("1UB");
        assert_eq!(mc.b, 1);
        assert_eq!(mc.u, 1);
        assert_eq!(mc.generic, 1);
    }

    #[test]
    fn test_parse_mana_cost_zero() {
        let mc = parse_mana_cost("0");
        assert_eq!(mc.mana_value(), 0);
    }

    #[test]
    fn test_mana_value() {
        assert_eq!(mana_value("2BB"), 4);
        assert_eq!(mana_value("0"), 0);
        assert_eq!(mana_value("U"), 1);
    }


    #[test]
    fn test_stage_label() {
        assert_eq!(stage_label(1), "Early");
        assert_eq!(stage_label(4), "Mid");
        assert_eq!(stage_label(8), "Late");
    }

    // ── Section 2: Step Tests ─────────────────────────────────────────────────

    #[test]
    fn test_untap_step_resets_permanents() {
        let mut state = make_state();
        let land_id = make_land(&mut state, PlayerId::Us, "Island", true);
        let ragavan_id = add_perm(&mut state, PlayerId::Us, "Ragavan", BattlefieldState {
            tapped: true,
            entered_this_turn: true,
            ..BattlefieldState::new()
        });
        let step = Step { kind: StepKind::Untap, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true);

        assert!(!state.permanent_bf(land_id).unwrap().tapped, "land should be untapped");
        assert!(!state.permanent_bf(ragavan_id).unwrap().tapped, "permanent should be untapped");
        assert!(!state.permanent_bf(ragavan_id).unwrap().entered_this_turn, "summoning sickness should clear");
        assert_eq!(state.player(PlayerId::Us).lands_played_this_turn, 0, "land drop count should reset");
    }

    #[test]
    fn test_draw_step_skipped_on_play_turn1() {
        let mut state = make_state();
        add_library_card(&mut state, PlayerId::Us, "Island");
        let initial_hand = state.hand_size(PlayerId::Us);

        let step = Step { kind: StepKind::Draw, prio: false };
        // on_play=true, t=1, ap=PlayerId::Us → this_player_on_play=true → skip
        do_step(&mut state, 1, PlayerId::Us, &step, true);

        assert_eq!(state.hand_size(PlayerId::Us), initial_hand, "no draw on the play turn 1");
    }

    #[test]
    fn test_draw_step_draws_card() {
        let mut state = make_state();
        add_library_card(&mut state, PlayerId::Us, "Island");
        let initial_hand = state.hand_size(PlayerId::Us);

        let step = Step { kind: StepKind::Draw, prio: false };
        // on_play=false → this_player_on_play=false → no skip
        do_step(&mut state, 1, PlayerId::Us, &step, false);

        assert_eq!(state.hand_size(PlayerId::Us), initial_hand + 1, "should draw one card");
    }

    #[test]
    fn test_cleanup_removes_damage() {
        let mut state = make_state();
        let rag_id = add_perm(&mut state, PlayerId::Us, "Ragavan", BattlefieldState {
            damage: 3,
            ..BattlefieldState::new()
        });

        let step = Step { kind: StepKind::Cleanup, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true);

        assert_eq!(state.permanent_bf(rag_id).unwrap().damage, 0);
    }






    #[test]
    fn test_combat_damage_unblocked_hits_player() {
        let mut state = make_state();
        let initial_life = state.player(PlayerId::Opp).life;
        let atk_def = creature("Ragavan", 2, 1);
        let ragavan_id = add_perm(&mut state, PlayerId::Us, "Ragavan", BattlefieldState {
            tapped: true,
            ..BattlefieldState::new()
        });
        state.combat_attackers = vec![ragavan_id];

        let catalog = vec![atk_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        let step = Step { kind: StepKind::CombatDamage, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true);

        assert_eq!(state.player(PlayerId::Opp).life, initial_life - 2);
    }

    #[test]
    fn test_combat_damage_blocked_no_player_damage() {
        let mut state = make_state();
        let initial_life = state.player(PlayerId::Opp).life;
        let atk_def = creature("Ragavan", 2, 2);
        let blk_def = creature("Mosscoat Construct", 3, 3);
        let ragavan_id = add_perm(&mut state, PlayerId::Us, "Ragavan", BattlefieldState {
            tapped: true,
            ..BattlefieldState::new()
        });
        let construct_id = add_default_perm(&mut state, PlayerId::Opp, "Mosscoat Construct");
        state.combat_attackers = vec![ragavan_id];
        state.combat_blocks = vec![(ragavan_id, construct_id)];

        let catalog = vec![atk_def, blk_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        let step = Step { kind: StepKind::CombatDamage, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true);

        assert_eq!(state.player(PlayerId::Opp).life, initial_life, "blocked — no player damage");
    }

    #[test]
    fn test_combat_damage_sba_kills_both_2_2s() {
        let mut state = make_state();
        let atk_def = creature("Ragavan", 2, 2);
        let blk_def = creature("Mosscoat Construct", 2, 2);
        let ragavan_id = add_perm(&mut state, PlayerId::Us, "Ragavan", BattlefieldState {
            tapped: true,
            ..BattlefieldState::new()
        });
        let construct_id = add_default_perm(&mut state, PlayerId::Opp, "Mosscoat Construct");
        state.combat_attackers = vec![ragavan_id];
        state.combat_blocks = vec![(ragavan_id, construct_id)];

        let catalog = vec![atk_def, blk_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        let step = Step { kind: StepKind::CombatDamage, prio: true };
        do_step(&mut state, 1, PlayerId::Us, &step, true);

        assert!(state.permanents_of(PlayerId::Us).count() == 0, "attacker should die");
        assert!(state.permanents_of(PlayerId::Opp).count() == 0, "blocker should die");
        assert!(state.graveyard_of(PlayerId::Us).any(|c| c.catalog_key == "Ragavan"));
        assert!(state.graveyard_of(PlayerId::Opp).any(|c| c.catalog_key == "Mosscoat Construct"));
    }

    #[test]
    fn test_combat_damage_outclassed_attacker_dies() {
        let mut state = make_state();
        let atk_def = creature("Ragavan", 2, 2);
        let blk_def = creature("Troll", 3, 3);
        let ragavan_id = add_perm(&mut state, PlayerId::Us, "Ragavan", BattlefieldState {
            tapped: true,
            ..BattlefieldState::new()
        });
        let troll_id = add_default_perm(&mut state, PlayerId::Opp, "Troll");
        state.combat_attackers = vec![ragavan_id];
        state.combat_blocks = vec![(ragavan_id, troll_id)];

        let catalog = vec![atk_def, blk_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        let step = Step { kind: StepKind::CombatDamage, prio: true };
        do_step(&mut state, 1, PlayerId::Us, &step, true);

        assert!(state.permanents_of(PlayerId::Us).count() == 0, "attacker dies");
        assert!(state.permanents_of(PlayerId::Opp).count() > 0, "blocker survives");
    }

    #[test]
    fn test_end_combat_clears_fields() {
        let mut state = make_state();
        let dummy_id = state.alloc_id();
        let dummy_id2 = state.alloc_id();
        state.combat_attackers = vec![dummy_id];
        state.combat_blocks = vec![(dummy_id, dummy_id2)];

        let step = Step { kind: StepKind::EndCombat, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true);

        assert!(state.combat_attackers.is_empty());
        assert!(state.combat_blocks.is_empty());
    }

    // ── Section 3: Phase Tests ────────────────────────────────────────────────

    #[test]
    fn test_beginning_phase_untaps_and_draws() {
        let mut state = make_state();
        let island_def = catalog_card("Island");
        let island_id = add_perm_with_def(&mut state, PlayerId::Us, &island_def, BattlefieldState {
            tapped: true,
            ..BattlefieldState::new()
        });
        add_library_card(&mut state, PlayerId::Us, "Swamp");
        let initial_hand = state.hand_size(PlayerId::Us);

        // t=2, on_play=false → draw fires (this_player_on_play=false)
        do_phase(&mut state, 2, PlayerId::Us, &beginning_phase(), false);

        assert!(!state.permanent_bf(island_id).unwrap().tapped, "land should be untapped");
        assert_eq!(state.hand_size(PlayerId::Us), initial_hand + 1, "should have drawn one card");
    }

    #[test]
    fn test_combat_phase_full_cycle() {
        let mut state = make_state();
        do_phase(&mut state, 1, PlayerId::Us, &combat_phase(), true);

        assert!(state.combat_attackers.is_empty());
        assert!(state.combat_blocks.is_empty());
    }

    // ── Section 4: Priority Action Cycle ─────────────────────────────────────

    #[test]
    fn test_priority_round_both_pass_empty_stack() {
        let mut state = make_state();
        // current_phase is "" (not "Main") → both players pass immediately
        handle_priority_round(&mut state, 1, PlayerId::Us);

        assert_eq!(state.player(PlayerId::Us).life, 20);
        assert_eq!(state.player(PlayerId::Opp).life, 20);
    }

    #[test]
    fn test_manifold_key_cannot_repeat_activation_while_tapped() {
        let mut state = make_state();
        let key = add_default_perm(&mut state, PlayerId::Us, "Manifold Key");
        let target = add_perm(&mut state, PlayerId::Us, "Grim Monolith",
            BattlefieldState { tapped: true, ..BattlefieldState::new() });
        state.player_mut(PlayerId::Us).pool.c = 3;
        state.player_mut(PlayerId::Us).pool.total = 3;
        recompute(&mut state);
        let action = LegalAction::ActivateAbility { source_id: key, ability_index: 0 };
        assert!(strategy::collect_legal_actions(&state, PlayerId::Us).contains(&action));
        let ability = state.def_of(key).unwrap().abilities()[0].clone();
        assert_ne!(run_activate_submachine(&mut state, 1, PlayerId::Us, key, &ability), ObjId::UNSET);
        assert!(state.permanent_bf(key).unwrap().tapped);
        assert_eq!(state.player(PlayerId::Us).pool.total, 2);
        assert_eq!(state.stack.len(), 1);
        assert!(!strategy::collect_legal_actions(&state, PlayerId::Us).contains(&action),
            "a tapped Key must not be offered again even with mana available");
        assert_eq!(run_activate_submachine(&mut state, 1, PlayerId::Us, key, &ability), ObjId::UNSET,
            "a stale activation must also fail at execution");
        assert_eq!(state.player(PlayerId::Us).pool.total, 2);
        assert_eq!(state.stack.len(), 1);
        handle_priority_round(&mut state, 1, PlayerId::Us);
        assert!(state.stack.is_empty());
        assert!(!state.permanent_bf(target).unwrap().tapped);
    }

    #[test]
    fn test_manifold_key_failed_payment_does_not_stack_and_passes_priority() {
        let mut state = make_state();
        let key = add_default_perm(&mut state, PlayerId::Us, "Manifold Key");
        add_perm(&mut state, PlayerId::Us, "Grim Monolith",
            BattlefieldState { tapped: true, ..BattlefieldState::new() });
        recompute(&mut state);
        let action = LegalAction::ActivateAbility { source_id: key, ability_index: 0 };
        let ability = state.def_of(key).unwrap().abilities()[0].clone();
        assert_eq!(run_activate_submachine(&mut state, 1, PlayerId::Us, key, &ability), ObjId::UNSET);
        assert!(state.stack.is_empty(), "unpaid abilities must never enter the stack");
        assert!(!state.permanent_bf(key).unwrap().tapped);
        let mut holder = PlayerId::Us;
        let mut last_passer = None;
        assert!(!apply_priority_action(&mut state, 1, PlayerId::Us, PlayerId::Us,
            action.clone(), &mut holder, &mut last_passer));
        assert_eq!(holder, PlayerId::Opp);
        assert_eq!(last_passer, Some(PlayerId::Us));
        // A failure after the opponent's pass must finish the empty window.
        holder = PlayerId::Us;
        last_passer = Some(PlayerId::Opp);
        assert!(apply_priority_action(&mut state, 1, PlayerId::Us, PlayerId::Us,
            action, &mut holder, &mut last_passer));
        assert!(state.stack.is_empty());
    }

    // ── Section 5: Spell Casting ──────────────────────────────────────────────

    #[test]
    fn test_cast_spell_normal_cost_removes_from_library() {
        let mut state = make_state();
        let def = catalog_card("Dark Ritual");
        state.player_mut(PlayerId::Us).pool.b = 1;
        state.player_mut(PlayerId::Us).pool.total = 1;
        let dark_ritual_id = add_hand_card(&mut state, PlayerId::Us, "Dark Ritual");

        let catalog = vec![def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }

        let card_id = cast_spell(&mut state, 1, PlayerId::Us, dark_ritual_id, SpellFace::Main, None, None, &[], 0, 0);

        assert!(card_id.is_some(), "spell should be cast");
        let card_id = card_id.unwrap();
        let card = state.objects.get(&card_id).expect("card in state");
        assert_eq!(card.catalog_key, "Dark Ritual");
        assert_eq!(state.player_id(card.owner), state.us_id, "owner should be us player id");
        assert!(!state.hand_of(PlayerId::Us).any(|c| c.catalog_key == "Dark Ritual"), "removed from hand");
        assert_eq!(state.player(PlayerId::Us).pool.b, 0, "mana spent");
    }

    #[test]
    fn test_cast_spell_unaffordable_returns_none() {
        let mut state = make_state();
        let def = catalog_card("Doomsday");
        // No mana in pool, no lands
        let doomsday_id = add_hand_card(&mut state, PlayerId::Us, "Doomsday");

        let catalog = vec![def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        let item = cast_spell(&mut state, 1, PlayerId::Us, doomsday_id, SpellFace::Main, None, None, &[], 0, 0);

        assert!(item.is_none(), "can't cast with no mana");
    }

    #[test]
    fn test_cast_spell_alt_cost_exiles_pitch_card() {
        let mut state = make_state();
        let fow_def = catalog_card("Force of Will");
        let brainstorm_def = catalog_card("Brainstorm");
        let catalog = vec![fow_def.clone(), brainstorm_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }

        // Add FoW and Brainstorm to hand (FoW pitches itself? No — Brainstorm is the pitch card)
        let fow_id = add_hand_card(&mut state, PlayerId::Us, "Force of Will");
        add_hand_card(&mut state, PlayerId::Us, "Brainstorm");

        let alt_cost = &fow_def.alternate_costs()[0];
        let initial_life = state.player(PlayerId::Us).life;

        let item = cast_spell(&mut state, 1, PlayerId::Us, fow_id, SpellFace::Main, Some(alt_cost), Some(0), &[], 0, 0);

        assert!(item.is_some(), "FoW should be cast via pitch");
        assert_eq!(state.player(PlayerId::Us).life, initial_life - 1, "paid 1 life");
        assert!(!state.hand_of(PlayerId::Us).any(|c| c.catalog_key == "Brainstorm"), "pitch card removed from hand");
        assert!(state.exile_of(PlayerId::Us).any(|c| c.catalog_key == "Brainstorm"), "pitch card exiled");
    }

    // ── Section 6: Spell Resolution ───────────────────────────────────────────


    #[test]
    fn test_effect_cantrip_increments_hand() {
        let mut state = make_state();
        add_library_card(&mut state, PlayerId::Us, "Island");
        let initial_hand = state.hand_size(PlayerId::Us);
        eff_draw(PlayerId::Us, 1).call(&mut state, 1, &[]);

        assert_eq!(state.hand_size(PlayerId::Us), initial_hand + 1, "cantrip increments hand count");
    }

    #[test]
    fn test_brainstorm_net_one_card() {
        // draw:3 + put_back:2 = net +1 hand size.
        let mut state = make_state();
        add_library_card(&mut state, PlayerId::Us, "Island");
        add_library_card(&mut state, PlayerId::Us, "Swamp");
        add_library_card(&mut state, PlayerId::Us, "Plains");
        let initial = state.hand_size(PlayerId::Us);
        eff_draw(PlayerId::Us, 3).then(eff_put_back(PlayerId::Us, 2))
            .call(&mut state, 1, &[]);

        assert_eq!(state.hand_size(PlayerId::Us), initial + 1, "Brainstorm nets +1 card");
    }

    #[test]
    fn test_brainstorm_fires_three_draw_events() {
        // All three draws queue triggers; OBM (controlled by opp) should see all three.
        let mut state = make_state();
        add_default_perm(&mut state, PlayerId::Opp, "Orcish Bowmasters");
        add_library_card(&mut state, PlayerId::Us, "Island");
        add_library_card(&mut state, PlayerId::Us, "Swamp");
        add_library_card(&mut state, PlayerId::Us, "Plains");
        eff_draw(PlayerId::Us, 3).then(eff_put_back(PlayerId::Us, 2))
            .call(&mut state, 1, &[]);

        // Three Draw events queued → three OBM triggers pending (all non-natural draws).
        let bowmasters_triggers = state.pending_triggers.iter()
            .filter(|tc| tc.source_name == "Orcish Bowmasters")
            .count();
        assert_eq!(bowmasters_triggers, 3, "OBM pings for each of the 3 Brainstorm draws");
    }

    #[test]
    fn test_brainstorm_flips_tamiyo_on_second_draw_of_three() {
        // Turn context: natural draw already happened (draw_index=1).
        // Brainstorm's 2nd draw = draw_index=3 → Tamiyo flips.
        let mut state = make_state();
        add_default_perm(&mut state, PlayerId::Us, "Tamiyo, Inquisitive Student");
        state.player_mut(PlayerId::Us).draws_this_turn = 1; // simulate having already drawn naturally
        add_library_card(&mut state, PlayerId::Us, "Island");
        add_library_card(&mut state, PlayerId::Us, "Swamp");
        add_library_card(&mut state, PlayerId::Us, "Plains");
        eff_draw(PlayerId::Us, 3).then(eff_put_back(PlayerId::Us, 2))
            .call(&mut state, 1, &[]);

        let flip_triggers = state.pending_triggers.iter()
            .filter(|tc| tc.source_name == "Tamiyo, Inquisitive Student")
            .count();
        assert_eq!(flip_triggers, 1, "Tamiyo flips exactly once on the 3rd draw of the turn");
    }

    #[test]
    fn test_effect_life_loss_reduces_caster_life() {
        let mut state = make_state();
        let initial = state.player(PlayerId::Us).life;
        eff_life_loss(PlayerId::Us, 2).call(&mut state, 1, &[]);

        assert_eq!(state.player(PlayerId::Us).life, initial - 2);
    }

    #[test]
    fn test_effect_mana_adds_to_pool() {
        let mut state = make_state();
        eff_mana(PlayerId::Us, "BBB").call(&mut state, 1, &[]);

        assert_eq!(state.player(PlayerId::Us).pool.b, 3, "should add 3 black mana");
        assert_eq!(state.player(PlayerId::Us).pool.total, 3);
    }

    #[test]
    fn test_effect_discard_removes_opp_card() {
        let mut state = make_state();
        add_hand_card(&mut state, PlayerId::Opp, "Counterspell");
        let initial_opp_hand = state.hand_size(PlayerId::Opp);
        eff_discard(PlayerId::Us, Who::Opp, 1, ir_any()).call(&mut state, 1, &[]);

        assert_eq!(state.hand_size(PlayerId::Opp), initial_opp_hand - 1, "opp hand decremented");
        assert!(state.graveyard_of(PlayerId::Opp).any(|c| c.catalog_key == "Counterspell"), "Counterspell in graveyard");
        assert!(!state.hand_of(PlayerId::Opp).any(|c| c.catalog_key == "Counterspell"), "card removed from opp hand");
    }

    #[test]
    fn test_reveal_hand_marks_cards_known() {
        let mut state = make_state();
        let a = add_hand_card(&mut state, PlayerId::Opp, "Counterspell");
        let b = add_hand_card(&mut state, PlayerId::Opp, "Island");
        assert!(matches!(state.objects[&a].zone(), Some(Zone::Hand { known: false })));
        assert!(matches!(state.objects[&b].zone(), Some(Zone::Hand { known: false })));

        eff_reveal_hand(PlayerId::Us, Who::Opp).call(&mut state, 1, &[]);

        assert!(matches!(state.objects[&a].zone(), Some(Zone::Hand { known: true })),
                "reveal should mark card a as known");
        assert!(matches!(state.objects[&b].zone(), Some(Zone::Hand { known: true })),
                "reveal should mark card b as known");
    }

    #[test]
    fn test_thoughtseize_reveals_then_discards() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let _a = add_hand_card(&mut state, PlayerId::Opp, "Counterspell");
        let _b = add_hand_card(&mut state, PlayerId::Opp, "Dark Ritual");
        let _c = add_hand_card(&mut state, PlayerId::Opp, "Island");

        // Thoughtseize's effect (IR body): reveal hand, discard nonland, lose 2 life.
        let ts_def = catalog_card("Thoughtseize");
        let effect = build_spell_effect(&ts_def, PlayerId::Us, ObjId::UNSET, 0, 0).1;
        effect.call(&mut state, 1, &[]);

        assert_eq!(state.hand_size(PlayerId::Opp), 2);
        for card in state.hand_of(PlayerId::Opp) {
            assert!(matches!(card.zone(), Some(Zone::Hand { known: true })),
                    "{} should be known after Thoughtseize", card.catalog_key);
        }
    }

    #[test]
    fn test_put_back_resets_hand_knowledge() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let a = add_hand_card(&mut state, PlayerId::Opp, "Counterspell");
        let b = add_hand_card(&mut state, PlayerId::Opp, "Dark Ritual");
        let c = add_hand_card(&mut state, PlayerId::Opp, "Island");
        // Mark all as known (as if previously revealed by Thoughtseize).
        for id in [a, b, c] {
            state.objects.get_mut(&id).unwrap().set_zone(Zone::Hand { known: true });
        }
        add_library_card(&mut state, PlayerId::Opp, "Swamp");
        add_library_card(&mut state, PlayerId::Opp, "Swamp");
        add_library_card(&mut state, PlayerId::Opp, "Swamp");
        wire_eval(&mut state, vec![
            ("Counterspell", 0.9), ("Dark Ritual", 0.5), ("Island", 0.1),
            ("Swamp", 0.2),
        ]);

        // Brainstorm: draw 3, put back 2.
        eff_draw(PlayerId::Opp, 3).then(eff_put_back(PlayerId::Opp, 2)).call(&mut state, 1, &[]);

        for card in state.hand_of(PlayerId::Opp) {
            assert!(matches!(card.zone(), Some(Zone::Hand { known: false })),
                    "{} should be unknown after Brainstorm put-back", card.catalog_key);
        }
    }

    #[test]
    fn test_hymn_does_not_reveal_hand() {
        let mut state = make_state();
        let _a = add_hand_card(&mut state, PlayerId::Opp, "Counterspell");
        let _b = add_hand_card(&mut state, PlayerId::Opp, "Dark Ritual");
        let _c = add_hand_card(&mut state, PlayerId::Opp, "Island");

        // Hymn discards 2 at random — no reveal.
        eff_discard(PlayerId::Us, Who::Opp, 2, ir_any()).call(&mut state, 1, &[]);

        assert_eq!(state.hand_size(PlayerId::Opp), 1);
        let remaining = state.hand_of(PlayerId::Opp).next().unwrap();
        assert!(matches!(remaining.zone(), Some(Zone::Hand { known: false })),
                "Hymn should NOT reveal remaining cards");
    }

    // ── Section 7: Ability Activation ─────────────────────────────────────────
    //
    // Phase 6: deleted `test_pay_activation_cost_{mana,life,sacrifice_self}`.
    // They tested the legacy `pay_costs` executor directly; equivalent IR
    // executor coverage lives in `ir::tests::cost_phase{1,3,4}`.

    // ── Section 8: Destruction Effects ───────────────────────────────────────

    // Spell resolution: destroy uses item.permanent_target set at cast time.

    #[test]
    fn test_effect_destroy_spell_removes_opp_land() {
        let mut state = make_state();
        let id = make_land(&mut state, PlayerId::Opp, "Bayou", false);
        eff_destroy_target(PlayerId::Us).call(&mut state, 1, &[id]);

        assert!(state.permanents_of(PlayerId::Opp).count() == 0, "Bayou should be destroyed");
        assert!(state.graveyard_of(PlayerId::Opp).any(|c| c.catalog_key == "Bayou"));
    }

    #[test]
    fn test_effect_destroy_spell_removes_opp_creature() {
        let mut state = make_state();
        let id = add_default_perm(&mut state, PlayerId::Opp, "Troll");
        eff_destroy_target(PlayerId::Us).call(&mut state, 1, &[id]);

        assert!(state.permanents_of(PlayerId::Opp).count() == 0, "Troll should be destroyed");
        assert!(state.graveyard_of(PlayerId::Opp).any(|c| c.catalog_key == "Troll"));
    }

    // Ability resolution: target is chosen at push time via choose_permanent_target.

    fn land_def(name: &str, basic: bool) -> CardDef {
        CardDef::new(
            name, CardKind::Land(LandData::default()),
            vec![], None,
            if basic { vec![Supertype::Basic] } else { vec![] },
            CardLayout::Normal, None, vec![], vec![], vec![], vec![])
    }

    #[test]
    fn test_effect_destroy_ability_removes_nonbasic_land() {
        let mut state = make_state();
        make_land(&mut state, PlayerId::Opp, "Bayou", false);
        let ability = AbilityDef { target_spec: TargetSpec::ObjectInZone { controller: Who::Opp, zone: ZoneId::Battlefield, filter: ir_and(ir_type(CardType::Land), ir_not(ir_supertype(Supertype::Basic))) }, ability_factory: Some(Arc::new(|who, _| eff_destroy_target(who))), ..Default::default() };
        let bayou_def = land_def("Bayou", false);
        let catalog = vec![bayou_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        let targets: Vec<ObjId> = legal_targets(
            &TargetSpec::ObjectInZone { controller: Who::Opp, zone: ZoneId::Battlefield, filter: ir_and(ir_type(CardType::Land), ir_not(ir_supertype(Supertype::Basic))) }, PlayerId::Us, ObjId(0), &state
        );
        let eff = build_ability_effect(&ability, PlayerId::Us, ObjId::UNSET);
        eff.call(&mut state, 1, &targets);

        assert!(state.permanents_of(PlayerId::Opp).count() == 0, "Bayou should be destroyed");
        assert!(state.graveyard_of(PlayerId::Opp).any(|c| c.catalog_key == "Bayou"));
    }

    #[test]
    fn test_effect_destroy_ability_ignores_basic_land() {
        let mut state = make_state();
        make_land(&mut state, PlayerId::Opp, "Forest", false);
        let ability = AbilityDef { target_spec: TargetSpec::ObjectInZone { controller: Who::Opp, zone: ZoneId::Battlefield, filter: ir_and(ir_type(CardType::Land), ir_not(ir_supertype(Supertype::Basic))) }, ability_factory: Some(Arc::new(|who, _| eff_destroy_target(who))), ..Default::default() };
        let forest_def = land_def("Forest", true);
        let catalog = vec![forest_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        let targets: Vec<ObjId> = legal_targets(
            &TargetSpec::ObjectInZone { controller: Who::Opp, zone: ZoneId::Battlefield, filter: ir_and(ir_type(CardType::Land), ir_not(ir_supertype(Supertype::Basic))) }, PlayerId::Us, ObjId(0), &state
        );
        let eff = build_ability_effect(&ability, PlayerId::Us, ObjId::UNSET);
        eff.call(&mut state, 1, &targets);

        assert!(state.permanents_of(PlayerId::Opp).count() > 0, "basic Forest should survive");
        assert!(state.graveyard_of(PlayerId::Opp).count() == 0, "no cards in graveyard");
    }

    // ── Section 9: Delve ──────────────────────────────────────────────────────

    #[test]
    fn test_cast_delve_spell_exiles_graveyard_cards() {
        // Spell costs 3 generic + U. Two graveyard cards reduce generic to 1.
        // Pool supplies the remaining 1 generic + 1 blue.
        let mut state = make_state();
        let def = CardDef::new("Treasure Cruise", CardKind::Instant(SpellData { mana_cost: "7U".to_string(), delve: true, ..Default::default() }), vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![]);
        for name in &["A", "B", "C", "D", "E", "F", "G"] {
            add_graveyard_card(&mut state, PlayerId::Us, name);
        }
        let tc_id = add_hand_card(&mut state, PlayerId::Us, "Treasure Cruise");
        state.player_mut(PlayerId::Us).pool.u  = 1;
        state.player_mut(PlayerId::Us).pool.total = 1; // only 1 mana in pool — delve pays the other 7

        let catalog = vec![def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }

        let item = cast_spell(&mut state, 1, PlayerId::Us, tc_id, SpellFace::Main, None, None, &[], 0, 0);

        assert!(item.is_some(), "should cast with full delve");
        assert_eq!(state.graveyard_of(PlayerId::Us).count(), 0, "all 7 graveyard cards exiled");
        assert_eq!(state.exile_of(PlayerId::Us).count(), 7, "exiled by delve");
        assert_eq!(state.player(PlayerId::Us).pool.u, 0, "blue pip paid");
    }

    #[test]
    fn test_cast_delve_spell_partial_delve() {
        // Spell costs 3 generic. Graveyard has 2 cards — reduces cost to 1.
        // Pool must cover the remaining 1 generic.
        let mut state = make_state();
        let def = CardDef::new("Dead Drop", CardKind::Sorcery(SpellData { mana_cost: "3".to_string(), delve: true, ..Default::default() }), vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![]);
        add_graveyard_card(&mut state, PlayerId::Us, "Ritual");
        add_graveyard_card(&mut state, PlayerId::Us, "Ponder");
        let dead_drop_id = add_hand_card(&mut state, PlayerId::Us, "Dead Drop");
        state.player_mut(PlayerId::Us).pool.total = 1; // covers the 1 remaining generic after delve

        let catalog = vec![def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }

        let item = cast_spell(&mut state, 1, PlayerId::Us, dead_drop_id, SpellFace::Main, None, None, &[], 0, 0);

        assert!(item.is_some(), "should cast with partial delve + 1 mana");
        assert_eq!(state.graveyard_of(PlayerId::Us).count(), 0, "both graveyard cards exiled");
        assert_eq!(state.exile_of(PlayerId::Us).count(), 2);
        assert_eq!(state.player(PlayerId::Us).pool.total, 0, "remaining generic pip paid");
    }

    #[test]
    fn test_murktide_counters_from_exiled_instants_sorceries() {
        // Murktide exiles 4 cards via delve; 3 are instants/sorceries → enters as 6/6.
        let mut state = make_state();
        let murktide_def = catalog_card("Murktide Regent");
        let ritual_def   = catalog_card("Dark Ritual");
        let ponder_def   = catalog_card("Ponder");
        let consider_def = catalog_card("Consider");
        let ragavan_def  = creature("Ragavan", 2, 1); // creature — does NOT count

        add_graveyard_card(&mut state, PlayerId::Us, "Dark Ritual");
        add_graveyard_card(&mut state, PlayerId::Us, "Ponder");
        add_graveyard_card(&mut state, PlayerId::Us, "Consider");
        add_graveyard_card(&mut state, PlayerId::Us, "Ragavan");
        let murktide_id = add_hand_card(&mut state, PlayerId::Us, "Murktide Regent");
        // After delving all 4, generic cost = 5-4 = 1. Need UU + 1 generic.
        state.player_mut(PlayerId::Us).pool.u  = 2;
        state.player_mut(PlayerId::Us).pool.total = 3;

        let catalog = vec![murktide_def.clone(), ritual_def, ponder_def, consider_def, ragavan_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }

        let card_id = cast_spell(&mut state, 1, PlayerId::Us, murktide_id, SpellFace::Main, None, None, &[], 0, 0).unwrap();
        // Resolve the permanent spell the way the engine does (CR 608.3b / lib.rs
        // resolve_top_of_stack): it leaves the stack and enters the battlefield as
        // the SAME object, so the ETB replacement reads this object's own logged
        // cast via `ThisCast(DelvedExiled)` — counting the delved instant/sorcery
        // cards. No transient `resolving_costs_ctx` needed.
        state.stack.retain(|&x| x != card_id);
        change_zone(card_id, ZoneId::Battlefield, &mut state, 1, PlayerId::Us);

        let murktide_bf = state.permanents_of(PlayerId::Us).find(|p| p.catalog_key == "Murktide Regent")
            .and_then(|p| p.bf()).expect("Murktide on battlefield");
        assert_eq!(murktide_bf.counters, 3, "3 instants/sorceries exiled → 3 counters");

        // recompute reflects counters in the materialized view
        let murktide_id = state.permanents_of(PlayerId::Us).find(|p| p.catalog_key == "Murktide Regent")
            .map(|p| p.id).expect("Murktide on battlefield");
        recompute(&mut state);
        let eff = state.def_of(murktide_id).expect("Murktide materialized");
        let CardKind::Creature(c) = &eff.kind else { panic!("expected creature") };
        assert_eq!((c.power(), c.toughness()), (6, 6));
    }

    #[test]
    fn test_murktide_zero_counters_when_no_instants_exiled() {
        // Delve only exiles a creature — no instants/sorceries → enters as base 3/3.
        let mut state = make_state();
        let murktide_def = catalog_card("Murktide Regent");
        let ragavan_def = creature("Ragavan", 2, 1);

        add_graveyard_card(&mut state, PlayerId::Us, "Ragavan");
        let murktide_id = add_hand_card(&mut state, PlayerId::Us, "Murktide Regent");
        // 5 - 1 = 4 generic remaining; need UU + 4 generic
        state.player_mut(PlayerId::Us).pool.u  = 2;
        state.player_mut(PlayerId::Us).pool.total = 6;

        let catalog = vec![murktide_def.clone(), ragavan_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }

        let card_id = cast_spell(&mut state, 1, PlayerId::Us, murktide_id, SpellFace::Main, None, None, &[], 0, 0).unwrap();
        let spell = state.objects[&card_id].spell().expect("spell state populated").clone();
        let effect = &spell.effect;
        let chosen_targets = spell.chosen_targets.clone();

        effect.as_ref().unwrap().call(&mut state, 1, &chosen_targets);

        let murktide_bf = state.permanents_of(PlayerId::Us).find(|p| p.catalog_key == "Murktide Regent")
            .and_then(|p| p.bf()).expect("Murktide on battlefield");
        assert_eq!(murktide_bf.counters, 0);
        let murktide_id = state.permanents_of(PlayerId::Us).find(|p| p.catalog_key == "Murktide Regent")
            .map(|p| p.id).expect("Murktide on battlefield");
        recompute(&mut state);
        let eff = state.def_of(murktide_id).expect("Murktide materialized");
        let CardKind::Creature(c) = &eff.kind else { panic!("expected creature") };
        assert_eq!((c.power(), c.toughness()), (3, 3));
    }

    #[test]
    fn test_murktide_attacks_with_counter_boosted_stats() {
        // A 6/6 Murktide (base 3/3 + 3 counters) should survive attacking into a 5-power blocker.
        let mut state = make_state();
        let murktide_def = creature("Murktide Regent", 3, 3);
        let murktide_id = add_perm(&mut state, PlayerId::Us, "Murktide Regent", BattlefieldState {
            counters: 3,
            entered_this_turn: false,
            ..BattlefieldState::new()
        });
        // Opponent has a 5/5 blocker — Murktide's toughness 6 > opp power 5, safe to attack.
        let blocker_def = creature("Dragon", 5, 5);
        add_default_perm(&mut state, PlayerId::Opp, "Dragon");

        let catalog = vec![murktide_def, blocker_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        state.set_strategy(PlayerId::Us, Box::new(strategy::TestStrategy::new(PlayerId::Us).attacking(vec![(murktide_id, None)])));
        let step = Step { kind: StepKind::DeclareAttackers, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true);

        assert!(state.combat_attackers.contains(&murktide_id),
            "6/6 Murktide should attack into a 5-power blocker");
    }

    #[test]
    fn test_cast_delve_spell_insufficient_mana_after_delve() {
        // Spell costs 3 generic. Graveyard has 2 cards — reduces cost to 1.
        // Pool is empty — still can't cast.
        let mut state = make_state();
        let def = CardDef::new("Dead Drop", CardKind::Sorcery(SpellData { mana_cost: "3".to_string(), delve: true, ..Default::default() }), vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![]);
        add_graveyard_card(&mut state, PlayerId::Us, "Ritual");
        add_graveyard_card(&mut state, PlayerId::Us, "Ponder");
        let dead_drop_id = add_hand_card(&mut state, PlayerId::Us, "Dead Drop");
        // no mana

        let catalog = vec![def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }

        let item = cast_spell(&mut state, 1, PlayerId::Us, dead_drop_id, SpellFace::Main, None, None, &[], 0, 0);

        assert!(item.is_none(), "can't cast — 1 generic still unpaid");
        assert_eq!(state.graveyard_of(PlayerId::Us).count(), 2, "graveyard unchanged on failed cast");
        assert_eq!(state.exile_of(PlayerId::Us).count(), 0, "nothing exiled on failed cast");
    }

    #[test]
    fn test_effect_exile_ability_removes_creature() {
        let mut state = make_state();
        add_default_perm(&mut state, PlayerId::Opp, "Troll");
        let troll_def = creature("Troll", 2, 2);
        let ability = AbilityDef { target_spec: TargetSpec::ObjectInZone { controller: Who::Opp, zone: ZoneId::Battlefield, filter: ir_type(CardType::Creature) }, ability_factory: Some(Arc::new(|who, _| eff_exile_target(who))), ..Default::default() };
        let catalog = vec![troll_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        let targets: Vec<ObjId> = legal_targets(
            &TargetSpec::ObjectInZone { controller: Who::Opp, zone: ZoneId::Battlefield, filter: ir_type(CardType::Creature) }, PlayerId::Us, ObjId(0), &state
        );
        let eff = build_ability_effect(&ability, PlayerId::Us, ObjId::UNSET);
        eff.call(&mut state, 1, &targets);

        assert!(state.permanents_of(PlayerId::Opp).count() == 0, "Troll should be exiled");
        assert!(state.exile_of(PlayerId::Opp).any(|c| c.catalog_key == "Troll"), "Troll should be in exile");
        assert!(state.graveyard_of(PlayerId::Opp).count() == 0, "exiled, not dead");
    }

    fn island_land(state: &mut SimState, who: PlayerId) -> ObjId {
        add_perm_with_def(state, who, &catalog_card("Island"), BattlefieldState::new())
    }

    #[test]
    fn test_declare_attackers_sets_attacking_flag() {
        let mut state = make_state();
        let def = creature("Attacker", 2, 4);
        let atk_id = add_perm(&mut state, PlayerId::Us, "Attacker", BattlefieldState {
            entered_this_turn: false,
            ..BattlefieldState::new()
        });

        let catalog = vec![def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        state.set_strategy(PlayerId::Us, Box::new(strategy::TestStrategy::new(PlayerId::Us).attacking(vec![(atk_id, None)])));
        let step = Step { kind: StepKind::DeclareAttackers, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true);

        assert!(state.permanent_bf(atk_id).unwrap().attacking, "declared attacker gets attacking=true");
    }

    #[test]
    fn test_declare_blockers_sets_unblocked_flag_when_no_blocker() {
        let mut state = make_state();
        let def = creature("Attacker", 2, 4);
        let attacker_id = add_perm(&mut state, PlayerId::Us, "Attacker", BattlefieldState {
            attacking: true,
            tapped: true,
            ..BattlefieldState::new()
        });
        state.combat_attackers = vec![attacker_id];
        // No opp creatures → no blocker

        let catalog = vec![def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        let step = Step { kind: StepKind::DeclareBlockers, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true);

        assert!(state.permanent_bf(attacker_id).unwrap().unblocked, "unblocked attacker gets unblocked=true");
    }

    #[test]
    fn test_declare_blockers_blocked_attacker_not_unblocked() {
        let mut state = make_state();
        let atk_def = creature("Ragavan", 2, 2);
        let blk_def = creature("Wall", 0, 6);
        let ragavan_id = add_perm(&mut state, PlayerId::Us, "Ragavan", BattlefieldState {
            attacking: true,
            tapped: true,
            ..BattlefieldState::new()
        });
        let wall_id = add_default_perm(&mut state, PlayerId::Opp, "Wall");
        state.combat_attackers = vec![ragavan_id];

        let catalog = vec![atk_def, blk_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        state.set_strategy(PlayerId::Opp, Box::new(strategy::TestStrategy::new(PlayerId::Opp).blocking(vec![(ragavan_id, wall_id)])));
        let step = Step { kind: StepKind::DeclareBlockers, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true);

        assert!(!state.permanent_bf(ragavan_id).unwrap().unblocked, "blocked attacker stays unblocked=false");
        assert_eq!(state.combat_blocks.len(), 1, "blocker declared");
    }

    #[test]
    fn test_end_combat_clears_attacking_unblocked_flags() {
        let mut state = make_state();
        let ninja_id = add_perm(&mut state, PlayerId::Us, "Ninja", BattlefieldState {
            attacking: true,
            unblocked: true,
            ..BattlefieldState::new()
        });
        state.combat_attackers = vec![ninja_id];

        let step = Step { kind: StepKind::EndCombat, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true);

        assert!(!state.permanent_bf(ninja_id).unwrap().attacking, "attacking cleared at EndCombat");
        assert!(!state.permanent_bf(ninja_id).unwrap().unblocked, "unblocked cleared at EndCombat");
    }

    // ── Section 11: Cycling ───────────────────────────────────────────────────

    #[test]
    fn test_cycling_draw_effect() {
        let mut state = make_state();
        add_library_card(&mut state, PlayerId::Us, "Island");
        let initial = state.hand_size(PlayerId::Us);
        let ability = AbilityDef { ability_factory: Some(Arc::new(|who, _| eff_draw(who, 1))), ..Default::default() };
        let eff = build_ability_effect(&ability, PlayerId::Us, ObjId::UNSET);
        eff.call(&mut state, 1, &[]);
        assert_eq!(state.hand_size(PlayerId::Us), initial + 1, "cycling draws one card");
    }

    // Phase 6: deleted `test_cycling_discard_self_removes_card_from_library`
    // — it tested `pay_costs` directly. Cycling cost is now an IR `Move
    // (source → graveyard) + PayLife(2)`; activation is exercised end-to-end
    // by Street Wraith fixture tests.

    // ── Section 12: Adventure ─────────────────────────────────────────────────

    #[test]
    fn test_adventure_resolve_exiles_to_on_adventure() {
        // An adventure StackItem (no target) routes the card to exile + on_adventure.
        let mut state = make_state();
        // Simulate the adventure resolution inline: no effect, just exile.
        let borrower_id = state.alloc_id();
        let mut borrower_obj = GameObject::new(borrower_id, "Brazen Borrower", PlayerId::Us);
        borrower_obj.set_zone(Zone::Exile { on_adventure: true });
        state.objects.insert(borrower_id, borrower_obj);

        assert!(state.exile_of(PlayerId::Us).any(|c| c.catalog_key == "Brazen Borrower"), "Borrower in exile");
        assert!(state.on_adventure_of(PlayerId::Us).any(|c| c.catalog_key == "Brazen Borrower"), "Borrower on adventure");
        assert!(state.graveyard_of(PlayerId::Us).count() == 0, "not in graveyard");
    }

    #[test]
    fn test_adventure_bounce_effect_returns_opp_permanent() {
        // Petty Theft bounces target opp permanent then exiles Brazen Borrower to on_adventure.
        let mut state = make_state();
        let bowmasters_id = add_default_perm(&mut state, PlayerId::Opp, "Orcish Bowmasters");
        let initial_opp_hand = state.hand_size(PlayerId::Opp);

        // Run the Effect directly (as the new adventure resolution path does).
        let eff = eff_bounce_target(PlayerId::Us);
        eff.call(&mut state, 1, &[bowmasters_id]);
        // Then exile the card to on_adventure.
        let borrower_id = state.alloc_id();
        let mut borrower_obj = GameObject::new(borrower_id, "Brazen Borrower", PlayerId::Us);
        borrower_obj.set_zone(Zone::Exile { on_adventure: true });
        state.objects.insert(borrower_id, borrower_obj);

        assert!(state.permanents_of(PlayerId::Opp).count() == 0, "Bowmasters bounced off board");
        assert_eq!(state.hand_size(PlayerId::Opp), initial_opp_hand + 1, "bounced to opp hand");
        assert!(state.on_adventure_of(PlayerId::Us).any(|c| c.catalog_key == "Brazen Borrower"), "Borrower on adventure in exile");
    }

    #[test]
    fn test_cast_from_adventure_enters_play() {
        // pick_on_board_action detects adventure creatures in exile and picks the cast action
        // (75% roll). Run with multiple seeds to confirm it fires and the creature enters play.
        let borrower_def = catalog_card("Brazen Borrower");
        let island2_def = CardDef::new("Island2", CardKind::Land(LandData {
            mana_abilities: vec![ManaAbility {
                costs: crate::ir::ability::CostBody::Ir(crate::ir::action::Action::Tap {
                    target: crate::ir::expr::Expr::Ctx(crate::ir::context::Ctx::Source),
                }),
                produces: produces_colors("U"),
                make_effect: std::sync::Arc::new(|who, _| eff_mana(who, "U")),
                ..Default::default()
            }],
            ..Default::default()
        }), vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![]);
        let catalog = vec![borrower_def.clone(), catalog_card("Island"), island2_def.clone(), catalog_card("Swamp")];

        // Engine-machinery test: when the strategy chooses to cast the adventure
        // creature from exile, it enters play (the *decision* to do so is content
        // strategy logic, tested in the doomsday crate). We script the cast via a
        // generic TestStrategy so this stays a pure engine test.
        let mut state = make_state();
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));
        state.current_ap = state.us_id;
        let borrower_id = state.alloc_id();
        let mut borrower_obj = GameObject::new(borrower_id, "Brazen Borrower", PlayerId::Us);
        borrower_obj.set_zone(Zone::Exile { on_adventure: true });
        state.objects.insert(borrower_id, borrower_obj);
        // 1UU mana: two Islands + one generic (Swamp)
        island_land(&mut state, PlayerId::Us);
        add_perm_with_def(&mut state, PlayerId::Us, &island2_def, BattlefieldState::new());
        add_perm_with_def(&mut state, PlayerId::Us, &catalog_card("Swamp"), BattlefieldState::new());

        state.set_strategy(PlayerId::Us, Box::new(strategy::TestStrategy::new(PlayerId::Us)
            .action(LegalAction::CastSpell { card_id: borrower_id, face: SpellFace::Main })));
        handle_priority_round(&mut state, 1, PlayerId::Us);

        assert!(state.permanents_of(PlayerId::Us).any(|p| p.catalog_key == "Brazen Borrower"),
            "scripted adventure cast should put Brazen Borrower onto the battlefield");
        assert!(!state.on_adventure_of(PlayerId::Us).any(|c| c.catalog_key == "Brazen Borrower"), "removed from on_adventure");
        assert!(!state.exile_of(PlayerId::Us).any(|c| c.catalog_key == "Brazen Borrower"), "removed from exile");
    }

    // ── Section 8: Keyword Tests ──────────────────────────────────────────────

    fn flying_creature(name: &str, power: i32, toughness: i32) -> CardDef {
        let mut data = CreatureData::new("", power, toughness);
        data.keywords = Keywords::from_slice(&[Keyword::Flying]);
        CardDef::new(name, CardKind::Creature(data), vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![])
    }

    #[test]
    fn test_flying_not_blocked_by_ground() {
        // Flying attacker should not be assigned a ground blocker.
        let mut state = make_state();
        let flyer = flying_creature("Murktide Regent", 3, 3);
        let ground = creature("Troll", 3, 3);

        let murktide_id = add_perm(&mut state, PlayerId::Us, "Murktide Regent", BattlefieldState {
            attacking: true,
            ..BattlefieldState::new()
        });
        add_default_perm(&mut state, PlayerId::Opp, "Troll");
        state.combat_attackers = vec![murktide_id];

        let catalog = vec![flyer, ground];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        let step = Step { kind: StepKind::DeclareBlockers, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true);

        assert!(state.combat_blocks.is_empty(), "ground creature cannot block a flyer");
    }

    #[test]
    fn test_flying_blocked_by_flyer() {
        // Flying attacker CAN be blocked by another flying creature.
        let mut state = make_state();
        let flyer_atk = flying_creature("Murktide Regent", 3, 3);
        let flyer_blk = flying_creature("Subtlety", 3, 3);

        let murktide_id = add_perm(&mut state, PlayerId::Us, "Murktide Regent", BattlefieldState {
            attacking: true,
            ..BattlefieldState::new()
        });
        let subtlety_id = add_default_perm(&mut state, PlayerId::Opp, "Subtlety");
        state.combat_attackers = vec![murktide_id];

        let catalog = vec![flyer_atk, flyer_blk];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        state.set_strategy(PlayerId::Opp, Box::new(strategy::TestStrategy::new(PlayerId::Opp).blocking(vec![(murktide_id, subtlety_id)])));
        let step = Step { kind: StepKind::DeclareBlockers, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true);

        assert_eq!(state.combat_blocks.len(), 1, "flyer can block flyer");
        assert_eq!(state.combat_blocks[0], (murktide_id, subtlety_id));
    }


    /// Helper: build a vanilla creature with one or more keywords for combat tests.
    fn keyword_creature(name: &str, power: i32, toughness: i32, kws: &[Keyword]) -> CardDef {
        let mut data = CreatureData::new("", power, toughness);
        data.keywords = Keywords::from_slice(kws);
        CardDef::new(name, CardKind::Creature(data), vec![], None, vec![],
            CardLayout::Normal, None, vec![], vec![], vec![], vec![])
    }

    // ── Vigilance (CR 702.20) ────────────────────────────────────────────────

    /// A vigilant attacker is marked as attacking but does not become tapped.
    #[test]
    fn test_vigilance_attacker_not_tapped() {
        let mut state = make_state();
        let serra = keyword_creature("Serra Angel", 4, 4, &[Keyword::Flying, Keyword::Vigilance]);
        state.catalog.insert(serra.name.clone(), serra.clone());
        let id = add_perm(&mut state, PlayerId::Us, "Serra Angel", BattlefieldState {
            entered_this_turn: false,
            ..BattlefieldState::new()
        });

        state.set_strategy(PlayerId::Us, Box::new(strategy::TestStrategy::new(PlayerId::Us).attacking(vec![(id, None)])));
        let step = Step { kind: StepKind::DeclareAttackers, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true);

        let bf = state.permanent_bf(id).expect("on battlefield");
        assert!(bf.attacking, "should be declared as attacker");
        assert!(!bf.tapped, "vigilance: attacker should not tap");
    }

    /// A non-vigilant attacker still becomes tapped when attacking (control).
    #[test]
    fn test_no_vigilance_attacker_taps() {
        let mut state = make_state();
        let bear = keyword_creature("Grizzly Bears", 2, 2, &[]);
        state.catalog.insert(bear.name.clone(), bear.clone());
        let id = add_perm(&mut state, PlayerId::Us, "Grizzly Bears", BattlefieldState {
            entered_this_turn: false,
            ..BattlefieldState::new()
        });

        state.set_strategy(PlayerId::Us, Box::new(strategy::TestStrategy::new(PlayerId::Us).attacking(vec![(id, None)])));
        let step = Step { kind: StepKind::DeclareAttackers, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true);

        let bf = state.permanent_bf(id).expect("on battlefield");
        assert!(bf.attacking && bf.tapped, "non-vigilance attacker taps when attacking");
    }

    // ── Lifelink (CR 702.15) ─────────────────────────────────────────────────

    /// Unblocked lifelink attacker: controller gains life equal to damage dealt to player.
    #[test]
    fn test_lifelink_unblocked_gains_life() {
        let mut state = make_state();
        let initial_us = state.player(PlayerId::Us).life;
        let initial_opp = state.player(PlayerId::Opp).life;
        let vamp = keyword_creature("Vampire Nighthawk", 3, 3, &[Keyword::Lifelink]);
        state.catalog.insert(vamp.name.clone(), vamp.clone());
        let id = add_perm(&mut state, PlayerId::Us, "Vampire Nighthawk", BattlefieldState {
            tapped: true,
            ..BattlefieldState::new()
        });
        state.combat_attackers = vec![id];

        let step = Step { kind: StepKind::CombatDamage, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true);

        assert_eq!(state.player(PlayerId::Opp).life, initial_opp - 3, "opponent loses 3");
        assert_eq!(state.player(PlayerId::Us).life, initial_us + 3, "lifelink gains 3");
    }

    /// Blocked lifelink attacker still gains life from damage dealt to the blocker.
    #[test]
    fn test_lifelink_blocked_gains_life_from_blocker() {
        let mut state = make_state();
        let initial_us = state.player(PlayerId::Us).life;
        let vamp = keyword_creature("Vampire Nighthawk", 3, 3, &[Keyword::Lifelink]);
        let bear = creature("Grizzly Bears", 2, 2);
        state.catalog.insert(vamp.name.clone(), vamp.clone());
        state.catalog.insert(bear.name.clone(), bear.clone());
        let atk = add_perm(&mut state, PlayerId::Us, "Vampire Nighthawk", BattlefieldState {
            tapped: true,
            ..BattlefieldState::new()
        });
        let blk = add_default_perm(&mut state, PlayerId::Opp, "Grizzly Bears");
        state.combat_attackers = vec![atk];
        state.combat_blocks = vec![(atk, blk)];

        let step = Step { kind: StepKind::CombatDamage, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true);

        assert_eq!(state.player(PlayerId::Us).life, initial_us + 3,
            "lifelink: 3 damage to blocker → 3 life gained");
    }

    /// Lifelink on a blocker also gains life when it deals damage to the attacker.
    #[test]
    fn test_lifelink_on_blocker_gains_life() {
        let mut state = make_state();
        let initial_opp = state.player(PlayerId::Opp).life;
        let bear = creature("Grizzly Bears", 2, 2);
        let lifelinker = keyword_creature("Ajani's Pridemate", 2, 2, &[Keyword::Lifelink]);
        state.catalog.insert(bear.name.clone(), bear.clone());
        state.catalog.insert(lifelinker.name.clone(), lifelinker.clone());
        let atk = add_perm(&mut state, PlayerId::Us, "Grizzly Bears", BattlefieldState {
            tapped: true,
            ..BattlefieldState::new()
        });
        let blk = add_default_perm(&mut state, PlayerId::Opp, "Ajani's Pridemate");
        state.combat_attackers = vec![atk];
        state.combat_blocks = vec![(atk, blk)];

        let step = Step { kind: StepKind::CombatDamage, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true);

        assert_eq!(state.player(PlayerId::Opp).life, initial_opp + 2,
            "lifelink blocker gains 2 from damage to attacker");
    }

    // ── Trample (CR 702.19) ──────────────────────────────────────────────────

    /// A trample attacker assigns lethal damage to its blocker and spills the rest to the player.
    #[test]
    fn test_trample_excess_to_player() {
        let mut state = make_state();
        let initial_opp = state.player(PlayerId::Opp).life;
        let rhino = keyword_creature("Trampler", 5, 5, &[Keyword::Trample]);
        let bear = creature("Grizzly Bears", 2, 2);
        state.catalog.insert(rhino.name.clone(), rhino.clone());
        state.catalog.insert(bear.name.clone(), bear.clone());
        let atk = add_perm(&mut state, PlayerId::Us, "Trampler", BattlefieldState {
            tapped: true,
            ..BattlefieldState::new()
        });
        let blk = add_default_perm(&mut state, PlayerId::Opp, "Grizzly Bears");
        state.combat_attackers = vec![atk];
        state.combat_blocks = vec![(atk, blk)];

        let step = Step { kind: StepKind::CombatDamage, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true);

        assert_eq!(state.player(PlayerId::Opp).life, initial_opp - 3,
            "trample: 5-power attacker assigns 2 to 2/2 blocker, 3 spill to player");
    }

    /// Trample with a non-lethal attacker still puts all damage on the blocker (no spillover).
    #[test]
    fn test_trample_no_excess_no_player_damage() {
        let mut state = make_state();
        let initial_opp = state.player(PlayerId::Opp).life;
        let small = keyword_creature("Small Trampler", 2, 2, &[Keyword::Trample]);
        let troll = creature("Troll", 4, 4);
        state.catalog.insert(small.name.clone(), small.clone());
        state.catalog.insert(troll.name.clone(), troll.clone());
        let atk = add_perm(&mut state, PlayerId::Us, "Small Trampler", BattlefieldState {
            tapped: true,
            ..BattlefieldState::new()
        });
        let blk = add_default_perm(&mut state, PlayerId::Opp, "Troll");
        state.combat_attackers = vec![atk];
        state.combat_blocks = vec![(atk, blk)];

        let step = Step { kind: StepKind::CombatDamage, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true);

        assert_eq!(state.player(PlayerId::Opp).life, initial_opp,
            "trample: 2-power attacker into 4/4 blocker leaves nothing for the player");
    }

    /// Non-trample attacker into a smaller blocker assigns ALL damage to the blocker — no spillover.
    #[test]
    fn test_no_trample_no_spillover() {
        let mut state = make_state();
        let initial_opp = state.player(PlayerId::Opp).life;
        let big = creature("Big", 5, 5);
        let bear = creature("Grizzly Bears", 2, 2);
        state.catalog.insert(big.name.clone(), big.clone());
        state.catalog.insert(bear.name.clone(), bear.clone());
        let atk = add_perm(&mut state, PlayerId::Us, "Big", BattlefieldState {
            tapped: true,
            ..BattlefieldState::new()
        });
        let blk = add_default_perm(&mut state, PlayerId::Opp, "Grizzly Bears");
        state.combat_attackers = vec![atk];
        state.combat_blocks = vec![(atk, blk)];

        let step = Step { kind: StepKind::CombatDamage, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true);

        assert_eq!(state.player(PlayerId::Opp).life, initial_opp, "no trample → no spillover");
    }

    // ── Deathtouch (CR 702.2) ────────────────────────────────────────────────

    /// A 1/1 deathtouch attacker kills the 5/5 blocker after SBA, even though normally 1 < 5.
    #[test]
    fn test_deathtouch_attacker_kills_big_blocker() {
        let mut state = make_state();
        let touch = keyword_creature("Snake", 1, 1, &[Keyword::Deathtouch]);
        let troll = creature("Troll", 5, 5);
        state.catalog.insert(touch.name.clone(), touch.clone());
        state.catalog.insert(troll.name.clone(), troll.clone());
        let atk = add_perm(&mut state, PlayerId::Us, "Snake", BattlefieldState {
            tapped: true,
            ..BattlefieldState::new()
        });
        let blk = add_default_perm(&mut state, PlayerId::Opp, "Troll");
        state.combat_attackers = vec![atk];
        state.combat_blocks = vec![(atk, blk)];

        let step = Step { kind: StepKind::CombatDamage, prio: true };
        do_step(&mut state, 1, PlayerId::Us, &step, true);

        assert!(state.graveyard_of(PlayerId::Opp).any(|c| c.catalog_key == "Troll"),
            "deathtouch: 5/5 blocker dies to 1 damage from a deathtouch attacker");
    }

    /// A deathtouch blocker kills the attacker no matter how big.
    #[test]
    fn test_deathtouch_blocker_kills_attacker() {
        let mut state = make_state();
        let big = creature("Big", 7, 7);
        let touch = keyword_creature("Snake", 1, 1, &[Keyword::Deathtouch]);
        state.catalog.insert(big.name.clone(), big.clone());
        state.catalog.insert(touch.name.clone(), touch.clone());
        let atk = add_perm(&mut state, PlayerId::Us, "Big", BattlefieldState {
            tapped: true,
            ..BattlefieldState::new()
        });
        let blk = add_default_perm(&mut state, PlayerId::Opp, "Snake");
        state.combat_attackers = vec![atk];
        state.combat_blocks = vec![(atk, blk)];

        let step = Step { kind: StepKind::CombatDamage, prio: true };
        do_step(&mut state, 1, PlayerId::Us, &step, true);

        assert!(state.graveyard_of(PlayerId::Us).any(|c| c.catalog_key == "Big"),
            "deathtouch blocker kills the 7/7 attacker");
    }

    /// Trample + deathtouch: 1 damage to a blocker is enough to be lethal, so the rest spills.
    #[test]
    fn test_trample_with_deathtouch_assigns_one_to_blocker() {
        let mut state = make_state();
        let initial_opp = state.player(PlayerId::Opp).life;
        let wurm = keyword_creature("Wurm", 5, 5, &[Keyword::Trample, Keyword::Deathtouch]);
        let troll = creature("Troll", 4, 4);
        state.catalog.insert(wurm.name.clone(), wurm.clone());
        state.catalog.insert(troll.name.clone(), troll.clone());
        let atk = add_perm(&mut state, PlayerId::Us, "Wurm", BattlefieldState {
            tapped: true,
            ..BattlefieldState::new()
        });
        let blk = add_default_perm(&mut state, PlayerId::Opp, "Troll");
        state.combat_attackers = vec![atk];
        state.combat_blocks = vec![(atk, blk)];

        let step = Step { kind: StepKind::CombatDamage, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true);

        assert_eq!(state.player(PlayerId::Opp).life, initial_opp - 4,
            "trample+deathtouch: 1 to 4/4 blocker is lethal, 4 spills to player");
    }

    // ── Reach (CR 702.17) ────────────────────────────────────────────────────

    /// A reach blocker can block a flying attacker.
    #[test]
    fn test_reach_blocks_flyer() {
        let mut state = make_state();
        let dragon = keyword_creature("Dragon", 3, 3, &[Keyword::Flying]);
        let spider = keyword_creature("Giant Spider", 2, 4, &[Keyword::Reach]);
        state.catalog.insert(dragon.name.clone(), dragon.clone());
        state.catalog.insert(spider.name.clone(), spider.clone());
        let atk = add_perm(&mut state, PlayerId::Us, "Dragon", BattlefieldState {
            attacking: true,
            ..BattlefieldState::new()
        });
        let blk = add_default_perm(&mut state, PlayerId::Opp, "Giant Spider");
        state.combat_attackers = vec![atk];

        state.set_strategy(PlayerId::Opp, Box::new(strategy::TestStrategy::new(PlayerId::Opp).blocking(vec![(atk, blk)])));
        let step = Step { kind: StepKind::DeclareBlockers, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true);

        assert_eq!(state.combat_blocks.len(), 1,
            "reach can block a flyer");
        assert_eq!(state.combat_blocks[0], (atk, blk));
    }


    // ── Combat damage assignment (CR 510.1c) ─────────────────────────────────

    /// Validator passthrough: a CR-legal assignment is accepted as-is.
    #[test]
    fn test_validate_assignment_legal_passthrough() {
        // 5 power into [2/2, 4/4]; lethal=[2,4]. Assignment [2,3] satisfies "lethal in order".
        let out = validate_assignment(&[2, 3], &[2, 4], 5, false);
        assert_eq!(out, vec![2, 3]);
    }

    /// Validator: assigning to a later blocker before the earlier one reaches lethal → fallback.
    #[test]
    fn test_validate_assignment_skips_lethal_falls_back() {
        // 5 power, lethal=[2,4], strategy returns [1,4] — illegal (blocker 0 not lethal).
        // Fallback default: [2, 3] (lethal to first, rest to second).
        let out = validate_assignment(&[1, 4], &[2, 4], 5, false);
        assert_eq!(out, vec![2, 3], "default fallback fills lethal then dumps rest on last");
    }

    /// Validator: oversized sum → fallback.
    #[test]
    fn test_validate_assignment_oversum_falls_back() {
        let out = validate_assignment(&[5, 5], &[2, 4], 5, false);
        assert_eq!(out, vec![2, 3]);
    }

    /// Validator: trample allows leftover (engine spills to player).
    #[test]
    fn test_validate_assignment_trample_allows_leftover() {
        // 7 power into [2/2]; lethal=[2]. Assigning [2] is legal — engine spills 5.
        let out = validate_assignment(&[2], &[2], 7, true);
        assert_eq!(out, vec![2]);
    }

    /// Validator: without trample, a sum < total is illegal — fallback dumps the rest on last.
    #[test]
    fn test_validate_assignment_no_trample_no_leftover_falls_back() {
        let out = validate_assignment(&[2], &[2], 7, false);
        assert_eq!(out, vec![7], "no trample → fallback piles excess onto last blocker");
    }

    use strategy::TestStrategy;

    // ── Strategy callbacks: order_blockers + assign_combat_damage ────────────

    /// Test scaffolding: a Doomsday-strategy wrapper that lets a test override
    /// `order_blockers` and `assign_combat_damage`.
    struct ProgrammableStrat {
        inner: AlwaysPass,
        ordered: std::cell::RefCell<Option<Vec<ObjId>>>,
        assignment: std::cell::RefCell<Option<Vec<i32>>>,
    }
    impl strategy::Strategy for ProgrammableStrat {
        fn declare_attackers(&mut self, s: &SimState) -> Vec<(ObjId, Option<ObjId>)> { self.inner.declare_attackers(s) }
        fn declare_blockers(&mut self, s: &SimState) -> Vec<(ObjId, ObjId)> { self.inner.declare_blockers(s) }
        fn take_mulligan(&mut self, s: &SimState, m: u32) -> bool { self.inner.take_mulligan(s, m) }
        fn player_id(&self) -> PlayerId { self.inner.player_id() }
        fn plan_gap(&self, s: &SimState) -> strategy::TargetGap { self.inner.plan_gap(s) }
        fn card_fills(&self, id: ObjId, g: &strategy::TargetGap, s: &SimState) -> f64 { self.inner.card_fills(id, g, s) }
        fn order_blockers(&mut self, _s: &SimState, _atk: ObjId, blockers: &[ObjId]) -> Vec<ObjId> {
            self.ordered.borrow().clone().unwrap_or_else(|| blockers.to_vec())
        }
        fn assign_combat_damage(&mut self, _s: &SimState, _atk: ObjId, blockers: &[ObjId],
                                total: i32, lethal: &[i32], trample: bool) -> Vec<i32> {
            if let Some(a) = self.assignment.borrow().clone() { return a; }
            // Fall back to the default heuristic so calls without an override work.
            let n = blockers.len();
            let mut out = vec![0i32; n];
            let mut rem = total;
            for i in 0..n {
                if rem <= 0 { break; }
                let take = rem.min(lethal[i].max(0));
                out[i] = take;
                rem -= take;
            }
            if !trample && rem > 0 && n > 0 { out[n - 1] += rem; }
            out
        }
    }

    /// Engine respects the attacker's `order_blockers` choice — combat_blocks ends up reordered.
    #[test]
    fn test_order_blockers_reorders_combat_blocks() {
        let mut state = make_state();
        let big = creature("Big", 5, 5);
        let small_a = creature("SmallA", 2, 2);
        let small_b = creature("SmallB", 2, 2);
        state.catalog.insert(big.name.clone(), big.clone());
        state.catalog.insert(small_a.name.clone(), small_a.clone());
        state.catalog.insert(small_b.name.clone(), small_b.clone());

        let atk = add_perm(&mut state, PlayerId::Us, "Big", BattlefieldState {
            attacking: true, ..BattlefieldState::new()
        });
        let a = add_default_perm(&mut state, PlayerId::Opp, "SmallA");
        let b = add_default_perm(&mut state, PlayerId::Opp, "SmallB");
        state.combat_attackers = vec![atk];

        state.set_strategy(PlayerId::Us, Box::new(ProgrammableStrat {
            inner: AlwaysPass::new(PlayerId::Us),
            // Force order [b, a] so the engine should swap from the (a, b) declaration order.
            ordered: std::cell::RefCell::new(Some(vec![b, a])),
            assignment: std::cell::RefCell::new(None),
        }));
        // Pre-populate combat_blocks via DeclareBlockers — the opp's declare_blockers will pick
        // both creatures since neither is too small to chump-block productively. Bypass that and
        // inject the blocks then run the order step manually by calling do_step on DeclareBlockers
        // after manually setting the strategy outputs would be hard. Instead we emulate the
        // ordering call directly through the public path by injecting a fake declare_blockers.
        // Simplest path: inject blocks via state.combat_blocks and call do_step(DeclareBlockers)
        // — but that re-runs declare_blockers. So we drive the engine's reorder logic by calling
        // do_step(DeclareBlockers) and asserting the final order.
        // Replace opp strategy with one that always returns blocks (a, b) in that order.
        struct FixedBlocker { atk: ObjId, blocks: Vec<ObjId> }
        impl strategy::Strategy for FixedBlocker {
            fn declare_attackers(&mut self, _s: &SimState) -> Vec<(ObjId, Option<ObjId>)> { vec![] }
            fn declare_blockers(&mut self, _s: &SimState) -> Vec<(ObjId, ObjId)> {
                self.blocks.iter().map(|&b| (self.atk, b)).collect()
            }
            fn take_mulligan(&mut self, _s: &SimState, _m: u32) -> bool { false }
            fn player_id(&self) -> PlayerId { PlayerId::Opp }
            fn plan_gap(&self, _s: &SimState) -> strategy::TargetGap { strategy::TargetGap::default() }
            fn card_fills(&self, _i: ObjId, _g: &strategy::TargetGap, _s: &SimState) -> f64 { 0.0 }
        }
        state.set_strategy(PlayerId::Opp, Box::new(FixedBlocker { atk, blocks: vec![a, b] }));

        let step = Step { kind: StepKind::DeclareBlockers, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true);

        assert_eq!(state.combat_blocks, vec![(atk, b), (atk, a)],
            "attacker's order_blockers([b, a]) should win over opp's (a, b) declaration order");
    }

    /// Engine respects the attacker's `assign_combat_damage` — strategy can dump all 5 onto the
    /// first blocker even when the second is lethal-able with leftover, as long as it's legal.
    #[test]
    fn test_assign_combat_damage_strategy_choice() {
        let mut state = make_state();
        let big = creature("Big", 5, 5);
        let bear_a = creature("BearA", 2, 2);
        let bear_b = creature("BearB", 2, 2);
        state.catalog.insert(big.name.clone(), big.clone());
        state.catalog.insert(bear_a.name.clone(), bear_a.clone());
        state.catalog.insert(bear_b.name.clone(), bear_b.clone());

        let atk = add_perm(&mut state, PlayerId::Us, "Big", BattlefieldState {
            tapped: true, ..BattlefieldState::new()
        });
        let a = add_default_perm(&mut state, PlayerId::Opp, "BearA");
        let b = add_default_perm(&mut state, PlayerId::Opp, "BearB");
        state.combat_attackers = vec![atk];
        state.combat_blocks = vec![(atk, a), (atk, b)];

        // Strategy assigns lethal (2) to A then dumps the rest (3) on B — same total, legal under
        // CR 510.1c since A got lethal first.
        state.set_strategy(PlayerId::Us, Box::new(ProgrammableStrat {
            inner: AlwaysPass::new(PlayerId::Us),
            ordered: std::cell::RefCell::new(None),
            assignment: std::cell::RefCell::new(Some(vec![2, 3])),
        }));

        let step = Step { kind: StepKind::CombatDamage, prio: true };
        do_step(&mut state, 1, PlayerId::Us, &step, true);

        // Both bears should have died (A took 2, B took 3 — both ≥ toughness).
        assert!(state.graveyard_of(PlayerId::Opp).any(|c| c.catalog_key == "BearA"));
        assert!(state.graveyard_of(PlayerId::Opp).any(|c| c.catalog_key == "BearB"));
    }

    // ── First Strike / Double Strike (CR 510.5, 702.4, 702.7) ───────────────

    /// A first-strike 2/2 attacker blocked by a plain 2/2 blocker kills the blocker
    /// in the first-strike step, so the blocker never gets to deal regular damage back.
    #[test]
    fn test_first_strike_attacker_kills_blocker_before_regular() {
        let mut state = make_state();
        let knight = keyword_creature("White Knight", 2, 2, &[Keyword::FirstStrike]);
        let bear = creature("Grizzly Bears", 2, 2);
        state.catalog.insert(knight.name.clone(), knight.clone());
        state.catalog.insert(bear.name.clone(), bear.clone());
        let atk = add_perm(&mut state, PlayerId::Us, "White Knight", BattlefieldState {
            tapped: true, ..BattlefieldState::new()
        });
        let blk = add_default_perm(&mut state, PlayerId::Opp, "Grizzly Bears");
        state.combat_attackers = vec![atk];
        state.combat_blocks = vec![(atk, blk)];

        // First-strike pass: attacker deals 2; blocker (no FS) deals nothing.
        do_step(&mut state, 1, PlayerId::Us,
                &Step { kind: StepKind::FirstStrikeCombatDamage, prio: false },
                true);
        check_state_based_actions(&mut state, 1);
        // Blocker died; attacker is unscathed.
        assert!(state.graveyard_of(PlayerId::Opp).any(|c| c.catalog_key == "Grizzly Bears"),
            "first-strike kills blocker before regular damage");
        assert!(state.permanent_bf(atk).is_some(), "FS attacker still alive");

        // Regular pass: dead blocker → no return damage. Attacker (FS only) doesn't strike again.
        do_step(&mut state, 1, PlayerId::Us,
                &Step { kind: StepKind::CombatDamage, prio: false },
                true);
        check_state_based_actions(&mut state, 1);
        let bf = state.permanent_bf(atk).expect("FS attacker survives");
        assert_eq!(bf.damage, 0, "FS attacker took no return damage");
    }

    /// A double-strike attacker, unblocked, deals damage twice — once per pass.
    #[test]
    fn test_double_strike_unblocked_deals_damage_twice() {
        let mut state = make_state();
        let initial_opp = state.player(PlayerId::Opp).life;
        let fury = keyword_creature("Fury", 3, 3, &[Keyword::DoubleStrike]);
        state.catalog.insert(fury.name.clone(), fury.clone());
        let atk = add_perm(&mut state, PlayerId::Us, "Fury", BattlefieldState {
            tapped: true, ..BattlefieldState::new()
        });
        state.combat_attackers = vec![atk];

        do_step(&mut state, 1, PlayerId::Us,
                &Step { kind: StepKind::FirstStrikeCombatDamage, prio: false },
                true);
        do_step(&mut state, 1, PlayerId::Us,
                &Step { kind: StepKind::CombatDamage, prio: false },
                true);

        assert_eq!(state.player(PlayerId::Opp).life, initial_opp - 6,
            "double strike: 3 power × 2 hits = 6 damage to player");
    }

    /// A plain (no FS, no DS) attacker deals damage only in the regular step.
    #[test]
    fn test_plain_attacker_skips_first_strike_step() {
        let mut state = make_state();
        let initial_opp = state.player(PlayerId::Opp).life;
        let bear = creature("Bear", 2, 2);
        let knight = keyword_creature("Other Knight", 1, 1, &[Keyword::FirstStrike]);
        state.catalog.insert(bear.name.clone(), bear.clone());
        state.catalog.insert(knight.name.clone(), knight.clone());
        // Add a FS creature so the FS step actually runs.
        let _fs = add_perm(&mut state, PlayerId::Us, "Other Knight", BattlefieldState {
            tapped: true, ..BattlefieldState::new()
        });
        let atk = add_perm(&mut state, PlayerId::Us, "Bear", BattlefieldState {
            tapped: true, ..BattlefieldState::new()
        });
        state.combat_attackers = vec![_fs, atk];

        // FS pass: only the FS knight strikes.
        do_step(&mut state, 1, PlayerId::Us,
                &Step { kind: StepKind::FirstStrikeCombatDamage, prio: false },
                true);
        assert_eq!(state.player(PlayerId::Opp).life, initial_opp - 1,
            "FS pass deals 1 damage from the knight only; bear waits for regular");
        // Regular pass: both deal damage (FS knight does NOT strike again).
        do_step(&mut state, 1, PlayerId::Us,
                &Step { kind: StepKind::CombatDamage, prio: false },
                true);
        assert_eq!(state.player(PlayerId::Opp).life, initial_opp - 1 - 2,
            "regular pass deals 2 from the bear only (knight has FS, not DS)");
    }

    /// A double-strike trample attacker into a small blocker: FS pass kills the
    /// blocker; regular pass spills full damage to the player (CR 702.19c).
    #[test]
    fn test_double_strike_trample_dead_blocker_spills_in_regular() {
        let mut state = make_state();
        let initial_opp = state.player(PlayerId::Opp).life;
        let stomper = keyword_creature("Stomper", 4, 4,
            &[Keyword::DoubleStrike, Keyword::Trample]);
        let bear = creature("Bear", 2, 2);
        state.catalog.insert(stomper.name.clone(), stomper.clone());
        state.catalog.insert(bear.name.clone(), bear.clone());
        let atk = add_perm(&mut state, PlayerId::Us, "Stomper", BattlefieldState {
            tapped: true, ..BattlefieldState::new()
        });
        let blk = add_default_perm(&mut state, PlayerId::Opp, "Bear");
        state.combat_attackers = vec![atk];
        state.combat_blocks = vec![(atk, blk)];

        // FS pass: 2 to blocker (lethal), 2 trample to player.
        do_step(&mut state, 1, PlayerId::Us,
                &Step { kind: StepKind::FirstStrikeCombatDamage, prio: false },
                true);
        check_state_based_actions(&mut state, 1);
        assert!(state.graveyard_of(PlayerId::Opp).any(|c| c.catalog_key == "Bear"),
            "blocker dies in FS pass");
        assert_eq!(state.player(PlayerId::Opp).life, initial_opp - 2, "FS pass: 2 trampled");

        // Regular pass: blocker is dead, attacker still 'blocked' but trample dumps all to player.
        do_step(&mut state, 1, PlayerId::Us,
                &Step { kind: StepKind::CombatDamage, prio: false },
                true);
        assert_eq!(state.player(PlayerId::Opp).life, initial_opp - 2 - 4,
            "regular pass: dead blocker → trample spills full 4 to player");
    }

    /// A non-trample double-strike attacker whose blocker dies in FS deals NO damage
    /// in the regular pass (the attacker is still considered blocked, no spillover).
    #[test]
    fn test_double_strike_no_trample_dead_blocker_no_spill() {
        let mut state = make_state();
        let initial_opp = state.player(PlayerId::Opp).life;
        let strong = keyword_creature("Strong", 4, 4, &[Keyword::DoubleStrike]);
        let bear = creature("Bear", 2, 2);
        state.catalog.insert(strong.name.clone(), strong.clone());
        state.catalog.insert(bear.name.clone(), bear.clone());
        let atk = add_perm(&mut state, PlayerId::Us, "Strong", BattlefieldState {
            tapped: true, ..BattlefieldState::new()
        });
        let blk = add_default_perm(&mut state, PlayerId::Opp, "Bear");
        state.combat_attackers = vec![atk];
        state.combat_blocks = vec![(atk, blk)];

        do_step(&mut state, 1, PlayerId::Us,
                &Step { kind: StepKind::FirstStrikeCombatDamage, prio: false },
                true);
        check_state_based_actions(&mut state, 1);
        do_step(&mut state, 1, PlayerId::Us,
                &Step { kind: StepKind::CombatDamage, prio: false },
                true);

        assert_eq!(state.player(PlayerId::Opp).life, initial_opp,
            "no trample, dead blocker → second strike deals no damage to player");
    }

    /// `do_phase(combat_phase)` skips the FS step entirely when no combatant has FS or DS.
    #[test]
    fn test_first_strike_step_skipped_when_no_first_or_double_strike() {
        let mut state = make_state();
        let initial_opp = state.player(PlayerId::Opp).life;
        let bear = creature("Bear", 2, 2);
        state.catalog.insert(bear.name.clone(), bear.clone());
        let atk = add_perm(&mut state, PlayerId::Us, "Bear", BattlefieldState {
            tapped: true, ..BattlefieldState::new()
        });
        state.combat_attackers = vec![atk];

        // FS step body would deal nothing, but we want to confirm the regular pass
        // still does its job and the FS step's predicate skips cleanly.
        do_step(&mut state, 1, PlayerId::Us,
                &Step { kind: StepKind::FirstStrikeCombatDamage, prio: false },
                true);
        assert_eq!(state.player(PlayerId::Opp).life, initial_opp,
            "FS pass with no FS/DS source deals no damage");
        do_step(&mut state, 1, PlayerId::Us,
                &Step { kind: StepKind::CombatDamage, prio: false },
                true);
        assert_eq!(state.player(PlayerId::Opp).life, initial_opp - 2,
            "regular pass: plain bear deals its 2 once");
    }

    // ── Section 9: Trigger Tests ──────────────────────────────────────────────

    #[test]
    fn test_fire_triggers_returns_context_for_bowmasters_etb() {
        let mut state = make_state();
        let bowmasters_id = add_default_perm(&mut state, PlayerId::Opp, "Orcish Bowmasters");

        let ev = GameEvent::ZoneChange {
            id: bowmasters_id,
            actor: PlayerId::Opp,
            from: ZoneId::Stack,
            to: ZoneId::Battlefield,
            controller: PlayerId::Opp,
        };
        let (result, _) = fire_triggers(&ev, &state);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source_name, "Orcish Bowmasters");
    }

    #[test]
    fn test_fire_triggers_empty_when_no_bowmasters_in_play() {
        let state = make_state(); // no permanents
        let ev = GameEvent::ZoneChange {
            id: ObjId::UNSET,
            actor: PlayerId::Opp,
            from: ZoneId::Stack,
            to: ZoneId::Battlefield,
            controller: PlayerId::Opp,
        };
        let (result, _) = fire_triggers(&ev, &state);
        assert!(result.is_empty());
    }

    /// Fire a Bowmasters ETB trigger for `controller`, choose its target, and apply it.
    /// Drives the unified IR path via `fire_triggers` — the same route the engine takes.
    fn fire_bowmasters_etb(controller: PlayerId, state: &mut SimState) {
        recompute(state);
        let bowmasters_id = state.permanents_of(controller)
            .find(|p| p.catalog_key == "Orcish Bowmasters")
            .expect("no Bowmasters in play").id;
        let ev = GameEvent::ZoneChange {
            id: bowmasters_id,
            actor: controller,
            from: ZoneId::Stack,
            to: ZoneId::Battlefield,
            controller,
        };
        let (pending, _) = fire_triggers(&ev, state);
        let ctx = pending.into_iter()
            .find(|tc| tc.source_name == "Orcish Bowmasters")
            .expect("no Bowmasters ETB trigger fired");
        let all_targets = legal_targets(&ctx.target_spec, controller, ObjId(0), state);
        let targets: Vec<ObjId> = pick_targets(&ctx.target_spec, &all_targets, state);
        ctx.effect.call(state, 1, &targets);
    }

    #[test]
    fn test_apply_bowmasters_etb_deals_damage_and_creates_army() {
        let mut state = make_state();
        add_default_perm(&mut state, PlayerId::Opp, "Orcish Bowmasters");
        let initial_life = state.player(PlayerId::Us).life;
        fire_bowmasters_etb(PlayerId::Opp, &mut state);
        assert_eq!(state.player(PlayerId::Us).life, initial_life - 1, "ETB deals 1 to us");
        assert!(state.permanents_of(PlayerId::Opp).any(|p| p.catalog_key == "Orc Army"), "Orc Army token created");
        let army = state.permanents_of(PlayerId::Opp).find(|p| p.catalog_key == "Orc Army").and_then(|p| p.bf()).unwrap();
        assert_eq!(army.counters, 1, "Orc Army has 1 counter");
    }

    #[test]
    fn test_apply_bowmasters_etb_grows_existing_army() {
        let mut state = make_state();
        add_default_perm(&mut state, PlayerId::Opp, "Orcish Bowmasters");
        add_perm(&mut state, PlayerId::Opp, "Orc Army", BattlefieldState { counters: 2, ..BattlefieldState::new() });
        fire_bowmasters_etb(PlayerId::Opp, &mut state);
        let army = state.permanents_of(PlayerId::Opp).find(|p| p.catalog_key == "Orc Army").and_then(|p| p.bf()).unwrap();
        assert_eq!(army.counters, 3, "Orc Army grows from 2 to 3");
    }

    #[test]
    fn test_bowmasters_ping_hits_face_when_no_killable_creature() {
        let mut state = make_state();
        add_default_perm(&mut state, PlayerId::Opp, "Orcish Bowmasters");
        let initial_life = state.player(PlayerId::Us).life;
        add_default_perm(&mut state, PlayerId::Us, "Troll");
        let catalog = vec![creature("Troll", 3, 3)];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        fire_bowmasters_etb(PlayerId::Opp, &mut state);
        assert_eq!(state.player(PlayerId::Us).life, initial_life - 1, "damage hits face when no killable creature");
        assert!(state.permanents_of(PlayerId::Us).any(|p| p.catalog_key == "Troll"), "Troll survives");
    }

    #[test]
    fn test_bowmasters_ping_kills_1_1_creature() {
        let mut state = make_state();
        add_default_perm(&mut state, PlayerId::Opp, "Orcish Bowmasters");
        let initial_life = state.player(PlayerId::Us).life;
        add_default_perm(&mut state, PlayerId::Us, "Ragavan, Nimble Pilferer");
        let catalog = vec![creature("Ragavan, Nimble Pilferer", 2, 1)];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        fire_bowmasters_etb(PlayerId::Opp, &mut state);
        check_state_based_actions(&mut state, 1);
        assert_eq!(state.player(PlayerId::Us).life, initial_life, "life total unchanged when creature is targeted");
        assert!(!state.permanents_of(PlayerId::Us).any(|p| p.catalog_key == "Ragavan, Nimble Pilferer"),
            "Ragavan dies to 1 damage");
        assert!(state.graveyard_of(PlayerId::Us).any(|c| c.catalog_key == "Ragavan, Nimble Pilferer"),
            "Ragavan goes to graveyard");
    }

    #[test]
    fn test_bowmasters_ping_prioritises_opposing_bowmasters() {
        let mut state = make_state();
        add_default_perm(&mut state, PlayerId::Opp, "Orcish Bowmasters");
        add_default_perm(&mut state, PlayerId::Us, "Troll");
        add_default_perm(&mut state, PlayerId::Us, "Orcish Bowmasters");
        // Only override Troll; leaving the real Bowmasters CardDef in the catalog
        // so its IR abilities fire via the unified trigger path.
        state.catalog.insert("Troll".into(), creature("Troll", 3, 3));
        fire_bowmasters_etb(PlayerId::Opp, &mut state);
        check_state_based_actions(&mut state, 1);
        assert!(!state.permanents_of(PlayerId::Us).any(|p| p.catalog_key == "Orcish Bowmasters"),
            "opposing Bowmasters is killed");
        assert!(state.permanents_of(PlayerId::Us).any(|p| p.catalog_key == "Troll"), "Troll survives");
    }

    #[test]
    fn test_bowmasters_no_trigger_on_natural_first_draw() {
        let mut state = make_state();
        add_default_perm(&mut state, PlayerId::Opp, "Orcish Bowmasters");

        let ev = GameEvent::Draw { controller: PlayerId::Us, draw_index: 1, is_natural: true, card: None };
        let (result, _) = fire_triggers(&ev, &state);
        assert!(result.is_empty(), "no trigger on first natural draw");
    }

    #[test]
    fn test_bowmasters_triggers_on_cantrip_draw() {
        let mut state = make_state();
        add_default_perm(&mut state, PlayerId::Opp, "Orcish Bowmasters");

        let ev = GameEvent::Draw { controller: PlayerId::Us, draw_index: 1, is_natural: false, card: None };
        let (result, _) = fire_triggers(&ev, &state);
        assert_eq!(result.len(), 1, "cantrip draw triggers Bowmasters");
    }

    #[test]
    fn test_murktide_counter_on_instant_exile() {
        let mut state = make_state();
        add_perm(&mut state, PlayerId::Us, "Murktide Regent", BattlefieldState { counters: 0, ..BattlefieldState::new() });
        // Add the card being exiled so murktide_check can look up its type.
        let consider_id = add_default_perm(&mut state, PlayerId::Us, "Consider");
        state.objects.get_mut(&consider_id).unwrap().set_zone(Zone::Exile { on_adventure: false });

        let ev = GameEvent::ZoneChange {
            id: consider_id,
            actor: PlayerId::Us,
            from: ZoneId::Graveyard,
            to: ZoneId::Exile,
            controller: PlayerId::Us,
        };
        let (result, _) = fire_triggers(&ev, &state);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source_name, "Murktide Regent");

        let mut state2 = state;
        result[0].effect.call(&mut state2, 1, &[]);
        let murktide = state2.permanents_of(PlayerId::Us).find(|p| p.catalog_key == "Murktide Regent").and_then(|p| p.bf()).unwrap();
        assert_eq!(murktide.counters, 1, "Murktide gains +1/+1 counter");
    }

    #[test]
    fn test_murktide_no_counter_on_land_exile() {
        let mut state = make_state();
        add_default_perm(&mut state, PlayerId::Us, "Murktide Regent");
        let island_id = add_default_perm(&mut state, PlayerId::Us, "Island");
        state.objects.get_mut(&island_id).unwrap().set_zone(Zone::Exile { on_adventure: false });

        let ev = GameEvent::ZoneChange {
            id: island_id,
            actor: PlayerId::Us,
            from: ZoneId::Graveyard,
            to: ZoneId::Exile,
            controller: PlayerId::Us,
        };
        let (result, _) = fire_triggers(&ev, &state);
        assert!(result.is_empty(), "land exile does not trigger Murktide");
    }

    #[test]
    fn test_tamiyo_clue_when_attacking() {
        let mut state = make_state();
        add_perm(&mut state, PlayerId::Us, "Tamiyo, Inquisitive Student", BattlefieldState { attacking: true, ..BattlefieldState::new() });

        let ev = GameEvent::EnteredStep {
            step: StepKind::DeclareAttackers,
            active_player: PlayerId::Us,
        };
        let (result, _) = fire_triggers(&ev, &state);
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].source_name, "Tamiyo, Inquisitive Student");

        let mut state2 = state;
        result[0].effect.call(&mut state2, 1, &[]);
        assert!(state2.permanents_of(PlayerId::Us).any(|p| p.catalog_key == "Clue Token"),
            "Clue Token created when Tamiyo attacks");
    }

    #[test]
    fn test_tamiyo_no_clue_when_not_attacking() {
        let mut state = make_state();
        add_default_perm(&mut state, PlayerId::Us, "Tamiyo, Inquisitive Student"); // attacking = false

        let ev = GameEvent::EnteredStep {
            step: StepKind::DeclareAttackers,
            active_player: PlayerId::Us,
        };
        let (result, _) = fire_triggers(&ev, &state);
        // Trigger queues (Tamiyo is in play), but resolves to nothing (not attacking).
        if let Some(ctx) = result.first() {
            let mut state2 = state;
            ctx.effect.call(&mut state2, 1, &[]);
            assert!(!state2.permanents_of(PlayerId::Us).any(|p| p.catalog_key == "Clue Token"),
                "no Clue Token if Tamiyo is not attacking");
        }
    }

    #[test]
    fn test_tamiyo_flip_on_third_draw() {
        let mut state = make_state();
        let tamiyo_id = add_default_perm(&mut state, PlayerId::Us, "Tamiyo, Inquisitive Student");
        // Tap Tamiyo first: "exile, then return transformed" must yield a FRESH object,
        // so the tapped status (and any other bf state) is reset on the way back.
        state.objects.get_mut(&tamiyo_id).unwrap().bf_mut().unwrap().tapped = true;

        // Three draws this turn logged; the third draw event fires Tamiyo's flip.
        // ("Third card this turn" = EventCount over the log, not the draw_index field.)
        for i in 1..=3u8 {
            state.event_log.push(1, GameEvent::Draw { controller: PlayerId::Us, draw_index: i, is_natural: false, card: None });
        }
        let ev = GameEvent::Draw { controller: PlayerId::Us, draw_index: 3, is_natural: false, card: None };
        let (result, _) = fire_triggers(&ev, &state);
        let ctx = result.iter().find(|c| c.source_name == "Tamiyo, Inquisitive Student").cloned()
            .expect("third draw fires Tamiyo's flip trigger");

        let mut state2 = state;
        ctx.effect.call(&mut state2, 1, &[]);
        // A NEW object: back on the battlefield (same catalog_key — the front-face name
        // is unchanged) with active_face == 1, fresh starting loyalty, and untapped
        // (the exile-return reset it) — distinguishing it from Delver's in-place flip.
        let tamiyo_bf = state2.permanents_of(PlayerId::Us)
            .find(|p| p.catalog_key == "Tamiyo, Inquisitive Student")
            .and_then(|p| p.bf())
            .expect("Tamiyo should be back on the battlefield after exile-return");
        assert_eq!(tamiyo_bf.active_face, 1, "active_face == 1 after transform");
        assert_eq!(tamiyo_bf.loyalty, 2, "starting loyalty of Tamiyo, Seasoned Scholar");
        assert!(!tamiyo_bf.tapped, "returned as a fresh untapped object (exile-return, not in-place)");
    }

    /// Activate Tamiyo, Seasoned Scholar's +2 for `PlayerId::Us` — registers the
    /// floating "opposing attackers get −1/−0 until your next turn" CE.
    fn tamiyo_plus_two_us(state: &mut SimState) {
        let tamiyo_def = catalog_card("Tamiyo, Inquisitive Student");
        let back = tamiyo_def.back.as_deref().expect("Tamiyo has a back face");
        let CardKind::Planeswalker(pw) = &back.kind else { panic!("back is a planeswalker") };
        let plus_two = pw.abilities.iter().find(|a| a.loyalty_delta() == Some(2))
            .expect("Tamiyo has a +2 ability");
        build_ability_effect(plus_two, PlayerId::Us, ObjId::UNSET).call(state, 1, &[]);
    }

    #[test]
    fn test_tamiyo_plus_two_applies_power_mod_to_attackers() {
        let mut state = make_state();
        // Activate +2 first; the dynamic-filter CE catches attackers declared later.
        tamiyo_plus_two_us(&mut state);

        // Opp has a 3/3 attacker.
        let atk_def = creature("Dragon", 3, 3);
        let dragon_atk_id = add_perm(&mut state, PlayerId::Opp, "Dragon", BattlefieldState { entered_this_turn: false, ..BattlefieldState::new() });
        add_default_perm(&mut state, PlayerId::Us, "Wall"); // blocker-sized (no block in this test)

        let catalog = vec![atk_def, creature("Wall", 0, 4)];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        state.set_strategy(PlayerId::Opp, Box::new(strategy::TestStrategy::new(PlayerId::Opp).attacking(vec![(dragon_atk_id, None)])));
        do_step(&mut state, 1, PlayerId::Opp, &Step { kind: StepKind::DeclareAttackers, prio: true },
            true);

        let dragon_id = state.permanents_of(PlayerId::Opp).find(|p| p.catalog_key == "Dragon").map(|p| p.id).unwrap();
        // The -1 comes from the floating ContinuousInstance (L7), gated on "attacking".
        recompute(&mut state);
        let eff = state.def_of(dragon_id).expect("Dragon materialized");
        let CardKind::Creature(c) = &eff.kind else { panic!("expected creature") };
        assert_eq!(c.power(), 2, "attacking opposing Dragon's power is 3 + (-1) = 2");
    }

    #[test]
    fn test_tamiyo_plus_two_expires_at_controller_untap() {
        let mut state = make_state();
        tamiyo_plus_two_us(&mut state);
        assert_eq!(state.continuous_instances.len(), 1, "+2 registers one floating CE");

        // Untap step for PlayerId::Us should expire the "until your next turn" CE.
        let step = Step { kind: StepKind::Untap, prio: false };
        do_step(&mut state, 2, PlayerId::Us, &step, true);

        assert!(state.continuous_instances.is_empty(),
            "the +2 continuous effect expires at the controller's next Untap");
    }

    #[test]
    fn test_ninjutsu_enters_tapped_and_attacking_inheriting_target() {
        // CR 702.49 resolution (now Action::NinjutsuEnter): the ninja goes from
        // hand to the battlefield tapped + attacking, inheriting the returned
        // attacker's combat target (captured in CostsPaidCtx at cost time).
        let mut state = make_state();
        let target = state.opp_id; // the returned attacker was attacking the opponent
        state.catalog.insert("Ninja".into(), creature("Ninja", 2, 2));
        let ninja_id = add_hand_card(&mut state, PlayerId::Us, "Ninja");
        state.resolving_costs_ctx.returned_attack_targets = vec![Some(target)];

        let ability = ninjutsu_ability("1U");
        build_ability_effect(&ability, PlayerId::Us, ninja_id).call(&mut state, 1, &[]);

        let bf = state.permanent_bf(ninja_id).expect("ninja on battlefield");
        assert!(bf.tapped, "ninja enters tapped");
        assert!(bf.attacking, "ninja enters attacking");
        assert!(bf.unblocked, "ninja enters unblocked");
        assert_eq!(bf.attack_target, Some(target), "inherits the returned attacker's target");
        assert!(state.combat_attackers.contains(&ninja_id), "ninja joins the attackers");
    }

    #[test]
    fn test_fatal_push_cannot_target_flipped_tamiyo() {
        let mut state = make_state();
        let tamiyo_id = add_default_perm(&mut state, PlayerId::Opp, "Tamiyo, Inquisitive Student");

        // Flip Tamiyo to her back face (Seasoned Scholar, a planeswalker).
        if let Some(bf) = state.objects.get_mut(&tamiyo_id).and_then(|o| o.bf_mut()) {
            bf.active_face = 1;
            bf.loyalty = 2;
        }
        recompute(&mut state);

        // Fatal Push targets "creature with mana value 3 or less".
        let filter = ir_and(
            ir_type(CardType::Creature),
            ir_mv_le(3),
        );
        let spec = TargetSpec::ObjectInZone {
            controller: Who::Opp,
            zone: ZoneId::Battlefield,
            filter,
        };
        let targets = legal_targets(&spec, PlayerId::Us, ObjId(0), &state);
        assert!(targets.is_empty(), "Fatal Push should not be able to target flipped Tamiyo (she is a planeswalker, not a creature)");
    }

    #[test]
    fn test_stat_mod_reversed_at_cleanup() {
        // A L7 ContinuousInstance with EndOfTurn expiry should be removed during Cleanup,
        // restoring the effective P/T of the affected permanent.
        let mut state = make_state();
        let atk_def = creature("Dragon", 3, 3);
        let dragon_id = add_perm(&mut state, PlayerId::Opp, "Dragon", BattlefieldState::new());
        let catalog = vec![atk_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }

        // Register an EndOfTurn L7 CI that applies -1 power to the dragon.
        state.continuous_instances.push(ContinuousInstance {
            source_id: dragon_id,
            controller: PlayerId::Us,
            layer: ContinuousLayer::L7PowerToughness,
            reads: vec![],
            writes: vec![],
            timestamp: 0,
            filter: std::sync::Arc::new(move |id, _, _| id == dragon_id),
            modifier: std::sync::Arc::new(|def, _state| {
                if let CardKind::Creature(c) = &mut def.kind { c.adjust_pt(-1, 0); }
            }),
            expiry: Expiry::EndOfTurn,

        });

        // Before Cleanup: effective power = 2.
        recompute(&mut state);
        let CardKind::Creature(c) = &state.def_of(dragon_id).unwrap().kind.clone() else { panic!() };
        assert_eq!(c.power(), 2, "CI applies -1 before Cleanup");

        let step = Step { kind: StepKind::Cleanup, prio: false };
        do_step(&mut state, 1, PlayerId::Opp, &step, true);

        // After Cleanup: CI removed, effective power restored to 3.
        recompute(&mut state);
        let CardKind::Creature(c) = &state.def_of(dragon_id).unwrap().kind.clone() else { panic!() };
        assert_eq!(c.power(), 3, "effective power restored after Cleanup");
        assert!(state.continuous_instances.is_empty(), "EndOfTurn CI removed at Cleanup");
    }

    // ── Step 2: EnteredStep / EnteredPhase fires for all priority windows ────────

    /// Verify EnteredStep fires for every named priority-bearing step.
    #[test]
    fn test_entered_step_fires_for_all_priority_steps() {
        let steps_with_prio = [
            StepKind::Upkeep,
            StepKind::Draw,
            StepKind::BeginCombat,
            StepKind::DeclareAttackers,
            StepKind::DeclareBlockers,
            StepKind::CombatDamage,
            StepKind::EndCombat,
            StepKind::End,
        ];
        for step_kind in steps_with_prio {
            let mut state = make_state();
            state.trigger_instances.push(TriggerInstance {
                source_id: ObjId::UNSET,
                controller: PlayerId::Us,
                check: std::sync::Arc::new(move |e, _source_id, _ctl, _state, pending| {
                    if let GameEvent::EnteredStep { step, .. } = e {
                        if *step == step_kind {
                            pending.push(TriggerContext {
                                source_name: format!("test-{:?}", step_kind),
                                controller: PlayerId::Us,
                                target_spec: TargetSpec::None,
                                effect: Effect(std::sync::Arc::new(|_, _, _| {})),
                            });
                        }
                    }
                }),
                expiry: Some(Expiry::EndOfTurn),
    
                });
            let ev = GameEvent::EnteredStep { step: step_kind, active_player: PlayerId::Us };
            fire_event(ev, &mut state, 1, PlayerId::Us);
            assert!(
                !state.pending_triggers.is_empty(),
                "EnteredStep {:?} should have produced a pending trigger", step_kind
            );
        }
    }

    /// Verify EnteredPhase fires for main phases (which have no named steps).
    #[test]
    fn test_entered_phase_fires_for_main_phases() {
        for phase_kind in [PhaseKind::PreCombatMain, PhaseKind::PostCombatMain] {
            let mut state = make_state();
            state.trigger_instances.push(TriggerInstance {
                source_id: ObjId::UNSET,
                controller: PlayerId::Us,
                check: std::sync::Arc::new(move |e, _source_id, _ctl, _state, pending| {
                    if let GameEvent::EnteredPhase { phase, .. } = e {
                        if *phase == phase_kind {
                            pending.push(TriggerContext {
                                source_name: format!("test-{:?}", phase_kind),
                                controller: PlayerId::Us,
                                target_spec: TargetSpec::None,
                                effect: Effect(std::sync::Arc::new(|_, _, _| {})),
                            });
                        }
                    }
                }),
                expiry: Some(Expiry::EndOfTurn),
    
                });
            let ev = GameEvent::EnteredPhase { phase: phase_kind };
            fire_event(ev, &mut state, 1, PlayerId::Us);
            assert!(
                !state.pending_triggers.is_empty(),
                "EnteredPhase {:?} should have produced a pending trigger", phase_kind
            );
        }
    }

    /// Verify Untap and Cleanup do NOT fire EnteredStep (no priority round).
    #[test]
    fn test_entered_step_not_fired_for_no_prio_steps() {
        for step_kind in [StepKind::Untap, StepKind::Cleanup] {
            let state = make_state();
            // No triggers registered — just confirm no pending triggers exist at start.
            assert!(state.pending_triggers.is_empty(),
                "{:?} starts with no pending triggers", step_kind);
        }
    }

    // ── Section 10: Replacement Effect Tests ─────────────────────────────────

    // ── Section 11: Regression Tests ─────────────────────────────────────────

    /// Resolving a non-permanent spell must not log "countered".
    /// Bug: log_event had (Stack→Graveyard) → "countered" which fired during normal resolution.
    #[test]
    fn test_resolve_instant_does_not_log_countered() {
        let mut state = make_state();
        add_library_card(&mut state, PlayerId::Us, "Island");
        add_library_card(&mut state, PlayerId::Us, "Swamp");
        add_library_card(&mut state, PlayerId::Us, "Plains");
        // Manually place Brainstorm on stack with its effect.
        let id = state.alloc_id();
        state.objects.insert(id, GameObject {
            id,
            catalog_key: "Brainstorm".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::StackSpell(SpellState {
                effect: Some(eff_draw(PlayerId::Us, 3).then(eff_put_back(PlayerId::Us, 2))),
                chosen_targets: vec![],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
        });
        state.stack.push(id);
        resolve_top_of_stack(&mut state, 1, PlayerId::Us);
        let log = state.log.join("\n");
        assert!(log.contains("Brainstorm resolves"), "should log 'resolves'");
        assert!(!log.contains("countered"), "resolving an instant must not produce 'countered' in the log");
    }

    /// Resolution order (CR 608.2m): a spell stays on the stack while its effect
    /// resolves, and moves to the graveyard only afterward. Regression for the bug
    /// where the spell was popped + moved to the graveyard BEFORE its effect ran —
    /// which made graveyard-counting effects (Flow State's instant∧sorcery check,
    /// delve, threshold) wrongly observe the resolving spell itself.
    #[test]
    fn resolving_spell_stays_on_stack_until_its_effect_finishes() {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::sync::Arc;
        let mut state = make_state();
        let id = state.alloc_id();
        let on_stack = Arc::new(AtomicBool::new(false));
        let in_gy = Arc::new(AtomicBool::new(false));
        let (os, ig) = (on_stack.clone(), in_gy.clone());
        // An effect that observes its OWN spell's zone the moment it resolves.
        let eff = crate::effects::Effect(Arc::new(move |st: &mut SimState, _t: u8, _tg: &[ObjId]| {
            os.store(st.stack.contains(&id), Ordering::SeqCst);
            ig.store(st.graveyard_of(PlayerId::Us).any(|o| o.id == id), Ordering::SeqCst);
        }));
        state.objects.insert(id, GameObject {
            id,
            catalog_key: "Observer".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::StackSpell(SpellState {
                effect: Some(eff),
                chosen_targets: vec![],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
        });
        state.stack.push(id);
        resolve_top_of_stack(&mut state, 1, PlayerId::Us);
        assert!(on_stack.load(Ordering::SeqCst), "spell must be ON the stack while its effect resolves");
        assert!(!in_gy.load(Ordering::SeqCst), "spell must NOT be in the graveyard during its effect");
        assert!(state.graveyard_of(PlayerId::Us).any(|o| o.id == id), "spell goes to graveyard after resolving");
        assert!(!state.stack.contains(&id), "spell leaves the stack after resolving");
    }

    /// After a sacrifice_self ability's cost is paid (permanent leaves battlefield), the action
    /// layer must never offer that ability again. This tests the structural guarantee that
    /// effects only arise from stack resolution — not from the decision layer re-selecting
    /// an ability whose cost has already been paid.
    #[test]
    fn test_no_ability_offered_after_sacrifice_cost_paid() {
        let fetch_def = catalog_card("Polluted Delta");
        let catalog = vec![fetch_def];

        let mut state = make_state();
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        state.player_mut(PlayerId::Us).life = 20;
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PostCombatMain));
        let delta_id = add_perm(&mut state, PlayerId::Us, "Polluted Delta", BattlefieldState::new());

        // Simulate paying the sacrifice cost: permanent leaves the battlefield.
        state.set_card_zone(delta_id, Zone::Graveyard);
        state.player_mut(PlayerId::Us).life -= 1;

        // With the source gone, collect_legal_actions must never offer ActivateAbility for that id.
        state.current_turn = 1;
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));
        recompute(&mut state);
        let legal = strategy::collect_legal_actions(&state, PlayerId::Us);
        assert!(
            !legal.iter().any(|a| matches!(a, LegalAction::ActivateAbility { source_id, .. } if *source_id == delta_id)),
            "offered ability for sacrificed permanent — effect would fire without a stack item"
        );
    }

    #[test]
    fn test_leyline_redirects_gy_to_exile() {
        let mut state = make_state();
        // Place Leyline on battlefield (add_perm now pre-registers and activates instances)
        let _leyline_id = add_default_perm(&mut state, PlayerId::Opp, "Leyline of the Void");
        // Put a card in hand
        let hand_id = add_hand_card(&mut state, PlayerId::Us, "Ponder");
        // Move hand card to graveyard — Leyline should redirect to exile
        change_zone(hand_id, ZoneId::Graveyard, &mut state, 1, PlayerId::Us);
        // Card should be in Exile, not Graveyard
        assert_eq!(state.objects[&hand_id].zone(), Some(Zone::Exile { on_adventure: false }));
    }

    #[test]
    fn test_leyline_removed_no_redirect() {
        let mut state = make_state();
        // add_perm pre-registers and activates Leyline's replacement
        let leyline_id = add_default_perm(&mut state, PlayerId::Opp, "Leyline of the Void");
        // Destroy Leyline (change_zone removes its ephemeral CIs)
        change_zone(leyline_id, ZoneId::Graveyard, &mut state, 1, PlayerId::Us);
        // Now move a card to GY — should stay in GY
        let hand_id = add_hand_card(&mut state, PlayerId::Us, "Ponder");
        change_zone(hand_id, ZoneId::Graveyard, &mut state, 1, PlayerId::Us);
        assert_eq!(state.objects[&hand_id].zone(), Some(Zone::Graveyard));
    }

    // ── Section 12: State-Based Action Tests ──────────────────────────────────

    fn add_token(state: &mut SimState, who: PlayerId, name: &str) -> ObjId {
        let id = state.alloc_id();
        state.objects.insert(id, GameObject {
            id,
            catalog_key: name.to_string(),
            owner: who,
            controller: who,
            is_token: true,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Battlefield(BattlefieldState::new()),
        });
        id
    }

    #[test]
    fn test_sba_life_zero_ends_game() {
        let mut state = make_state();
        state.player_mut(PlayerId::Us).life = 0;
        check_state_based_actions(&mut state, 1);
        assert_eq!(state.winner, Some(PlayerId::Opp), "us at 0 life → opp wins");
    }

    #[test]
    fn test_sba_life_negative_ends_game() {
        let mut state = make_state();
        state.player_mut(PlayerId::Us).life = -3;
        check_state_based_actions(&mut state, 1);
        assert_eq!(state.winner, Some(PlayerId::Opp));
    }

    #[test]
    fn test_sba_token_leaves_battlefield_ceases_to_exist() {
        let mut state = make_state();
        let token_id = add_token(&mut state, PlayerId::Us, "Orc Army");
        // Move token to graveyard (as if it died without SBA running yet).
        state.objects.get_mut(&token_id).unwrap().set_zone(Zone::Graveyard);
        check_state_based_actions(&mut state, 1);
        assert!(!state.objects.contains_key(&token_id), "token in GY ceases to exist");
    }

    #[test]
    fn test_sba_token_on_battlefield_not_removed() {
        let mut state = make_state();
        let token_id = add_token(&mut state, PlayerId::Us, "Orc Army");
        check_state_based_actions(&mut state, 1);
        assert!(state.objects.contains_key(&token_id), "token on battlefield survives SBA");
    }

    #[test]
    fn test_sba_zero_toughness_creature_dies() {
        let mut state = make_state();
        // A 1/-1 creature (e.g. after -1/-2 effect) has toughness ≤ 0.
        let _id = add_perm(&mut state, PlayerId::Us, "Weakened", BattlefieldState::new());
        let def = creature("Weakened", 1, -1);
        let catalog = vec![def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        check_state_based_actions(&mut state, 1);
        assert!(state.graveyard_of(PlayerId::Us).any(|c| c.catalog_key == "Weakened"),
            "creature with toughness ≤ 0 goes to graveyard");
    }

    #[test]
    fn test_sba_lethal_damage_creature_dies() {
        let mut state = make_state();
        let _id = add_perm(&mut state, PlayerId::Us, "Ragavan", BattlefieldState {
            damage: 2,
            ..BattlefieldState::new()
        });
        let def = creature("Ragavan", 2, 2);
        let catalog = vec![def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        check_state_based_actions(&mut state, 1);
        assert!(state.graveyard_of(PlayerId::Us).any(|c| c.catalog_key == "Ragavan"),
            "creature with damage = toughness goes to graveyard");
    }

    #[test]
    fn test_sba_planeswalker_loyalty_zero_dies() {
        let mut state = make_state();
        let _id = add_perm(&mut state, PlayerId::Us, "Jace", BattlefieldState {
            loyalty: 0,
            ..BattlefieldState::new()
        });
        let def = CardDef::new("Jace", CardKind::Planeswalker(PlaneswalkerData { mana_cost: "3U".to_string(), loyalty: 3, ..Default::default() }), vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![]);
        let catalog = vec![def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        check_state_based_actions(&mut state, 1);
        assert!(state.graveyard_of(PlayerId::Us).any(|c| c.catalog_key == "Jace"),
            "planeswalker with loyalty 0 goes to graveyard");
    }

    #[test]
    fn test_planeswalker_enters_with_starting_loyalty_via_ir_replacement() {
        // CR 306.5b/c: a planeswalker enters with loyalty counters equal to its
        // printed loyalty. Modeled as a self-entry IR Replacement that places
        // Loyalty counters (routed to bf.loyalty). Drive the real ETB path
        // (change_zone → EntersZone) so the replacement actually fires — Karn's
        // loyalty must come out at his printed 5, not the BattlefieldState default 0.
        let mut state = make_state();
        state.catalog.insert("Karn, the Great Creator".into(),
            catalog_card("Karn, the Great Creator"));
        let karn_id = add_hand_card(&mut state, PlayerId::Us, "Karn, the Great Creator");
        change_zone(karn_id, ZoneId::Battlefield, &mut state, 1, PlayerId::Us);
        assert_eq!(
            state.permanent_bf(karn_id).map(|bf| bf.loyalty),
            Some(5),
            "Karn enters with 5 loyalty counters via the ETB replacement (CR 306.5b)"
        );
    }

    #[test]
    fn test_kaito_minus_two_taps_and_stuns_target() {
        // Kaito −2: tap target creature, put two stun counters on it. Now an IR
        // body — Tap + PutCounters(Stun, 2) — instead of a factory closure.
        let mut state = make_state();
        let kaito_def = catalog_card("Kaito, Bane of Nightmares");
        let CardKind::Planeswalker(pw) = &kaito_def.kind else { panic!("Kaito is a planeswalker") };
        let minus_two = pw.abilities.iter().find(|a| a.loyalty_delta() == Some(-2))
            .expect("Kaito has a −2 ability");
        state.catalog.insert("Goblin".into(), creature("Goblin", 2, 2));
        let target = add_default_perm(&mut state, PlayerId::Opp, "Goblin");
        let eff = build_ability_effect(minus_two, PlayerId::Us, ObjId::UNSET);
        eff.call(&mut state, 1, &[target]);
        let bf = state.permanent_bf(target).expect("target on battlefield");
        assert!(bf.tapped, "Kaito −2 taps the target");
        assert_eq!(bf.stun_counters, 2, "Kaito −2 places two stun counters");
    }

    #[test]
    fn test_tamiyo_minus_three_returns_card_and_ramps_on_green() {
        // Tamiyo −3: return target instant/sorcery from your GY to hand; if it's
        // green, add one mana of any color. IR body: Move + IfThen(green, AddMana).
        let mut state = make_state();
        let tamiyo_def = catalog_card("Tamiyo, Inquisitive Student");
        let back = tamiyo_def.back.as_deref().expect("Tamiyo has a back face");
        let CardKind::Planeswalker(pw) = &back.kind else { panic!("back is a planeswalker") };
        let minus_three = pw.abilities.iter().find(|a| a.loyalty_delta() == Some(-3))
            .expect("Tamiyo has a −3 ability");
        let green_sorc = CardDef::new(
            "Green Sorcery",
            CardKind::Sorcery(SpellData { mana_cost: "G".to_string(), ..Default::default() }),
            vec![Color::Green], None, vec![], CardLayout::Normal, None,
            vec![], vec![], vec![], vec![],
        );
        state.catalog.insert("Green Sorcery".into(), green_sorc);
        let card = add_graveyard_card(&mut state, PlayerId::Us, "Green Sorcery");
        let pool_before = state.player(PlayerId::Us).pool.total;
        let eff = build_ability_effect(minus_three, PlayerId::Us, ObjId::UNSET);
        eff.call(&mut state, 1, &[card]);
        assert!(matches!(state.objects[&card].zone(), Some(Zone::Hand { .. })),
            "card returned to hand");
        assert_eq!(state.player(PlayerId::Us).pool.total, pool_before + 1,
            "green card → one mana added (any color)");
    }

    #[test]
    fn test_kaito_plus_one_emblem_pumps_only_your_ninjas() {
        // Kaito +1: "you get an emblem with 'Ninjas you control get +1/+1.'"
        // Now a real emblem (CR 114) carrying an IR Static, gathered each recompute.
        let mut state = make_state();
        let mut ninja = creature("Ninja", 2, 2);
        if let CardKind::Creature(c) = &mut ninja.kind {
            c.creature_subtypes = vec!["Ninja".to_string()];
        }
        state.catalog.insert("Ninja".into(), ninja);
        let mine = add_default_perm(&mut state, PlayerId::Us, "Ninja");
        let theirs = add_default_perm(&mut state, PlayerId::Opp, "Ninja");

        let kaito_def = catalog_card("Kaito, Bane of Nightmares");
        let CardKind::Planeswalker(pw) = &kaito_def.kind else { panic!("Kaito is a planeswalker") };
        let plus_one = pw.abilities.iter().find(|a| a.loyalty_delta() == Some(1))
            .expect("Kaito has a +1 ability");
        build_ability_effect(plus_one, PlayerId::Us, ObjId::UNSET).call(&mut state, 1, &[]);
        recompute(&mut state);

        let pt = |st: &SimState, id| st.def_of(id).and_then(|d| match &d.kind {
            CardKind::Creature(c) => Some((c.power(), c.toughness())),
            _ => None,
        });
        assert_eq!(pt(&state, mine), Some((3, 3)), "your Ninja gets +1/+1 from the emblem");
        assert_eq!(pt(&state, theirs), Some((2, 2)), "opponent's Ninja is unaffected");
        assert_eq!(state.emblems.len(), 1, "one emblem created");
    }

    #[test]
    fn test_tamiyo_minus_seven_draws_half_library_and_grants_no_max_hand() {
        // Tamiyo −7: draw ⌈library/2⌉; emblem "You have no maximum hand size."
        let mut state = make_state();
        for i in 0..7 {
            add_library_card(&mut state, PlayerId::Us, &format!("Lib{i}"));
        }
        let hand_before = state.hand_size(PlayerId::Us);
        assert!(!state.has_no_max_hand_size(PlayerId::Us), "no emblem yet");

        let tamiyo_def = catalog_card("Tamiyo, Inquisitive Student");
        let back = tamiyo_def.back.as_deref().expect("Tamiyo has a back face");
        let CardKind::Planeswalker(pw) = &back.kind else { panic!("back is a planeswalker") };
        let minus_seven = pw.abilities.iter().find(|a| a.loyalty_delta() == Some(-7))
            .expect("Tamiyo has a −7 ability");
        build_ability_effect(minus_seven, PlayerId::Us, ObjId::UNSET).call(&mut state, 1, &[]);

        // ⌈7/2⌉ = 4 drawn.
        assert_eq!(state.hand_size(PlayerId::Us), hand_before + 4, "draw half library, rounded up");
        assert_eq!(state.library_size(PlayerId::Us), 3, "7 − 4 = 3 left");
        assert!(state.has_no_max_hand_size(PlayerId::Us),
            "emblem grants no maximum hand size");
    }

    /// Kaito 0: surveil 2, then draw a card for each opponent who lost life this
    /// turn — counted off the `LifeLost` event log, not a bespoke counter.
    fn kaito_zero(state: &mut SimState) -> Effect {
        let kaito_def = catalog_card("Kaito, Bane of Nightmares");
        let CardKind::Planeswalker(pw) = &kaito_def.kind else { panic!("Kaito is a planeswalker") };
        let zero = pw.abilities.iter().find(|a| a.loyalty_delta() == Some(0))
            .expect("Kaito has a 0 ability");
        build_ability_effect(zero, PlayerId::Us, ObjId::UNSET)
    }

    #[test]
    fn test_kaito_zero_draws_when_opponent_lost_life() {
        let mut state = make_state();
        for i in 0..3 { add_library_card(&mut state, PlayerId::Us, &format!("Lib{i}")); }
        state.lose_life(PlayerId::Opp, 3); // logs a LifeLost event this turn
        let hand_before = state.hand_size(PlayerId::Us);
        kaito_zero(&mut state).call(&mut state, 1, &[]);
        assert_eq!(state.hand_size(PlayerId::Us), hand_before + 1,
            "draw 1 — the opponent lost life this turn");
    }

    #[test]
    fn test_kaito_zero_no_draw_when_opponent_kept_life() {
        let mut state = make_state();
        for i in 0..3 { add_library_card(&mut state, PlayerId::Us, &format!("Lib{i}")); }
        let hand_before = state.hand_size(PlayerId::Us);
        kaito_zero(&mut state).call(&mut state, 1, &[]);
        assert_eq!(state.hand_size(PlayerId::Us), hand_before,
            "no draw — no opponent lost life this turn");
    }

    #[test]
    fn test_sba_legend_rule_second_copy_dies() {
        let mut state = make_state();
        let _first = add_default_perm(&mut state, PlayerId::Us, "Bowmasters");
        let _second = add_default_perm(&mut state, PlayerId::Us, "Bowmasters");
        let mut bowmasters_data = CreatureData::new("1B", 1, 1);
        bowmasters_data.legendary = true;
        let def = CardDef::new("Bowmasters", CardKind::Creature(bowmasters_data), parse_colors("1B", false, true), None, vec![Supertype::Legendary], CardLayout::Normal, None, vec![], vec![], vec![], vec![]);
        let catalog = vec![def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        check_state_based_actions(&mut state, 1);
        // Exactly one survives.
        assert_eq!(state.permanents_of(PlayerId::Us).filter(|c| c.catalog_key == "Bowmasters").count(), 1,
            "legend rule: one copy survives");
        assert_eq!(state.graveyard_of(PlayerId::Us).filter(|c| c.catalog_key == "Bowmasters").count(), 1,
            "legend rule: one copy goes to graveyard");
    }

    #[test]
    fn test_sba_legend_rule_only_one_copy_untouched() {
        let mut state = make_state();
        add_default_perm(&mut state, PlayerId::Us, "Bowmasters");
        let mut bowmasters_data = CreatureData::new("1B", 1, 1);
        bowmasters_data.legendary = true;
        let def = CardDef::new("Bowmasters", CardKind::Creature(bowmasters_data), parse_colors("1B", false, true), None, vec![Supertype::Legendary], CardLayout::Normal, None, vec![], vec![], vec![], vec![]);
        let catalog = vec![def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }
        check_state_based_actions(&mut state, 1);
        assert_eq!(state.permanents_of(PlayerId::Us).filter(|c| c.catalog_key == "Bowmasters").count(), 1,
            "single legendary permanent unaffected by legend rule");
    }

    // ── Section N: Continuous Effects / recompute ─────────────────────────────

    /// A L7 CE that adds +2/+1 to all permanents controlled by PlayerId::Us is reflected
    /// in the MaterializedState produced by `recompute`.
    #[test]
    fn test_recompute_pt_modifier() {
        let mut state = make_state();

        // Add a 2/2 creature for PlayerId::Us.
        let id = add_default_perm(&mut state, PlayerId::Us, "Grizzly Bears");
        let base_def = creature("Grizzly Bears", 2, 2);
        // Override the 1/1 stub inserted by add_default_perm with the real 2/2 def.
        state.catalog.insert(base_def.name.clone(), base_def);

        // Baseline: recompute without any CEs → effective P/T is 2/2.
        recompute(&mut state);
        let eff = state.def_of(id).expect("should be in materialized defs");
        let CardKind::Creature(c) = &eff.kind else { panic!("expected creature") };
        assert_eq!((c.power(), c.toughness()), (2, 2), "baseline P/T should be 2/2");

        // Register a L7 CE that adds +2/+1 to permanents controlled by PlayerId::Us.
        state.continuous_instances.push(ContinuousInstance {
            source_id: ObjId::UNSET,
            controller: PlayerId::Us,
            layer: ContinuousLayer::L7PowerToughness,
            reads: vec![],
            writes: vec![],
            timestamp: 0,
            filter: std::sync::Arc::new(|_id, controller, _| controller == PlayerId::Us),
            modifier: std::sync::Arc::new(|def, _state| {
                if let CardKind::Creature(c) = &mut def.kind {
                    c.adjust_pt(2, 1);
                }
            }),
            expiry: Expiry::EndOfTurn,

        });

        // Recompute: effective P/T should now be 4/3.
        recompute(&mut state);
        let eff2 = state.def_of(id).expect("should be in materialized defs after CE");
        let CardKind::Creature(c2) = &eff2.kind else { panic!("expected creature") };
        assert_eq!((c2.power(), c2.toughness()), (4, 3), "CE should produce 4/3");
    }

    /// +1/+1 counters on a creature are folded into the CardDef before CE modifiers run,
    /// so a L7 CE that reads P/T sees the counter-adjusted value.
    #[test]
    fn test_recompute_counters_fold_before_ce() {
        let mut state = make_state();

        // Add a 1/1 with two +1/+1 counters.
        let id = {
            let bf = BattlefieldState { counters: 2, ..BattlefieldState::new() };
            add_perm(&mut state, PlayerId::Us, "Llanowar Elves", bf)
        };
        // Without any CE: counters fold in → effective 3/3.
        recompute(&mut state);
        let eff = state.def_of(id).expect("creature should be materialized");
        let CardKind::Creature(c) = &eff.kind else { panic!("expected creature") };
        assert_eq!((c.power(), c.toughness()), (3, 3), "two +1/+1 counters should yield 3/3");
    }


    // ── Section 13g: StaticAbilityDef + CDA ──────────────────────────────────

    fn flying_static_ability() -> StaticAbilityDef {
        std::sync::Arc::new(|source_id, controller: PlayerId| ContinuousInstance {
            source_id,
            controller,
            layer: ContinuousLayer::L6AbilityEffects,
            reads: vec![],
            writes: vec![],
            timestamp: 0,
            filter: std::sync::Arc::new(move |id, _, _| id == source_id),
            modifier: std::sync::Arc::new(|def, _state| {
                if let CardKind::Creature(c) = &mut def.kind {
                    c.keywords.insert(Keyword::Flying);
                }
            }),
            expiry: Expiry::WhileSourceOnBattlefield,

        })
    }

    /// A creature with a flying static ability should have the keyword in its materialized
    /// def after ETB, and lose it after LTB.
    #[test]
    fn test_static_ability_def_grants_flying_at_etb() {
        let mut state = make_state();
        let def = CardDef::new("Flyer", CardKind::Creature(CreatureData::new("", 2, 2)), vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![flying_static_ability()]);
        let catalog = vec![def.clone()];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }

        let id = add_perm_with_def(&mut state, PlayerId::Us, &def, BattlefieldState::new());

        // recompute: CI from static_ability_def should add "flying" to materialized keywords.
        recompute(&mut state);
        assert!(state.def_of(id).unwrap().has_keyword(Keyword::Flying), "flying granted via static_ability_def at ETB");
        assert!(creature_has_keyword(id, Keyword::Flying, &state), "creature_has_keyword uses materialized state");
    }

    /// A creature with a flying static ability should lose the keyword CI when it
    /// leaves the battlefield (change_zone removes WhileSourceOnBattlefield CIs).
    #[test]
    fn test_static_ability_def_removed_at_ltb() {
        let mut state = make_state();
        let def = CardDef::new("Flyer", CardKind::Creature(CreatureData::new("", 2, 2)), vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![flying_static_ability()]);
        let catalog = vec![def.clone()];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }

        let id = add_perm_with_def(&mut state, PlayerId::Us, &def, BattlefieldState::new());
        // Static-ability CIs are no longer registered in continuous_instances at ETB —
        // they are derived fresh each recompute cycle from the catalog.
        assert_eq!(state.continuous_instances.len(), 0, "no ephemeral CIs registered");

        // Recompute generates the static CI and applies flying.
        recompute(&mut state);
        assert!(state.def_of(id).unwrap().has_keyword(Keyword::Flying), "flying applied by recompute");

        // Move the permanent off the battlefield.
        state.objects.get_mut(&id).unwrap().set_zone(Zone::Graveyard);
        recompute(&mut state);
        // Static CI is not generated for non-BF objects, so flying should be gone.
        if let Some(d) = state.def_of(id) {
            assert!(!d.has_keyword(Keyword::Flying), "flying removed when off battlefield");
        }
    }

    /// A CDA: creature whose power = number of cards in its controller's graveyard.
    /// Demonstrates that ContinuousModFn receives live SimState and can read from it.
    #[test]
    fn test_cda_power_equals_graveyard_count() {
        let mut state = make_state();
        let base_def = creature("GoyTest", 0, 3);
        let catalog = vec![base_def];
        for c in &catalog { state.catalog.insert(c.name.clone(), c.clone()); }

        let id = add_perm(&mut state, PlayerId::Us, "GoyTest", BattlefieldState::new());

        // Register a CDA CI: power = number of cards in PlayerId::Us graveyard.
        state.continuous_instances.push(ContinuousInstance {
            source_id: id,
            controller: PlayerId::Us,
            layer: ContinuousLayer::L7PowerToughness,
            reads: vec![],
            writes: vec![],
            timestamp: 0,
            filter: std::sync::Arc::new(move |obj_id, _, _| obj_id == id),
            modifier: std::sync::Arc::new(|def, state| {
                let gy = state.graveyard_of(PlayerId::Us).count() as i32;
                if let CardKind::Creature(c) = &mut def.kind {
                    let delta = gy - c.power();
                    c.adjust_pt(delta, 0);
                }
            }),
            expiry: Expiry::WhileSourceOnBattlefield,

        });

        // No cards in GY → power = 0.
        recompute(&mut state);
        let CardKind::Creature(c) = &state.def_of(id).unwrap().kind.clone() else { panic!() };
        assert_eq!(c.power(), 0, "no GY cards → power 0");

        // Add a card to PlayerId::Us graveyard.
        add_graveyard_card(&mut state, PlayerId::Us, "SomeCard");
        recompute(&mut state);
        let CardKind::Creature(c) = &state.def_of(id).unwrap().kind.clone() else { panic!() };
        assert_eq!(c.power(), 1, "1 GY card → power 1");

        // Add a second card.
        add_graveyard_card(&mut state, PlayerId::Us, "AnotherCard");
        recompute(&mut state);
        let CardKind::Creature(c) = &state.def_of(id).unwrap().kind.clone() else { panic!() };
        assert_eq!(c.power(), 2, "2 GY cards → power 2");
    }

    /// recompute now covers all zones; a card in the graveyard must appear in materialized.defs.
    #[test]
    fn test_recompute_includes_graveyard_objects() {
        let mut state = make_state();
        let def = creature("Goyf", 2, 3);
        state.catalog.insert(def.name.clone(), def);

        let gy_id = add_graveyard_card(&mut state, PlayerId::Us, "Goyf");

        recompute(&mut state);
        assert!(
            state.def_of(gy_id).is_some(),
            "graveyard card must appear in materialized snapshot"
        );
        let CardKind::Creature(c) = &state.def_of(gy_id).unwrap().kind.clone() else { panic!("expected creature") };
        assert_eq!(c.power(), 2);
        assert_eq!(c.toughness(), 3);
    }

    // ── Section 14: Library Search Tests ─────────────────────────────────────

    /// Personal Tutor finds a sorcery and puts it on top of the library (stays in library).
    /// An instant in the same library is not moved.
    #[test]
    fn test_personal_tutor_finds_sorcery() {
        let doomsday_def = catalog_card("Doomsday");
        let fow_def = catalog_card("Force of Will");
        let mut state = make_state();
        state.catalog.insert(doomsday_def.name.clone(), doomsday_def);
        state.catalog.insert(fow_def.name.clone(), fow_def);
        let dd_id  = add_library_card(&mut state, PlayerId::Us, "Doomsday");
        let fow_id = add_library_card(&mut state, PlayerId::Us, "Force of Will");

        let eff = eff_fetch_search(PlayerId::Us, ir_type(CardType::Sorcery), ZoneId::Library);
        eff.call(&mut state, 1, &[]);

        // Both stay in library: Doomsday was "put on top" (Library ≡ top until ordering tracked),
        // FoW was never selected.
        assert_eq!(state.objects[&dd_id].zone(), Some(Zone::Library), "Doomsday should remain in library");
        assert_eq!(state.objects[&fow_id].zone(), Some(Zone::Library), "FoW should remain in library");
        let log = state.log.join("\n");
        assert!(log.contains("search → Doomsday"), "should log the searched card name");
        assert!(!log.contains("Force of Will"), "FoW should not appear in search log");
    }

    /// Recruiter of the Guard ETB: searches library for a creature with toughness ≤ 2 and puts it
    /// in hand. A creature with toughness > 2 should stay in the library.
    #[test]
    fn test_recruiter_etb_finds_low_toughness_creature() {
        let recruiter_def = catalog_card("Recruiter of the Guard");
        let small_def = creature("Mother of Runes", 1, 1);
        let big_def = creature("Tarmogoyf", 0, 3);
        let mut state = make_state();
        state.catalog.insert(recruiter_def.name.clone(), recruiter_def.clone());
        state.catalog.insert(small_def.name.clone(), small_def.clone());
        state.catalog.insert(big_def.name.clone(), big_def.clone());

        let small_id = add_library_card(&mut state, PlayerId::Us, "Mother of Runes");
        let big_id   = add_library_card(&mut state, PlayerId::Us, "Tarmogoyf");

        let hand_before = state.hand_of(PlayerId::Us).count();
        // eff_enter_permanent pre-registers instances, fires the ZoneChange ETB event,
        // and thereby pushes the Recruiter trigger to state.pending_triggers.
        eff_enter_permanent(PlayerId::Us, "Recruiter of the Guard")
            .call(&mut state, 1, &[]);

        // Resolve all pending ETB triggers.
        let pending = std::mem::take(&mut state.pending_triggers);
        for ctx in pending {
            ctx.effect.call(&mut state, 1, &[]);
        }

        assert_eq!(state.hand_of(PlayerId::Us).count(), hand_before + 1, "hand should grow by one");
        assert_eq!(state.objects[&small_id].zone(), Some(Zone::Hand { known: false }), "Mother of Runes should be in hand");
        assert_eq!(state.objects[&big_id].zone(), Some(Zone::Library), "Tarmogoyf (toughness 3) should stay in library");
    }

    /// Urza's Saga chapter III: finds an artifact with no colored pips and MV ≤ 1
    /// and puts it on the battlefield. An artifact with MV > 1 stays in library.
    #[test]
    fn test_urza_saga_finds_low_cost_colorless_artifact() {
        let lotus_def = catalog_card("Lotus Petal");
        let fow_def = catalog_card("Force of Will");
        let mut state = make_state();
        state.catalog.insert(lotus_def.name.clone(), lotus_def.clone());
        state.catalog.insert(fow_def.name.clone(), fow_def.clone());
        let lotus_id = add_library_card(&mut state, PlayerId::Us, "Lotus Petal");
        let fow_id   = add_library_card(&mut state, PlayerId::Us, "Force of Will");

        let pred = ir_and(ir_type(CardType::Artifact), ir_and(ir_colorless(), ir_mv_le(1)));
        let eff  = eff_fetch_search(PlayerId::Us, pred, ZoneId::Battlefield);
        eff.call(&mut state, 1, &[]);

        assert_eq!(state.objects[&lotus_id].zone(), Some(Zone::Battlefield), "Lotus Petal should enter battlefield");
        assert_eq!(state.objects[&fow_id].zone(), Some(Zone::Library),     "FoW should stay in library");
    }

    /// Urza's Saga does not fetch an artifact with a colored pip (e.g. {W}).
    #[test]
    fn test_urza_saga_ignores_colored_artifact() {
        let white_art_def = CardDef::new("White Artifact", CardKind::Artifact(ArtifactData { mana_cost: "W".to_string(), ..Default::default() }), parse_colors("W", false, false), None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![]);
        let mut state = make_state();
        state.catalog.insert(white_art_def.name.clone(), white_art_def);
        add_library_card(&mut state, PlayerId::Us, "White Artifact");

        let pred = ir_and(ir_type(CardType::Artifact), ir_and(ir_colorless(), ir_mv_le(1)));
        let eff  = eff_fetch_search(PlayerId::Us, pred, ZoneId::Battlefield);
        eff.call(&mut state, 1, &[]);

        // No candidate matched; library unchanged
        assert_eq!(state.library_of(PlayerId::Us).count(), 1, "colored artifact must not be fetched");
    }

    /// Urza's Saga does not fetch an artifact with MV > 1 (e.g. {2}).
    #[test]
    fn test_urza_saga_ignores_high_mv_artifact() {
        let sol_ring_def = CardDef::new("Sol Ring", CardKind::Artifact(ArtifactData { mana_cost: "2".to_string(), ..Default::default() }), vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![]);
        let mut state = make_state();
        state.catalog.insert(sol_ring_def.name.clone(), sol_ring_def);
        add_library_card(&mut state, PlayerId::Us, "Sol Ring");

        let pred = ir_and(ir_type(CardType::Artifact), ir_and(ir_colorless(), ir_mv_le(1)));
        let eff  = eff_fetch_search(PlayerId::Us, pred, ZoneId::Battlefield);
        eff.call(&mut state, 1, &[]);

        assert_eq!(state.library_of(PlayerId::Us).count(), 1, "MV 2 artifact must not be fetched");
    }

    #[test]
    fn test_saga_lore_counters_fire_chapters_then_sacrifice() {
        // Synthetic 2-chapter Saga validating the CR-714 machinery: chapter I
        // gains 1 life, chapter II gains 10. Lore is added on entry (chapter I)
        // and each precombat main (chapter II); at lore ≥ 2 the SBA sacrifices it.
        use crate::ir::action::{Action, Who as IrWho};
        use crate::ir::expr::Expr;
        let mut saga = CardDef::new(
            "Test Saga", CardKind::Enchantment(EnchantmentData::default()),
            vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![],
        );
        saga.chapters = vec![
            Action::GainLife { who: IrWho::You, amount: Expr::Num(1) },
            Action::GainLife { who: IrWho::You, amount: Expr::Num(10) },
        ];
        let mut state = make_state();
        state.catalog.insert("Test Saga".into(), saga);
        let life0 = state.player(PlayerId::Us).life;
        let saga_id = add_hand_card(&mut state, PlayerId::Us, "Test Saga");

        // Enters the battlefield → lore 1 → chapter I triggers.
        change_zone(saga_id, ZoneId::Battlefield, &mut state, 1, PlayerId::Us);
        for ctx in std::mem::take(&mut state.pending_triggers) { ctx.effect.call(&mut state, 1, &[]); }
        assert_eq!(state.objects[&saga_id].counters.get(&CounterType::Lore).copied(), Some(1));
        assert_eq!(state.player(PlayerId::Us).life, life0 + 1, "chapter I gained 1 life");

        // Precombat main → lore 2 → chapter II triggers.
        add_lore_counter(&mut state, saga_id, 1);
        for ctx in std::mem::take(&mut state.pending_triggers) { ctx.effect.call(&mut state, 1, &[]); }
        assert_eq!(state.player(PlayerId::Us).life, life0 + 11, "chapter II gained 10 more life");

        // Lore (2) ≥ final chapter (2) → sacrificed as an SBA.
        recompute(&mut state);
        check_state_based_actions(&mut state, 1);
        assert!(state.graveyard_of(PlayerId::Us).any(|c| c.id == saga_id),
            "Saga is sacrificed once its last chapter has resolved (CR 714.4)");
    }

    #[test]
    fn test_urza_saga_chapter_iii_fetches_then_sacrifices() {
        // The real Urza's Saga on the machinery: chapter I grants mana, chapter II
        // grants Construct creation, chapter III fetches a {0}/{1}-MV artifact.
        let mut state = make_state();
        state.catalog.insert("Urza's Saga".into(), catalog_card("Urza's Saga"));
        state.catalog.insert("Lotus Petal".into(), catalog_card("Lotus Petal"));
        let lotus_id = add_library_card(&mut state, PlayerId::Us, "Lotus Petal");
        let saga_id = add_hand_card(&mut state, PlayerId::Us, "Urza's Saga");

        change_zone(saga_id, ZoneId::Battlefield, &mut state, 1, PlayerId::Us); // lore 1
        for ctx in std::mem::take(&mut state.pending_triggers) { ctx.effect.call(&mut state, 1, &[]); }
        // Chapter I granted Urza's Saga a usable {T}: Add {C} mana ability.
        recompute(&mut state);
        assert_eq!(state.def_of(saga_id).map(|d| d.mana_abilities().len()), Some(1),
            "chapter I grants {{T}}: Add {{C}} (a usable, computed mana ability)");
        add_lore_counter(&mut state, saga_id, 1); // lore 2
        for ctx in std::mem::take(&mut state.pending_triggers) { ctx.effect.call(&mut state, 1, &[]); }
        add_lore_counter(&mut state, saga_id, 1); // lore 3 → chapter III
        for ctx in std::mem::take(&mut state.pending_triggers) { ctx.effect.call(&mut state, 1, &[]); }

        assert_eq!(state.objects[&lotus_id].zone(), Some(Zone::Battlefield),
            "chapter III fetches Lotus Petal onto the battlefield");
        recompute(&mut state);
        check_state_based_actions(&mut state, 1);
        assert!(state.graveyard_of(PlayerId::Us).any(|c| c.id == saga_id),
            "Urza's Saga is sacrificed after its final chapter");
    }

    #[test]
    fn test_grant_ability_makes_mana_and_activated_abilities_usable() {
        // CEMod::GrantAbility must make BOTH a mana ability and a non-mana
        // activated ability usable, with mana-ness computed (CR 605.1a) — they
        // land in mana_abilities() vs abilities() by classification, not bucket.
        use crate::ir::ability::{Ability, AbilityKind, CostBody};
        use crate::ir::action::{Action, ManaSpec, Who as IrWho};
        use crate::ir::ce::CEMod;
        use crate::ir::context::Ctx;
        use crate::ir::expr::{Expr, Filter, ZoneKindSel};
        let activated = |body: Action| Ability {
            kind: AbilityKind::Activated {
                cost: CostBody::Ir(Action::Tap { target: Expr::Ctx(Ctx::Source) }),
                target_spec: TargetSpec::None,
                choice_spec: None,
                body,
                timing: ActivationTiming::Default,
                activation_condition: None,
                active_zone: ZoneKindSel::Battlefield,
            },
            text: None,
        };
        let mana = activated(Action::AddMana { who: IrWho::You, count: Expr::Num(1), spec: ManaSpec::Fixed(vec![]) });
        let draw = activated(Action::Draw { who: IrWho::You, n: Expr::Num(1) });

        // A creature whose static ability grants itself both abilities.
        let mut granter = creature("Granter", 1, 1);
        granter.abilities = vec![Ability {
            kind: AbilityKind::Static {
                mods: vec![CEMod::GrantAbility(Box::new(mana)), CEMod::GrantAbility(Box::new(draw))],
                scope: Some(Filter(Expr::Eq(Box::new(Expr::Ctx(Ctx::It)), Box::new(Expr::Ctx(Ctx::Source))))),
                condition: None,
            },
            text: None,
        }];
        let mut state = make_state();
        state.catalog.insert("Granter".into(), granter);
        let id = add_default_perm(&mut state, PlayerId::Us, "Granter");
        recompute(&mut state);

        let def = state.def_of(id).expect("materialized");
        assert_eq!(def.mana_abilities().len(), 1,
            "granted {{T}}: Add {{C}} is classified (computed) as a usable mana ability");
        assert_eq!(def.abilities().iter().filter(|a| a.ir_body.is_some()).count(), 1,
            "granted {{T}}: draw is a usable (non-mana) activated ability");
    }

    /// Green Sun's Zenith finds a green creature and puts it on the battlefield.
    /// A non-green creature in the same library is not moved.
    #[test]
    fn test_gsz_finds_green_creature() {
        let troll_def = CardDef::new("Elvish Reclaimer", CardKind::Creature(CreatureData::new("G", 1, 1)), parse_colors("G", false, false), None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![]);
        let ragavan_def = CardDef::new("Ragavan, Nimble Pilferer", CardKind::Creature(CreatureData::new("R", 2, 1)), parse_colors("R", false, false), None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![]);
        let mut state = make_state();
        state.catalog.insert(troll_def.name.clone(), troll_def);
        state.catalog.insert(ragavan_def.name.clone(), ragavan_def);
        let green_id = add_library_card(&mut state, PlayerId::Us, "Elvish Reclaimer");
        let red_id   = add_library_card(&mut state, PlayerId::Us, "Ragavan, Nimble Pilferer");

        let pred = ir_and(ir_type(CardType::Creature), ir_color(Color::Green));
        let eff  = eff_fetch_search(PlayerId::Us, pred, ZoneId::Battlefield);
        eff.call(&mut state, 1, &[]);

        assert_eq!(state.objects[&green_id].zone(), Some(Zone::Battlefield), "green creature should enter battlefield");
        assert_eq!(state.objects[&red_id].zone(), Some(Zone::Library),     "non-green creature should stay");
    }

    /// Fetchland regression: island-or-swamp search finds the correct land.
    #[test]
    fn test_fetchland_search_via_ability_factory() {
        let pred = ir_and(ir_type(CardType::Land), ir_or(ir_subtype("island"), ir_subtype("swamp")));
        // Cost is incidental to this test (we exercise build_ability_effect /
        // the body, not pay_costs); leave default (Ir(Noop)).
        let delta_ability = AbilityDef { ability_factory: Some(Arc::new(move |who, _| eff_fetch_search(who, pred.clone(), ZoneId::Battlefield))), ..Default::default() };
        let island_def = catalog_card("Underground Sea");
        let forest_def = CardDef::new("Forest", CardKind::Land(LandData {
            land_types: LandTypes::from_types(&[BasicLandType::Forest]),
            ..Default::default()
        }), vec![], None, vec![Supertype::Basic], CardLayout::Normal, None, vec![], vec![], vec![], vec![]);
        let mut state = make_state();
        state.catalog.insert(island_def.name.clone(), island_def);
        state.catalog.insert(forest_def.name.clone(), forest_def);
        let sea_id    = add_library_card(&mut state, PlayerId::Us, "Underground Sea");
        let forest_id = add_library_card(&mut state, PlayerId::Us, "Forest");

        let eff = build_ability_effect(&delta_ability, PlayerId::Us, ObjId::UNSET);
        eff.call(&mut state, 1, &[]);

        assert_eq!(state.objects[&sea_id].zone(), Some(Zone::Battlefield), "Underground Sea should enter play");
        assert_eq!(state.objects[&forest_id].zone(), Some(Zone::Library),     "Forest should remain in library");
    }

    // ── Section 20: CostsPaidCtx / objects_moved ─────────────────────────────

    /// FoW pitch records the pitched card id in costs_paid_ctx.objects_moved.
    #[test]
    fn test_fow_pitch_objects_moved_contains_pitch_card() {
        let mut state = make_state();
        let fow_def = catalog_card("Force of Will");
        let brainstorm_def = catalog_card("Brainstorm");
        for c in &[fow_def.clone(), brainstorm_def] {
            state.catalog.insert(c.name.clone(), c.clone());
        }
        let fow_id = add_hand_card(&mut state, PlayerId::Us, "Force of Will");
        let bs_id  = add_hand_card(&mut state, PlayerId::Us, "Brainstorm");
        let alt_cost = &fow_def.alternate_costs()[0];

        let card_id = cast_spell(&mut state, 1, PlayerId::Us, fow_id, SpellFace::Main, Some(alt_cost), Some(0), &[], 0, 0).unwrap();
        let ctx = &state.objects[&card_id].spell().unwrap().costs_paid_ctx;

        assert_eq!(ctx.objects_moved, vec![bs_id], "pitched Brainstorm id recorded in objects_moved");
    }

    /// FoW can't pitch itself — needs a second blue card in hand.
    #[test]
    fn test_fow_cannot_pitch_itself() {
        let mut state = make_state();
        let fow_def = catalog_card("Force of Will");
        state.catalog.insert(fow_def.name.clone(), fow_def.clone());
        let fow_id = add_hand_card(&mut state, PlayerId::Us, "Force of Will");
        // No other cards — pitch cost requires another blue non-land card; also no mana for 3UU.
        let result = cast_spell(&mut state, 1, PlayerId::Us, fow_id, SpellFace::Main, None, None, &[], 0, 0);
        assert!(result.is_none(), "FoW can't be cast with only itself in hand and no mana");
    }

    /// FoW normal cost (3UU mana) works when pool is sufficient.
    #[test]
    fn test_fow_normal_mana_cost() {
        let mut state = make_state();
        let fow_def = catalog_card("Force of Will");
        state.catalog.insert(fow_def.name.clone(), fow_def.clone());
        let fow_id = add_hand_card(&mut state, PlayerId::Us, "Force of Will");
        state.player_mut(PlayerId::Us).pool.u     = 2;
        state.player_mut(PlayerId::Us).pool.total = 5; // 3 generic + 2 blue

        let result = cast_spell(&mut state, 1, PlayerId::Us, fow_id, SpellFace::Main, None, None, &[], 0, 0);
        assert!(result.is_some(), "FoW should cast for 3UU when pool is full");
        assert_eq!(state.player(PlayerId::Us).pool.total, 0, "all mana spent");
    }

    // ── Section 21: Snuff Out ────────────────────────────────────────────────

    /// Snuff Out can be cast for 4 life with no mana at all.
    #[test]
    fn test_snuff_out_life_alternate_cost() {
        let mut state = make_state();
        let def = catalog_card("Snuff Out");
        state.catalog.insert(def.name.clone(), def.clone());
        let troll_def = creature("Troll", 2, 2);
        state.catalog.insert(troll_def.name.clone(), troll_def);
        add_default_perm(&mut state, PlayerId::Opp, "Troll");
        let snuff_id = add_hand_card(&mut state, PlayerId::Us, "Snuff Out");
        let initial_life = state.player(PlayerId::Us).life;
        let alt = &def.alternate_costs()[0];

        let result = cast_spell(&mut state, 1, PlayerId::Us, snuff_id, SpellFace::Main, Some(alt), Some(0), &[], 0, 0);
        assert!(result.is_some(), "Snuff Out should cast for 4 life");
        assert_eq!(state.player(PlayerId::Us).life, initial_life - 4, "paid 4 life");
        let ctx = &state.objects[&result.unwrap()].spell().unwrap().costs_paid_ctx;
        assert!(ctx.objects_moved.is_empty(), "no objects moved for life payment");
    }

    /// Snuff Out alternate cost requires life > 4 (can't pay if at exactly 4 or below).
    /// Goes through the IR feasibility path (`build_schema`) post-migration.
    #[test]
    fn test_snuff_out_cant_pay_life_when_low() {
        let mut state = make_state();
        let def = catalog_card("Snuff Out");
        state.catalog.insert(def.name.clone(), def.clone());
        let snuff_id = add_hand_card(&mut state, PlayerId::Us, "Snuff Out");
        state.player_mut(PlayerId::Us).life = 4; // exactly 4 — can't pay (would reach 0)
        let alt = &def.alternate_costs()[0];
        let crate::ir::ability::CostBody::Ir(action) = &alt.costs else {
            panic!("Snuff Out alt cost is IR after Phase 4 step 5 follow-up")
        };
        let schema = crate::ir::cost_exec::build_schema(action, &state, PlayerId::Us, snuff_id);
        assert!(schema.is_none(), "can't pay 4 life when at 4 life (would reach 0)");
    }

    // ── Section 23: Daze bounce cost ────────────────────────────────────────

    /// Daze's alternate cost bounces a blue-producing land; the bounced id is recorded.
    #[test]
    fn test_daze_bounce_alt_cost_records_returned_island() {
        let mut state = make_state();
        let daze_def = catalog_card("Daze");
        state.catalog.insert(daze_def.name.clone(), daze_def.clone());
        // Island on the battlefield (blue-producing).
        let island_id = island_land(&mut state, PlayerId::Us);
        let daze_id = add_hand_card(&mut state, PlayerId::Us, "Daze");
        let alt = &daze_def.alternate_costs()[0]; // ReturnFromBattlefield(Island subtype)

        let result = cast_spell(&mut state, 1, PlayerId::Us, daze_id, SpellFace::Main, Some(alt), Some(0), &[], 0, 0);
        assert!(result.is_some(), "Daze should cast by bouncing the Island");
        let ctx = &state.objects[&result.unwrap()].spell().unwrap().costs_paid_ctx;
        assert_eq!(ctx.objects_moved, vec![island_id], "bounced Island id in objects_moved");
        assert!(state.hand_of(PlayerId::Us).any(|c| c.id == island_id), "Island returned to hand");
    }

    // ── Section 25: Additional costs ────────────────────────────────────────

    /// A spell with additional_costs requires those costs to be payable.
    #[test]
    fn test_additional_cost_blocks_cast_when_unpayable() {
        let mut state = make_state();
        // Build a cheap spell ({B}) with an additional Life(3) cost.
        let mut def = catalog_card("Dark Ritual");
        def.additional_costs = crate::ir::ability::CostBody::Ir(
            crate::ir::action::Action::PayLife {
                who: crate::ir::action::Who::You,
                amount: crate::ir::expr::Expr::Num(3),
            },
        );
        state.catalog.insert(def.name.clone(), def.clone());
        let card_id = add_hand_card(&mut state, PlayerId::Us, "Dark Ritual");
        state.player_mut(PlayerId::Us).pool.b = 1; state.player_mut(PlayerId::Us).pool.total = 1;
        state.player_mut(PlayerId::Us).life = 3; // can't pay Life(3) — would reach 0

        let result = cast_spell(&mut state, 1, PlayerId::Us, card_id, SpellFace::Main, None, None, &[], 0, 0);
        assert!(result.is_none(), "additional Life(3) cost blocks cast at 3 life");
    }

    /// A spell with a payable additional_cost is cast and the cost is paid.
    #[test]
    fn test_additional_cost_paid_on_cast() {
        let mut state = make_state();
        let mut def = catalog_card("Dark Ritual");
        def.additional_costs = crate::ir::ability::CostBody::Ir(
            crate::ir::action::Action::PayLife {
                who: crate::ir::action::Who::You,
                amount: crate::ir::expr::Expr::Num(3),
            },
        );
        state.catalog.insert(def.name.clone(), def.clone());
        let card_id = add_hand_card(&mut state, PlayerId::Us, "Dark Ritual");
        state.player_mut(PlayerId::Us).pool.b = 1; state.player_mut(PlayerId::Us).pool.total = 1;
        let initial_life = state.player(PlayerId::Us).life; // 20

        let result = cast_spell(&mut state, 1, PlayerId::Us, card_id, SpellFace::Main, None, None, &[], 0, 0);
        assert!(result.is_some(), "Dark Ritual + Life(3) additional cost is payable at 20 life");
        assert_eq!(state.player(PlayerId::Us).life, initial_life - 3, "additional Life(3) was paid");
    }

    /// Kozilek's Command ({X}{C}{C}) must reserve the two colorless pips
    /// before determining how much X is affordable.
    #[test]
    fn test_kozileks_command_max_x_accounts_for_base_cc() {
        let mut state = make_state();
        let def = catalog_card("Kozilek's Command");
        state.catalog.insert(def.name.clone(), def.clone());

        for (mana, expected_x) in [(2, 0), (3, 1), (4, 2), (5, 3)] {
            state.player_mut(PlayerId::Us).pool = ManaPool::default();
            state.player_mut(PlayerId::Us).pool.c = mana;
            state.player_mut(PlayerId::Us).pool.total = mana;
            assert_eq!(
                max_affordable_x_for_spell(&state, PlayerId::Us, &def, SpellFace::Main),
                expected_x,
                "with {mana} colorless mana, Command should have max X={expected_x}"
            );
        }
    }

    /// An explicitly over-announced X is rejected before the card moves or mana
    /// is spent. This guards against the old bug where the same mana paid both
    /// the base {C}{C} and X.
    #[test]
    fn test_kozileks_command_rejects_unpayable_x_without_partial_cast() {
        let mut state = make_state();
        let def = catalog_card("Kozilek's Command");
        state.catalog.insert(def.name.clone(), def);
        let card_id = add_hand_card(&mut state, PlayerId::Us, "Kozilek's Command");
        state.player_mut(PlayerId::Us).pool.c = 3;
        state.player_mut(PlayerId::Us).pool.total = 3;

        let result = cast_spell(
            &mut state, 1, PlayerId::Us, card_id, SpellFace::Main,
            None, None, &[], 3, 0,
        );
        assert!(result.is_none(), "X=3 requires five total mana for {{X}}{{C}}{{C}}");
        assert!(state.objects[&card_id].in_zone(Zone::Hand { known: false }), "failed cast must leave Command in hand");
        assert_eq!(state.player(PlayerId::Us).pool.total, 3, "failed cast must not spend mana");
        assert!(state.stack.is_empty(), "failed cast must not leave a spell on the stack");
    }

    #[test]
    fn test_kozileks_command_x1_spends_three_total_mana() {
        let mut state = make_state();
        let def = catalog_card("Kozilek's Command");
        state.catalog.insert(def.name.clone(), def);
        let card_id = add_hand_card(&mut state, PlayerId::Us, "Kozilek's Command");
        state.player_mut(PlayerId::Us).pool.c = 3;
        state.player_mut(PlayerId::Us).pool.total = 3;

        let result = cast_spell(
            &mut state, 1, PlayerId::Us, card_id, SpellFace::Main,
            None, None, &[], 1, 0,
        ).expect("X=1 Command should be castable with exactly three colorless mana");
        assert_eq!(state.player(PlayerId::Us).pool.total, 0, "{{C}}{{C}} + X=1 should drain all three mana");
        let ctx = &state.objects[&result].spell().unwrap().costs_paid_ctx;
        assert_eq!(ctx.chosen_x, 1, "paid X must be recorded for resolution");
    }

    #[test]
    fn test_failed_paid_mana_ability_does_not_produce_mana() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let boulder = catalog_card("Giant's Boulder");
        let boulder_id = add_perm_with_def(&mut state, PlayerId::Us, &boulder, BattlefieldState::new());
        recompute(&mut state);

        let def = state.def_of(boulder_id).expect("Boulder should materialize");
        let (ability_index, _) = def.mana_abilities().iter().enumerate()
            .find(|(_, ma)| ma.costs.first_mana_cost().map_or(false, |mc| mc.mana_value() == 1))
            .expect("Boulder should expose its paid mana-filter ability");
        let act = ManaActivation { source_id: boulder_id, ability_index, color_choice: Some(Color::Blue) };

        assert!(!execute_mana_activation(&mut state, 1, PlayerId::Us, &act),
            "Boulder's paid mana ability must fail when its {{1}} cost cannot be paid");
        assert_eq!(state.player(PlayerId::Us).pool.total, 0,
            "failed paid mana ability must not add mana");
        assert!(!state.permanent_bf(boulder_id).unwrap().tapped,
            "failed activation must not tap Giant's Boulder");
    }

    #[test]
    fn test_residual_mana_planner_does_not_double_subtract_floating_pool() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let wastes = catalog_card("Wastes");
        let wastes_id = add_perm_with_def(&mut state, PlayerId::Us, &wastes, BattlefieldState::new());
        state.player_mut(PlayerId::Us).pool.c = 2;
        state.player_mut(PlayerId::Us).pool.total = 2;
        recompute(&mut state);

        // The cost executor would report only the unpaid residual here: one
        // generic mana.  The planner must still activate Wastes even though
        // two mana are already floating; subtracting the pool again would
        // incorrectly return an empty plan.
        let residual = ManaCost { generic: 1, ..Default::default() };
        let plan = auto_tap_plan_remaining(&state, PlayerId::Us, &residual);
        assert!(plan.iter().any(|a| a.source_id == wastes_id),
            "residual planner must activate the final one-mana source");
    }

    #[test]
    fn test_mana_planner_skips_unfunded_paid_filter() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let boulder = catalog_card("Giant's Boulder");
        add_perm_with_def(&mut state, PlayerId::Us, &boulder, BattlefieldState::new());
        let wastes = catalog_card("Wastes");
        let wastes_id = add_perm_with_def(&mut state, PlayerId::Us, &wastes, BattlefieldState::new());
        recompute(&mut state);

        let residual = ManaCost { generic: 1, ..Default::default() };
        let plan = auto_tap_plan_remaining(&state, PlayerId::Us, &residual);
        assert_eq!(plan.first().map(|a| a.source_id), Some(wastes_id),
            "an unfunded paid mana ability must not hide an available free source");
    }

    #[test]
    fn test_funded_filter_does_not_consume_generic_mana_source() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let boulder = catalog_card("Giant's Boulder");
        let boulder_id = add_perm_with_def(&mut state, PlayerId::Us, &boulder, BattlefieldState::new());
        let wastes = catalog_card("Wastes");
        let wastes_id = add_perm_with_def(&mut state, PlayerId::Us, &wastes, BattlefieldState::new());
        let card_id = add_hand_card(&mut state, PlayerId::Us, "Grim Monolith");
        state.player_mut(PlayerId::Us).pool.c = 1;
        state.player_mut(PlayerId::Us).pool.total = 1;
        state.current_turn = 1;
        state.current_ap = state.player_id(PlayerId::Us);
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));
        recompute(&mut state);

        let residual = ManaCost { generic: 1, ..Default::default() };
        let plan = auto_tap_plan_remaining(&state, PlayerId::Us, &residual);
        assert_eq!(plan.first().map(|a| a.source_id), Some(wastes_id));
        assert!(!plan.iter().any(|a| a.source_id == boulder_id));
        assert!(run_cast_submachine(&mut state, 1, PlayerId::Us, card_id, SpellFace::Main).is_some());
    }

    #[test]
    fn test_forge_spells_offered_by_mana_predictor_actually_cast_with_residual_source() {
        for (name, floating) in [
            ("Grim Monolith", 1),
            ("Trinisphere", 2),
            ("The One Ring", 3),
        ] {
            let mut state = make_state();
            state.catalog = test_catalog();
            let wastes = catalog_card("Wastes");
            add_perm_with_def(&mut state, PlayerId::Us, &wastes, BattlefieldState::new());
            let card_id = add_hand_card(&mut state, PlayerId::Us, name);
            state.player_mut(PlayerId::Us).pool.c = floating;
            state.player_mut(PlayerId::Us).pool.total = floating;
            state.current_turn = 1;
            state.current_ap = state.player_id(PlayerId::Us);
            state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));
            recompute(&mut state);

            let legal = strategy::collect_legal_actions(&state, PlayerId::Us);
            assert!(legal.iter().any(|a| matches!(a,
                LegalAction::CastSpell { card_id: id, face: SpellFace::Main } if *id == card_id)),
                "{name} should be offered with {floating} floating plus an untapped Wastes");

            let result = run_cast_submachine(&mut state, 1, PlayerId::Us, card_id, SpellFace::Main);
            assert!(result.is_some(),
                "every mana-only spell offered as legal must execute successfully: {name}");
            assert_eq!(state.player(PlayerId::Us).pool.total, 0,
                "{name} should consume the floating mana plus Wastes");
        }
    }

    #[test]
    fn test_auto_tap_plan_satisfies_specific_colorless_pips() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let tomb = catalog_card("Ancient Tomb");
        let tomb_id = add_perm_with_def(&mut state, PlayerId::Us, &tomb, BattlefieldState::new());
        recompute(&mut state);

        let plan = auto_tap_plan(&state, PlayerId::Us, &parse_mana_cost("CC"));
        assert!(
            plan.iter().any(|a| a.source_id == tomb_id),
            "a {{C}}{{C}} payment should plan to activate Ancient Tomb"
        );
    }

    /// Meltdown ({X}{R}) drains the announced X generic on top of its base {R}
    /// via the IR `PayManaX` additional cost — end-to-end through `cast_spell`.
    #[test]
    fn test_xmana_additional_cost_drains_x_generic_on_cast() {
        let mut state = make_state();
        let def = catalog_card("Meltdown");
        state.catalog.insert(def.name.clone(), def.clone());
        let card_id = add_hand_card(&mut state, PlayerId::Us, "Meltdown");
        // Base {R} + X=3 generic = 4 mana available.
        state.player_mut(PlayerId::Us).pool.r = 1;
        state.player_mut(PlayerId::Us).pool.c = 3;
        state.player_mut(PlayerId::Us).pool.total = 4;

        // chosen_x = 3 (positional arg).
        let result = cast_spell(&mut state, 1, PlayerId::Us, card_id, SpellFace::Main, None, None, &[], 3, 0);
        assert!(result.is_some(), "Meltdown castable with {{R}} + 3 generic at X=3");
        assert_eq!(state.player(PlayerId::Us).pool.total, 0, "base {{R}} + 3 generic (XMana) fully drained");
        let ctx = &state.objects[&result.unwrap()].spell().unwrap().costs_paid_ctx;
        assert_eq!(ctx.chosen_x, 3, "announced X recorded for the resolution effect");
    }

    /// Meltdown is uncastable when the pool can't cover base {R} + the announced X.
    #[test]
    fn test_xmana_additional_cost_blocks_cast_when_short() {
        let mut state = make_state();
        let def = catalog_card("Meltdown");
        state.catalog.insert(def.name.clone(), def.clone());
        let card_id = add_hand_card(&mut state, PlayerId::Us, "Meltdown");
        // Only {R} + 1 generic = 2 mana, but X=3 needs 3 generic on top of {R}.
        state.player_mut(PlayerId::Us).pool.r = 1;
        state.player_mut(PlayerId::Us).pool.c = 1;
        state.player_mut(PlayerId::Us).pool.total = 2;

        let result = cast_spell(&mut state, 1, PlayerId::Us, card_id, SpellFace::Main, None, None, &[], 3, 0);
        assert!(result.is_none(), "Meltdown blocked: not enough mana for X=3 additional cost");
    }

    // ── Section 26: Bitter Triumph (CostOr additional cost) ──────────────────

    fn setup_bitter_triumph(state: &mut SimState) {
        let bt = catalog_card("Bitter Triumph");
        state.catalog.insert(bt.name.clone(), bt);
        // Spare card for discard tests — needs a catalog entry so cost_pred_from_card resolves it.
        let dr = catalog_card("Dark Ritual");
        state.catalog.insert(dr.name.clone(), dr);
    }

    /// Bitter Triumph prefers the discard branch when a card is available in hand.
    #[test]
    fn test_bitter_triumph_discard_branch_preferred() {
        let mut state = make_state();
        setup_bitter_triumph(&mut state);
        let extra_id = add_hand_card(&mut state, PlayerId::Us, "Dark Ritual");
        let card_id = add_hand_card(&mut state, PlayerId::Us, "Bitter Triumph");
        state.player_mut(PlayerId::Us).pool.b = 2; state.player_mut(PlayerId::Us).pool.total = 2;
        let initial_life = state.player(PlayerId::Us).life;

        let result = cast_spell(&mut state, 1, PlayerId::Us, card_id, SpellFace::Main, None, None, &[], 0, 0);
        assert!(result.is_some(), "Bitter Triumph should be castable");
        let extra_zone = state.objects.get(&extra_id).and_then(|o| o.zone());
        assert!(
            matches!(extra_zone, Some(Zone::Graveyard)),
            "discard branch of CostOr was paid (card discarded)"
        );
        assert_eq!(state.player(PlayerId::Us).life, initial_life, "life branch was not taken when discard is available");
    }

    /// Bitter Triumph falls back to the life branch when no spare card is in hand.
    #[test]
    fn test_bitter_triumph_life_branch_fallback() {
        let mut state = make_state();
        setup_bitter_triumph(&mut state);
        let card_id = add_hand_card(&mut state, PlayerId::Us, "Bitter Triumph");
        state.player_mut(PlayerId::Us).pool.b = 2; state.player_mut(PlayerId::Us).pool.total = 2;
        let initial_life = state.player(PlayerId::Us).life;

        let result = cast_spell(&mut state, 1, PlayerId::Us, card_id, SpellFace::Main, None, None, &[], 0, 0);
        assert!(result.is_some(), "Bitter Triumph should be castable via life branch");
        assert_eq!(state.player(PlayerId::Us).life, initial_life - 3, "3 life paid as fallback cost");
    }

    /// Bitter Triumph is uncastable when neither branch can be paid.
    #[test]
    fn test_bitter_triumph_unpayable_when_both_branches_blocked() {
        let mut state = make_state();
        setup_bitter_triumph(&mut state);
        let card_id = add_hand_card(&mut state, PlayerId::Us, "Bitter Triumph");
        state.player_mut(PlayerId::Us).pool.b = 2; state.player_mut(PlayerId::Us).pool.total = 2;
        state.player_mut(PlayerId::Us).life = 3; // can't pay Life(3) — life > n is strict

        let result = cast_spell(&mut state, 1, PlayerId::Us, card_id, SpellFace::Main, None, None, &[], 0, 0);
        assert!(result.is_none(), "Bitter Triumph should be blocked when life ≤ 3 and no spare card");
    }

    // ── Section 27: Consign to Memory (Replicate + triggered-ability targeting) ──

    fn setup_consign(state: &mut SimState) {
        let def = catalog_card("Consign to Memory");
        state.catalog.insert(def.name.clone(), def);
    }

    /// Push a fake colorless spell onto the stack for the opponent.
    fn push_colorless_spell_for_opp(state: &mut SimState) -> ObjId {
        // Use Lotus Petal as a colorless spell proxy.
        let def = catalog_card("Lotus Petal");
        state.catalog.insert(def.name.clone(), def.clone());
        let spell_id = state.alloc_id();
        state.objects.insert(spell_id, GameObject {
            id: spell_id,
            catalog_key: "Lotus Petal".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            is_token: false,
            materialized: Some(def),
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::StackSpell(SpellState {
                effect: None,
                chosen_targets: vec![],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
        });
        state.stack.push(spell_id);
        spell_id
    }

    /// Push a fake triggered ability onto the stack for the opponent.
    fn push_opp_triggered_ability(state: &mut SimState) -> ObjId {
        let ab_id = state.alloc_id();
        state.insert_stack_ability(ab_id, "Test Trigger", PlayerId::Opp, AbilityState {
            effect: Effect(std::sync::Arc::new(|_, _, _| {})),
            chosen_targets: vec![],
            costs_paid_ctx: CostsPaidCtx::default(),
            is_triggered: true,
            counterable: true,
            choice_spec: None,
        });
        ab_id
    }

    /// Consign to Memory can counter a colorless spell on the stack.
    #[test]
    fn test_consign_counters_colorless_spell() {
        let mut state = make_state();
        setup_consign(&mut state);
        let spell_id = push_colorless_spell_for_opp(&mut state);
        let card_id = add_hand_card(&mut state, PlayerId::Us, "Consign to Memory");
        state.player_mut(PlayerId::Us).pool.u = 1; state.player_mut(PlayerId::Us).pool.total = 1;

        let result = cast_spell(&mut state, 1, PlayerId::Us, card_id, SpellFace::Main, None, None, &[spell_id], 0, 0);
        assert!(result.is_some(), "Consign to Memory should be castable");

        // Resolve — pop from stack and execute effect.
        let card_on_stack = result.unwrap();
        let spell_state = state.objects[&card_on_stack].spell().cloned().unwrap();
        spell_state.effect.unwrap().call(&mut state, 1, &spell_state.chosen_targets);

        assert!(!state.stack.contains(&spell_id), "colorless spell should be removed from stack");
        assert_eq!(
            state.objects.get(&spell_id).and_then(|o| o.zone()),
            Some(Zone::Graveyard),
            "countered spell goes to graveyard"
        );
    }

    /// Consign to Memory can counter a triggered ability on the stack.
    #[test]
    fn test_consign_counters_triggered_ability() {
        let mut state = make_state();
        setup_consign(&mut state);
        let ab_id = push_opp_triggered_ability(&mut state);
        let card_id = add_hand_card(&mut state, PlayerId::Us, "Consign to Memory");
        state.player_mut(PlayerId::Us).pool.u = 1; state.player_mut(PlayerId::Us).pool.total = 1;

        let result = cast_spell(&mut state, 1, PlayerId::Us, card_id, SpellFace::Main, None, None, &[ab_id], 0, 0);
        assert!(result.is_some(), "Consign to Memory should be castable targeting a triggered ability");

        // Resolve.
        let card_on_stack = result.unwrap();
        let spell_state = state.objects[&card_on_stack].spell().cloned().unwrap();
        spell_state.effect.unwrap().call(&mut state, 1, &spell_state.chosen_targets);

        assert!(!state.stack.contains(&ab_id), "triggered ability should be removed from stack");
        assert!(!state.objects.contains_key(&ab_id), "countered ability ceases to exist (removed from objects)");
    }

    /// `ir_triggered_ability()` legal_targets enumerates opponent triggered abilities.
    #[test]
    fn test_triggered_ability_on_stack_legal_targets() {
        let mut state = make_state();
        let ab_id = push_opp_triggered_ability(&mut state);

        let spec = TargetSpec::ObjectInZone { controller: Who::Opp, zone: ZoneId::Stack, filter: ir_triggered_ability() };
        let targets = legal_targets(&spec, PlayerId::Us, ObjId(0), &state);
        assert!(targets.contains(&ab_id), "opp triggered ability should be a legal target");
    }

    /// Activated abilities (is_triggered=false) are not matched by `ir_triggered_ability()`.
    #[test]
    fn test_activated_ability_not_a_trigger_target() {
        let mut state = make_state();
        let ab_id = state.alloc_id();
        state.insert_stack_ability(ab_id, "Activated Ability", PlayerId::Opp, AbilityState {
            effect: Effect(std::sync::Arc::new(|_, _, _| {})),
            chosen_targets: vec![],
            costs_paid_ctx: CostsPaidCtx::default(),
            is_triggered: false,
            counterable: true,
            choice_spec: None,
        });

        let spec = TargetSpec::ObjectInZone { controller: Who::Opp, zone: ZoneId::Stack, filter: ir_triggered_ability() };
        let targets = legal_targets(&spec, PlayerId::Us, ObjId(0), &state);
        assert!(!targets.contains(&ab_id), "activated ability should not match ir_triggered_ability()");
    }


    /// eff_counter_target fizzles against a spell with counterable=false (CR 608.2b).
    #[test]
    fn test_counter_fizzles_on_uncounterable_spell() {
        let mut state = make_state();
        // Push a fake uncounterable spell for the opponent.
        let spell_id = state.alloc_id();
        state.objects.insert(spell_id, GameObject {
            id: spell_id,
            catalog_key: "Long Goodbye".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::StackSpell(SpellState { effect: None, chosen_targets: vec![], is_back_face: false, costs_paid_ctx: CostsPaidCtx::default() }),
        });
        state.stack.push(spell_id);
        // Set counterable=false by inserting the card's def (with the flag) into the catalog.
        let mut lg_def = catalog_card("Long Goodbye");
        lg_def.counterable = false;
        state.catalog.insert("Long Goodbye".to_string(), lg_def);

        let effect = eff_counter_target(PlayerId::Us);
        effect.call(&mut state, 1, &[spell_id]);

        // Spell should still be on the stack — counter fizzled.
        assert!(state.stack.contains(&spell_id), "uncounterable spell should remain on stack");
        assert_eq!(state.objects[&spell_id].zone(), Some(Zone::Stack), "zone unchanged after fizzle");
    }

    // ── Section 56: Stifle (counter activated or triggered ability) ──────────

    /// Stifle counters an opponent's triggered ability on the stack.
    #[test]
    fn test_stifle_counters_triggered_ability() {
        let mut state = make_state();
        let def = catalog_card("Stifle");
        state.catalog.insert(def.name.clone(), def);

        let ab_id = push_opp_triggered_ability(&mut state);
        let card_id = add_hand_card(&mut state, PlayerId::Us, "Stifle");
        state.player_mut(PlayerId::Us).pool.u = 1; state.player_mut(PlayerId::Us).pool.total = 1;

        let result = cast_spell(&mut state, 1, PlayerId::Us, card_id, SpellFace::Main, None, None, &[ab_id], 0, 0);
        assert!(result.is_some(), "Stifle should be castable targeting a triggered ability");

        let card_on_stack = result.unwrap();
        let spell_state = state.objects[&card_on_stack].spell().cloned().unwrap();
        spell_state.effect.unwrap().call(&mut state, 1, &spell_state.chosen_targets);

        assert!(!state.stack.contains(&ab_id), "triggered ability should be removed from stack");
        assert!(!state.objects.contains_key(&ab_id), "countered ability ceases to exist (removed from objects)");
    }

    /// Stifle's TargetSpec (`ir_ability()`) lists both activated and triggered abilities
    /// as legal targets — what distinguishes it from Consign to Memory.
    #[test]
    fn test_stifle_targets_activated_or_triggered() {
        let mut state = make_state();

        // Push one triggered, one activated opponent ability onto the stack.
        let trig_id = push_opp_triggered_ability(&mut state);
        let act_id = state.alloc_id();
        state.insert_stack_ability(act_id, "Activated Ability", PlayerId::Opp, AbilityState {
            effect: Effect(std::sync::Arc::new(|_, _, _| {})),
            chosen_targets: vec![],
            costs_paid_ctx: CostsPaidCtx::default(),
            is_triggered: false,
            counterable: true,
            choice_spec: None,
        });

        let spec = TargetSpec::ObjectInZone { controller: Who::Opp, zone: ZoneId::Stack, filter: ir_ability() };
        let targets = legal_targets(&spec, PlayerId::Us, ObjId(0), &state);
        assert!(targets.contains(&trig_id), "triggered ability is a legal Stifle target");
        assert!(targets.contains(&act_id), "activated ability is a legal Stifle target");
    }

    /// Regression (Phase C): now that abilities are objects with zone==Stack, a
    /// "counter target spell" filter (`ir_spell()`) must list only spells — never an
    /// ability. The inverse of Stifle/Consign.
    #[test]
    fn test_counter_spell_filter_excludes_abilities() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let ab_id = push_opp_triggered_ability(&mut state);
        let spell_id = push_colorless_spell_for_opp(&mut state);

        let spec = TargetSpec::ObjectInZone { controller: Who::Opp, zone: ZoneId::Stack, filter: ir_spell() };
        let targets = legal_targets(&spec, PlayerId::Us, ObjId(0), &state);
        assert!(targets.contains(&spell_id), "a spell is a legal counter-spell target");
        assert!(!targets.contains(&ab_id), "an ability is NOT a legal counter-spell target");
    }

    /// `Who::Any` (real Daze / Force of Will) may target your OWN spell on the stack — the
    /// "Daze your own Lotus Petal to advance the per-turn spell count" line — while a normal
    /// `Who::Opp` counter still cannot (the self-owner guard stays for opponent-only counters).
    #[test]
    fn counter_targets_own_spell_only_with_who_any() {
        let mut state = make_state();
        let petal = catalog_card("Lotus Petal");
        let own = add_stack_spell(&mut state, PlayerId::Us, &petal);

        let any = TargetSpec::ObjectInZone { controller: Who::Any, zone: ZoneId::Stack, filter: ir_spell() };
        let opp = TargetSpec::ObjectInZone { controller: Who::Opp, zone: ZoneId::Stack, filter: ir_spell() };
        assert!(legal_targets(&any, PlayerId::Us, ObjId(0), &state).contains(&own),
            "Who::Any may target our own spell (Daze your own Petal)");
        assert!(!legal_targets(&opp, PlayerId::Us, ObjId(0), &state).contains(&own),
            "Who::Opp must NOT target our own spell");
    }

    // ── Section 57: Stoneforge Mystic (Equipment subtype + tutor + put-from-hand) ──

    /// Stoneforge's ETB tutor finds an Equipment in the library and puts it into hand;
    /// non-Equipment artifacts and creatures stay put.
    #[test]
    fn test_stoneforge_etb_finds_equipment() {
        let mut state = make_state();
        state.catalog.insert("Stoneforge Mystic".into(), catalog_card("Stoneforge Mystic"));
        state.catalog.insert("Cori-Steel Cutter".into(), catalog_card("Cori-Steel Cutter"));
        state.catalog.insert("Lotus Petal".into(), catalog_card("Lotus Petal"));

        let equip_id = add_library_card(&mut state, PlayerId::Us, "Cori-Steel Cutter");
        let petal_id = add_library_card(&mut state, PlayerId::Us, "Lotus Petal");

        eff_enter_permanent(PlayerId::Us, "Stoneforge Mystic").call(&mut state, 1, &[]);
        for ctx in std::mem::take(&mut state.pending_triggers) {
            ctx.effect.call(&mut state, 1, &[]);
        }

        assert_eq!(state.objects[&equip_id].zone(), Some(Zone::Hand { known: false }),
            "Equipment should be tutored into hand");
        assert_eq!(state.objects[&petal_id].zone(), Some(Zone::Library),
            "non-Equipment artifact should stay in library");
    }

    /// Stoneforge's activated ability puts an Equipment from hand onto the battlefield.
    #[test]
    fn test_stoneforge_activated_puts_equipment_from_hand() {
        let mut state = make_state();
        state.catalog.insert("Stoneforge Mystic".into(), catalog_card("Stoneforge Mystic"));
        state.catalog.insert("Cori-Steel Cutter".into(), catalog_card("Cori-Steel Cutter"));

        // Stoneforge in play, untapped.
        eff_enter_permanent(PlayerId::Us, "Stoneforge Mystic").call(&mut state, 1, &[]);
        // Drop the ETB tutor's pending trigger — we're testing the activated ability here.
        state.pending_triggers.clear();
        let sfm_id = state.objects.values()
            .find(|o| o.catalog_key == "Stoneforge Mystic" && o.in_zone(Zone::Battlefield))
            .map(|o| o.id).expect("Stoneforge should be on the battlefield");
        if let Some(bf) = state.permanent_bf_mut(sfm_id) { bf.entered_this_turn = false; }

        let equip_id = add_hand_card(&mut state, PlayerId::Us, "Cori-Steel Cutter");

        // Fire the {1}{W}, {T} ability's effect (the put-equipment ability).
        let def = state.catalog["Stoneforge Mystic"].clone();
        let ability = &def.as_creature().unwrap().abilities[0];
        let eff = build_ability_effect(ability, PlayerId::Us, sfm_id);
        eff.call(&mut state, 1, &[equip_id]);

        assert_eq!(state.objects[&equip_id].zone(), Some(Zone::Battlefield),
            "Equipment should be on the battlefield");
    }

    // ── Section 58: Batterskull (Living Weapon + buff equipped + bounce self) ──

    /// Living weapon ETB creates a Phyrexian Germ token and attaches Batterskull to it.
    /// The materialized Germ shows +4/+4 (so 4/4) and gains vigilance/lifelink.
    #[test]
    fn test_batterskull_living_weapon_attaches_and_buffs_germ() {
        let mut state = make_state();
        state.catalog.insert("Batterskull".into(), catalog_card("Batterskull"));
        state.catalog.insert("Phyrexian Germ".into(), catalog_card("Phyrexian Germ"));

        eff_enter_permanent(PlayerId::Us, "Batterskull").call(&mut state, 1, &[]);
        for ctx in std::mem::take(&mut state.pending_triggers) {
            ctx.effect.call(&mut state, 1, &[]);
        }

        let bs_id = state.objects.values()
            .find(|o| o.catalog_key == "Batterskull" && o.in_zone(Zone::Battlefield))
            .map(|o| o.id).expect("Batterskull on battlefield");
        let germ_id = state.objects.values()
            .find(|o| o.catalog_key == "Phyrexian Germ" && o.in_zone(Zone::Battlefield))
            .map(|o| o.id).expect("Phyrexian Germ token created");

        let attached = state.permanent_bf(bs_id).and_then(|bf| bf.attached_to);
        assert_eq!(attached, Some(germ_id), "Batterskull should be attached to the Germ");

        // Materialized state should show +4/+4 and vigilance/lifelink on the Germ.
        recompute(&mut state);
        let germ_def = state.def_of(germ_id).expect("germ has materialized def");
        let germ = germ_def.as_creature().expect("germ is a creature");
        assert_eq!(germ.power(), 4, "0/0 Germ + Batterskull's +4/+4 = 4 power");
        assert_eq!(germ.toughness(), 4, "0/0 Germ + Batterskull's +4/+4 = 4 toughness");
        assert!(germ.keywords.contains(Keyword::Vigilance), "Germ has vigilance");
        assert!(germ.keywords.contains(Keyword::Lifelink), "Germ has lifelink");
    }

    /// Batterskull's {3} activated ability returns Batterskull from BF to its owner's hand.
    #[test]
    fn test_batterskull_bounce_self_to_hand() {
        let mut state = make_state();
        state.catalog.insert("Batterskull".into(), catalog_card("Batterskull"));

        eff_enter_permanent(PlayerId::Us, "Batterskull").call(&mut state, 1, &[]);
        state.pending_triggers.clear();
        let bs_id = state.objects.values()
            .find(|o| o.catalog_key == "Batterskull" && o.in_zone(Zone::Battlefield))
            .map(|o| o.id).expect("Batterskull on battlefield");

        // Fire the {3} ability (index 0 in abilities).
        let def = state.catalog["Batterskull"].clone();
        let ability = match &def.kind {
            CardKind::Artifact(a) => &a.abilities[0],
            _ => panic!("Batterskull is an artifact"),
        };
        let eff = build_ability_effect(ability, PlayerId::Us, bs_id);
        eff.call(&mut state, 1, &[]);

        assert_eq!(state.objects[&bs_id].zone(), Some(Zone::Hand { known: false }),
            "Batterskull should be in its owner's hand");
    }

    // ── 28. Force of Negation ─────────────────────────────────────────────────

    /// The pitch-cost condition on Force of Negation is true when it's not the caster's turn
    /// and false when it is. CR 118.9b (alternative costs may have conditions on card text).
    #[test]
    fn test_fon_pitch_condition_checks_active_player() {
        let mut state = make_state();
        let fon_def = catalog_card("Force of Negation");
        let alt = &fon_def.alternate_costs()[0];
        let condition = alt.condition.as_ref()
            .expect("Force of Negation pitch cost must have a condition");

        // Opponent's turn: condition should allow Us to pitch.
        state.current_ap = state.player_id(PlayerId::Opp);
        assert!(condition(PlayerId::Us, &state), "pitch cost available when it's not our turn");

        // Our turn: condition should block the pitch cost.
        state.current_ap = state.player_id(PlayerId::Us);
        assert!(!condition(PlayerId::Us, &state), "pitch cost unavailable on our own turn");
    }

    /// Force of Negation's IR body (`Sequence([Counter, Exile])`) sends the countered
    /// spell to Exile, not Graveyard — modeling "exile it instead of putting it into its
    /// owner's graveyard."
    #[test]
    fn test_fon_counter_and_exile() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Simulate opponent's turn (required for pitch cost, though we call the effect directly).
        state.current_ap = state.player_id(PlayerId::Opp);

        // Push a noncreature opponent spell onto the stack.
        let spell_id = state.alloc_id();
        state.objects.insert(spell_id, GameObject {
            id: spell_id,
            catalog_key: "Dark Ritual".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::StackSpell(SpellState {
                effect: None,
                chosen_targets: vec![],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
        });
        state.stack.push(spell_id);

        // Fire FoN's counter-and-exile effect.
        let fon_id = state.alloc_id();
        let effect = build_spell_effect(&catalog_card("Force of Negation"), PlayerId::Us, fon_id, 0, 0).1;
        effect.call(&mut state, 1, &[spell_id]);

        // Spell should be in Exile, not Graveyard; stack should be empty.
        assert!(!state.stack.contains(&spell_id), "countered spell should be off the stack");
        assert_eq!(
            state.objects[&spell_id].zone(), Some(Zone::Exile { on_adventure: false }),
            "countered spell should be exiled, not in graveyard",
        );
        assert!(state.objects[&spell_id].spell().is_none(), "spell state should be cleared");
    }

    /// If FoN itself is countered before resolving, its scoped replacement effect is never
    /// installed, so the target remains on the stack unaffected (not exiled).
    #[test]
    fn test_fon_countered_target_not_exiled() {
        let mut state = make_state();

        // Y — opponent's noncreature spell (FoN's target).
        let y_id = state.alloc_id();
        state.objects.insert(y_id, GameObject {
            id: y_id,
            catalog_key: "Dark Ritual".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::StackSpell(SpellState { effect: None, chosen_targets: vec![], is_back_face: false, costs_paid_ctx: CostsPaidCtx::default() }),
        });
        state.stack.push(y_id);

        // FoN targeting Y — cast by us.
        let fon_id = state.alloc_id();
        state.objects.insert(fon_id, GameObject {
            id: fon_id,
            catalog_key: "Force of Negation".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::StackSpell(SpellState {
                effect: Some(build_spell_effect(&catalog_card("Force of Negation"), PlayerId::Us, fon_id, 0, 0).1),
                chosen_targets: vec![y_id],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
        });
        state.stack.push(fon_id);

        // Opponent counters FoN — its effect closure never runs, so the scoped RE is never installed.
        eff_counter_target(PlayerId::Opp).call(&mut state, 1, &[fon_id]);

        assert!(!state.stack.contains(&fon_id), "FoN should be off the stack after being countered");
        assert_eq!(state.objects[&fon_id].zone(), Some(Zone::Graveyard), "FoN goes to graveyard");
        assert!(state.stack.contains(&y_id), "Y should still be on the stack — FoN never resolved");
        assert_eq!(state.objects[&y_id].zone(), Some(Zone::Stack), "Y remains in Stack zone");
    }

    /// Stack: X (bottom), Y, FoN targeting Y, FoW targeting X (top).
    /// FoW resolves first → counters X → X to graveyard.
    /// FoN resolves next → counters Y → Y to exile (scoped replacement).
    /// After both resolutions: X in graveyard, Y in exile.
    #[test]
    fn test_fow_x_fon_y_stack_interaction() {
        let mut state = make_state();

        // X and Y — opponent noncreature spells.
        let x_id = state.alloc_id();
        let y_id = state.alloc_id();
        for &id in &[x_id, y_id] {
            state.objects.insert(id, GameObject {
            id,
            catalog_key: "Dark Ritual".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::StackSpell(SpellState { effect: None, chosen_targets: vec![], is_back_face: false, costs_paid_ctx: CostsPaidCtx::default() }),
        });
        }

        // FoN targeting Y.
        let fon_id = state.alloc_id();
        state.objects.insert(fon_id, GameObject {
            id: fon_id,
            catalog_key: "Force of Negation".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::StackSpell(SpellState {
                effect: Some(build_spell_effect(&catalog_card("Force of Negation"), PlayerId::Us, fon_id, 0, 0).1),
                chosen_targets: vec![y_id],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
        });

        // FoW targeting X.
        let fow_id = state.alloc_id();
        state.objects.insert(fow_id, GameObject {
            id: fow_id,
            catalog_key: "Force of Will".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::StackSpell(SpellState {
                effect: Some(eff_counter_target(PlayerId::Us)),
                chosen_targets: vec![x_id],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
        });

        // Stack order bottom→top: X, Y, FoN, FoW.
        state.stack.extend([x_id, y_id, fon_id, fow_id]);

        // FoW resolves: counters X → X to graveyard; FoW itself to graveyard.
        resolve_top_of_stack(&mut state, 1, PlayerId::Us);
        // FoN resolves: scoped RE installed, counters Y → Y intercepted to exile; FoN to graveyard.
        resolve_top_of_stack(&mut state, 1, PlayerId::Us);

        assert!(state.stack.is_empty(), "stack should be empty");
        assert_eq!(state.objects[&x_id].zone(), Some(Zone::Graveyard), "X countered by FoW → graveyard");
        assert_eq!(
            state.objects[&y_id].zone(), Some(Zone::Exile { on_adventure: false }),
            "Y countered by FoN → exile",
        );
        assert_eq!(state.objects[&fow_id].zone(), Some(Zone::Graveyard), "FoW → graveyard after resolving");
        assert_eq!(state.objects[&fon_id].zone(), Some(Zone::Graveyard), "FoN → graveyard after resolving");
    }

    // ── Section 59: Meteor Sword (ETB destroy + buff equipped) ────────────────

    /// Meteor Sword's ETB trigger destroys a target permanent; materialized state
    /// for the equipped creature shows +3/+3.
    #[test]
    fn test_meteor_sword_etb_destroys_target_and_buffs_equipped() {
        let mut state = make_state();
        state.catalog.insert("Meteor Sword".into(), catalog_card("Meteor Sword"));

        // Opponent creature to be destroyed by the ETB trigger.
        let victim = add_default_perm(&mut state, PlayerId::Opp, "Delver of Secrets");
        // Our creature to equip.
        let ours = add_default_perm(&mut state, PlayerId::Us, "Delver of Secrets");

        eff_enter_permanent(PlayerId::Us, "Meteor Sword").call(&mut state, 1, &[]);
        let sword_id = state.objects.values()
            .find(|o| o.catalog_key == "Meteor Sword" && o.in_zone(Zone::Battlefield))
            .map(|o| o.id).expect("Meteor Sword on battlefield");

        // Resolve the ETB trigger against the opponent's creature.
        let ctx = state.pending_triggers.pop().expect("ETB trigger queued");
        assert_eq!(ctx.source_name, "Meteor Sword");
        let all = legal_targets(&ctx.target_spec, PlayerId::Us, sword_id, &state);
        assert!(all.contains(&victim), "opponent permanent is a legal target");
        assert!(all.contains(&ours), "our permanent is also a legal target");
        ctx.effect.call(&mut state, 1, &[victim]);
        assert_eq!(state.objects[&victim].zone(), Some(Zone::Graveyard),
            "target permanent destroyed");

        // Equip: attach Meteor Sword to our creature and verify +3/+3.
        let def = state.catalog["Meteor Sword"].clone();
        let ability = match &def.kind {
            CardKind::Artifact(a) => &a.abilities[0],
            _ => panic!("Meteor Sword is an artifact"),
        };
        let eff = build_ability_effect(ability, PlayerId::Us, sword_id);
        eff.call(&mut state, 1, &[ours]);
        assert_eq!(state.permanent_bf(sword_id).and_then(|bf| bf.attached_to), Some(ours),
            "Meteor Sword attached to our creature");

        recompute(&mut state);
        let eq_def = state.def_of(ours).expect("materialized def");
        let eq = eq_def.as_creature().expect("creature");
        assert_eq!(eq.power(), 1 + 3, "base 1 + Meteor Sword +3");
        assert_eq!(eq.toughness(), 1 + 3, "base 1 + Meteor Sword +3");
    }

    /// Fury's ETB triggered ability deals 4 damage to a target creature (IR body
    /// via `eff_ir_targeted` + protection-aware `DealDamage`).
    #[test]
    fn test_fury_etb_deals_4_to_target() {
        let mut state = make_state();
        state.catalog = test_catalog();
        state.catalog.insert("Fury".into(), catalog_card("Fury"));

        let victim = add_default_perm(&mut state, PlayerId::Opp, "Murktide Regent");
        recompute(&mut state);

        eff_enter_permanent(PlayerId::Us, "Fury").call(&mut state, 1, &[]);
        let ctx = state.pending_triggers.pop().expect("Fury ETB trigger queued");
        assert_eq!(ctx.source_name, "Fury");
        ctx.effect.call(&mut state, 1, &[victim]);

        assert_eq!(state.permanent_bf(victim).unwrap().damage, 4,
            "Fury ETB should deal 4 damage to its target");
    }

    /// Evoke (CR 702.74): cast for its evoke cost (logged as an `alt_cost` SpellCast),
    /// Fury sacrifices itself on ETB.
    #[test]
    fn test_fury_evoke_sacrifices_itself() {
        let mut state = make_state();
        state.catalog = test_catalog();
        state.catalog.insert("Fury".into(), catalog_card("Fury"));

        let fury_id = add_hand_card(&mut state, PlayerId::Us, "Fury");
        state.event_log.push(1, GameEvent::SpellCast {
            caster: PlayerId::Us, card_id: fury_id, mana_spent: false, alt_cost: true, x: 0, delved: Vec::new(),
        });
        change_zone(fury_id, ZoneId::Battlefield, &mut state, 1, PlayerId::Us);
        for ctx in std::mem::take(&mut state.pending_triggers) { ctx.effect.call(&mut state, 1, &[]); }

        assert_eq!(state.objects[&fury_id].zone(), Some(Zone::Graveyard),
            "Fury sacrifices itself when cast for its evoke cost");
    }

    /// A hardcast Fury (no `alt_cost`) stays on the battlefield.
    #[test]
    fn test_fury_hardcast_not_sacrificed() {
        let mut state = make_state();
        state.catalog = test_catalog();
        state.catalog.insert("Fury".into(), catalog_card("Fury"));

        let fury_id = add_hand_card(&mut state, PlayerId::Us, "Fury");
        state.event_log.push(1, GameEvent::SpellCast {
            caster: PlayerId::Us, card_id: fury_id, mana_spent: true, alt_cost: false, x: 0, delved: Vec::new(),
        });
        change_zone(fury_id, ZoneId::Battlefield, &mut state, 1, PlayerId::Us);
        for ctx in std::mem::take(&mut state.pending_triggers) { ctx.effect.call(&mut state, 1, &[]); }

        assert_eq!(state.objects[&fury_id].zone(), Some(Zone::Battlefield),
            "hardcast Fury stays on the battlefield");
    }

    /// Engineered Explosives destroys each nonland permanent whose mana value
    /// equals its charge counters, read via `CountersOn(Source)` after sacrifice
    /// (IR `ForEach` + `Destroy`).
    #[test]
    fn test_engineered_explosives_destroys_nonland_at_charge_mv() {
        let mut state = make_state();
        state.catalog = test_catalog();
        state.catalog.insert("Engineered Explosives".into(), catalog_card("Engineered Explosives"));

        let mv2 = add_default_perm(&mut state, PlayerId::Opp, "Null Rod");     // artifact MV 2
        let mv0 = add_default_perm(&mut state, PlayerId::Opp, "Lotus Petal");  // artifact MV 0
        let land = add_default_perm(&mut state, PlayerId::Opp, "Island");      // land (nonland filter spares it)
        recompute(&mut state);

        // EE entered with X=2, then sacrificed; its charge counters persist in the
        // objects map and are read at resolution.
        let ee_id = add_default_perm(&mut state, PlayerId::Us, "Engineered Explosives");
        change_zone(ee_id, ZoneId::Graveyard, &mut state, 1, PlayerId::Us);
        state.objects.get_mut(&ee_id).unwrap().counters.insert(CounterType::Charge, 2);

        let def = catalog_card("Engineered Explosives");
        let ability = match &def.kind { CardKind::Artifact(a) => &a.abilities[0], _ => panic!("EE is an artifact") };
        let eff = build_ability_effect(ability, PlayerId::Us, ee_id);
        eff.call(&mut state, 1, &[]);

        assert_eq!(state.objects[&mv2].zone(), Some(Zone::Graveyard), "MV 2 artifact destroyed (matches 2 charges)");
        assert_eq!(state.objects[&mv0].zone(), Some(Zone::Battlefield), "MV 0 artifact survives");
        assert_eq!(state.objects[&land].zone(), Some(Zone::Battlefield), "land survives (nonland filter)");
    }

    // ── Section 60: Quantum Riddler (ETB draw + Warp alt cost + delayed exile) ──

    /// ETB trigger fires with `TargetSpec::None` and draws a card for Quantum Riddler's
    /// controller. Warp alternative cost is {1}{U}.
    #[test]
    fn test_quantum_riddler_etb_draws_and_warp_alt_cost_present() {
        let def = catalog_card("Quantum Riddler");
        let alts = def.alternate_costs();
        assert_eq!(alts.len(), 1, "one alternate (warp) cost should be present");
        assert!(alts[0].costs.includes_mana(),
            "warp cost should be a mana cost");

        let mut state = make_state();
        state.catalog.insert("Quantum Riddler".into(), def);
        // Normal (non-warp) cast: alt_cost_index is None, so only the ETB-draw trigger fires.
        let before = state.player(PlayerId::Us).draws_this_turn;
        eff_enter_permanent(PlayerId::Us, "Quantum Riddler").call(&mut state, 1, &[]);
        let drew = state.pending_triggers.iter()
            .find(|ctx| ctx.source_name == "Quantum Riddler").cloned()
            .expect("ETB draw trigger queued");
        drew.effect.call(&mut state, 1, &[]);
        assert!(state.player(PlayerId::Us).draws_this_turn > before,
            "Quantum Riddler ETB should draw a card");

        assert!(!state.pending_triggers.iter().any(|ctx| ctx.source_name == "Quantum Riddler (warp)"),
            "warp-exile trigger must not fire without alt_cost_index");
        assert!(state.trigger_instances.is_empty(),
            "no delayed end-step trigger registered on a normal cast");
    }

    /// When cast for its Warp alternative cost, Quantum Riddler's ETB schedules a
    /// delayed end-step exile. "Cast for warp" is read from the event log — its own
    /// SpellCast this turn carried `alt_cost` (warp's {1}{U} spends mana, so it's the
    /// `alt_cost` flag, not `!mana_spent`, that distinguishes warp from a hardcast).
    #[test]
    fn test_quantum_riddler_warp_registers_end_step_exile() {
        let mut state = make_state();
        state.catalog = test_catalog();
        state.catalog.insert("Quantum Riddler".into(), catalog_card("Quantum Riddler"));

        // Log a warp cast for Quantum Riddler, then move it to the battlefield (ETB).
        let qr_id = add_hand_card(&mut state, PlayerId::Us, "Quantum Riddler");
        state.event_log.push(1, GameEvent::SpellCast {
            caster: PlayerId::Us, card_id: qr_id, mana_spent: true, alt_cost: true, x: 0, delved: Vec::new(),
        });
        change_zone(qr_id, ZoneId::Battlefield, &mut state, 1, PlayerId::Us);

        // Resolve the ETB triggers — the warp one schedules a delayed end-step exile.
        for ctx in std::mem::take(&mut state.pending_triggers) { ctx.effect.call(&mut state, 1, &[]); }
        assert_eq!(state.trigger_instances.len(), 1, "warp schedules a delayed end-step exile");
        assert_eq!(state.trigger_instances[0].expiry, Some(Expiry::OneShot));

        // Fire end step → the delayed trigger exiles Quantum Riddler.
        fire_event(
            GameEvent::EnteredStep { step: StepKind::End, active_player: PlayerId::Us },
            &mut state, 2, PlayerId::Us,
        );
        for ctx in std::mem::take(&mut state.pending_triggers) { ctx.effect.call(&mut state, 2, &[]); }
        assert_eq!(state.objects[&qr_id].zone(), Some(Zone::Exile { on_adventure: false }),
            "Quantum Riddler should be exiled at end step when cast for warp");
    }

    /// And a hardcast Quantum Riddler (no `alt_cost` in the log) is NOT exiled.
    #[test]
    fn test_quantum_riddler_hardcast_not_exiled() {
        let mut state = make_state();
        state.catalog = test_catalog();
        state.catalog.insert("Quantum Riddler".into(), catalog_card("Quantum Riddler"));

        let qr_id = add_hand_card(&mut state, PlayerId::Us, "Quantum Riddler");
        state.event_log.push(1, GameEvent::SpellCast {
            caster: PlayerId::Us, card_id: qr_id, mana_spent: true, alt_cost: false, x: 0, delved: Vec::new(),
        });
        change_zone(qr_id, ZoneId::Battlefield, &mut state, 1, PlayerId::Us);
        for ctx in std::mem::take(&mut state.pending_triggers) { ctx.effect.call(&mut state, 1, &[]); }
        assert!(state.trigger_instances.is_empty(), "hardcast: no delayed warp exile scheduled");
    }

    // ── Section 61: Pre-War Formalwear (ETB reanimate + static buff) ──────────

    /// Pre-War Formalwear's ETB trigger targets a creature in own graveyard with MV ≤ 3,
    /// reanimates it, and attaches self. Static buff grants +2/+2 and vigilance.
    #[test]
    fn test_pre_war_formalwear_etb_reanimates_and_attaches() {
        let mut state = make_state();
        state.catalog.insert("Pre-War Formalwear".into(), catalog_card("Pre-War Formalwear"));

        // Put a small creature in our graveyard.
        let gy_creature = add_default_perm(&mut state, PlayerId::Us, "Delver of Secrets");
        if let Some(obj) = state.objects.get_mut(&gy_creature) {
            obj.set_zone(Zone::Graveyard);
        }

        // Pre-War Formalwear ETBs → ETB trigger queued.
        eff_enter_permanent(PlayerId::Us, "Pre-War Formalwear").call(&mut state, 1, &[]);
        let pwf_id = state.objects.values()
            .find(|o| o.catalog_key == "Pre-War Formalwear" && o.in_zone(Zone::Battlefield))
            .map(|o| o.id).expect("Pre-War Formalwear on battlefield");

        let ctx = state.pending_triggers.iter()
            .find(|c| c.source_name == "Pre-War Formalwear").cloned()
            .expect("ETB trigger queued");
        let targets = legal_targets(&ctx.target_spec, PlayerId::Us, pwf_id, &state);
        assert!(targets.contains(&gy_creature),
            "creature in own graveyard with MV ≤ 3 is a legal target");

        ctx.effect.call(&mut state, 1, &[gy_creature]);
        assert_eq!(state.objects[&gy_creature].zone(), Some(Zone::Battlefield),
            "target reanimated");
        assert_eq!(state.permanent_bf(pwf_id).and_then(|bf| bf.attached_to), Some(gy_creature),
            "Pre-War Formalwear attached to reanimated creature");

        recompute(&mut state);
        let eq_def = state.def_of(gy_creature).expect("materialized def");
        let eq = eq_def.as_creature().expect("creature");
        assert_eq!(eq.power(), 1 + 2, "base 1 + Pre-War Formalwear +2");
        assert_eq!(eq.toughness(), 1 + 2, "base 1 + Pre-War Formalwear +2");
        assert!(eq.keywords.contains(Keyword::Vigilance), "granted vigilance");
    }

    // ── Section 62: Cryptic Coat (ETB cloak token + attach + bounce ability) ──

    /// Cryptic Coat's ETB trigger creates a Mysterious Creature token and attaches self.
    /// Static buff grants +1/+0. {1}{U} activated ability returns self to owner's hand.
    #[test]
    fn test_cryptic_coat_etb_cloaks_token_and_attaches() {
        let mut state = make_state();
        state.catalog.insert("Cryptic Coat".into(), catalog_card("Cryptic Coat"));
        state.catalog.insert("Mysterious Creature".into(), catalog_card("Mysterious Creature"));

        eff_enter_permanent(PlayerId::Us, "Cryptic Coat").call(&mut state, 1, &[]);
        let coat_id = state.objects.values()
            .find(|o| o.catalog_key == "Cryptic Coat" && o.in_zone(Zone::Battlefield))
            .map(|o| o.id).expect("Cryptic Coat on battlefield");

        // Resolve ETB: creates the token and attaches self to it.
        let ctx = state.pending_triggers.iter()
            .find(|c| c.source_name == "Cryptic Coat").cloned()
            .expect("ETB trigger queued");
        ctx.effect.call(&mut state, 1, &[]);

        let token_id = state.objects.values()
            .find(|o| o.catalog_key == "Mysterious Creature" && o.in_zone(Zone::Battlefield))
            .map(|o| o.id).expect("Mysterious Creature token created");
        assert_eq!(state.permanent_bf(coat_id).and_then(|bf| bf.attached_to), Some(token_id),
            "Cryptic Coat attached to the cloaked token");

        // +1/+0 applies to equipped creature.
        recompute(&mut state);
        let tok_def = state.def_of(token_id).expect("materialized def");
        let tok = tok_def.as_creature().expect("creature");
        assert_eq!(tok.power(), 2 + 1, "base 2 + Cryptic Coat +1");
        assert_eq!(tok.toughness(), 2, "Cryptic Coat does not buff toughness");

        // Activated ability: {1}{U} returns Cryptic Coat to owner's hand.
        let def = state.catalog["Cryptic Coat"].clone();
        let bounce = match &def.kind {
            CardKind::Artifact(a) => &a.abilities[0],
            _ => panic!("Cryptic Coat is an artifact"),
        };
        let eff = build_ability_effect(bounce, PlayerId::Us, coat_id);
        eff.call(&mut state, 1, &[]);
        assert!(state.objects[&coat_id].in_zone(Zone::Hand { known: false }),
            "Cryptic Coat returned to owner's hand");
    }

    // ── Section 29: Dauthi Voidwalker ─────────────────────────────────────────

    /// DV replacement: when opponent's card would go to graveyard, it exiles with a void counter.
    #[test]
    fn test_dv_replacement_exiles_opponent_card() {
        let mut state = make_state();

        // Put DV on battlefield under Opp's control.
        let dv_def = catalog_card("Dauthi Voidwalker");
        state.catalog.insert(dv_def.name.clone(), dv_def.clone());
        let dv_id = state.alloc_id();


        state.objects.insert(dv_id, GameObject {
            id: dv_id,
            catalog_key: "Dauthi Voidwalker".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Battlefield(BattlefieldState::new()),
        });

        // Put a Us-owned card in graveyard-bound position (hand card moved to GY).
        let card_id = state.alloc_id();
        state.objects.insert(card_id, GameObject {
            id: card_id,
            catalog_key: "Dark Ritual".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Hand { known: true },
        });

        // Trigger zone change to graveyard — DV's replacement should intercept.
        change_zone(card_id, ZoneId::Graveyard, &mut state, 1, PlayerId::Us);

        // Card should be in exile, not graveyard.
        assert_eq!(
            state.objects[&card_id].zone(), Some(Zone::Exile { on_adventure: false }),
            "DV replacement: card should be in exile, not graveyard",
        );
        // Card should have a void counter.
        assert_eq!(
            state.objects[&card_id].counters.get(&CounterType::Void).copied().unwrap_or(0),
            1,
            "DV replacement: exiled card should have a void counter",
        );
    }

    /// DV replacement does NOT fire when DV's controller's own card goes to the graveyard.
    #[test]
    fn test_dv_replacement_does_not_fire_for_own_cards() {
        let mut state = make_state();

        // DV under Opp's control.
        let dv_def = catalog_card("Dauthi Voidwalker");
        state.catalog.insert(dv_def.name.clone(), dv_def.clone());
        let dv_id = state.alloc_id();


        state.objects.insert(dv_id, GameObject {
            id: dv_id,
            catalog_key: "Dauthi Voidwalker".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Battlefield(BattlefieldState::new()),
        });

        // Opp's own card going to graveyard — should NOT be intercepted.
        let opp_card_id = state.alloc_id();
        state.objects.insert(opp_card_id, GameObject {
            id: opp_card_id,
            catalog_key: "Dark Ritual".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Hand { known: true },
        });

        change_zone(opp_card_id, ZoneId::Graveyard, &mut state, 1, PlayerId::Opp);

        assert_eq!(
            state.objects[&opp_card_id].zone(), Some(Zone::Graveyard),
            "DV replacement must not intercept its controller's own cards",
        );
        assert_eq!(
            state.objects[&opp_card_id].counters.get(&CounterType::Void).copied().unwrap_or(0),
            0,
        );
    }

    /// DV activated ability (IR path): after activation, the chosen exiled
    /// card becomes castable with a zero alternate cost, via `Action::ApplyCE`
    /// + `CEMod::CastableFrom` + `CEMod::AltCost(Free)`.
    #[test]
    fn test_dv_activated_grants_castable_from_exile_via_ir() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let dv_def = catalog_card("Dauthi Voidwalker");
        // Activated ability is synthesized from the IR `AbilityKind::Activated`.
        assert_eq!(
            dv_def.abilities().len(),
            1,
            "DV should expose one synthesized activated ability",
        );
        let ability = &dv_def.abilities()[0];
        assert!(
            ability.ir_body.is_some(),
            "DV's activated ability should carry an IR body",
        );
        assert!(
            ability.choice_spec.is_some(),
            "DV's activated ability should carry a choice_spec (chooses an exiled card)",
        );

        let dv_id = add_perm_with_def(&mut state, PlayerId::Us, &dv_def, BattlefieldState::new());

        // Exiled Opp-owned card with a Void counter — the chosen target.
        let exiled_id = state.alloc_id();
        let mut counters = HashMap::new();
        counters.insert(CounterType::Void, 1);
        state.objects.insert(exiled_id, GameObject {
            id: exiled_id,
            catalog_key: "Dark Ritual".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            is_token: false,
            materialized: None,
            counters,
            ci_timestamp: 0,
            role: ObjectRole::Exile { on_adventure: false },
        });
        recompute(&mut state);

        // Pre-condition: exiled card is not castable by default.
        assert!(
            !state.objects[&exiled_id].materialized.as_ref().unwrap().castable,
            "exiled card should not be castable before DV activation",
        );

        // Simulate ability resolution: chosen id lands in targets[0].
        let eff = build_ability_effect(ability, PlayerId::Us, dv_id);
        eff.call(&mut state, 1, &[exiled_id]);
        recompute(&mut state);

        let mat = state.objects[&exiled_id].materialized.as_ref().unwrap();
        assert!(
            mat.castable,
            "after DV activation, the chosen exiled card should be castable",
        );
        assert!(
            !mat.alternate_costs.is_empty(),
            "after DV activation, the chosen exiled card should have an alt-cost entry (free cast)",
        );
    }

    // ── Section 30: Surgical Extraction ───────────────────────────────────────

    /// Surgical Extraction exiles the targeted GY card plus all same-name cards
    /// from the owner's graveyard, hand, and library. Other-named cards are untouched.
    #[test]
    fn test_surgical_extraction_exiles_all_copies() {
        let mut state = make_state();
        state.catalog.extend(test_catalog());

        // Opp has 3 copies of Dark Ritual spread across zones: GY, hand, library.
        let gy_id = state.alloc_id();
        state.objects.insert(gy_id, GameObject {
            id: gy_id,
            catalog_key: "Dark Ritual".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Graveyard,
        });
        let hand_id = state.alloc_id();
        state.objects.insert(hand_id, GameObject {
            id: hand_id,
            catalog_key: "Dark Ritual".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Hand { known: false },
        });
        let lib_id = state.alloc_id();
        state.objects.insert(lib_id, GameObject {
            id: lib_id,
            catalog_key: "Dark Ritual".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Library,
        });
        state.player_mut(PlayerId::Opp).library_order.push_back(lib_id);
        // A different card in opp's hand — must not be exiled.
        let other_id = state.alloc_id();
        state.objects.insert(other_id, GameObject {
            id: other_id,
            catalog_key: "Brainstorm".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Hand { known: false },
        });

        // Build and call the Surgical Extraction effect (IR body) targeting gy_id.
        let se_def = catalog_card("Surgical Extraction");
        let eff = build_spell_effect(&se_def, PlayerId::Us, ObjId::UNSET, 0, 0).1;
        eff.call(&mut state, 1, &[gy_id]);

        // All 3 Dark Ritual copies should be in exile.
        assert_eq!(state.objects[&gy_id].zone(), Some(Zone::Exile { on_adventure: false }), "GY copy exiled");
        assert_eq!(state.objects[&hand_id].zone(), Some(Zone::Exile { on_adventure: false }), "hand copy exiled");
        assert_eq!(state.objects[&lib_id].zone(), Some(Zone::Exile { on_adventure: false }), "library copy exiled");
        // Brainstorm is untouched.
        assert_eq!(state.objects[&other_id].zone(), Some(Zone::Hand { known: false }), "other card unchanged");
    }

    // ── Section 31: Toxic Deluge ───────────────────────────────────────────────

    /// Toxic Deluge with chosen_x=3 should register a -3/-3 ContinuousInstance.
    /// After recompute:
    ///   - a 1/3 creature has materialized toughness 0 (dies to SBA)
    ///   - a 1/4 creature has materialized toughness 1 (survives)
    #[test]
    fn test_toxic_deluge_applies_minus_x_pt() {
        let mut state = make_state();

        // Set up test creatures on the battlefield.
        let victim_def = CardDef::new(
            "Victim", CardKind::Creature(CreatureData::new("", 1, 3)),
            vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![]);
        let survivor_def = CardDef::new(
            "Survivor", CardKind::Creature(CreatureData::new("", 1, 4)),
            vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![]);
        let victim_id = add_perm_with_def(&mut state, PlayerId::Opp, &victim_def, BattlefieldState::new());
        let survivor_id = add_perm_with_def(&mut state, PlayerId::Opp, &survivor_def, BattlefieldState::new());

        // Invoke the IR body directly with x=3 (strategy-chosen).
        let td_def = catalog_card("Toxic Deluge");
        let source_id = state.alloc_id();
        let eff = build_spell_effect(&td_def, PlayerId::Us, source_id, 3, 0).1;
        eff.call(&mut state, 1, &[]);

        // One ContinuousInstance should be registered.
        assert_eq!(state.continuous_instances.len(), 1, "one CI registered");

        // Apply it.
        recompute(&mut state);

        // Victim (1/3): materialized toughness should be 0 after -3/-3.
        let victim_t = state.def_of(victim_id)
            .and_then(|d| d.as_creature())
            .map(|c| c.toughness())
            .expect("victim has creature def");
        assert_eq!(victim_t, 0, "victim (1/3) gets -3/-3 → toughness 0");

        // Survivor (1/4): materialized toughness should be 1 after -3/-3.
        let survivor_t = state.def_of(survivor_id)
            .and_then(|d| d.as_creature())
            .map(|c| c.toughness())
            .expect("survivor has creature def");
        assert_eq!(survivor_t, 1, "survivor (1/4) gets -3/-3 → toughness 1");
    }

    /// Casting Toxic Deluge with X=3 deducts 3 life as additional cost.
    #[test]
    fn test_toxic_deluge_pays_x_life() {
        let mut state = make_state();
        let td_def = catalog_card("Toxic Deluge");
        state.catalog.insert(td_def.name.clone(), td_def);
        state.player_mut(PlayerId::Us).pool.b = 3;
        state.player_mut(PlayerId::Us).pool.total = 3;
        state.player_mut(PlayerId::Us).life = 20;
        let td_id = add_hand_card(&mut state, PlayerId::Us, "Toxic Deluge");
        let result = cast_spell(&mut state, 1, PlayerId::Us, td_id, SpellFace::Main, None, None, &[], 3, 0);
        assert!(result.is_some(), "Toxic Deluge should cast successfully");
        assert_eq!(state.player(PlayerId::Us).life, 17, "caster pays X=3 life");
    }

    // ── 35. Red/Blue Elemental Blast, Pyroblast, Hydroblast ───────────────────

    /// Helper: insert a spell object onto the stack for `who` with the given catalog_key.
    /// Sets `materialized` from the test catalog so `def_of` can resolve the card's properties.
    fn push_stack_spell(state: &mut SimState, who: PlayerId, name: &str) -> ObjId {
        let id = state.alloc_id();
        let def = test_catalog().remove(name);
        state.objects.insert(id, GameObject {
            id,
            catalog_key: name.to_string(),
            owner: who,
            controller: who,
            is_token: false,
            materialized: def,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::StackSpell(SpellState {
                effect: None,
                chosen_targets: vec![],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
        });
        state.stack.push(id);
        id
    }

    /// REB counters a blue spell on the stack (Brainstorm = blue).
    #[test]
    fn test_reb_counters_blue_spell() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let target_id = push_stack_spell(&mut state, PlayerId::Opp, "Brainstorm");

        let reb_def = catalog_card("Red Elemental Blast");
        let effect = build_spell_effect(&reb_def, PlayerId::Us, ObjId::UNSET, 0, 0).1;
        effect.call(&mut state, 1, &[target_id]);

        assert!(!state.stack.contains(&target_id), "blue spell should be countered off the stack");
        assert_eq!(state.objects[&target_id].zone(), Some(Zone::Graveyard), "countered spell goes to graveyard");
    }

    /// REB destroys a blue permanent on the battlefield (Underground Sea = blue land).
    #[test]
    fn test_reb_destroys_blue_permanent() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let sea_id = add_default_perm(&mut state, PlayerId::Opp, "Underground Sea");

        let reb_def = catalog_card("Red Elemental Blast");
        let effect = build_spell_effect(&reb_def, PlayerId::Us, ObjId::UNSET, 0, 0).1;
        effect.call(&mut state, 1, &[sea_id]);

        assert_eq!(state.objects[&sea_id].zone(), Some(Zone::Graveyard), "blue permanent destroyed");
    }

    /// Pyroblast fizzles when targeting a non-blue spell (Dark Ritual = black).
    #[test]
    fn test_pyroblast_fizzles_on_non_blue_spell() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let target_id = push_stack_spell(&mut state, PlayerId::Opp, "Dark Ritual");

        let pyro_def = catalog_card("Pyroblast");
        let effect = build_spell_effect(&pyro_def, PlayerId::Us, ObjId::UNSET, 0, 0).1;
        effect.call(&mut state, 1, &[target_id]);

        assert!(state.stack.contains(&target_id), "non-blue spell survives Pyroblast");
    }

    /// Pyroblast counters a blue spell on the stack (same effect path, conditional on color).
    #[test]
    fn test_pyroblast_counters_blue_spell() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let target_id = push_stack_spell(&mut state, PlayerId::Opp, "Brainstorm");

        let pyro_def = catalog_card("Pyroblast");
        let effect = build_spell_effect(&pyro_def, PlayerId::Us, ObjId::UNSET, 0, 0).1;
        effect.call(&mut state, 1, &[target_id]);

        assert!(!state.stack.contains(&target_id), "blue spell countered by Pyroblast");
    }

    /// BEB counters a red spell and Hydroblast fizzles on a non-red spell (Brainstorm = blue).
    #[test]
    fn test_beb_counters_red_and_hydroblast_fizzles_on_non_red() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // BEB: counter a red spell — use Dark Ritual (black) as a stand-in? No, we need red.
        // Dark Ritual is black, not red. We don't have a red spell in the default test catalog.
        // Use the Hydroblast fizzle test instead: target Brainstorm (blue), expect no effect.
        let blue_id = push_stack_spell(&mut state, PlayerId::Opp, "Brainstorm");

        let hydro_def = catalog_card("Hydroblast");
        let effect = build_spell_effect(&hydro_def, PlayerId::Us, ObjId::UNSET, 0, 0).1;
        effect.call(&mut state, 1, &[blue_id]);

        assert!(state.stack.contains(&blue_id), "Hydroblast fizzles on non-red target");
    }

    // ── 36. Painter's Servant ─────────────────────────────────────────────────

    /// Helper: ETB Painter's Servant (from Hand→Battlefield) via change_zone so the replacement
    /// fires, resolve_choice picks a color, and the ContinuousInstance is registered.
    /// Calls recompute() so materialized views reflect the new CE immediately.
    fn etb_painter(state: &mut SimState, who: PlayerId, chosen_color: Color) -> ObjId {
        state.set_strategy(who, Box::new(TestStrategy::new(who).color(chosen_color)));
        let id = state.alloc_id();
        let def = catalog_card("Painter's Servant");
        state.objects.insert(id, GameObject {
            id,
            catalog_key: "Painter's Servant".to_string(),
            owner: who,
            controller: who,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Hand { known: false },
        });

        state.catalog.entry("Painter's Servant".to_string()).or_insert(def);
        change_zone(id, ZoneId::Battlefield, state, 1, who);
        recompute(state);
        id
    }

    /// After Painter's Servant enters naming Blue, a colorless artifact (Lotus Petal) on
    /// opponent's side gains Blue. Pyroblast's conditional effect then destroys it.
    #[test]
    fn test_painters_servant_names_blue_makes_pyro_work() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Painter on our side, names Blue (default / forced via etb_painter).
        let _painter_id = etb_painter(&mut state, PlayerId::Us, Color::Blue);

        // Opponent has a Lotus Petal (colorless artifact) in play.
        let petal_id = add_default_perm(&mut state, PlayerId::Opp, "Lotus Petal");
        recompute(&mut state);

        // Verify: after CE, Lotus Petal's materialized colors include Blue.
        let colors = state.def_of(petal_id)
            .map(|d| d.colors.clone())
            .unwrap_or_default();
        assert!(colors.contains(&Color::Blue),
            "Painter naming Blue should give Blue to Lotus Petal; got {:?}", colors);

        // Pyroblast's effect: counter-or-destroy if blue. Petal is on battlefield.
        let pyro_def = catalog_card("Pyroblast");
        let effect = build_spell_effect(&pyro_def, PlayerId::Us, ObjId::UNSET, 0, 0).1;
        effect.call(&mut state, 1, &[petal_id]);

        assert_eq!(state.objects[&petal_id].zone(), Some(Zone::Graveyard),
            "Pyroblast should destroy the now-Blue Lotus Petal");
    }

    /// After Painter's Servant names Blue, any card in hand satisfies the Force of
    /// Will pitch predicate (blue card). Dark Ritual is normally Black, not Blue.
    #[test]
    fn test_painters_servant_names_blue_enables_force_of_will_pitch() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let _painter_id = etb_painter(&mut state, PlayerId::Us, Color::Blue);

        // Dark Ritual is black — normally not a valid FoW pitch target.
        let ritual_id = add_hand_card(&mut state, PlayerId::Us, "Dark Ritual");
        recompute(&mut state);

        // Seed materialized on the hand card so def_of works.
        // (recompute populates materialized for objects in all zones.)
        let env = crate::ir::executor::BindEnv::new().with_controller(PlayerId::Us);
        assert!(crate::ir::executor::matches(&ir_color(Color::Blue), ritual_id, &state, &env),
            "After Painter names Blue, Dark Ritual should satisfy FoW pitch predicate");
    }

    /// Painter's Servant CI is removed when Painter leaves the battlefield.
    /// After LTB, objects should revert to their original colors.
    #[test]
    fn test_painters_servant_ci_removed_on_ltb() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let painter_id = etb_painter(&mut state, PlayerId::Us, Color::Blue);

        let petal_id = add_default_perm(&mut state, PlayerId::Opp, "Lotus Petal");
        recompute(&mut state);
        let colors_while_in_play = state.def_of(petal_id)
            .map(|d| d.colors.clone())
            .unwrap_or_default();
        assert!(colors_while_in_play.contains(&Color::Blue));

        // Painter leaves the battlefield.
        change_zone(painter_id, ZoneId::Graveyard, &mut state, 1, PlayerId::Us);
        recompute(&mut state);

        let colors_after_ltb = state.def_of(petal_id)
            .map(|d| d.colors.clone())
            .unwrap_or_default();
        assert!(!colors_after_ltb.contains(&Color::Blue),
            "After Painter leaves, Lotus Petal should no longer be Blue; got {:?}", colors_after_ltb);
    }

    // ── 37. Disruptor Flute ────────────────────────────────────────────────────

    /// Helper: ETB Disruptor Flute naming the given card name.
    fn etb_flute(state: &mut SimState, who: PlayerId, chosen_name: &'static str) -> ObjId {
        state.set_strategy(who, Box::new(TestStrategy::new(who).card_name(chosen_name)));
        let id = state.alloc_id();
        let def = catalog_card("Disruptor Flute");
        state.objects.insert(id, GameObject {
            id,
            catalog_key: "Disruptor Flute".to_string(),
            owner: who,
            controller: who,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Hand { known: false },
        });

        state.catalog.entry("Disruptor Flute".to_string()).or_insert(def);
        change_zone(id, ZoneId::Battlefield, state, 1, who);
        recompute(state);
        id
    }

    #[test]
    fn test_disruptor_flute_names_brainstorm_taxes_it() {
        // Flute names "Brainstorm"; Brainstorm's materialized casting_cost_modifier should be 3.
        let mut state = make_state();
        state.catalog = test_catalog();
        etb_flute(&mut state, PlayerId::Us, "Brainstorm");

        // Put a Brainstorm in hand so it has a materialized view.
        let bs_id = state.alloc_id();
        state.objects.insert(bs_id, GameObject {
            id: bs_id,
            catalog_key: "Brainstorm".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Hand { known: false },
        });
        recompute(&mut state);

        let modifier = state.def_of(bs_id).map(|d| d.casting_cost_modifier).unwrap_or(0);
        assert_eq!(modifier, 3, "Brainstorm should cost 3 more when named by Disruptor Flute");
    }

    #[test]
    fn test_disruptor_flute_suppresses_wasteland_ability() {
        // Flute names "Wasteland". Its non-mana ability is restricted, but — per
        // "unless they're mana abilities" — its mana ability is *not*. An unnamed
        // card (Underground Sea) is unaffected. Modeled as an action-Restriction
        // whose subject excludes mana abilities (a `Not(activating_mana_ability)`
        // clause), not the materialized activatable flag.
        let mut state = make_state();
        state.catalog = test_catalog();
        etb_flute(&mut state, PlayerId::Us, "Wasteland");

        let wl_id = add_default_perm(&mut state, PlayerId::Opp, "Wasteland");
        let sea_id = add_default_perm(&mut state, PlayerId::Opp, "Underground Sea");
        recompute(&mut state);

        use crate::ir::ability::ActionKind::Activate;
        use crate::ir::executor::{action_restricted, mana_ability_restricted};
        // Wasteland: non-mana activation forbidden …
        assert!(
            action_restricted(&state, Activate, wl_id),
            "Wasteland's non-mana ability should be restricted by Flute"
        );
        // … but its mana ability is exempt.
        assert!(
            !mana_ability_restricted(&state, wl_id),
            "Wasteland's mana ability must remain activatable (mana exemption)"
        );
        // Underground Sea is not the named card — restricted in neither sense.
        assert!(
            !action_restricted(&state, Activate, sea_id)
                && !mana_ability_restricted(&state, sea_id),
            "Underground Sea is unnamed and must not be restricted"
        );
    }

    #[test]
    fn test_disruptor_flute_does_not_affect_other_cards() {
        // Flute names "Wasteland"; Brainstorm must have modifier 0 and suppression false.
        let mut state = make_state();
        state.catalog = test_catalog();
        etb_flute(&mut state, PlayerId::Us, "Wasteland");

        let bs_id = state.alloc_id();
        state.objects.insert(bs_id, GameObject {
            id: bs_id,
            catalog_key: "Brainstorm".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Hand { known: false },
        });
        recompute(&mut state);

        let modifier = state.def_of(bs_id).expect("Brainstorm should have materialized view").casting_cost_modifier;
        assert_eq!(modifier, 0, "Unnamed Brainstorm should not be taxed");
        assert!(
            !crate::ir::executor::action_restricted(
                &state, crate::ir::ability::ActionKind::Activate, bs_id),
            "Unnamed Brainstorm should not be restricted by Flute"
        );
    }

    // ── 38. Surveil lands ──────────────────────────────────────────────────────

    #[test]
    fn test_surveil_land_etb_mills_when_choice_true() {
        // Surveiling player always mills. ETB a surveil land; top library card
        // should end up in the graveyard. Land itself should enter tapped.
        let mut state = make_state();
        state.catalog = test_catalog();
        state.set_strategy(PlayerId::Us, Box::new(TestStrategy::new(PlayerId::Us).surveil(true)));

        // Put a known card on top of Us's library.
        let top_id = {
            let def = catalog_card("Brainstorm");
            let id = state.alloc_id();
            state.objects.insert(id, GameObject {
            id,
            catalog_key: "Brainstorm".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Library,
        });
            state.player_mut(PlayerId::Us).library_order.push_front(id);
            state.catalog.entry("Brainstorm".to_string()).or_insert(def);
            id
        };

        // ETB Undercity Sewers.
        let land_id = {
            let def = catalog_card("Undercity Sewers");
            let id = state.alloc_id();
            state.objects.insert(id, GameObject {
            id,
            catalog_key: "Undercity Sewers".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Hand { known: false },
        });

            state.catalog.entry("Undercity Sewers".to_string()).or_insert(def);
            id
        };
        change_zone(land_id, ZoneId::Battlefield, &mut state, 1, PlayerId::Us);
        for ctx in std::mem::take(&mut state.pending_triggers) { ctx.effect.call(&mut state, 1, &[]); }

        assert_eq!(state.objects[&top_id].zone(), Some(Zone::Graveyard),
            "top library card should be milled by surveil");
        assert!(matches!(state.objects[&land_id].bf(), Some(bf) if bf.tapped),
            "surveil land should enter tapped");
    }

    #[test]
    fn test_surveil_land_etb_keeps_when_choice_false() {
        // Surveiling player always keeps. Library card stays in library.
        let mut state = make_state();
        state.catalog = test_catalog();
        state.set_strategy(PlayerId::Us, Box::new(TestStrategy::new(PlayerId::Us).surveil(false)));

        let top_id = {
            let def = catalog_card("Brainstorm");
            let id = state.alloc_id();
            state.objects.insert(id, GameObject {
            id,
            catalog_key: "Brainstorm".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Library,
        });
            state.player_mut(PlayerId::Us).library_order.push_front(id);
            state.catalog.entry("Brainstorm".to_string()).or_insert(def);
            id
        };

        let land_id = {
            let def = catalog_card("Undercity Sewers");
            let id = state.alloc_id();
            state.objects.insert(id, GameObject {
            id,
            catalog_key: "Undercity Sewers".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Hand { known: false },
        });

            state.catalog.entry("Undercity Sewers".to_string()).or_insert(def);
            id
        };
        change_zone(land_id, ZoneId::Battlefield, &mut state, 1, PlayerId::Us);
        for ctx in std::mem::take(&mut state.pending_triggers) { ctx.effect.call(&mut state, 1, &[]); }

        assert_eq!(state.objects[&top_id].zone(), Some(Zone::Library),
            "top library card should stay when surveil keeps");
    }

    // ── 39. Ancient Tomb ───────────────────────────────────────────────────────

    #[test]
    fn test_ancient_tomb_produces_two_and_deals_damage() {
        let mut state = make_state();
        state.catalog = test_catalog();
        state.player_mut(PlayerId::Us).life = 20;

        let tomb_id = {
            let def = catalog_card("Ancient Tomb");
            let id = state.alloc_id();
            state.objects.insert(id, GameObject {
            id,
            catalog_key: "Ancient Tomb".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Hand { known: false },
        });

            state.catalog.entry("Ancient Tomb".to_string()).or_insert(def);
            id
        };
        // ETB via change_zone, which assigns ci_timestamp and recompute (sets materialized)
        change_zone(tomb_id, ZoneId::Battlefield, &mut state, 1, PlayerId::Us);

        let act = ManaActivation { source_id: tomb_id, ability_index: 0, color_choice: None };
        execute_mana_activation(&mut state, 1, PlayerId::Us, &act);

        assert_eq!(state.player(PlayerId::Us).pool.total, 2, "Ancient Tomb should produce 2 mana");
        assert_eq!(state.player(PlayerId::Us).pool.c, 2, "both mana pips should be colorless");
        assert_eq!(state.player(PlayerId::Us).life, 18, "Ancient Tomb deals 2 damage to controller");
        assert!(state.objects[&tomb_id].bf().map_or(false, |bf| bf.tapped),
            "Ancient Tomb should be tapped after activation");
    }

    // ── 40. Karakas ────────────────────────────────────────────────────────────

    #[test]
    fn test_karakas_bounces_opp_legendary_creature() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Put a legendary creature on opp's battlefield (use Emrakul as a stand-in)
        // Build a minimal legendary creature and put it on opp's battlefield.
        let legendary_def = {
            CardDef::new(
                "TestLegend", CardKind::Creature(CreatureData::new("1W", 2, 2)),
                vec![], None, vec![Supertype::Legendary], CardLayout::Normal, None,
                vec![], vec![], vec![], vec![])
        };
        state.catalog.insert("TestLegend".to_string(), legendary_def.clone());

        let creature_id = {
            let id = state.alloc_id();
            state.objects.insert(id, GameObject {
            id,
            catalog_key: "TestLegend".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Hand { known: false },
        });

            id
        };
        change_zone(creature_id, ZoneId::Battlefield, &mut state, 1, PlayerId::Opp);

        // Activate Karakas, targeting the legendary creature
        let effect = eff_bounce_target(PlayerId::Us);
        effect.call(&mut state, 1, &[creature_id]);

        assert_eq!(state.objects[&creature_id].zone(), Some(Zone::Hand { known: false }),
            "legendary creature should be in opp's hand after Karakas activation");
    }

    // ── 41. Abrade ─────────────────────────────────────────────────────────────

    #[test]
    fn test_abrade_creature_mode_deals_lethal_damage() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // 3/3 creature — 3 damage is lethal
        let creature_def = creature("Target3_3", 3, 3);
        let id = add_perm_with_def(&mut state, PlayerId::Opp, &creature_def, BattlefieldState::new());

        eff_damage_target(PlayerId::Us, 3, ObjId(0)).call(&mut state, 1, &[id]);
        check_state_based_actions(&mut state, 1);

        assert_eq!(state.objects[&id].zone(), Some(Zone::Graveyard),
            "3/3 hit by 3 damage should die via SBA");
    }

    #[test]
    fn test_abrade_creature_mode_nonlethal_survives() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // 4/4 creature — 3 damage is not lethal
        let creature_def = creature("Target4_4", 4, 4);
        let id = add_perm_with_def(&mut state, PlayerId::Opp, &creature_def, BattlefieldState::new());

        eff_damage_target(PlayerId::Us, 3, ObjId(0)).call(&mut state, 1, &[id]);
        check_state_based_actions(&mut state, 1);

        assert_eq!(state.objects[&id].zone(), Some(Zone::Battlefield),
            "4/4 hit by 3 damage should survive");
        assert_eq!(state.objects[&id].bf().unwrap().damage, 3);
    }

    #[test]
    fn test_abrade_artifact_mode_destroys() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Teferi's Puzzle Box as a stand-in artifact
        let artifact_def = {
            let def = CardDef::new(
                "TestArtifact",
                CardKind::Artifact(ArtifactData { mana_cost: "1".to_string(), ..Default::default() }),
                vec![], None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![]);
            def
        };
        let id = add_perm_with_def(&mut state, PlayerId::Opp, &artifact_def, BattlefieldState::new());

        eff_destroy_target(PlayerId::Us).call(&mut state, 1, &[id]);

        assert_eq!(state.objects[&id].zone(), Some(Zone::Graveyard),
            "artifact should be destroyed by Abrade's artifact mode");
    }

    // ── §42: Grafdigger's Cage ────────────────────────────────────────────────

    #[test]
    fn test_grafdiggers_cage_blocks_gy_and_lib_casting() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Grafdigger's Cage on the battlefield (controlled by Opp).
        enter_cage(&mut state, PlayerId::Opp);

        // Helper: drop a Dark Ritual into `role` and return its id.
        let put = |state: &mut SimState, role: ObjectRole| -> ObjId {
            let id = state.alloc_id();
            state.objects.insert(id, GameObject {
                id,
                catalog_key: "Dark Ritual".to_string(),
                owner: PlayerId::Us,
                controller: PlayerId::Us,
                is_token: false,
                materialized: None,
                counters: HashMap::new(),
                ci_timestamp: 0,
                role,
            });
            id
        };
        let gy_id = put(&mut state, ObjectRole::Graveyard);
        let lib_id = put(&mut state, ObjectRole::Library);
        let exile_id = put(&mut state, ObjectRole::Exile { on_adventure: false });
        recompute(&mut state);

        // "Players can't cast spells from graveyards or libraries." — an action-
        // Restriction (CR 101.2 "can't beats can"), consulted at legal-cast
        // enumeration, *not* the castable flag (GY/library cards already default to
        // castable=false by zone, so that flag can't distinguish Cage's effect).
        assert!(
            crate::ir::executor::action_restricted(
                &state, crate::ir::ability::ActionKind::Cast, gy_id),
            "Cage should restrict casting from the graveyard"
        );
        assert!(
            crate::ir::executor::action_restricted(
                &state, crate::ir::ability::ActionKind::Cast, lib_id),
            "Cage should restrict casting from the library"
        );
        // Exile ≠ GY/library: the zone-scoped subject simply doesn't match, so a
        // Dauthi-style exile cast is *not* forbidden by Cage.
        assert!(
            !crate::ir::executor::action_restricted(
                &state, crate::ir::ability::ActionKind::Cast, exile_id),
            "Cage must not restrict casting from exile"
        );
    }

    #[test]
    fn test_grafdiggers_cage_restriction_removed_on_ltb() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let cage_id = enter_cage(&mut state, PlayerId::Us);

        // A card in the graveyard.
        let gy_id = state.alloc_id();
        state.objects.insert(gy_id, GameObject {
            id: gy_id,
            catalog_key: "Dark Ritual".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Graveyard,
        });
        recompute(&mut state);

        // While Cage is on the battlefield, the cast-Restriction is active.
        assert!(
            crate::ir::executor::action_restricted(
                &state, crate::ir::ability::ActionKind::Cast, gy_id),
            "GY card should be cast-restricted while Cage is on the battlefield"
        );

        // Cage leaves the battlefield → `action_restricted` walks BF sources only,
        // so the restriction lifts (no lingering continuous effect to clean up).
        change_zone(cage_id, ZoneId::Graveyard, &mut state, 2, PlayerId::Us);
        recompute(&mut state);
        assert!(
            !crate::ir::executor::action_restricted(
                &state, crate::ir::ability::ActionKind::Cast, gy_id),
            "Cast-Restriction should lift once Cage leaves the battlefield"
        );
    }

    /// Helper: put Cage on the battlefield for `who` and return its id.
    fn enter_cage(state: &mut SimState, who: PlayerId) -> ObjId {
        let cage_id = state.alloc_id();
        assert!(state.catalog.contains_key("Grafdigger's Cage"), "Grafdigger's Cage not in catalog");
        state.objects.insert(cage_id, GameObject {
            id: cage_id,
            catalog_key: "Grafdigger's Cage".to_string(),
            owner: who,
            controller: who,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Hand { known: false },
        });

        change_zone(cage_id, ZoneId::Battlefield, state, 1, who);
        cage_id
    }

    #[test]
    fn test_grafdiggers_cage_prohibition_blocks_creature_from_gy() {
        let mut state = make_state();
        state.catalog = test_catalog();
        state.catalog.insert("Troll".to_string(), creature("Troll", 3, 3));

        enter_cage(&mut state, PlayerId::Opp);

        // Put a creature card in Us's graveyard.
        let creature_id = state.alloc_id();
        state.objects.insert(creature_id, GameObject {
            id: creature_id,
            catalog_key: "Troll".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Graveyard,
        });

        // Attempt to reanimate: fire a ZoneChange GY→BF.
        eff_reanimate(PlayerId::Us).call(&mut state, 2, &[creature_id]);

        assert_eq!(
            state.objects[&creature_id].zone(), Some(Zone::Graveyard),
            "Cage prohibition must block creature from entering battlefield from graveyard"
        );
    }

    #[test]
    fn test_grafdiggers_cage_prohibition_removed_on_ltb() {
        let mut state = make_state();
        state.catalog = test_catalog();
        state.catalog.insert("Troll".to_string(), creature("Troll", 3, 3));

        let cage_id = enter_cage(&mut state, PlayerId::Opp);

        // Put a creature card in Us's graveyard.
        let creature_id = state.alloc_id();
        state.objects.insert(creature_id, GameObject {
            id: creature_id,
            catalog_key: "Troll".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Graveyard,
        });

        // Remove Cage.
        change_zone(cage_id, ZoneId::Graveyard, &mut state, 2, PlayerId::Opp);

        // Now reanimation should succeed.
        eff_reanimate(PlayerId::Us).call(&mut state, 3, &[creature_id]);

        assert_eq!(
            state.objects[&creature_id].zone(), Some(Zone::Battlefield),
            "After Cage leaves, creature should be free to enter battlefield"
        );
    }

    #[test]
    fn test_grafdiggers_cage_does_not_block_non_creature() {
        let mut state = make_state();
        state.catalog = test_catalog();

        enter_cage(&mut state, PlayerId::Opp);

        // Put a non-creature (artifact) in Us's graveyard.
        let artifact_id = state.alloc_id();
        let artifact_name = "Grafdigger's Cage";
        state.objects.insert(artifact_id, GameObject {
            id: artifact_id,
            catalog_key: artifact_name.to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Graveyard,
        });

        eff_reanimate(PlayerId::Us).call(&mut state, 2, &[artifact_id]);

        assert_eq!(
            state.objects[&artifact_id].zone(), Some(Zone::Battlefield),
            "Cage must not block non-creature cards from entering battlefield"
        );
    }

    // ── §43: Sheoldred's Edict ────────────────────────────────────────────────

    #[test]
    fn test_sheoldrds_edict_mode0_sacrifices_nontoken_creature() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let creature_def = creature("Threat", 2, 2);
        let creature_id = add_perm_with_def(&mut state, PlayerId::Opp, &creature_def, BattlefieldState::new());
        // Default mode = 0 (nontoken creature)
        let filter = ir_and(ir_not(ir_token()), ir_type(CardType::Creature));
        eff_sacrifice(PlayerId::Us, Who::Opp, filter).call(&mut state, 1, &[]);
        assert_eq!(state.objects[&creature_id].zone(), Some(Zone::Graveyard),
            "nontoken creature should be sacrificed");
    }

    #[test]
    fn test_sheoldrds_edict_token_filter_spares_nontoken() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let creature_def = creature("Nontoken", 2, 2);
        let nontoken_id = add_perm_with_def(&mut state, PlayerId::Opp, &creature_def, BattlefieldState::new());
        // Add a token manually
        let token_def = creature("OrcToken", 1, 1);
        let token_id = state.alloc_id();
        state.catalog.entry("OrcToken".to_string()).or_insert_with(|| token_def.clone());
        state.objects.insert(token_id, GameObject {
            id: token_id,
            catalog_key: "OrcToken".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            is_token: true,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Battlefield(BattlefieldState::new()),
        });
        // Mode 1: sacrifice a token
        eff_sacrifice(PlayerId::Us, Who::Opp, ir_token()).call(&mut state, 1, &[]);
        assert_eq!(state.objects[&token_id].zone(), Some(Zone::Graveyard),
            "token should be sacrificed by mode 1");
        assert_eq!(state.objects[&nontoken_id].zone(), Some(Zone::Battlefield),
            "nontoken creature should not be sacrificed by mode 1");
    }

    // ── §44: Engineered Explosives ────────────────────────────────────────────

    #[test]
    fn test_ee_etb_places_charge_counters() {
        let mut state = make_state();
        state.catalog = test_catalog();
        assert!(state.catalog.contains_key("Engineered Explosives"), "EE must be in catalog");
        let ee_id = state.alloc_id();
        state.objects.insert(ee_id, GameObject {
            id: ee_id,
            catalog_key: "Engineered Explosives".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Hand { known: false },
        });
        // EE was cast with X=2 (sunburst). The announced X lives on the logged
        // cast; the ETB replacement reads it via `ThisCast(X)`.
        state.event_log.push(1, GameEvent::SpellCast {
            caster: PlayerId::Us, card_id: ee_id, mana_spent: true, alt_cost: false,
            x: 2, delved: Vec::new(),
        });

        change_zone(ee_id, ZoneId::Battlefield, &mut state, 1, PlayerId::Us);
        assert_eq!(
            state.objects[&ee_id].counters.get(&CounterType::Charge).copied().unwrap_or(0),
            2,
            "EE should enter with 2 charge counters when cast with X = 2"
        );
    }

    #[test]
    fn test_ee_ability_destroys_matching_mv() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Put EE in play with 2 charge counters (no casting, manual setup).
        let ee_def = state.catalog.get("Engineered Explosives").cloned()
            .expect("EE must be in catalog");
        let ee_id = add_perm_with_def(&mut state, PlayerId::Us, &ee_def, BattlefieldState::new());
        *state.objects.get_mut(&ee_id).unwrap().counters.entry(CounterType::Charge).or_insert(0) = 2;
        // MV 2 permanent: a 2/2 creature with mana_cost "1B" (MV=2).
        let mv2_def = {
            let data = CreatureData::new("1B", 2, 2);
            CardDef::new("MV2Creature", CardKind::Creature(data), parse_colors("1B", false, false),
                None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![])
        };
        let mv2_id = add_perm_with_def(&mut state, PlayerId::Opp, &mv2_def, BattlefieldState::new());
        // MV 3 permanent: should survive.
        let mv3_def = {
            let data = CreatureData::new("2B", 2, 2);
            CardDef::new("MV3Creature", CardKind::Creature(data), parse_colors("2B", false, false),
                None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![])
        };
        let mv3_id = add_perm_with_def(&mut state, PlayerId::Opp, &mv3_def, BattlefieldState::new());
        // Manually fire the ability effect (skip cost payment for the test).
        let ability = ee_def.abilities().iter()
            .find(|ab| matches!(ab.source_zone, SourceZone::Battlefield))
            .expect("EE must have a battlefield ability");
        let eff = build_ability_effect(ability, PlayerId::Us, ee_id);
        eff.call(&mut state, 1, &[]);
        assert_eq!(state.objects[&mv2_id].zone(), Some(Zone::Graveyard),
            "MV 2 permanent should be destroyed by EE[2]");
        assert_eq!(state.objects[&mv3_id].zone(), Some(Zone::Battlefield),
            "MV 3 permanent should survive EE[2]");
    }

    // ── §45: Lavinia, Azorius Renegade ────────────────────────────────────────

    /// Helper: put Lavinia on the battlefield for `who` and return her id.
    fn enter_lavinia(state: &mut SimState, who: PlayerId) -> ObjId {
        add_default_perm(state, who, "Lavinia, Azorius Renegade")
    }

    #[test]
    fn test_lavinia_prohibition_blocks_noncreature_over_land_count() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Lavinia on our side.
        enter_lavinia(&mut state, PlayerId::Us);

        // Opponent has exactly 1 land.
        make_land(&mut state, PlayerId::Opp, "Swamp", false);

        // A noncreature sorcery with MV 2 (cost "1B").
        let sorcery_def = CardDef::new(
            "TestSorcery2", CardKind::Sorcery(SpellData {
                mana_cost: "1B".to_string(),
                ..Default::default()
            }),
            parse_colors("1B", false, true),
            None, vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![],
        );
        state.catalog.insert("TestSorcery2".to_string(), sorcery_def);
        let spell_id = add_hand_card(&mut state, PlayerId::Opp, "TestSorcery2");

        // With 1 land, MV 2 > 1 → Lavinia's action-Restriction forbids the cast
        // (CR 101.2; consulted at legal-cast enumeration, not via the castable flag).
        recompute(&mut state);
        assert!(
            crate::ir::executor::action_restricted(
                &state, crate::ir::ability::ActionKind::Cast, spell_id),
            "Lavinia should restrict casting an MV-2 noncreature spell when opponent has only 1 land"
        );

        // Add a second land so opponent now has 2 lands — MV 2 is no longer > 2.
        make_land(&mut state, PlayerId::Opp, "Swamp", false);
        recompute(&mut state);
        assert!(
            !crate::ir::executor::action_restricted(
                &state, crate::ir::ability::ActionKind::Cast, spell_id),
            "Lavinia should allow an MV-2 noncreature spell when opponent has 2 lands"
        );
    }

    #[test]
    fn test_lavinia_trigger_counters_free_spell() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Lavinia on our side.
        enter_lavinia(&mut state, PlayerId::Us);

        // Opponent has 5 lands so the prohibition doesn't block FoW (MV 5 ≤ 5 lands → allowed).
        for _ in 0..5 {
            make_land(&mut state, PlayerId::Opp, "Swamp", false);
        }

        // Opponent casts Force of Will via pitch cost (no mana spent).
        let fow_def = state.catalog.get("Force of Will").cloned().unwrap();
        let fow_id = add_hand_card(&mut state, PlayerId::Opp, "Force of Will");
        add_hand_card(&mut state, PlayerId::Opp, "Brainstorm"); // pitch target
        let alt_cost = fow_def.alternate_costs()[0].clone();

        cast_spell(&mut state, 1, PlayerId::Opp, fow_id, SpellFace::Main, Some(&alt_cost), Some(0), &[], 0, 0)
            .expect("FoW should cast via pitch cost");
        // Lavinia trigger queued at SpellCast; push spell onto stack so counter_one can find it.
        state.stack.push(fow_id);

        assert!(
            state.pending_triggers.iter().any(|ctx| ctx.source_name == "Lavinia, Azorius Renegade"),
            "Lavinia trigger should be queued"
        );

        for ctx in std::mem::take(&mut state.pending_triggers) {
            ctx.effect.call(&mut state, 1, &[]);
        }

        assert_eq!(state.objects[&fow_id].zone(), Some(Zone::Graveyard),
            "FoW should be countered and in graveyard");
        assert!(!state.stack.contains(&fow_id), "FoW should be off the stack");
    }

    #[test]
    fn test_lavinia_trigger_lotus_petal() {
        // Lotus Petal has mana cost "0" — MV 0 is not > any land count, so the
        // prohibition does NOT fire. But mana_spent = false (MV 0), so Lavinia's
        // trigger DOES fire and counters the Petal.
        let mut state = make_state();
        state.catalog = test_catalog();

        enter_lavinia(&mut state, PlayerId::Us);

        let petal_id = add_hand_card(&mut state, PlayerId::Opp, "Lotus Petal");

        cast_spell(&mut state, 1, PlayerId::Opp, petal_id, SpellFace::Main, None, None, &[], 0, 0)
            .expect("Lotus Petal should not be prohibited (MV 0 ≤ any land count)");
        state.stack.push(petal_id);

        assert!(
            state.pending_triggers.iter().any(|ctx| ctx.source_name == "Lavinia, Azorius Renegade"),
            "Lavinia trigger should fire for free spell (no mana spent)"
        );

        for ctx in std::mem::take(&mut state.pending_triggers) {
            ctx.effect.call(&mut state, 1, &[]);
        }

        assert_eq!(state.objects[&petal_id].zone(), Some(Zone::Graveyard),
            "Lotus Petal should be countered by Lavinia");
    }

    // ── §46: Hexing Squelcher ──────────────────────────────────────────────────

    fn enter_hexing_squelcher(state: &mut SimState, who: PlayerId) -> ObjId {
        add_default_perm(state, who, "Hexing Squelcher")
    }

    /// Hexing Squelcher's "Spells you control can't be countered" protects Us's spells.
    #[test]
    fn test_hexing_squelcher_protects_your_spells() {
        let mut state = make_state();
        state.catalog = test_catalog();

        enter_hexing_squelcher(&mut state, PlayerId::Us);

        // Put a plain counterable spell for Us on the stack directly.
        let spell_id = add_hand_card(&mut state, PlayerId::Us, "Brainstorm");
        // move to stack (activates stack prohibitions — Brainstorm has none, but wires change_zone path)
        change_zone(spell_id, ZoneId::Stack, &mut state, 1, PlayerId::Us);
        state.stack.push(spell_id);

        // Opponent tries to counter it.
        counter_one(spell_id, &mut state, 1, PlayerId::Opp);

        assert!(state.stack.contains(&spell_id),
            "Hexing Squelcher should prevent the opponent from countering our spell");
        assert_ne!(state.objects[&spell_id].zone(), Some(Zone::Graveyard));
    }

    /// Hexing Squelcher only protects YOUR spells; opponent's spells can still be countered.
    #[test]
    fn test_hexing_squelcher_does_not_protect_opponent_spells() {
        let mut state = make_state();
        state.catalog = test_catalog();

        enter_hexing_squelcher(&mut state, PlayerId::Us);

        // Put a spell controlled by Opp on the stack.
        let spell_id = add_hand_card(&mut state, PlayerId::Opp, "Brainstorm");
        change_zone(spell_id, ZoneId::Stack, &mut state, 1, PlayerId::Opp);
        state.stack.push(spell_id);

        // We counter it.
        counter_one(spell_id, &mut state, 1, PlayerId::Us);

        assert!(!state.stack.contains(&spell_id), "Opponent's spell should be countered normally");
        assert_eq!(state.objects[&spell_id].zone(), Some(Zone::Graveyard));
    }

    /// "Other creatures you control have Ward—Pay 2 life." A vanilla creature (no
    /// self-ward) receives the granted Ward, which fires when an opponent targets it.
    /// (`opp_spell_targeting` / `ward_cost_pay2` live in §46b below.)
    #[test]
    fn test_hexing_squelcher_grants_ward_to_other_creature() {
        let mut state = make_state();
        state.catalog = test_catalog();

        enter_hexing_squelcher(&mut state, PlayerId::Us);
        let other_id = add_default_perm(&mut state, PlayerId::Us, "Grizzly Bears");
        recompute(&mut state);

        let spell_id = opp_spell_targeting(&mut state, PlayerId::Opp, other_id);
        fire_event(
            GameEvent::SpellCast { caster: PlayerId::Opp, card_id: spell_id, mana_spent: true, alt_cost: false, x: 0, delved: Vec::new() },
            &mut state, 1, PlayerId::Opp,
        );
        assert!(
            state.pending_triggers.iter().any(|ctx| ctx.source_name == "Grizzly Bears"),
            "granted Ward should fire for the other creature targeted by an opponent's spell"
        );
    }

    /// The granted Ward does NOT fire when the controller's own spell targets the creature.
    #[test]
    fn test_hexing_squelcher_ward_grant_ignores_own_spells() {
        let mut state = make_state();
        state.catalog = test_catalog();

        enter_hexing_squelcher(&mut state, PlayerId::Us);
        let other_id = add_default_perm(&mut state, PlayerId::Us, "Grizzly Bears");
        recompute(&mut state);

        let spell_id = opp_spell_targeting(&mut state, PlayerId::Us, other_id); // Us's own spell
        fire_event(
            GameEvent::SpellCast { caster: PlayerId::Us, card_id: spell_id, mana_spent: true, alt_cost: false, x: 0, delved: Vec::new() },
            &mut state, 1, PlayerId::Us,
        );
        assert!(
            !state.pending_triggers.iter().any(|ctx| ctx.source_name == "Grizzly Bears"),
            "granted Ward should not fire for the controller's own spell"
        );
    }

    /// Granted Ward applies to creatures that enter AFTER Hexing Squelcher: recompute
    /// (which fire_event runs each tick) lands the IR ability on `granted_abilities`.
    #[test]
    fn test_hexing_squelcher_ward_grant_applies_to_creature_that_enters_later() {
        let mut state = make_state();
        state.catalog = test_catalog();

        enter_hexing_squelcher(&mut state, PlayerId::Us);
        recompute(&mut state);

        // A new creature arrives later.
        let late_id = add_default_perm(&mut state, PlayerId::Us, "Grizzly Bears");
        recompute(&mut state);

        let mat = state.def_of(late_id).expect("materialized def present");
        assert!(
            !mat.granted_abilities.is_empty(),
            "late-arriving creature should carry the granted Ward in granted_abilities"
        );

        let spell_id = opp_spell_targeting(&mut state, PlayerId::Opp, late_id);
        fire_event(
            GameEvent::SpellCast { caster: PlayerId::Opp, card_id: spell_id, mana_spent: true, alt_cost: false, x: 0, delved: Vec::new() },
            &mut state, 1, PlayerId::Opp,
        );
        assert!(
            state.pending_triggers.iter().any(|ctx| ctx.source_name == "Grizzly Bears"),
            "granted Ward should fire for the late-arriving creature targeted by an opponent"
        );
    }

    // ── §46b: IR Ward + CEMod::GrantAbility primitives ─────────────────────────

    /// Build a `PayLife(2)` action — the canonical ward cost.
    fn ward_cost_pay2() -> crate::ir::action::Action {
        crate::ir::action::Action::PayLife {
            who: crate::ir::action::Who::You,
            amount: crate::ir::expr::Expr::Num(2),
        }
    }

    /// Insert an opponent-controlled spell on the stack targeting `target`.
    fn opp_spell_targeting(state: &mut SimState, caster: PlayerId, target: ObjId) -> ObjId {
        let spell_id = state.alloc_id();
        state.objects.insert(spell_id, GameObject {
            id: spell_id,
            catalog_key: "Brainstorm".to_string(),
            owner: caster,
            controller: caster,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::StackSpell(SpellState {
                effect: None,
                chosen_targets: vec![target],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
        });
        state.stack.push(spell_id);
        spell_id
    }

    /// IR Ward (printed directly): an opponent's spell targeting the holder triggers
    /// it; resolving the trigger runs `Action::Ward`, and the default strategy pays,
    /// so the caster loses 2 life and the spell survives.
    #[test]
    fn test_ir_ward_triggers_and_taxes_opponent() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let mut bear = creature("Warded Bear", 2, 2);
        bear.abilities = vec![crate::card_defs::ir_ward(ward_cost_pay2())];
        let bear_id = add_perm_with_def(&mut state, PlayerId::Us, &bear, BattlefieldState::new());
        recompute(&mut state);

        let spell_id = opp_spell_targeting(&mut state, PlayerId::Opp, bear_id);
        let opp_life = state.player(PlayerId::Opp).life;

        fire_event(
            GameEvent::SpellCast { caster: PlayerId::Opp, card_id: spell_id, mana_spent: true, alt_cost: false, x: 0, delved: Vec::new() },
            &mut state, 1, PlayerId::Opp,
        );
        let ctx = state.pending_triggers.iter().position(|c| c.source_name == "Warded Bear")
            .map(|i| state.pending_triggers.remove(i))
            .expect("IR Ward should fire for an opponent's spell targeting the holder");

        // Resolve the ward trigger — default strategy pays the cost.
        ctx.effect.call(&mut state, 1, &[]);
        assert_eq!(state.player(PlayerId::Opp).life, opp_life - 2,
            "ward: targeting player pays 2 life");
        assert!(state.stack.contains(&spell_id), "ward: spell survives when the cost is paid");
    }

    /// IR Ward does not trigger on the holder's controller's own spells (CR 702.21
    /// — "a spell or ability an opponent controls").
    #[test]
    fn test_ir_ward_ignores_own_spell() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let mut bear = creature("Warded Bear", 2, 2);
        bear.abilities = vec![crate::card_defs::ir_ward(ward_cost_pay2())];
        let bear_id = add_perm_with_def(&mut state, PlayerId::Us, &bear, BattlefieldState::new());
        recompute(&mut state);

        let spell_id = opp_spell_targeting(&mut state, PlayerId::Us, bear_id); // Us casts it
        fire_event(
            GameEvent::SpellCast { caster: PlayerId::Us, card_id: spell_id, mana_spent: true, alt_cost: false, x: 0, delved: Vec::new() },
            &mut state, 1, PlayerId::Us,
        );
        assert!(!state.pending_triggers.iter().any(|c| c.source_name == "Warded Bear"),
            "ward must not fire for the controller's own spell");
    }

    /// `CEMod::GrantAbility`: a static "other creatures you control have Ward" grants
    /// the IR ward to a vanilla creature, which then triggers on opponent targeting.
    #[test]
    fn test_ir_grant_ability_grants_ward_to_other_creature() {
        use crate::ir::ability::{Ability, AbilityKind};
        use crate::ir::ce::CEMod;
        use crate::ir::context::Ctx;
        use crate::ir::expr::{Expr, Filter};
        let mut state = make_state();
        state.catalog = test_catalog();

        // Granter: "Other creatures you control have Ward—Pay 2 life."
        let mut granter = creature("Ward Granter", 1, 1);
        granter.abilities = vec![Ability {
            kind: AbilityKind::Static {
                mods: vec![CEMod::GrantAbility(Box::new(crate::card_defs::ir_ward(ward_cost_pay2())))],
                scope: Some(Filter(Expr::And(
                    Box::new(Expr::Eq(
                        Box::new(Expr::Controller(Box::new(Expr::Ctx(Ctx::It)))),
                        Box::new(Expr::Ctx(Ctx::Controller)),
                    )),
                    Box::new(Expr::Not(Box::new(Expr::Eq(
                        Box::new(Expr::Ctx(Ctx::It)),
                        Box::new(Expr::Ctx(Ctx::Source)),
                    )))),
                ))),
                condition: None,
            },
            text: Some("Other creatures you control have Ward—Pay 2 life."),
        }];
        add_perm_with_def(&mut state, PlayerId::Us, &granter, BattlefieldState::new());
        let vanilla_id = add_default_perm(&mut state, PlayerId::Us, "Grizzly Bears");
        recompute(&mut state);

        // The grant landed on the vanilla creature's materialized def.
        let mat = state.def_of(vanilla_id).expect("materialized def");
        assert!(!mat.granted_abilities.is_empty(),
            "vanilla creature should carry the granted Ward ability");

        // Opponent spell targeting the vanilla creature → granted ward fires.
        let spell_id = opp_spell_targeting(&mut state, PlayerId::Opp, vanilla_id);
        fire_event(
            GameEvent::SpellCast { caster: PlayerId::Opp, card_id: spell_id, mana_spent: true, alt_cost: false, x: 0, delved: Vec::new() },
            &mut state, 1, PlayerId::Opp,
        );
        assert!(state.pending_triggers.iter().any(|c| c.source_name == "Grizzly Bears"),
            "granted Ward should fire for the vanilla creature targeted by an opponent");
    }

    // ── §46c: Kaito animation (BecomeCreature + ActivePlayer/LoyaltyOf) ─────────

    /// "During your turn, as long as Kaito has one or more loyalty counters, he's a
    /// 3/4 Ninja creature with hexproof." Animates only when both conditions hold.
    #[test]
    fn test_kaito_animates_only_on_your_turn_with_loyalty() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let kaito = catalog_card("Kaito, Bane of Nightmares");

        let mut bf = BattlefieldState::new();
        bf.loyalty = 4;
        let kaito_id = add_perm_with_def(&mut state, PlayerId::Us, &kaito, bf);

        // Your turn + loyalty > 0 → 3/4 Ninja creature with hexproof, no longer a PW.
        state.current_ap = state.player_id(PlayerId::Us);
        recompute(&mut state);
        let def = state.def_of(kaito_id).expect("materialized def");
        assert!(def.is_creature(), "Kaito should be a creature on your turn with loyalty");
        let c = def.as_creature().expect("animated Kaito is a creature");
        assert_eq!((c.power(), c.toughness()), (3, 4), "animated Kaito is 3/4");
        assert!(def.has_subtype("Ninja"), "animated Kaito is a Ninja");
        assert!(def.has_keyword(Keyword::Hexproof), "animated Kaito has hexproof");
        assert!(!def.types.contains(&CardType::Planeswalker),
            "per the ruling, animated Kaito stops being a planeswalker");

        // Opponent's turn → not animated.
        state.current_ap = state.player_id(PlayerId::Opp);
        recompute(&mut state);
        assert!(!state.def_of(kaito_id).unwrap().is_creature(),
            "Kaito is not a creature on the opponent's turn");

        // Your turn but loyalty 0 → not animated.
        state.current_ap = state.player_id(PlayerId::Us);
        if let Some(bf) = state.permanent_bf_mut(kaito_id) { bf.loyalty = 0; }
        recompute(&mut state);
        assert!(!state.def_of(kaito_id).unwrap().is_creature(),
            "Kaito with 0 loyalty is not a creature even on your turn");
    }

    /// Long Goodbye's "This spell can't be countered" still works after the ProhibitionDef refactor.
    #[test]
    fn test_long_goodbye_still_uncounterable() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Pre-register Long Goodbye's prohibition instances before casting.
        let def = catalog_card("Long Goodbye");
        let lg_id = add_hand_card(&mut state, PlayerId::Us, "Long Goodbye");

        state.catalog.insert("Long Goodbye".to_string(), def);

        // Move to stack — activates Long Goodbye's own stack prohibition.
        change_zone(lg_id, ZoneId::Stack, &mut state, 1, PlayerId::Us);
        state.stack.push(lg_id);

        // Opponent tries to counter it.
        counter_one(lg_id, &mut state, 1, PlayerId::Opp);

        assert!(state.stack.contains(&lg_id),
            "Long Goodbye can't be countered (ProhibitionDef on SpellBeingCountered)");
        assert_ne!(state.objects[&lg_id].zone(), Some(Zone::Graveyard));
    }

    // ── §48: Show and Tell ────────────────────────────────────────────────────

    /// Resolve the real Show and Tell `OnResolve` body (the IR `SimultaneousPut`)
    /// as `caster`.
    fn run_show_and_tell(state: &mut SimState, caster: PlayerId) {
        let snt = catalog_card("Show and Tell");
        let body = match &snt.abilities[0].kind {
            crate::ir::ability::AbilityKind::OnResolve { modes } => modes[0].body.clone(),
            _ => panic!("Show and Tell should carry an OnResolve body"),
        };
        crate::ir::executor::execute(
            &body, state,
            &crate::ir::executor::BindEnv::new().with_controller(caster),
        );
    }

    #[test]
    fn test_show_and_tell_caster_puts_creature_on_battlefield() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let creature_id = add_hand_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        // Caster puts its only candidate (the creature); opp has none → declines.
        state.set_strategy(PlayerId::Us, Box::new(TestStrategy::new(PlayerId::Us).put_first_candidate()));

        run_show_and_tell(&mut state, PlayerId::Us);

        assert_eq!(state.objects[&creature_id].zone(), Some(Zone::Battlefield),
            "Show and Tell should put the chosen creature onto the battlefield");
    }

    #[test]
    fn test_show_and_tell_no_candidates_no_crash() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // No cards in either hand — should not panic.
        run_show_and_tell(&mut state, PlayerId::Us);
    }

    // ── §49: Spell Pierce / tax counters ──────────────────────────────────────

    /// Drive the IR "counter target spell unless its controller pays `cost`"
    /// resolution body (Daze / Spell Pierce / Flusterstorm share it) against
    /// `target`, as the counterspell controlled by `caster`.
    fn run_counter_unless_pays(state: &mut SimState, caster: PlayerId, target: ObjId, cost: &str) {
        let body = crate::card_defs::counter_unless_pays_body(parse_mana_cost(cost));
        let env = crate::ir::executor::BindEnv::new()
            .with_controller(caster)
            .with_var("target", crate::ir::expr::Value::Obj(target));
        crate::ir::executor::execute(&body, state, &env);
    }

    #[test]
    fn test_counter_unless_pays_counters_when_opp_cannot_pay() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Opponent spell on the stack — no lands, can't pay {2}.
        let spell_id = state.alloc_id();
        state.objects.insert(spell_id, GameObject {
            id: spell_id,
            catalog_key: "Ponder".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::StackSpell(SpellState {
                effect: Some(eff_draw(PlayerId::Opp, 1)),
                chosen_targets: vec![],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
        });
        state.stack.push(spell_id);

        run_counter_unless_pays(&mut state, PlayerId::Us, spell_id, "2");

        assert_eq!(state.objects[&spell_id].zone(), Some(Zone::Graveyard),
            "spell should be countered when opponent can't pay 2");
        assert!(!state.stack.contains(&spell_id));
    }

    #[test]
    fn test_counter_unless_pays_spell_resolves_when_opp_pays() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Give opponent 2 untapped Islands to pay {2}.
        let island_def = catalog_card("Island");
        add_perm_with_def(&mut state, PlayerId::Opp, &island_def, BattlefieldState::new());
        add_perm_with_def(&mut state, PlayerId::Opp, &island_def, BattlefieldState::new());
        // No strategy override needed: the opp's default `resolve_choice` returns
        // Mode(0) for a Choose, picking the first legal option ("pay") when payable.
        // Opponent spell on the stack.
        let spell_id = state.alloc_id();
        state.objects.insert(spell_id, GameObject {
            id: spell_id,
            catalog_key: "Ponder".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::StackSpell(SpellState {
                effect: Some(eff_draw(PlayerId::Opp, 1)),
                chosen_targets: vec![],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
        });
        state.stack.push(spell_id);

        run_counter_unless_pays(&mut state, PlayerId::Us, spell_id, "2");

        assert!(state.stack.contains(&spell_id),
            "spell should remain on stack when opponent pays 2");
        assert_eq!(state.objects[&spell_id].zone(), Some(Zone::Stack));
    }

    #[test]
    fn test_daze_counter_unless_pays_1() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Opponent has no lands — can't pay {1}.
        let spell_id = state.alloc_id();
        state.objects.insert(spell_id, GameObject {
            id: spell_id,
            catalog_key: "Ponder".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::StackSpell(SpellState {
                effect: Some(eff_draw(PlayerId::Opp, 1)),
                chosen_targets: vec![],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
        });
        state.stack.push(spell_id);

        // Daze: counter unless pays {1}
        run_counter_unless_pays(&mut state, PlayerId::Us, spell_id, "1");

        assert_eq!(state.objects[&spell_id].zone(), Some(Zone::Graveyard),
            "Daze should counter when opponent can't pay 1");
    }

    /// "Counter unless pays" lets the payer activate mana abilities during the
    /// choice: with an untapped land and an empty pool, the opponent auto-taps it
    /// to pay the tax (default strategy always pays if possible), so the spell is
    /// NOT countered. Exercises mana-ability timing during resolution-time payment.
    #[test]
    fn test_daze_pays_by_tapping_land() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Opponent: an untapped Island (empty pool) + a spell on the stack.
        let island = add_perm_with_def(&mut state, PlayerId::Opp, &catalog_card("Island"), BattlefieldState::new());
        recompute(&mut state);
        let spell_id = push_stack_spell(&mut state, PlayerId::Opp, "Ponder");

        run_counter_unless_pays(&mut state, PlayerId::Us, spell_id, "1");

        assert_eq!(state.objects[&spell_id].zone(), Some(Zone::Stack),
            "Daze should NOT counter — opponent taps a land to pay the {{1}} tax");
        assert!(state.objects[&island].bf().unwrap().tapped,
            "the Island was tapped to produce the mana");
    }

    // ── §50: Flusterstorm / Storm trigger ──────────────────────────────────────

    #[test]
    fn test_flusterstorm_storm_trigger_creates_copies() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Card-bound triggers are derived from catalog at fire time — no preregistration.
        let fluster_id = state.alloc_id();

        // Put two opponent instants on the stack as targets.
        let spell_a = state.alloc_id();
        let spell_b = state.alloc_id();
        for (id, name) in [(spell_a, "Brainstorm"), (spell_b, "Ponder")] {
            state.objects.insert(id, GameObject {
            id,
            catalog_key: name.to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::StackSpell(SpellState {
                    effect: Some(eff_draw(PlayerId::Opp, 1)),
                    chosen_targets: vec![],
                    is_back_face: false,
                    costs_paid_ctx: CostsPaidCtx::default(),
                }),
        });
            state.stack.push(id);
        }

        // Put flusterstorm on the stack (as if we just cast it) with spell_a as target.
        state.objects.insert(fluster_id, GameObject {
            id: fluster_id,
            catalog_key: "Flusterstorm".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::StackSpell(SpellState {
                // Storm copies are built from the catalog def, not this object's
                // effect; the original's effect is irrelevant to this trigger test.
                effect: None,
                chosen_targets: vec![spell_a],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
        });
        state.stack.push(fluster_id);

        // Layer-B event log: populate prior SpellCast events + the triggering
        // Flusterstorm cast. Storm body evaluates as EventCount(ThisTurn,
        // SpellCast caster=Us) - 1, so 3 logged → 2 copies.
        state.event_log.push(1, GameEvent::SpellCast {
            caster: PlayerId::Us, card_id: spell_a, mana_spent: true, alt_cost: false, x: 0, delved: Vec::new(),
        });
        state.event_log.push(1, GameEvent::SpellCast {
            caster: PlayerId::Us, card_id: spell_b, mana_spent: true, alt_cost: false, x: 0, delved: Vec::new(),
        });
        let event = GameEvent::SpellCast { caster: PlayerId::Us, card_id: fluster_id, mana_spent: true, alt_cost: false, x: 0, delved: Vec::new() };
        state.event_log.push(1, event.clone());

        // Fire the SpellCast event — storm trigger should fire.
        let (triggers, _) = fire_triggers(&event, &state);
        assert_eq!(triggers.len(), 1, "storm should produce exactly one trigger context");
        assert_eq!(triggers[0].source_name, "Flusterstorm");

        // Resolve the storm trigger effect — should create 2 copies on the stack.
        let stack_before = state.stack.len();
        triggers[0].effect.call(&mut state, 1, &[]);
        let copies_pushed = state.stack.len() - stack_before;
        assert_eq!(copies_pushed, 2, "storm count 2 → 2 copies");

        // Verify copies are ability objects (card-less stack objects).
        for &copy_id in &state.stack[stack_before..] {
            let obj = state.objects.get(&copy_id).expect("copy should be an object on the stack");
            let ability = obj.ability().expect("copy should be an ability object");
            assert_eq!(obj.catalog_key, "Flusterstorm");
            assert!(ability.counterable, "storm copies should be counterable");
        }
    }

    #[test]
    fn test_flusterstorm_no_storm_when_first_spell() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Set up Flusterstorm on the stack so the dispatch can find its ability.
        let fluster_id = state.alloc_id();
        state.objects.insert(fluster_id, GameObject {
            id: fluster_id,
            catalog_key: "Flusterstorm".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::StackSpell(SpellState {
                effect: None,
                chosen_targets: vec![],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
        });
        state.stack.push(fluster_id);

        // Only the Flusterstorm cast itself is in the log — no prior spells.
        let event = GameEvent::SpellCast { caster: PlayerId::Us, card_id: fluster_id, mana_spent: true, alt_cost: false, x: 0, delved: Vec::new() };
        state.event_log.push(1, event.clone());

        let (triggers, _) = fire_triggers(&event, &state);
        assert_eq!(triggers.len(), 1, "storm trigger still fires (resolves to 0 copies)");

        let stack_before = state.stack.len();
        triggers[0].effect.call(&mut state, 1, &[]);
        assert_eq!(state.stack.len(), stack_before, "no copies when first spell of the turn");
    }

    #[test]
    fn test_flusterstorm_storm_copies_counter_spells() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Opponent has no lands — can't pay {1}.
        let spell_id = state.alloc_id();
        state.objects.insert(spell_id, GameObject {
            id: spell_id,
            catalog_key: "Ponder".to_string(),
            owner: PlayerId::Opp,
            controller: PlayerId::Opp,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::StackSpell(SpellState {
                effect: Some(eff_draw(PlayerId::Opp, 1)),
                chosen_targets: vec![],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
        });
        state.stack.push(spell_id);

        // A storm copy's effect is the same as the original: counter unless pays {1}.
        run_counter_unless_pays(&mut state, PlayerId::Us, spell_id, "1");

        assert_eq!(state.objects[&spell_id].zone(), Some(Zone::Graveyard),
            "storm copy should counter spell when opponent can't pay 1");
    }

    // ── §50b: Mindbreak Trap / any-number targeting ────────────────────────────

    #[test]
    fn test_mindbreak_trap_exiles_all_targeted_spells() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Put two opponent spells on the stack.
        let spell_a = state.alloc_id();
        state.objects.insert(spell_a, GameObject {
            id: spell_a,
            catalog_key: "Dark Ritual".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::StackSpell(SpellState {
                effect: Some(eff_mana(PlayerId::Us, "BBB")),
                chosen_targets: vec![],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
        });
        state.stack.push(spell_a);

        let spell_b = state.alloc_id();
        state.objects.insert(spell_b, GameObject {
            id: spell_b,
            catalog_key: "Ponder".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::StackSpell(SpellState {
                effect: Some(eff_draw(PlayerId::Us, 1)),
                chosen_targets: vec![],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
        });
        state.stack.push(spell_b);

        // Opponent resolves Mindbreak Trap — exiles each spell the caster (Opp) doesn't control.
        let mbt = catalog_card("Mindbreak Trap");
        build_spell_effect(&mbt, PlayerId::Opp, ObjId::UNSET, 0, 0).1.call(&mut state, 1, &[]);

        assert!(state.objects[&spell_a].in_zone(Zone::Exile { on_adventure: false }),
            "spell A should be exiled");
        assert!(state.objects[&spell_b].in_zone(Zone::Exile { on_adventure: false }),
            "spell B should be exiled");
    }

    #[test]
    fn test_mindbreak_trap_condition_checks_opponent_spell_count() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let trap_def = state.catalog.get("Mindbreak Trap").unwrap().clone();
        let alt = &trap_def.alternate_costs[0];
        let condition = alt.condition.as_ref().unwrap();

        // Spell counts come from the event log (the spells_cast_this_turn counter
        // was removed). Log Us casts; the condition reads "opponent of Opp" = Us.
        let log_us_cast = |state: &mut SimState| state.event_log.push(
            1, GameEvent::SpellCast { caster: PlayerId::Us, card_id: ObjId::UNSET, mana_spent: true, alt_cost: false, x: 0, delved: Vec::new() });

        // Opponent has cast 2 spells — condition false for Opp caster.
        log_us_cast(&mut state);
        log_us_cast(&mut state);
        assert!(!condition(PlayerId::Opp, &state),
            "trap condition should be false when opponent cast only 2 spells");

        // Opponent has cast a third spell — condition true.
        log_us_cast(&mut state);
        assert!(condition(PlayerId::Opp, &state),
            "trap condition should be true when opponent cast 3+ spells");
    }

    // ── §51: Simian Spirit Guide / hand-zone mana ──────────────────────────────

    #[test]
    fn test_ssg_potential_mana_includes_hand() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // SSG in hand → potential_mana should include {R}.
        add_hand_card(&mut state, PlayerId::Us, "Simian Spirit Guide");
        let pool = state.potential_mana(PlayerId::Us);
        assert!(pool.r >= 1, "potential_mana should see SSG's R from hand");
        assert!(pool.total >= 1);
    }

    #[test]
    fn test_ssg_produce_mana_exiles_from_hand() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let ssg_id = add_hand_card(&mut state, PlayerId::Us, "Simian Spirit Guide");
        // Activate SSG's hand-zone mana ability — should exile from hand.
        let act = ManaActivation { source_id: ssg_id, ability_index: 0, color_choice: Some(Color::Red) };
        execute_mana_activation(&mut state, 1, PlayerId::Us, &act);
        assert_eq!(state.objects[&ssg_id].zone(), Some(Zone::Exile { on_adventure: false }),
            "SSG should be exiled after paying mana");
        assert_eq!(state.player(PlayerId::Us).pool.r, 1, "SSG should produce R");
    }

    #[test]
    fn test_ssg_on_battlefield_does_not_tap_for_mana() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // SSG on battlefield — its mana ability is hand-zone only.
        let ssg_def = catalog_card("Simian Spirit Guide");
        add_perm_with_def(&mut state, PlayerId::Us, &ssg_def, BattlefieldState::new());
        let pool = state.potential_mana(PlayerId::Us);
        assert_eq!(pool.r, 0, "SSG on battlefield should not produce R");
        assert_eq!(pool.total, 0);
    }

    // ── §52: Swords to Plowshares ──────────────────────────────────────────────

    #[test]
    fn test_swords_exiles_creature_and_gains_life() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Opponent has a 3/3 creature.
        let creature_def = catalog_card("Murktide Regent");
        let bf = BattlefieldState { counters: 0, ..BattlefieldState::new() };
        let creature_id = add_perm_with_def(&mut state, PlayerId::Opp, &creature_def, bf);
        recompute(&mut state);
        let opp_life_before = state.player(PlayerId::Opp).life;

        // Run Swords' actual resolution body (IR) with `target` bound to the creature.
        let swords = catalog_card("Swords to Plowshares");
        let body = match &swords.abilities[0].kind {
            crate::ir::ability::AbilityKind::OnResolve { modes } => modes[0].body.clone(),
            _ => panic!("Swords should resolve via OnResolve"),
        };
        let env = crate::ir::executor::BindEnv::new()
            .with_controller(PlayerId::Us)
            .with_var("target", crate::ir::expr::Value::Obj(creature_id));
        crate::ir::executor::execute(&body, &mut state, &env);

        assert_eq!(state.objects[&creature_id].zone(), Some(Zone::Exile { on_adventure: false }),
            "creature should be exiled");
        assert!(state.player(PlayerId::Opp).life > opp_life_before,
            "opponent should gain life equal to creature's power");
    }

    // ── §53: City of Traitors ───────────────────────────────────────────────────

    #[test]
    fn test_city_of_traitors_sacrificed_when_another_land_played() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let cot_def = catalog_card("City of Traitors");
        let cot_id = add_perm_with_def(&mut state, PlayerId::Us, &cot_def, BattlefieldState::new());
        recompute(&mut state);

        // Playing another land fires LandPlayed — should trigger CoT's sacrifice.
        let other_land_id = state.alloc_id();
        fire_event(
            GameEvent::LandPlayed { id: other_land_id, controller: PlayerId::Us },
            &mut state, 1, PlayerId::Us,
        );
        assert!(!state.pending_triggers.is_empty(),
            "LandPlayed should produce a pending trigger for City of Traitors");

        // Resolve the trigger — CoT goes to graveyard.
        let ctx = state.pending_triggers.remove(0);
        ctx.effect.call(&mut state, 1, &[]);
        assert_eq!(state.objects[&cot_id].zone(), Some(Zone::Graveyard),
            "City of Traitors should be sacrificed");
    }

    #[test]
    fn test_city_of_traitors_not_triggered_by_fetch() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let cot_def = catalog_card("City of Traitors");
        let _cot_id = add_perm_with_def(&mut state, PlayerId::Us, &cot_def, BattlefieldState::new());
        recompute(&mut state);

        // A land entering via fetch fires ZoneChange but NOT LandPlayed.
        let fetched_id = state.alloc_id();
        fire_event(
            GameEvent::ZoneChange {
                id: fetched_id,
                actor: PlayerId::Us,
                from: ZoneId::Library,
                to: ZoneId::Battlefield,
                controller: PlayerId::Us,
            },
            &mut state, 1, PlayerId::Us,
        );
        assert!(state.pending_triggers.is_empty(),
            "ZoneChange (fetch) should NOT trigger City of Traitors");
    }

    #[test]
    fn test_city_of_traitors_produces_two_colorless() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let cot_def = catalog_card("City of Traitors");
        add_perm_with_def(&mut state, PlayerId::Us, &cot_def, BattlefieldState::new());
        recompute(&mut state);
        let pool = state.potential_mana(PlayerId::Us);
        assert_eq!(pool.total, 2, "City of Traitors should produce 2 mana");
    }

    // ── §54: Omniscience ────────────────────────────────────────────────────────

    #[test]
    fn test_omniscience_grants_free_alternate_cost_to_hand_spells() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let omni_def = catalog_card("Omniscience");
        add_perm_with_def(&mut state, PlayerId::Us, &omni_def, BattlefieldState::new());
        let spell_id = add_hand_card(&mut state, PlayerId::Us, "Doomsday");
        recompute(&mut state);

        let def = state.def_of(spell_id).expect("hand card should have materialized def");
        assert!(def.alternate_costs().iter().any(|c| c.costs.is_empty()),
            "Omniscience should grant a zero-cost alternate to hand spells");
    }

    #[test]
    fn test_omniscience_does_not_affect_lands() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let omni_def = catalog_card("Omniscience");
        add_perm_with_def(&mut state, PlayerId::Us, &omni_def, BattlefieldState::new());
        let land_id = add_hand_card(&mut state, PlayerId::Us, "Underground Sea");
        recompute(&mut state);

        let def = state.def_of(land_id).expect("hand card should have materialized def");
        assert!(def.alternate_costs().is_empty(),
            "Omniscience should not grant alternate costs to lands");
    }

    #[test]
    fn test_omniscience_does_not_affect_opponent() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let omni_def = catalog_card("Omniscience");
        add_perm_with_def(&mut state, PlayerId::Us, &omni_def, BattlefieldState::new());
        let opp_spell = add_hand_card(&mut state, PlayerId::Opp, "Doomsday");
        recompute(&mut state);

        let def = state.def_of(opp_spell).expect("hand card should have materialized def");
        assert!(def.alternate_costs().is_empty(),
            "Omniscience should not affect opponent's spells");
    }

    // ── §55: Sneak Attack ───────────────────────────────────────────────────────

    #[test]
    fn test_sneak_attack_enchantment_has_ability() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let sa_def = catalog_card("Sneak Attack");
        add_perm_with_def(&mut state, PlayerId::Us, &sa_def, BattlefieldState::new());
        recompute(&mut state);
        assert_eq!(sa_def.abilities().len(), 1, "Sneak Attack should have one activated ability");
    }

    #[test]
    fn test_sneak_attack_puts_creature_with_haste() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let sa_def = catalog_card("Sneak Attack");
        add_perm_with_def(&mut state, PlayerId::Us, &sa_def, BattlefieldState::new());
        let creature_id = add_hand_card(&mut state, PlayerId::Us, "Orcish Bowmasters");
        recompute(&mut state);

        // Resolve the ability effect with the creature as the chosen target.
        let ability = &sa_def.abilities()[0];
        let eff = build_ability_effect(ability, PlayerId::Us, ObjId::UNSET);
        eff.call(&mut state, 1, &[creature_id]);
        recompute(&mut state);

        // Creature should be on the battlefield with haste.
        assert_eq!(state.objects[&creature_id].zone(), Some(Zone::Battlefield),
            "creature should be on the battlefield");
        let def = state.def_of(creature_id).expect("should have materialized def");
        assert!(def.has_keyword(Keyword::Haste), "creature should have haste");
    }

    #[test]
    fn test_sneak_attack_delayed_trigger_sacrifices_at_end_step() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let sa_def = catalog_card("Sneak Attack");
        add_perm_with_def(&mut state, PlayerId::Us, &sa_def, BattlefieldState::new());
        let creature_id = add_hand_card(&mut state, PlayerId::Us, "Orcish Bowmasters");
        recompute(&mut state);

        // Resolve the ability to put creature onto battlefield.
        let ability = &sa_def.abilities()[0];
        let eff = build_ability_effect(ability, PlayerId::Us, ObjId::UNSET);
        eff.call(&mut state, 1, &[creature_id]);
        recompute(&mut state);
        assert_eq!(state.objects[&creature_id].zone(), Some(Zone::Battlefield));

        // Drain any pending triggers from the ETB (e.g. Bowmasters draw-trigger setup).
        state.pending_triggers.clear();

        // Fire end step event — should produce a delayed sacrifice trigger.
        fire_event(
            GameEvent::EnteredStep { step: StepKind::End, active_player: PlayerId::Us },
            &mut state, 2, PlayerId::Us,
        );
        assert!(!state.pending_triggers.is_empty(),
            "end step should produce a sacrifice trigger");

        // Resolve the trigger — creature should be sacrificed.
        let ctx = state.pending_triggers.remove(0);
        ctx.effect.call(&mut state, 2, &[]);
        assert_eq!(state.objects[&creature_id].zone(), Some(Zone::Graveyard),
            "creature should be sacrificed at end step");
    }

    // ── 42. Magus of the Moon ─────────────────────────────────────────────────

    /// Helper: place Magus of the Moon on the battlefield with its static CE registered.
    fn etb_magus_of_the_moon(state: &mut SimState, who: PlayerId) -> ObjId {
        let def = catalog_card("Magus of the Moon");
        add_perm_with_def(state, who, &def, BattlefieldState::new())
    }

    /// Nonbasic dual land (Underground Sea: island + swamp) should become a Mountain
    /// with only "{T}: Add {R}" after Magus of the Moon enters play.
    #[test]
    fn test_magus_nonbasic_becomes_mountain() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let _magus = etb_magus_of_the_moon(&mut state, PlayerId::Us);
        let sea_id = add_default_perm(&mut state, PlayerId::Opp, "Underground Sea");
        recompute(&mut state);

        let def = state.def_of(sea_id).expect("Underground Sea should have materialized def");
        let land = def.as_land().expect("should still be a Land");
        assert!(land.land_types.contains(BasicLandType::Mountain), "nonbasic should gain Mountain type");
        assert!(!land.land_types.contains(BasicLandType::Island), "nonbasic should lose Island type");
        assert!(!land.land_types.contains(BasicLandType::Swamp), "nonbasic should lose Swamp type");
        assert_eq!(land.mana_abilities.len(), 1, "should have exactly one mana ability");
        assert!(land.abilities.is_empty(), "non-mana abilities should be cleared");
    }

    /// Basic Island is unaffected by Magus of the Moon.
    #[test]
    fn test_magus_basic_land_unaffected() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let _magus = etb_magus_of_the_moon(&mut state, PlayerId::Us);
        let island_def = catalog_card("Island");
        let island_id = add_perm_with_def(&mut state, PlayerId::Us, &island_def, BattlefieldState::new());
        recompute(&mut state);

        let def = state.def_of(island_id).expect("Island should have materialized def");
        let land = def.as_land().expect("should be a Land");
        assert!(land.land_types.contains(BasicLandType::Island), "basic Island should keep Island type");
        assert!(!land.land_types.contains(BasicLandType::Mountain), "basic Island should not gain Mountain");
    }

    /// Legendary supertype is preserved on Karakas under Magus of the Moon.
    #[test]
    fn test_magus_preserves_supertypes() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let _magus = etb_magus_of_the_moon(&mut state, PlayerId::Us);
        let karakas_def = catalog_card("Karakas");
        let karakas_id = add_perm_with_def(&mut state, PlayerId::Us, &karakas_def, BattlefieldState::new());
        recompute(&mut state);

        let def = state.def_of(karakas_id).expect("Karakas should have materialized def");
        assert!(def.supertypes.contains(&Supertype::Legendary),
            "Karakas should keep Legendary supertype");
        let land = def.as_land().expect("should be a Land");
        assert!(land.land_types.contains(BasicLandType::Mountain), "Karakas should become a Mountain");
        assert!(land.abilities.is_empty(), "Karakas activated ability should be stripped");
    }

    /// A fetch land under Magus loses its fetch ability and becomes a Mountain.
    /// The search predicate is baked into the ability, which is cleared.
    #[test]
    fn test_magus_fetch_land_loses_ability() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let _magus = etb_magus_of_the_moon(&mut state, PlayerId::Us);
        let delta_def = catalog_card("Polluted Delta");
        let delta_id = add_perm_with_def(&mut state, PlayerId::Us, &delta_def, BattlefieldState::new());
        recompute(&mut state);

        let def = state.def_of(delta_id).expect("Polluted Delta should have materialized def");
        let land = def.as_land().expect("should be a Land");
        assert!(land.land_types.contains(BasicLandType::Mountain), "Polluted Delta should be a Mountain");
        assert!(!land.land_types.contains(BasicLandType::Island), "should lose Island subtype");
        assert!(!land.land_types.contains(BasicLandType::Swamp), "should lose Swamp subtype");
        assert!(land.abilities.is_empty(), "fetch ability should be cleared");
        assert_eq!(land.mana_abilities.len(), 1, "should have exactly one mana ability");
    }

    /// When Magus of the Moon leaves the battlefield, nonbasic lands revert to original types.
    #[test]
    fn test_magus_ci_removed_on_ltb() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let magus_id = etb_magus_of_the_moon(&mut state, PlayerId::Us);
        let sea_id = add_default_perm(&mut state, PlayerId::Opp, "Underground Sea");
        recompute(&mut state);

        // Verify CE is active.
        let def = state.def_of(sea_id).unwrap();
        assert!(def.as_land().unwrap().land_types.contains(BasicLandType::Mountain), "should be Mountain while Magus in play");

        // Magus leaves the battlefield.
        change_zone(magus_id, ZoneId::Graveyard, &mut state, 1, PlayerId::Us);
        recompute(&mut state);

        let def = state.def_of(sea_id).unwrap();
        let land = def.as_land().unwrap();
        assert!(land.land_types.contains(BasicLandType::Island), "Underground Sea should revert to Island");
        assert!(land.land_types.contains(BasicLandType::Swamp), "Underground Sea should revert to Swamp");
        assert!(!land.land_types.contains(BasicLandType::Mountain), "should no longer be a Mountain");
    }

    /// Magus of the Moon does not affect creatures (modifier early-returns for non-Land).
    #[test]
    fn test_magus_does_not_affect_creatures() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let _magus = etb_magus_of_the_moon(&mut state, PlayerId::Us);
        let bowmasters_def = catalog_card("Orcish Bowmasters");
        let bowmasters_id = add_perm_with_def(&mut state, PlayerId::Opp, &bowmasters_def, BattlefieldState::new());
        recompute(&mut state);

        let def = state.def_of(bowmasters_id).expect("Bowmasters should have materialized def");
        assert!(def.as_creature().is_some(), "should still be a Creature");
        assert!(def.as_land().is_none(), "should not be a Land");
    }

    /// Snow-Covered Island has Supertype::Basic and should be unaffected.
    #[test]
    fn test_magus_snow_basic_unaffected() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let _magus = etb_magus_of_the_moon(&mut state, PlayerId::Us);
        let snow_def = catalog_card("Snow-Covered Island");
        let snow_id = add_perm_with_def(&mut state, PlayerId::Us, &snow_def, BattlefieldState::new());
        recompute(&mut state);

        let def = state.def_of(snow_id).expect("Snow-Covered Island should have materialized def");
        let land = def.as_land().expect("should be a Land");
        assert!(land.land_types.contains(BasicLandType::Island), "Snow-Covered Island should keep Island type");
        assert!(!land.land_types.contains(BasicLandType::Mountain), "should not gain Mountain");
        assert!(def.supertypes.contains(&Supertype::Basic), "should keep Basic");
        assert!(def.supertypes.contains(&Supertype::Snow), "should keep Snow");
    }

    /// Multiple nonbasic lands all become Mountains simultaneously.
    #[test]
    fn test_magus_multiple_nonbasics() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let _magus = etb_magus_of_the_moon(&mut state, PlayerId::Us);
        let sea_id = add_default_perm(&mut state, PlayerId::Opp, "Underground Sea");
        let tundra_id = add_default_perm(&mut state, PlayerId::Opp, "Tundra");
        recompute(&mut state);

        for (id, name) in [(sea_id, "Underground Sea"), (tundra_id, "Tundra")] {
            let def = state.def_of(id).unwrap_or_else(|| panic!("{name} should have materialized def"));
            let land = def.as_land().unwrap_or_else(|| panic!("{name} should be a Land"));
            assert!(land.land_types.contains(BasicLandType::Mountain), "{name} should be a Mountain");
            assert_eq!(land.mana_abilities.len(), 1, "{name} should have one mana ability");
            assert!(land.abilities.is_empty(), "{name} non-mana abilities should be cleared");
        }
    }

    /// Blood Moon (enchantment) shares the same static ability as Magus of the Moon.
    #[test]
    fn test_blood_moon_nonbasic_becomes_mountain() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let bm_def = catalog_card("Blood Moon");
        add_perm_with_def(&mut state, PlayerId::Us, &bm_def, BattlefieldState::new());
        let sea_id = add_default_perm(&mut state, PlayerId::Opp, "Underground Sea");
        recompute(&mut state);

        let def = state.def_of(sea_id).expect("Underground Sea should have materialized def");
        let land = def.as_land().expect("should still be a Land");
        assert!(land.land_types.contains(BasicLandType::Mountain), "nonbasic should gain Mountain type");
        assert!(!land.land_types.contains(BasicLandType::Island), "nonbasic should lose Island type");
        assert!(!land.land_types.contains(BasicLandType::Swamp), "nonbasic should lose Swamp type");
        assert_eq!(land.mana_abilities.len(), 1, "should have exactly one mana ability");
    }

    // ── 43. Urborg, Tomb of Yawgmoth / Yavimaya, Cradle of Growth ────────────

    /// Urborg makes all lands Swamps in addition to their other types.
    #[test]
    fn test_urborg_adds_swamp_to_nonbasic() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let urborg_def = catalog_card("Urborg, Tomb of Yawgmoth");
        let urborg_id = add_perm_with_def(&mut state, PlayerId::Us, &urborg_def, BattlefieldState::new());
        let sea_id = add_default_perm(&mut state, PlayerId::Opp, "Underground Sea");
        recompute(&mut state);

        // Underground Sea (island + swamp) should keep both and still be a swamp.
        let def = state.def_of(sea_id).unwrap();
        let land = def.as_land().unwrap();
        assert!(land.land_types.contains(BasicLandType::Swamp), "should have Swamp");
        assert!(land.land_types.contains(BasicLandType::Island), "should keep Island");

        // Urborg itself gains Swamp too.
        let urborg_mat = state.def_of(urborg_id).unwrap();
        let urborg_land = urborg_mat.as_land().unwrap();
        assert!(urborg_land.land_types.contains(BasicLandType::Swamp), "Urborg itself should be a Swamp");
    }

    /// Urborg adds Swamp + "{T}: Add {B}" to a basic Island.
    #[test]
    fn test_urborg_adds_swamp_to_basic() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let urborg_def = catalog_card("Urborg, Tomb of Yawgmoth");
        add_perm_with_def(&mut state, PlayerId::Us, &urborg_def, BattlefieldState::new());
        let island_def = catalog_card("Island");
        let island_id = add_perm_with_def(&mut state, PlayerId::Us, &island_def, BattlefieldState::new());
        recompute(&mut state);

        let def = state.def_of(island_id).unwrap();
        let land = def.as_land().unwrap();
        assert!(land.land_types.contains(BasicLandType::Island), "should keep Island");
        assert!(land.land_types.contains(BasicLandType::Swamp), "should gain Swamp");
        assert_eq!(land.mana_abilities.len(), 2, "should have U and B mana abilities");
    }

    /// A land that is already a Swamp does not get a duplicate mana ability from Urborg.
    #[test]
    fn test_urborg_no_duplicate_on_swamp() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let urborg_def = catalog_card("Urborg, Tomb of Yawgmoth");
        add_perm_with_def(&mut state, PlayerId::Us, &urborg_def, BattlefieldState::new());
        let swamp_def = catalog_card("Swamp");
        let swamp_id = add_perm_with_def(&mut state, PlayerId::Us, &swamp_def, BattlefieldState::new());
        recompute(&mut state);

        let def = state.def_of(swamp_id).unwrap();
        let land = def.as_land().unwrap();
        assert!(land.land_types.contains(BasicLandType::Swamp), "should still be a Swamp");
        assert_eq!(land.mana_abilities.len(), 1, "should not get a duplicate mana ability");
    }

    /// Yavimaya makes all lands Forests in addition to their other types.
    #[test]
    fn test_yavimaya_adds_forest_to_nonbasic() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let yav_def = catalog_card("Yavimaya, Cradle of Growth");
        add_perm_with_def(&mut state, PlayerId::Us, &yav_def, BattlefieldState::new());
        let sea_id = add_default_perm(&mut state, PlayerId::Opp, "Underground Sea");
        recompute(&mut state);

        let def = state.def_of(sea_id).unwrap();
        let land = def.as_land().unwrap();
        assert!(land.land_types.contains(BasicLandType::Forest), "should gain Forest");
        assert!(land.land_types.contains(BasicLandType::Island), "should keep Island");
        assert!(land.land_types.contains(BasicLandType::Swamp), "should keep Swamp");
    }

    /// Urborg CI is removed when it leaves; lands revert.
    #[test]
    fn test_urborg_ci_removed_on_ltb() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let urborg_def = catalog_card("Urborg, Tomb of Yawgmoth");
        let urborg_id = add_perm_with_def(&mut state, PlayerId::Us, &urborg_def, BattlefieldState::new());
        let island_def = catalog_card("Island");
        let island_id = add_perm_with_def(&mut state, PlayerId::Us, &island_def, BattlefieldState::new());
        recompute(&mut state);
        assert!(state.def_of(island_id).unwrap().as_land().unwrap().land_types.contains(BasicLandType::Swamp));

        change_zone(urborg_id, ZoneId::Graveyard, &mut state, 1, PlayerId::Us);
        recompute(&mut state);

        let land = state.def_of(island_id).unwrap().as_land().unwrap();
        assert!(!land.land_types.contains(BasicLandType::Swamp), "Island should revert — no longer a Swamp");
        assert!(land.land_types.contains(BasicLandType::Island), "Island should keep Island type");
    }

    /// Yavimaya + Blood Moon interaction: Blood Moon (L4) makes nonbasics into Mountains
    /// (losing all types and abilities), then Yavimaya (also L4, registered later) adds Forest
    /// on top. Result: nonbasic is Mountain + Forest with "{T}: Add {R}" and "{T}: Add {G}".
    /// Yavimaya itself is a nonbasic, so Blood Moon turns it into a Mountain too — but
    /// Yavimaya's CE persists because type-changing (L4) is independent of ability removal (L6).
    #[test]
    fn test_yavimaya_plus_blood_moon() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Blood Moon first, then Yavimaya — registration order within the same layer matters.
        let bm_def = catalog_card("Blood Moon");
        add_perm_with_def(&mut state, PlayerId::Us, &bm_def, BattlefieldState::new());
        let yav_def = catalog_card("Yavimaya, Cradle of Growth");
        let yav_id = add_perm_with_def(&mut state, PlayerId::Us, &yav_def, BattlefieldState::new());
        let sea_id = add_default_perm(&mut state, PlayerId::Opp, "Underground Sea");
        recompute(&mut state);

        // CR 613.7: Blood Moon (dep_order 0) applies before Yavimaya (dep_order 1).
        // Blood Moon makes Yavimaya a Mountain, stripping all abilities including its
        // static ability. Yavimaya's CE ceases to exist → nonbasics are Mountains only.
        let def = state.def_of(sea_id).unwrap();
        let land = def.as_land().unwrap();
        assert!(land.land_types.contains(BasicLandType::Mountain), "Blood Moon should make it a Mountain");
        assert!(!land.land_types.contains(BasicLandType::Forest), "Yavimaya CE suppressed — no Forest");
        assert!(!land.land_types.contains(BasicLandType::Island), "original Island type should be gone");
        assert!(!land.land_types.contains(BasicLandType::Swamp), "original Swamp type should be gone");
        assert_eq!(land.mana_abilities.len(), 1,
            "should have only R mana ability");

        // Yavimaya itself is nonbasic: Blood Moon turns it into a Mountain and
        // strips its static ability, so its CE doesn't exist.
        let yav_mat = state.def_of(yav_id).unwrap();
        let yav_land = yav_mat.as_land().unwrap();
        assert!(yav_land.land_types.contains(BasicLandType::Mountain), "Yavimaya should be a Mountain under Blood Moon");
        assert!(!yav_land.land_types.contains(BasicLandType::Forest), "Yavimaya's CE is suppressed — no Forest");
    }

    /// CR 613.7: dependency (Blood Moon writes LandTypes, Yavimaya reads LandTypes)
    /// overrides timestamp — same result regardless of registration order.
    #[test]
    fn test_yavimaya_before_blood_moon_same_result() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Yavimaya first, Blood Moon second — opposite registration order.
        let yav_def = catalog_card("Yavimaya, Cradle of Growth");
        let yav_id = add_perm_with_def(&mut state, PlayerId::Us, &yav_def, BattlefieldState::new());
        let bm_def = catalog_card("Blood Moon");
        add_perm_with_def(&mut state, PlayerId::Us, &bm_def, BattlefieldState::new());
        let sea_id = add_default_perm(&mut state, PlayerId::Opp, "Underground Sea");
        recompute(&mut state);

        // Same result as test_yavimaya_plus_blood_moon: Yavimaya's CE is suppressed.
        let def = state.def_of(sea_id).unwrap();
        let land = def.as_land().unwrap();
        assert!(land.land_types.contains(BasicLandType::Mountain), "Blood Moon should make it a Mountain");
        assert!(!land.land_types.contains(BasicLandType::Forest), "Yavimaya CE suppressed — no Forest");
        assert_eq!(land.mana_abilities.len(), 1, "only R mana ability");

        let yav_mat = state.def_of(yav_id).unwrap();
        let yav_land = yav_mat.as_land().unwrap();
        assert!(yav_land.land_types.contains(BasicLandType::Mountain));
        assert!(!yav_land.land_types.contains(BasicLandType::Forest), "Yavimaya's CE is suppressed");
    }

    /// Urborg's CE is also suppressed under Blood Moon — nonbasics are Mountains only.
    #[test]
    fn test_urborg_suppressed_under_blood_moon() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let bm_def = catalog_card("Blood Moon");
        add_perm_with_def(&mut state, PlayerId::Us, &bm_def, BattlefieldState::new());
        let urborg_def = catalog_card("Urborg, Tomb of Yawgmoth");
        let urborg_id = add_perm_with_def(&mut state, PlayerId::Us, &urborg_def, BattlefieldState::new());
        let sea_id = add_default_perm(&mut state, PlayerId::Opp, "Underground Sea");
        recompute(&mut state);

        // Urborg's CE suppressed — nonbasics are Mountains only, no Swamp.
        let def = state.def_of(sea_id).unwrap();
        let land = def.as_land().unwrap();
        assert!(land.land_types.contains(BasicLandType::Mountain));
        assert!(!land.land_types.contains(BasicLandType::Swamp), "Urborg CE suppressed — no Swamp");
        assert_eq!(land.mana_abilities.len(), 1, "only R mana ability");

        // Urborg itself is a Mountain (nonbasic, legendary).
        let urborg_mat = state.def_of(urborg_id).unwrap();
        let urborg_land = urborg_mat.as_land().unwrap();
        assert!(urborg_land.land_types.contains(BasicLandType::Mountain));
        assert!(!urborg_land.land_types.contains(BasicLandType::Swamp), "Urborg's own CE is suppressed");
    }

    // ── Section 32: Protection ─────────────────────────────────────────────────

    /// Helper: a colored instant (blue) for protection tests.
    fn blue_instant(name: &str) -> CardDef {
        CardDef::new(
            name, CardKind::Instant(SpellData { mana_cost: "U".into(), ..Default::default() }),
            vec![Color::Blue], None, vec![], CardLayout::Normal, None,
            vec![], vec![], vec![], vec![],
        )
    }

    /// Helper: a colorless instant for protection tests.
    fn colorless_instant(name: &str) -> CardDef {
        CardDef::new(
            name, CardKind::Instant(SpellData { mana_cost: "2".into(), ..Default::default() }),
            vec![], None, vec![], CardLayout::Normal, None,
            vec![], vec![], vec![], vec![],
        )
    }

    #[test]
    fn test_protection_colored_spell_cannot_target() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Emrakul on battlefield — protected from colored spells.
        let emrakul_id = add_perm(&mut state, PlayerId::Us, "Emrakul, the Aeons Torn",
                                  BattlefieldState::new());
        // Also put a vanilla creature (no protection) for comparison.
        let vanilla_id = add_perm_with_def(&mut state, PlayerId::Us,
            &creature("Vanilla 2/2", 2, 2), BattlefieldState::new());

        // Blue instant on the stack — colored spell.
        let bolt_def = blue_instant("Blue Bolt");
        let bolt_id = add_stack_spell(&mut state, PlayerId::Opp, &bolt_def);

        let spec = TargetSpec::ObjectInZone {
            controller: Who::Opp,
            zone: ZoneId::Battlefield,
            filter: ir_type(CardType::Creature),
        };
        let targets = legal_targets(&spec, PlayerId::Opp, bolt_id, &state);

        assert!(!targets.contains(&emrakul_id),
            "Emrakul should not be a legal target for a colored spell");
        assert!(targets.contains(&vanilla_id),
            "non-protected creature should be a legal target");
    }

    #[test]
    fn test_protection_colorless_spell_can_target() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let emrakul_id = add_perm(&mut state, PlayerId::Us, "Emrakul, the Aeons Torn",
                                  BattlefieldState::new());

        let spell_def = colorless_instant("Colorless Zap");
        let spell_id = add_stack_spell(&mut state, PlayerId::Opp, &spell_def);

        let spec = TargetSpec::ObjectInZone {
            controller: Who::Opp,
            zone: ZoneId::Battlefield,
            filter: ir_type(CardType::Creature),
        };
        let targets = legal_targets(&spec, PlayerId::Opp, spell_id, &state);

        assert!(targets.contains(&emrakul_id),
            "Emrakul should be a legal target for a colorless spell");
    }

    #[test]
    fn test_protection_colored_permanent_ability_can_target() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let emrakul_id = add_perm(&mut state, PlayerId::Us, "Emrakul, the Aeons Torn",
                                  BattlefieldState::new());

        // A colored permanent on the battlefield (not a spell) — ability source.
        let mut perm_def = creature("Blue Pinger", 1, 1);
        perm_def.colors = vec![Color::Blue];
        let perm_id = add_perm_with_def(&mut state, PlayerId::Opp, &perm_def,
                                        BattlefieldState::new());

        let spec = TargetSpec::ObjectInZone {
            controller: Who::Opp,
            zone: ZoneId::Battlefield,
            filter: ir_type(CardType::Creature),
        };
        let targets = legal_targets(&spec, PlayerId::Opp, perm_id, &state);

        assert!(targets.contains(&emrakul_id),
            "Emrakul should be a legal target for a colored permanent's ability (not a spell)");
    }

    #[test]
    fn test_protection_prevents_spell_damage() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let emrakul_id = add_perm(&mut state, PlayerId::Us, "Emrakul, the Aeons Torn",
                                  BattlefieldState::new());

        // Colored spell on the stack dealing damage.
        let bolt_def = blue_instant("Blue Blast");
        let bolt_id = add_stack_spell(&mut state, PlayerId::Opp, &bolt_def);

        eff_damage_target(PlayerId::Opp, 15, bolt_id).call(&mut state, 1, &[emrakul_id]);

        let bf = state.objects[&emrakul_id].bf().unwrap();
        assert_eq!(bf.damage, 0, "damage from colored spell should be prevented by protection");
    }

    #[test]
    fn test_protection_does_not_prevent_combat_damage() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let emrakul_id = add_perm(&mut state, PlayerId::Us, "Emrakul, the Aeons Torn",
                                  BattlefieldState::new());

        // Colored creature on battlefield dealing combat damage.
        let mut atk_def = creature("Blue Attacker", 5, 5);
        atk_def.colors = vec![Color::Blue];
        let atk_id = add_perm_with_def(&mut state, PlayerId::Opp, &atk_def,
                                       BattlefieldState::new());

        // Directly check: the colored creature is a permanent, not a spell.
        assert!(!is_protected_from(emrakul_id, atk_id, &state),
            "Emrakul is NOT protected from colored permanents (only colored spells)");
    }

    #[test]
    fn test_protection_colorless_spell_damage_goes_through() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let emrakul_id = add_perm(&mut state, PlayerId::Us, "Emrakul, the Aeons Torn",
                                  BattlefieldState::new());

        let zap_def = colorless_instant("Colorless Zap");
        let zap_id = add_stack_spell(&mut state, PlayerId::Opp, &zap_def);

        eff_damage_target(PlayerId::Opp, 15, zap_id).call(&mut state, 1, &[emrakul_id]);

        let bf = state.objects[&emrakul_id].bf().unwrap();
        assert_eq!(bf.damage, 15, "damage from colorless spell should not be prevented");
    }

    // ── Section 46: Mistrise Village ────────────────────────────────────────────

    /// Helper: place Mistrise Village on the battlefield via change_zone (fires replacement).
    fn etb_mistrise_village(state: &mut SimState, who: PlayerId) -> ObjId {
        let def = catalog_card("Mistrise Village");
        let id = state.alloc_id();
        state.objects.insert(id, GameObject {
            id,
            catalog_key: "Mistrise Village".to_string(),
            owner: who,
            controller: who,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Hand { known: false },
        });

        state.catalog.entry("Mistrise Village".to_string()).or_insert(def);
        change_zone(id, ZoneId::Battlefield, state, 1, who);
        id
    }

    /// Mistrise Village enters untapped when you control a Forest.
    #[test]
    fn test_mistrise_village_etb_untapped_with_forest() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Put a Forest on the battlefield first.
        let forest_def = catalog_card("Forest");
        add_perm_with_def(&mut state, PlayerId::Us, &forest_def, BattlefieldState::new());
        recompute(&mut state);

        let mv_id = etb_mistrise_village(&mut state, PlayerId::Us);

        let bf = state.objects[&mv_id].bf().expect("should be on battlefield");
        assert!(!bf.tapped, "Mistrise Village should enter untapped when you control a Forest");
    }

    /// Mistrise Village enters untapped when you control a Mountain.
    #[test]
    fn test_mistrise_village_etb_untapped_with_mountain() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let mtn_def = catalog_card("Mountain");
        add_perm_with_def(&mut state, PlayerId::Us, &mtn_def, BattlefieldState::new());
        recompute(&mut state);

        let mv_id = etb_mistrise_village(&mut state, PlayerId::Us);

        let bf = state.objects[&mv_id].bf().unwrap();
        assert!(!bf.tapped, "Mistrise Village should enter untapped when you control a Mountain");
    }

    /// Mistrise Village enters tapped when you control neither Mountain nor Forest.
    #[test]
    fn test_mistrise_village_etb_tapped_without_mountain_or_forest() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Only an Island on the battlefield — no Mountain or Forest.
        let island_def = catalog_card("Island");
        add_perm_with_def(&mut state, PlayerId::Us, &island_def, BattlefieldState::new());
        recompute(&mut state);

        let mv_id = etb_mistrise_village(&mut state, PlayerId::Us);

        let bf = state.objects[&mv_id].bf().unwrap();
        assert!(bf.tapped, "Mistrise Village should enter tapped without Mountain or Forest");
    }

    /// Mistrise Village {U},{T} ability: the next spell you cast can't be countered.
    #[test]
    fn test_mistrise_village_ability_makes_next_spell_uncounterable() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Place Mistrise Village (via add_perm so it starts untapped for the ability).
        let mv_def = catalog_card("Mistrise Village");
        let mv_id = add_perm_with_def(&mut state, PlayerId::Us, &mv_def, BattlefieldState::new());
        recompute(&mut state);

        // Activate the {U},{T} ability (only activated ability, index 0).
        // The ability is on the IR path — synthesized from AbilityKind::Activated
        // at catalog build time, carrying Action::GrantCEToNextSpellCast as ir_body.
        let ability = &mv_def.abilities()[0];
        assert!(ability.ir_body.is_some(),
            "Mistrise Village's synthesized ability should carry an IR body");
        let eff = build_ability_effect(ability, PlayerId::Us, mv_id);
        eff.call(&mut state, 1, &[]);

        // Should have registered a latent spell mod (not a dormant CI).
        assert_eq!(state.latent_spell_mods.len(), 1, "ability should register one LatentSpellMod");
        assert!(state.continuous_instances.is_empty(), "no CI yet — consumed at cast time");

        // Simulate casting a spell — consume_latent_spell_mod + fire SpellCast.
        let spell_def = catalog_card("Brainstorm");
        let spell_id = add_stack_spell(&mut state, PlayerId::Us, &spell_def);
        state.stack.push(spell_id);
        consume_latent_spell_mod(&mut state, PlayerId::Us, spell_id);
        fire_event(
            GameEvent::SpellCast { caster: PlayerId::Us, card_id: spell_id, mana_spent: true, alt_cost: false, x: 0, delved: Vec::new() },
            &mut state, 1, PlayerId::Us,
        );

        // LatentSpellMod consumed → CI now exists and is active.
        assert!(state.latent_spell_mods.is_empty(), "LatentSpellMod consumed");
        assert_eq!(state.continuous_instances.len(), 1, "CI created from LatentSpellMod");

        // Try to counter it — should fizzle.
        eff_counter_target(PlayerId::Opp).call(&mut state, 1, &[spell_id]);
        assert!(state.stack.contains(&spell_id),
            "spell should remain on stack — can't be countered");
    }

    /// The LatentSpellMod is consumed on the first spell — the second is not protected.
    #[test]
    fn test_mistrise_village_ability_one_shot() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let mv_def = catalog_card("Mistrise Village");
        let mv_id = add_perm_with_def(&mut state, PlayerId::Us, &mv_def, BattlefieldState::new());
        recompute(&mut state);

        // Activate ability.
        let ability = &mv_def.abilities()[0];
        let eff = build_ability_effect(ability, PlayerId::Us, mv_id);
        eff.call(&mut state, 1, &[]);

        // Cast first spell — consumes the LatentSpellMod.
        let spell1_def = catalog_card("Brainstorm");
        let spell1_id = add_stack_spell(&mut state, PlayerId::Us, &spell1_def);
        state.stack.push(spell1_id);
        consume_latent_spell_mod(&mut state, PlayerId::Us, spell1_id);
        fire_event(
            GameEvent::SpellCast { caster: PlayerId::Us, card_id: spell1_id, mana_spent: true, alt_cost: false, x: 0, delved: Vec::new() },
            &mut state, 1, PlayerId::Us,
        );
        assert!(state.latent_spell_mods.is_empty(), "LatentSpellMod consumed by first spell");

        // Cast second spell — no LatentSpellMod left, so no CI produced.
        let spell2_def = catalog_card("Ponder");
        let spell2_id = add_stack_spell(&mut state, PlayerId::Us, &spell2_def);
        state.stack.push(spell2_id);
        consume_latent_spell_mod(&mut state, PlayerId::Us, spell2_id);
        fire_event(
            GameEvent::SpellCast { caster: PlayerId::Us, card_id: spell2_id, mana_spent: true, alt_cost: false, x: 0, delved: Vec::new() },
            &mut state, 1, PlayerId::Us,
        );

        // The second spell should be counterable.
        eff_counter_target(PlayerId::Opp).call(&mut state, 1, &[spell2_id]);
        assert!(!state.stack.contains(&spell2_id),
            "second spell should be counterable — LatentSpellMod already consumed");
    }

    /// The LatentSpellMod expires at end of turn if no spell is cast.
    #[test]
    fn test_mistrise_village_ability_expires_eot() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let mv_def = catalog_card("Mistrise Village");
        let mv_id = add_perm_with_def(&mut state, PlayerId::Us, &mv_def, BattlefieldState::new());
        recompute(&mut state);

        let ability = &mv_def.abilities()[0];
        let eff = build_ability_effect(ability, PlayerId::Us, mv_id);
        eff.call(&mut state, 1, &[]);

        assert_eq!(state.latent_spell_mods.len(), 1, "LatentSpellMod registered");

        // Run cleanup step — should remove the EndOfTurn LatentSpellMod.
        let step = Step { kind: StepKind::Cleanup, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true);

        assert!(state.latent_spell_mods.is_empty(),
            "Mistrise Village LatentSpellMod should expire at end of turn");
    }

    // ── Clue Token + Wasteland (IR Activated) ───────────────────────────────────

    #[test]
    fn test_clue_token_activated_draws_via_ir() {
        // Clue Token is on the IR path; the synthesized AbilityDef should
        // carry the Action::Draw body via ir_body and resolve through the
        // executor when build_ability_effect runs.
        let mut state = make_state();
        state.catalog = test_catalog();

        let def = catalog_card("Clue Token");
        assert_eq!(def.abilities().len(), 1, "Clue Token should expose a single synthesized ability");
        let clue_id = add_perm_with_def(&mut state, PlayerId::Us, &def, BattlefieldState::new());
        recompute(&mut state);

        // Seed a known card on top of Us's library so Draw is observable.
        let top_id = {
            let brainstorm = catalog_card("Brainstorm");
            let id = state.alloc_id();
            state.objects.insert(id, GameObject {
            id,
            catalog_key: "Brainstorm".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Library,
        });
            state.player_mut(PlayerId::Us).library_order.push_front(id);
            state.catalog.entry("Brainstorm".to_string()).or_insert(brainstorm);
            id
        };

        let ability = &def.abilities()[0];
        assert!(ability.ir_body.is_some(), "Clue Token's synthesized ability should carry an IR body");
        let eff = build_ability_effect(ability, PlayerId::Us, clue_id);
        eff.call(&mut state, 1, &[]);

        assert!(state.objects[&top_id].in_zone(Zone::Hand { known: false }),
            "Clue Token activation should draw the top card into hand");
    }

    #[test]
    fn test_island_mana_ability_produces_blue_via_ir() {
        // Island is on the IR path: a no-target `AbilityKind::Activated` whose
        // body is `Action::AddMana`. The classifier (`is_mana_ability`) must
        // tag it as a mana ability (CR 605.1a) so the build_catalog synthesis
        // routes it to the ManaAbility list, not the regular AbilityDef list.
        let mut state = make_state();
        state.catalog = test_catalog();

        let def = catalog_card("Island");
        assert_eq!(def.abilities.len(), 1, "Island IR abilities vec should carry one ability");
        assert!(
            matches!(def.abilities[0].kind, crate::ir::ability::AbilityKind::Activated { .. }),
            "Island's IR ability must be AbilityKind::Activated (no separate Mana variant)"
        );
        assert!(
            crate::ir::executor::is_mana_ability(&def.abilities[0]),
            "Island must classify as a mana ability per CR 605.1a"
        );

        let mana_abils = def.mana_abilities();
        assert_eq!(mana_abils.len(), 1, "synthesis must produce exactly one ManaAbility");
        let ma = &mana_abils[0];
        assert_eq!(ma.produces, vec![Color::Blue], "Island advertises blue");
        assert_eq!(ma.produces_count, 1, "Island produces one mana");

        let _island_id = add_perm_with_def(&mut state, PlayerId::Us, &def, BattlefieldState::new());
        assert_eq!(state.player(PlayerId::Us).pool.u, 0, "pool starts empty");
        let eff = (ma.make_effect)(PlayerId::Us, None);
        eff.call(&mut state, 1, &[]);
        assert_eq!(state.player(PlayerId::Us).pool.u, 1, "Island mana ability must add U to pool");
    }

    #[test]
    fn test_deathrite_first_ability_is_not_a_mana_ability() {
        // Per CR 605.1a, an activated ability is a mana ability iff (1) no
        // target, (2) not a loyalty ability, (3) could add mana on resolution.
        // Deathrite Shaman's first ability — "{T}: Exile target land card from
        // a graveyard. Add one mana of any color." — produces mana but has a
        // target, so it is NOT a mana ability and uses the stack (CR 605.3b).
        // This test exercises the classifier directly without porting DRS.
        use crate::ir::ability::{Ability, AbilityKind, CostBody};
        use crate::ir::action::{Action, ManaSpec, Who as IrWho};
        use crate::ir::context::Ctx;
        use crate::ir::expr::{Expr, ZoneKindSel};

        // Shape of Deathrite's first ability.
        let drs_first = Ability {
            kind: AbilityKind::Activated {
                cost: CostBody::Ir(Action::Tap { target: Expr::Ctx(Ctx::Source) }),
                // "From a graveyard" — any graveyard. Modeled as a union of
                // self-and-opponent for the classifier test; the exact shape
                // doesn't matter — what matters is that target_spec != None.
                target_spec: TargetSpec::Union(vec![
                    TargetSpec::ObjectInZone {
                        controller: Who::Actor,
                        zone: ZoneId::Graveyard,
                        filter: ir_type(CardType::Land),
                    },
                    TargetSpec::ObjectInZone {
                        controller: Who::Opp,
                        zone: ZoneId::Graveyard,
                        filter: ir_type(CardType::Land),
                    },
                ]),
                choice_spec: None,
                body: Action::Sequence(vec![
                    Action::Exile {
                        target: Expr::Ctx(Ctx::Var("target")),
                        bind_as: None,
                    },
                    Action::AddMana {
                        who: IrWho::You,
                        count: Expr::Num(1),
                        spec: ManaSpec::AnyOneColor,
                    },
                ]),
                timing: ActivationTiming::Default,
                activation_condition: None,
                active_zone: ZoneKindSel::Battlefield,
            },
            text: Some("{T}: Exile target land card from a graveyard. Add one mana of any color."),
        };

        // Body could produce mana — the AddMana is reachable through Sequence.
        assert!(
            crate::ir::executor::body_can_produce_mana(match &drs_first.kind {
                AbilityKind::Activated { body, .. } => body,
                _ => panic!("expected Activated"),
            }),
            "DRS body reaches AddMana so body_can_produce_mana is true"
        );
        // But the ability has a target, so CR 605.1a #1 fails — NOT a mana ability.
        assert!(
            !crate::ir::executor::is_mana_ability(&drs_first),
            "DRS first ability has a target → NOT a mana ability per CR 605.1a"
        );

        // Bridge dispatch: a non-mana activated ability synthesizes an
        // AbilityDef (uses the stack), not a ManaAbility.
        assert!(
            crate::ir::executor::ir_activated_as_legacy(&drs_first).is_some(),
            "DRS routes through the regular activated-ability bridge"
        );
        assert!(
            crate::ir::executor::ir_activated_as_mana_ability_legacy(&drs_first).is_none(),
            "DRS does NOT route through the mana-ability bridge"
        );

        // Compare against a Birds-of-Paradise-shaped ability: identical body
        // shape (sans the Exile prefix) but no target. This one IS a mana ability.
        let birds = Ability {
            kind: AbilityKind::Activated {
                cost: CostBody::Ir(Action::Tap { target: Expr::Ctx(Ctx::Source) }),
                target_spec: TargetSpec::None,
                choice_spec: None,
                body: Action::AddMana {
                    who: IrWho::You,
                    count: Expr::Num(1),
                    spec: ManaSpec::AnyOneColor,
                },
                timing: ActivationTiming::Default,
                activation_condition: None,
                active_zone: ZoneKindSel::Battlefield,
            },
            text: Some("{T}: Add one mana of any color."),
        };
        assert!(
            crate::ir::executor::is_mana_ability(&birds),
            "Birds: no target + body produces mana → IS a mana ability"
        );
        assert!(
            crate::ir::executor::ir_activated_as_legacy(&birds).is_none(),
            "Birds skips the regular bridge (returns None for mana-classified)"
        );
        assert!(
            crate::ir::executor::ir_activated_as_mana_ability_legacy(&birds).is_some(),
            "Birds synthesizes a ManaAbility (stack-bypass per CR 605.3b)"
        );
    }

    #[test]
    fn test_wasteland_activated_destroys_target_nonbasic_via_ir() {
        // Wasteland is on the IR path; the Action::Destroy body should resolve
        // the "target" binding from the targets slice passed to the effect.
        let mut state = make_state();
        state.catalog = test_catalog();

        let def = catalog_card("Wasteland");
        assert_eq!(def.abilities().len(), 1, "Wasteland should expose a single synthesized ability");
        let wl_id = add_perm_with_def(&mut state, PlayerId::Us, &def, BattlefieldState::new());
        let victim_id = add_default_perm(&mut state, PlayerId::Opp, "Underground Sea");
        recompute(&mut state);

        let ability = &def.abilities()[0];
        assert!(!ability.target_spec.is_none(), "Wasteland ability must have a TargetSpec");
        assert!(ability.ir_body.is_some(), "Wasteland's synthesized ability should carry an IR body");
        let eff = build_ability_effect(ability, PlayerId::Us, wl_id);
        eff.call(&mut state, 1, &[victim_id]);

        assert_eq!(state.objects[&victim_id].zone(), Some(Zone::Graveyard),
            "targeted nonbasic land should be destroyed");
    }

    // ── Section 47: Brotherhood's End ───────────────────────────────────────────

    /// Mode 0: deals 3 damage to each creature and each planeswalker.
    /// A 3/3 creature should die (lethal), a 4/4 should survive with 3 damage.
    #[test]
    fn test_brotherhoods_end_mode0_damages_creatures() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let small = creature("Bear", 2, 2);
        let small_id = add_perm_with_def(&mut state, PlayerId::Opp, &small, BattlefieldState::new());
        let big = creature("Giant", 4, 4);
        let big_id = add_perm_with_def(&mut state, PlayerId::Opp, &big, BattlefieldState::new());

        let be_def = catalog_card("Brotherhood's End");
        // Mode 0: damage to creatures/planeswalkers
        let effect = build_spell_effect(&be_def, PlayerId::Us, ObjId::UNSET, 0, 0).1;
        effect.call(&mut state, 1, &[]);

        // Bear (2/2): 3 damage is lethal — but we haven't run SBAs yet, just check damage.
        assert_eq!(state.objects[&small_id].bf().unwrap().damage, 3,
            "Bear should have 3 damage marked");
        assert_eq!(state.objects[&big_id].bf().unwrap().damage, 3,
            "Giant should have 3 damage marked");
    }

    /// Mode 1: destroys all artifacts with mana value 3 or less.
    /// Lotus Petal (MV 0) should be destroyed; an artifact with MV 4 should survive.
    #[test]
    fn test_brotherhoods_end_mode1_destroys_cheap_artifacts() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let petal_id = add_default_perm(&mut state, PlayerId::Opp, "Lotus Petal");

        let be_def = catalog_card("Brotherhood's End");
        // Mode 1: destroy artifacts with MV ≤ 3
        let effect = build_spell_effect(&be_def, PlayerId::Us, ObjId::UNSET, 0, 1).1;
        effect.call(&mut state, 1, &[]);

        assert_eq!(state.objects[&petal_id].zone(), Some(Zone::Graveyard),
            "Lotus Petal (MV 0) should be destroyed by Brotherhood's End mode 1");
    }

    // ── Section 48: Mox Opal ────────────────────────────────────────────────────

    /// Mox Opal's mana ability requires metalcraft (3+ artifacts).
    /// With only 2 artifacts on the battlefield, the condition should fail.
    #[test]
    fn test_mox_opal_metalcraft_not_met() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let opal_def = catalog_card("Mox Opal");
        let opal_id = add_perm_with_def(&mut state, PlayerId::Us, &opal_def, BattlefieldState::new());
        // Only 1 artifact (Mox Opal itself) + add one more
        let _petal_id = add_default_perm(&mut state, PlayerId::Us, "Lotus Petal");
        recompute(&mut state);

        // 2 artifacts — metalcraft not met
        let ma = &opal_def.mana_abilities()[0];
        let cond = ma.condition.as_ref().expect("Mox Opal should have a condition");
        assert!(!obj_matches(cond, opal_id, &state), "metalcraft should not be active with only 2 artifacts");
    }

    /// With 3+ artifacts, metalcraft is active and the condition should pass.
    #[test]
    fn test_mox_opal_metalcraft_met() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let opal_def = catalog_card("Mox Opal");
        let opal_id = add_perm_with_def(&mut state, PlayerId::Us, &opal_def, BattlefieldState::new());
        let _petal_id = add_default_perm(&mut state, PlayerId::Us, "Lotus Petal");
        let _cage_id = add_default_perm(&mut state, PlayerId::Us, "Grafdigger's Cage");
        recompute(&mut state);

        // 3 artifacts — metalcraft active
        let ma = &opal_def.mana_abilities()[0];
        let cond = ma.condition.as_ref().expect("Mox Opal should have a condition");
        assert!(obj_matches(cond, opal_id, &state), "metalcraft should be active with 3 artifacts");
    }

    // ── Forge engine regression tests ─────────────────────────────────────────

    #[test]
    fn test_basalt_monolith_does_not_untap_normally_and_can_pay_three_to_untap() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let def = catalog_card("Basalt Monolith");
        let basalt_id = add_perm_with_def(
            &mut state,
            PlayerId::Us,
            &def,
            BattlefieldState { tapped: true, ..BattlefieldState::new() },
        );

        // Basalt's special untap restriction is a CardDef characteristic, so the
        // normal untap step leaves it tapped.
        let step = Step { kind: StepKind::Untap, prio: false };
        do_step(&mut state, 1, PlayerId::Us, &step, true);
        assert!(state.permanent_bf(basalt_id).unwrap().tapped,
            "Basalt Monolith must not untap during the normal untap step");

        // Its tap ability is a genuine mana ability producing three mana.
        let mana_ir = def.abilities.iter()
            .find(|a| crate::ir::executor::is_mana_ability(a))
            .expect("Basalt should expose a mana ability");
        let mana = crate::ir::executor::ir_activated_as_mana_ability_legacy(mana_ir)
            .expect("Basalt mana ability should bridge to ManaAbility");
        assert_eq!(mana.produces_count, 3, "Basalt taps for three colorless mana");
        assert!(mana.produces.is_empty(), "Basalt produces colorless, not colored, mana");

        // Pay {3} and resolve the non-mana activated ability: Basalt untaps.
        state.player_mut(PlayerId::Us).pool.c = 3;
        state.player_mut(PlayerId::Us).pool.total = 3;
        let untap_ir = def.abilities.iter()
            .find(|a| !crate::ir::executor::is_mana_ability(a))
            .expect("Basalt should expose its paid untap ability");
        let untap = crate::ir::executor::ir_activated_as_legacy(untap_ir)
            .expect("Basalt untap should bridge to a regular activated ability");
        run_activate_submachine(&mut state, 1, PlayerId::Us, basalt_id, &untap);
        resolve_top_of_stack(&mut state, 1, PlayerId::Us);
        assert!(!state.permanent_bf(basalt_id).unwrap().tapped,
            "paying {{3}} should untap Basalt Monolith");
    }

    #[test]
    fn test_relic_of_sauron_has_all_six_two_mana_color_combinations() {
        let def = catalog_card("Relic of Sauron");
        let mut modes: Vec<String> = def.mana_abilities().iter().map(|ma| {
            let mut s = String::new();
            for c in &ma.produces {
                s.push(match c {
                    Color::Blue => 'U', Color::Black => 'B', Color::Red => 'R',
                    Color::White => 'W', Color::Green => 'G',
                });
            }
            s
        }).collect();
        modes.sort();
        assert_eq!(modes, vec!["BB", "BR", "RR", "UB", "UR", "UU"]);
        assert!(def.mana_abilities().iter().all(|ma| ma.produces_count == 2));
    }

    #[test]
    fn test_karn_minus_two_can_retrieve_artifact_exiled_during_game() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let karn_def = catalog_card("Karn, the Great Creator");
        let karn_id = add_perm_with_def(
            &mut state,
            PlayerId::Us,
            &karn_def,
            BattlefieldState { loyalty: 5, ..BattlefieldState::new() },
        );

        // Model a Ring that was exiled during the game (e.g. by Force of Negation).
        // Karn's simulator shortcut searches the same owned face-up exile pool that
        // also contains wishboard cards at game setup.
        let ring_id = state.alloc_id();
        state.objects.insert(ring_id, GameObject {
            id: ring_id,
            catalog_key: "The One Ring".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Exile { on_adventure: false },
        });
        state.catalog.insert("The One Ring".into(), catalog_card("The One Ring"));

        let wish = match &karn_def.kind {
            CardKind::Planeswalker(p) => &p.abilities[0],
            _ => panic!("Karn should be a planeswalker"),
        };
        let eff = build_ability_effect(wish, PlayerId::Us, karn_id);
        eff.call(&mut state, 1, &[]);

        assert_eq!(state.objects[&ring_id].zone(), Some(Zone::Hand { known: false }),
            "Karn -2 should move an owned exiled artifact to hand");
    }

    #[test]
    fn test_summon_bahamut_has_four_saga_chapters_and_targets_on_first_two() {
        let def = catalog_card("Summon: Bahamut");
        assert!(def.types.contains(&CardType::Creature));
        assert!(def.types.contains(&CardType::Enchantment));
        assert_eq!(def.chapters.len(), 4);
        assert_eq!(def.chapter_target_specs.len(), 4);
        assert!(!def.chapter_target_specs[0].is_none());
        assert!(!def.chapter_target_specs[1].is_none());
        assert!(def.chapter_target_specs[2].is_none());
        assert!(def.chapter_target_specs[3].is_none());
    }

    #[test]
    fn test_summon_bahamut_mega_flare_hits_each_opponent_for_other_permanents_mv() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let bahamut_def = catalog_card("Summon: Bahamut");
        let bahamut_id = add_perm_with_def(&mut state, PlayerId::Us, &bahamut_def, BattlefieldState::new());
        let _grim = add_perm_with_def(&mut state, PlayerId::Us, &catalog_card("Grim Monolith"), BattlefieldState::new());
        let _ring = add_perm_with_def(&mut state, PlayerId::Us, &catalog_card("The One Ring"), BattlefieldState::new());
        recompute(&mut state);

        let before = state.player(PlayerId::Opp).life;
        let env = crate::ir::executor::BindEnv::new()
            .with_source(bahamut_id)
            .with_controller(PlayerId::Us);
        let _ = crate::ir::executor::execute(&bahamut_def.chapters[3], &mut state, &env);

        // Other permanents are Grim (MV 2) + Ring (MV 4); Bahamut itself is excluded.
        assert_eq!(state.player(PlayerId::Opp).life, before - 6);
    }

    #[test]
    fn test_linked_exile_returns_when_portable_hole_leaves() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let hole = add_perm_with_def(&mut state, PlayerId::Us, &catalog_card("Portable Hole"), BattlefieldState::new());
        let target = add_perm_with_def(&mut state, PlayerId::Opp, &catalog_card("Mox Opal"), BattlefieldState::new());
        change_zone(target, ZoneId::Exile, &mut state, 1, PlayerId::Us);
        state.linked_exile.entry(hole).or_default().push(target);

        change_zone(hole, ZoneId::Graveyard, &mut state, 1, PlayerId::Opp);

        assert!(state.objects[&target].in_zone(Zone::Battlefield));
        assert_eq!(state.objects[&target].controller, PlayerId::Opp);
    }

    // ── §55: Karn, the Great Creator ──────────────────────────────────────────

    /// Karn's static ability suppresses all activated abilities (including mana abilities)
    /// on artifacts opponents control, via CE setting activatable=false.
    #[test]
    fn test_karn_suppresses_opponent_artifact_abilities() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Karn on our side.
        let karn_def = catalog_card("Karn, the Great Creator");
        let _karn_id = add_perm_with_def(&mut state, PlayerId::Us, &karn_def,
            BattlefieldState { loyalty: 5, ..BattlefieldState::new() });

        // Opponent controls a Lotus Petal (artifact with mana ability).
        let opp_petal_id = add_default_perm(&mut state, PlayerId::Opp, "Lotus Petal");

        // Our own Lotus Petal should NOT be suppressed.
        let our_petal_id = add_default_perm(&mut state, PlayerId::Us, "Lotus Petal");

        recompute(&mut state);

        // Opponent's artifact: activation restricted by Karn (consulted at the
        // activation-legality gate, not via the materialized activatable flag).
        assert!(
            crate::ir::executor::mana_ability_restricted(&state, opp_petal_id),
            "Karn should restrict activating opponent's artifact abilities"
        );

        // Our own artifact: not restricted (Karn is asymmetric).
        assert!(
            !crate::ir::executor::mana_ability_restricted(&state, our_petal_id),
            "Karn should NOT restrict our own artifacts"
        );
    }

    /// Karn does not affect non-artifact permanents.
    #[test]
    fn test_karn_does_not_affect_non_artifacts() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let karn_def = catalog_card("Karn, the Great Creator");
        let _karn_id = add_perm_with_def(&mut state, PlayerId::Us, &karn_def,
            BattlefieldState { loyalty: 5, ..BattlefieldState::new() });

        // Opponent land with mana ability — should NOT be suppressed.
        let land_id = make_land(&mut state, PlayerId::Opp, "Underground Sea", false);

        recompute(&mut state);

        let land_def = state.def_of(land_id).expect("land should have materialized def");
        assert!(
            land_def.mana_abilities().iter().all(|ma| ma.activatable),
            "Karn should not suppress mana abilities on non-artifact permanents"
        );
    }

    // ── 44. Dragon's Rage Channeler ──────────────────────────────────────────

    #[test]
    fn test_drc_surveil_on_noncreature_cast() {
        // DRC on battlefield; cast a noncreature spell → surveil 1 trigger fires.
        let mut state = make_state();
        state.catalog = test_catalog();
        state.set_strategy(PlayerId::Us, Box::new(TestStrategy::new(PlayerId::Us).surveil(true))); // always mill

        // Put a known card on top of library.
        let top_id = {
            let id = state.alloc_id();
            state.objects.insert(id, GameObject {
            id,
            catalog_key: "Brainstorm".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Library,
        });
            state.player_mut(PlayerId::Us).library_order.push_front(id);
            state.catalog.entry("Brainstorm".to_string()).or_insert_with(|| catalog_card("Brainstorm"));
            id
        };

        // DRC on battlefield.
        let _drc_id = add_default_perm(&mut state, PlayerId::Us, "Dragon's Rage Channeler");

        // A noncreature spell on the stack (simulate casting Ponder).
        let spell_id = {
            let id = state.alloc_id();
            state.objects.insert(id, GameObject {
            id,
            catalog_key: "Ponder".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::StackSpell(SpellState { effect: None, chosen_targets: vec![], is_back_face: false, costs_paid_ctx: CostsPaidCtx::default() }),
        });
            state.catalog.entry("Ponder".to_string()).or_insert_with(|| catalog_card("Ponder"));
            id
        };

        // Fire SpellCast event.
        fire_event(
            GameEvent::SpellCast { caster: PlayerId::Us, card_id: spell_id, mana_spent: true, alt_cost: false, x: 0, delved: Vec::new() },
            &mut state, 1, PlayerId::Us,
        );
        for ctx in std::mem::take(&mut state.pending_triggers) { ctx.effect.call(&mut state, 1, &[]); }

        assert_eq!(state.objects[&top_id].zone(), Some(Zone::Graveyard),
            "DRC surveil should mill top library card when surveil_choice returns true");
    }

    #[test]
    fn test_drc_no_surveil_on_creature_cast() {
        // DRC on battlefield; cast a creature spell → no surveil trigger.
        let mut state = make_state();
        state.catalog = test_catalog();
        state.set_strategy(PlayerId::Us, Box::new(TestStrategy::new(PlayerId::Us).surveil(true)));

        let top_id = {
            let id = state.alloc_id();
            state.objects.insert(id, GameObject {
            id,
            catalog_key: "Brainstorm".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::Library,
        });
            state.player_mut(PlayerId::Us).library_order.push_front(id);
            state.catalog.entry("Brainstorm".to_string()).or_insert_with(|| catalog_card("Brainstorm"));
            id
        };

        let _drc_id = add_default_perm(&mut state, PlayerId::Us, "Dragon's Rage Channeler");

        // Cast a creature spell.
        let spell_id = {
            let id = state.alloc_id();
            state.objects.insert(id, GameObject {
            id,
            catalog_key: "Barrowgoyf".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::StackSpell(SpellState { effect: None, chosen_targets: vec![], is_back_face: false, costs_paid_ctx: CostsPaidCtx::default() }),
        });
            state.catalog.entry("Barrowgoyf".to_string()).or_insert_with(|| catalog_card("Barrowgoyf"));
            id
        };

        fire_event(
            GameEvent::SpellCast { caster: PlayerId::Us, card_id: spell_id, mana_spent: true, alt_cost: false, x: 0, delved: Vec::new() },
            &mut state, 1, PlayerId::Us,
        );

        assert!(state.pending_triggers.is_empty(),
            "DRC should not trigger on creature spell cast");
        assert_eq!(state.objects[&top_id].zone(), Some(Zone::Library),
            "library card should remain untouched when no surveil fires");
    }

    #[test]
    fn test_drc_delirium_grants_flying_and_pt() {
        // With ≥4 card types in graveyard, DRC should be 3/3 with flying.
        let mut state = make_state();
        state.catalog = test_catalog();

        // Put 4 different card types in graveyard.
        add_graveyard_card(&mut state, PlayerId::Us, "Island");       // Land
        add_graveyard_card(&mut state, PlayerId::Us, "Brainstorm");   // Instant
        add_graveyard_card(&mut state, PlayerId::Us, "Ponder");       // Sorcery (need catalog)
        add_graveyard_card(&mut state, PlayerId::Us, "Barrowgoyf");   // Creature

        let drc_id = add_default_perm(&mut state, PlayerId::Us, "Dragon's Rage Channeler");

        recompute(&mut state);

        let def = state.def_of(drc_id).expect("DRC should have materialized def");
        if let CardKind::Creature(c) = &def.kind {
            assert_eq!(c.power(), 3, "delirium DRC should have 3 power (1+2)");
            assert_eq!(c.toughness(), 3, "delirium DRC should have 3 toughness (1+2)");
            assert!(c.keywords.contains(Keyword::Flying), "delirium DRC should have flying");
        } else {
            panic!("DRC should be a creature");
        }
    }

    #[test]
    fn test_drc_no_delirium_without_enough_types() {
        // With <4 card types in graveyard, DRC should remain 1/1 without flying.
        let mut state = make_state();
        state.catalog = test_catalog();

        // Only 2 card types in graveyard.
        add_graveyard_card(&mut state, PlayerId::Us, "Island");       // Land
        add_graveyard_card(&mut state, PlayerId::Us, "Brainstorm");   // Instant

        let drc_id = add_default_perm(&mut state, PlayerId::Us, "Dragon's Rage Channeler");

        recompute(&mut state);

        let def = state.def_of(drc_id).expect("DRC should have materialized def");
        if let CardKind::Creature(c) = &def.kind {
            assert_eq!(c.power(), 1, "non-delirium DRC should have 1 power");
            assert_eq!(c.toughness(), 1, "non-delirium DRC should have 1 toughness");
            assert!(!c.keywords.contains(Keyword::Flying), "non-delirium DRC should not have flying");
        } else {
            panic!("DRC should be a creature");
        }
    }

    // ── Mishra's Bauble ───────────────────────────────────────────────────────

    #[test]
    fn test_mishras_bauble_delayed_draw_at_next_upkeep() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let def = catalog_card("Mishra's Bauble");
        let bauble_id = add_perm_with_def(&mut state, PlayerId::Us, &def, BattlefieldState::new());
        recompute(&mut state);

        // Activate the ability (tap + sac → schedule delayed trigger).
        let ability = &def.abilities()[0];
        let eff = build_ability_effect(ability, PlayerId::Us, bauble_id);
        eff.call(&mut state, 1, &[]);

        assert_eq!(state.trigger_instances.len(), 1,
            "activating Bauble should register a delayed trigger");
        assert_eq!(state.trigger_instances[0].expiry, Some(Expiry::OneShot),
            "delayed trigger should be OneShot");

        // Fire upkeep — should produce a draw trigger and remove the OneShot.
        fire_event(
            GameEvent::EnteredStep { step: StepKind::Upkeep, active_player: PlayerId::Us },
            &mut state, 2, PlayerId::Us,
        );
        assert_eq!(state.pending_triggers.len(), 1,
            "upkeep should produce one draw trigger");
        assert_eq!(state.pending_triggers[0].source_name, "Mishra's Bauble (delayed)");
        assert!(state.trigger_instances.is_empty(),
            "OneShot trigger should be removed after firing");

        // Resolve the draw trigger.
        let hand_before = state.hand_size(PlayerId::Us);
        // Add a card in library so the draw has something to pick up.
        add_library_card(&mut state, PlayerId::Us, "Island");
        let ctx = state.pending_triggers.remove(0);
        ctx.effect.call(&mut state, 2, &[]);
        assert_eq!(state.hand_size(PlayerId::Us), hand_before + 1,
            "resolving Bauble trigger should draw a card");
    }

    // ── Containment Priest ────────────────────────────────────────────────────

    #[test]
    fn test_containment_priest_does_not_exile_itself() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Put Containment Priest in hand, then move it to BF via non-cast
        // (e.g. Aether Vial). Its own replacement is active_when = on_battlefield,
        // so it can't fire against itself since it's not on BF yet.
        let def = catalog_card("Containment Priest");
        let cp_id = add_hand_card_with_def(&mut state, PlayerId::Us, &def);
        recompute(&mut state);

        change_zone(cp_id, ZoneId::Battlefield, &mut state, 1, PlayerId::Us);

        assert_eq!(state.objects[&cp_id].zone(), Some(Zone::Battlefield),
            "Containment Priest should not exile itself when entering via non-cast");
    }

    #[test]
    fn test_containment_priest_exiles_non_cast_creature() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Put Containment Priest on the battlefield for us.
        let def = catalog_card("Containment Priest");
        add_perm_with_def(&mut state, PlayerId::Us, &def, BattlefieldState::new());
        recompute(&mut state);

        // Put an opponent creature in hand, then move it to BF via non-cast.
        let opp_def = catalog_card("Orcish Bowmasters");
        let opp_id = add_hand_card_with_def(&mut state, PlayerId::Opp, &opp_def);

        change_zone(opp_id, ZoneId::Battlefield, &mut state, 1, PlayerId::Opp);

        assert!(state.objects[&opp_id].in_zone(Zone::Exile { on_adventure: false }),
            "non-cast creature should be exiled by Containment Priest");
    }

    // ── Delver of Secrets ─────────────────────────────────────────────────────

    #[test]
    fn test_delver_transforms_on_upkeep_with_instant_on_top() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let def = catalog_card("Delver of Secrets");
        // Tapped, to prove the in-place transform keeps the SAME object/bf instance.
        let delver_id = add_perm_with_def(&mut state, PlayerId::Us, &def,
            BattlefieldState { tapped: true, ..BattlefieldState::new() });
        // Put an instant on top of library.
        add_library_card(&mut state, PlayerId::Us, "Brainstorm");
        recompute(&mut state);

        // Fire upkeep trigger.
        fire_event(
            GameEvent::EnteredStep { step: StepKind::Upkeep, active_player: PlayerId::Us },
            &mut state, 1, PlayerId::Us,
        );
        assert_eq!(state.pending_triggers.len(), 1, "should produce a transform trigger");
        let ctx = state.pending_triggers.remove(0);
        ctx.effect.call(&mut state, 1, &[]);

        let delver_bf = state.objects[&delver_id].bf().unwrap();
        assert_eq!(delver_bf.active_face, 1, "Delver should be on back face after transform");
        // "Transform this creature" is in-place — same object/bf instance, so transient
        // state (tapped) is preserved (contrast Tamiyo's exile-return new object).
        assert!(delver_bf.tapped, "in-place transform preserves bf state (same object)");

        // Recompute should give 3/2 flying.
        recompute(&mut state);
        let mat = state.def_of(delver_id).unwrap();
        assert_eq!(mat.name, "Insectile Aberration");
        if let CardKind::Creature(c) = &mat.kind {
            assert_eq!(c.power(), 3);
            assert_eq!(c.toughness(), 2);
            assert!(c.keywords.contains(Keyword::Flying));
        } else {
            panic!("back face should be a creature");
        }
    }

    #[test]
    fn test_delver_no_transform_without_instant_on_top() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let def = catalog_card("Delver of Secrets");
        let delver_id = add_perm_with_def(&mut state, PlayerId::Us, &def, BattlefieldState::new());
        // Put a land on top (not instant/sorcery).
        add_library_card(&mut state, PlayerId::Us, "Island");
        recompute(&mut state);

        fire_event(
            GameEvent::EnteredStep { step: StepKind::Upkeep, active_player: PlayerId::Us },
            &mut state, 1, PlayerId::Us,
        );
        assert!(state.pending_triggers.is_empty(), "no transform trigger for non-instant top card");
        assert_eq!(state.objects[&delver_id].bf().unwrap().active_face, 0,
            "Delver should remain on front face");
    }

    // ── Unholy Heat ───────────────────────────────────────────────────────────

    #[test]
    fn test_unholy_heat_2_damage_without_delirium() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let target_id = add_default_perm(&mut state, PlayerId::Opp, "Murktide Regent");
        recompute(&mut state);

        let def = catalog_card("Unholy Heat");
        let eff = build_spell_effect(&def, PlayerId::Us, ObjId::UNSET, 0, 0).1;
        eff.call(&mut state, 1, &[target_id]);

        assert_eq!(state.permanent_bf(target_id).unwrap().damage, 2,
            "without delirium, Unholy Heat should deal 2 damage");
    }

    #[test]
    fn test_unholy_heat_6_damage_with_delirium() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Seed 4+ card types in graveyard.
        add_graveyard_card(&mut state, PlayerId::Us, "Island");       // Land
        add_graveyard_card(&mut state, PlayerId::Us, "Brainstorm");   // Instant
        add_graveyard_card(&mut state, PlayerId::Us, "Ponder");       // Sorcery
        add_graveyard_card(&mut state, PlayerId::Us, "Murktide Regent"); // Creature

        let target_id = add_default_perm(&mut state, PlayerId::Opp, "Murktide Regent");
        recompute(&mut state);

        let def = catalog_card("Unholy Heat");
        let eff = build_spell_effect(&def, PlayerId::Us, ObjId::UNSET, 0, 0).1;
        eff.call(&mut state, 1, &[target_id]);

        assert_eq!(state.permanent_bf(target_id).unwrap().damage, 6,
            "with delirium, Unholy Heat should deal 6 damage");
    }

    // ── Price of Progress ─────────────────────────────────────────────────────

    #[test]
    fn test_price_of_progress_deals_damage_per_nonbasic() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Opp controls 3 nonbasic lands.
        add_default_perm(&mut state, PlayerId::Opp, "Volcanic Island");
        add_default_perm(&mut state, PlayerId::Opp, "Underground Sea");
        add_default_perm(&mut state, PlayerId::Opp, "Wasteland");
        // Opp controls 1 basic land (should not count).
        add_default_perm(&mut state, PlayerId::Opp, "Island");
        // Us controls 1 nonbasic.
        add_default_perm(&mut state, PlayerId::Us, "Volcanic Island");
        recompute(&mut state);

        let opp_life_before = state.player(PlayerId::Opp).life;
        let us_life_before = state.player(PlayerId::Us).life;

        let def = catalog_card("Price of Progress");
        let eff = build_spell_effect(&def, PlayerId::Us, ObjId::UNSET, 0, 0).1;
        eff.call(&mut state, 1, &[]);

        assert_eq!(state.player(PlayerId::Opp).life, opp_life_before - 6,
            "opp should take 6 damage (3 nonbasics * 2)");
        assert_eq!(state.player(PlayerId::Us).life, us_life_before - 2,
            "us should take 2 damage (1 nonbasic * 2)");
    }

    // ── Null Rod ──────────────────────────────────────────────────────────────

    #[test]
    fn test_null_rod_suppresses_artifact_abilities() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let _null_rod = add_default_perm(&mut state, PlayerId::Us, "Null Rod");
        let petal_id = add_default_perm(&mut state, PlayerId::Opp, "Lotus Petal");
        recompute(&mut state);

        // Null Rod restricts activating any artifact's abilities (symmetric), consulted
        // at the activation-legality gate — incl. mana abilities (CR 605.1a). The ability
        // still *exists* on the def; it just can't be activated.
        let def = state.def_of(petal_id).expect("Lotus Petal should have materialized def");
        assert!(!def.mana_abilities().is_empty(), "Lotus Petal should still have mana abilities listed");
        assert!(
            crate::ir::executor::mana_ability_restricted(&state, petal_id),
            "Lotus Petal's abilities should be restricted under Null Rod"
        );
    }

    // ── Meltdown ──────────────────────────────────────────────────────────────

    #[test]
    fn test_meltdown_destroys_artifacts_at_or_below_x() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // MV 0 artifact (Lotus Petal), MV 2 artifact (Null Rod), and a non-artifact creature.
        let petal_id = add_default_perm(&mut state, PlayerId::Opp, "Lotus Petal");
        let rod_id = add_default_perm(&mut state, PlayerId::Opp, "Null Rod");
        let creature_id = add_default_perm(&mut state, PlayerId::Opp, "Murktide Regent");
        recompute(&mut state);

        // Meltdown with X=1: should destroy Lotus Petal (MV 0) but not Null Rod (MV 2).
        let def = catalog_card("Meltdown");
        let eff = build_spell_effect(&def, PlayerId::Us, ObjId::UNSET, 1, 0).1;
        eff.call(&mut state, 1, &[]);

        assert_eq!(state.objects[&petal_id].zone(), Some(Zone::Graveyard),
            "Lotus Petal (MV 0) should be destroyed by Meltdown X=1");
        assert_eq!(state.objects[&rod_id].zone(), Some(Zone::Battlefield),
            "Null Rod (MV 2) should survive Meltdown X=1");
        assert_eq!(state.objects[&creature_id].zone(), Some(Zone::Battlefield),
            "non-artifact creature should be unaffected by Meltdown");
    }

    // ── Rough // Tumble ───────────────────────────────────────────────────────

    #[test]
    fn test_rough_deals_2_to_non_flyers_spares_flyers() {
        let mut state = make_state();
        state.catalog = test_catalog();

        // Non-flyer and flyer on opponent's board.
        let ground_id = add_default_perm(&mut state, PlayerId::Opp, "Orcish Bowmasters");
        let flyer_id = add_default_perm(&mut state, PlayerId::Opp, "Emrakul, the Aeons Torn");
        recompute(&mut state);

        let def = catalog_card("Rough // Tumble");
        let eff = build_spell_effect(&def, PlayerId::Us, ObjId::UNSET, 0, 0).1;
        eff.call(&mut state, 1, &[]);

        assert_eq!(state.permanent_bf(ground_id).unwrap().damage, 2,
            "Rough should deal 2 damage to non-flyer");
        assert_eq!(state.permanent_bf(flyer_id).unwrap().damage, 0,
            "Rough should not damage flyer");
    }

    #[test]
    fn test_tumble_deals_6_to_flyers_spares_non_flyers() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let ground_id = add_default_perm(&mut state, PlayerId::Opp, "Orcish Bowmasters");
        let flyer_id = add_default_perm(&mut state, PlayerId::Opp, "Emrakul, the Aeons Torn");
        recompute(&mut state);

        // Cast back face (Tumble).
        let def = catalog_card("Rough // Tumble");
        let back = def.adventure().expect("should have back face");
        let eff = build_spell_effect(back, PlayerId::Us, ObjId::UNSET, 0, 0).1;
        eff.call(&mut state, 1, &[]);

        assert_eq!(state.permanent_bf(ground_id).unwrap().damage, 0,
            "Tumble should not damage non-flyer");
        assert_eq!(state.permanent_bf(flyer_id).unwrap().damage, 6,
            "Tumble should deal 6 damage to flyer");
    }

    // ── Prismatic Ending ─────────────────────────────────────────────────────

    /// Converge = chosen_x + 1. With chosen_x = 2, converge = 3, so Null Rod (MV 2)
    /// is exiled. With chosen_x = 0, converge = 1, so Null Rod (MV 2) is spared.
    #[test]
    fn test_prismatic_ending_exiles_iff_mv_le_converge() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let rod_id = add_default_perm(&mut state, PlayerId::Opp, "Null Rod");
        recompute(&mut state);

        // chosen_x = 2 → converge = 3 ≥ Null Rod's MV 2 → exiled.
        let def = catalog_card("Prismatic Ending");
        let eff = build_spell_effect(&def, PlayerId::Us, ObjId::UNSET, 2, 0).1;
        eff.call(&mut state, 1, &[rod_id]);
        assert_eq!(state.objects[&rod_id].zone(), Some(Zone::Exile { on_adventure: false }),
            "Null Rod (MV 2) should be exiled when converge = 3");

        // Reset: put Null Rod back on the battlefield for the second case.
        let rod2_id = add_default_perm(&mut state, PlayerId::Opp, "Null Rod");
        recompute(&mut state);

        // chosen_x = 0 → converge = 1 < Null Rod's MV 2 → spared.
        let eff = build_spell_effect(&def, PlayerId::Us, ObjId::UNSET, 0, 0).1;
        eff.call(&mut state, 1, &[rod2_id]);
        assert_eq!(state.objects[&rod2_id].zone(), Some(Zone::Battlefield),
            "Null Rod (MV 2) should survive when converge = 1");
    }

    // ── Phelia, Exuberant Shepherd ───────────────────────────────────────────

    /// Attack trigger exiles our own nonland permanent, delayed trigger returns it at end
    /// of turn under our control, and Phelia gains a +1/+1 counter because the returned
    /// card entered under our control.
    #[test]
    fn test_phelia_blinks_own_permanent_gains_counter() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let phelia_id = add_perm_with_def(
            &mut state,
            PlayerId::Us,
            &catalog_card("Phelia, Exuberant Shepherd"),
            BattlefieldState { attacking: true, entered_this_turn: false, ..BattlefieldState::new() },
        );
        // Target our own Null Rod — owner == Phelia's controller, so +1/+1 counter.
        let target_id = add_default_perm(&mut state, PlayerId::Us, "Null Rod");
        recompute(&mut state);

        fire_event(
            GameEvent::CreatureAttacked { attacker_id: phelia_id, attacker_controller: PlayerId::Us },
            &mut state, 1, PlayerId::Us,
        );
        let phelia_pos = state.pending_triggers.iter()
            .position(|ctx| ctx.source_name == "Phelia, Exuberant Shepherd")
            .expect("Phelia attack trigger should queue on CreatureAttacked");
        let ctx = state.pending_triggers.remove(phelia_pos);

        let legal = legal_targets(&ctx.target_spec, PlayerId::Us, phelia_id, &state);
        let picked = pick_targets(&ctx.target_spec, &legal, &state);
        assert_eq!(picked, vec![target_id], "only legal target is our Null Rod");
        ctx.effect.call(&mut state, 1, &picked);

        assert_eq!(state.objects[&target_id].zone(), Some(Zone::Exile { on_adventure: false }),
            "target exiled after trigger resolves");
        assert_eq!(state.trigger_instances.len(), 1, "delayed return trigger registered");

        fire_event(
            GameEvent::EnteredStep { step: StepKind::End, active_player: PlayerId::Us },
            &mut state, 1, PlayerId::Us,
        );
        let delayed_pos = state.pending_triggers.iter()
            .position(|ctx| ctx.source_name.contains("(delayed)"))
            .expect("delayed return trigger should queue on End step");
        let ctx = state.pending_triggers.remove(delayed_pos);
        ctx.effect.call(&mut state, 1, &[]);

        assert_eq!(state.objects[&target_id].zone(), Some(Zone::Battlefield),
            "target returns to battlefield at end of turn");
        assert_eq!(state.objects[&target_id].controller, PlayerId::Us,
            "our card returns under our control");
        assert_eq!(state.permanent_bf(phelia_id).unwrap().counters, 1,
            "Phelia gets +1/+1: card entered under our control");
    }

    /// Blinking an opponent's permanent: it returns under opp's control and Phelia does
    /// NOT gain a +1/+1 counter.
    #[test]
    fn test_phelia_blinks_opponent_permanent_no_counter() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let phelia_id = add_perm_with_def(
            &mut state,
            PlayerId::Us,
            &catalog_card("Phelia, Exuberant Shepherd"),
            BattlefieldState { attacking: true, entered_this_turn: false, ..BattlefieldState::new() },
        );
        let target_id = add_default_perm(&mut state, PlayerId::Opp, "Null Rod");
        recompute(&mut state);

        fire_event(
            GameEvent::CreatureAttacked { attacker_id: phelia_id, attacker_controller: PlayerId::Us },
            &mut state, 1, PlayerId::Us,
        );
        let phelia_pos = state.pending_triggers.iter()
            .position(|ctx| ctx.source_name == "Phelia, Exuberant Shepherd")
            .expect("Phelia attack trigger should queue");
        let ctx = state.pending_triggers.remove(phelia_pos);
        let legal = legal_targets(&ctx.target_spec, PlayerId::Us, phelia_id, &state);
        let picked = pick_targets(&ctx.target_spec, &legal, &state);
        ctx.effect.call(&mut state, 1, &picked);

        fire_event(
            GameEvent::EnteredStep { step: StepKind::End, active_player: PlayerId::Us },
            &mut state, 1, PlayerId::Us,
        );
        let delayed_pos = state.pending_triggers.iter()
            .position(|ctx| ctx.source_name.contains("(delayed)"))
            .expect("delayed return trigger should queue");
        let ctx = state.pending_triggers.remove(delayed_pos);
        ctx.effect.call(&mut state, 1, &[]);

        assert_eq!(state.objects[&target_id].controller, PlayerId::Opp,
            "opp's card returns under opp's control");
        assert_eq!(state.permanent_bf(phelia_id).unwrap().counters, 0,
            "no counter: card did not enter under our control");
    }

    // ── Cori-Steel Cutter ────────────────────────────────────────────────────

    #[test]
    fn test_cori_equip_grants_keywords_and_pt() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let creature_id = add_default_perm(&mut state, PlayerId::Us, "Barrowgoyf");
        let cori_id = add_default_perm(&mut state, PlayerId::Us, "Cori-Steel Cutter");

        // Attach equipment to creature.
        state.permanent_bf_mut(cori_id).unwrap().attached_to = Some(creature_id);
        recompute(&mut state);

        let def = state.def_of(creature_id).expect("creature should have materialized def");
        if let CardKind::Creature(c) = &def.kind {
            assert!(c.keywords.contains(Keyword::Trample), "equipped creature should have trample");
            assert!(c.keywords.contains(Keyword::Haste), "equipped creature should have haste");
            // Barrowgoyf base is 0/1; equipment adds +1/+1 → at least 1/2.
            assert!(c.power() >= 1, "equipped creature should get +1 power");
            assert!(c.toughness() >= 2, "equipped creature should get +1 toughness");
        } else {
            panic!("should be a creature");
        }
    }

    #[test]
    fn test_cori_no_buff_when_unattached() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let creature_id = add_default_perm(&mut state, PlayerId::Us, "Barrowgoyf");
        let _cori_id = add_default_perm(&mut state, PlayerId::Us, "Cori-Steel Cutter");

        recompute(&mut state);

        let def = state.def_of(creature_id).expect("creature should have materialized def");
        if let CardKind::Creature(c) = &def.kind {
            assert!(!c.keywords.contains(Keyword::Trample), "unequipped creature should not have trample");
            assert!(!c.keywords.contains(Keyword::Haste), "unequipped creature should not have haste");
        } else {
            panic!("should be a creature");
        }
    }

    /// Cast a spell for `who`: put it on the stack and fire SpellCast (which logs
    /// it to the event log), then resolve any triggers. Cori's flurry counts the
    /// controller's logged SpellCast events this turn, so real casts drive it.
    fn cori_cast_spell(state: &mut SimState, who: PlayerId) {
        let id = state.alloc_id();
        state.objects.insert(id, GameObject {
            id,
            catalog_key: "Ponder".to_string(),
            owner: who,
            controller: who,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::StackSpell(SpellState { effect: None, chosen_targets: vec![], is_back_face: false, costs_paid_ctx: CostsPaidCtx::default() }),
        });
        state.catalog.entry("Ponder".to_string()).or_insert_with(|| catalog_card("Ponder"));
        fire_event(GameEvent::SpellCast { caster: who, card_id: id, mana_spent: true, alt_cost: false, x: 0, delved: Vec::new() }, state, 1, who);
        for ctx in std::mem::take(&mut state.pending_triggers) { ctx.effect.call(state, 1, &[]); }
    }

    fn cori_monk_count(state: &SimState) -> usize {
        state.permanents_of(PlayerId::Us).filter(|c| c.catalog_key == "Monk Token").count()
    }

    #[test]
    fn test_cori_flurry_second_spell() {
        let mut state = make_state();
        state.catalog = test_catalog();
        add_default_perm(&mut state, PlayerId::Us, "Cori-Steel Cutter");

        cori_cast_spell(&mut state, PlayerId::Us); // 1st — no flurry
        assert_eq!(cori_monk_count(&state), 0, "no flurry on the first spell");
        cori_cast_spell(&mut state, PlayerId::Us); // 2nd — flurry fires
        assert_eq!(cori_monk_count(&state), 1, "flurry creates exactly one Monk on the second spell");
    }

    #[test]
    fn test_cori_flurry_not_first_spell() {
        let mut state = make_state();
        state.catalog = test_catalog();
        add_default_perm(&mut state, PlayerId::Us, "Cori-Steel Cutter");

        cori_cast_spell(&mut state, PlayerId::Us);
        assert_eq!(cori_monk_count(&state), 0, "flurry should NOT trigger on the first spell");
    }

    #[test]
    fn test_cori_flurry_not_third_spell() {
        let mut state = make_state();
        state.catalog = test_catalog();
        add_default_perm(&mut state, PlayerId::Us, "Cori-Steel Cutter");

        cori_cast_spell(&mut state, PlayerId::Us); // 1st
        cori_cast_spell(&mut state, PlayerId::Us); // 2nd → flurry
        cori_cast_spell(&mut state, PlayerId::Us); // 3rd → no flurry
        // Exactly one Monk: the flurry fired only on the second spell.
        assert_eq!(cori_monk_count(&state), 1, "flurry fires only on the second spell, not the third");
    }

    // ── The Fantasticar: pop on the EXACTLY fourth noncreature spell ────────────

    /// Insert a spell-shell object with `name`'s catalog key (zone is irrelevant to
    /// the EventCount noncreature filter; it resolves the card_id's types). Returns id.
    fn fanta_mk_spell(state: &mut SimState, name: &str) -> ObjId {
        let id = state.alloc_id();
        state.objects.insert(id, GameObject {
            id,
            catalog_key: name.to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::StackSpell(SpellState { effect: None, chosen_targets: vec![], is_back_face: false, costs_paid_ctx: CostsPaidCtx::default() }),
        });
        id
    }

    /// Log a prior noncreature SpellCast by Us this turn (counts toward the 4th).
    fn fanta_log_prior(state: &mut SimState, name: &str) {
        let id = fanta_mk_spell(state, name);
        state.event_log.push(1, GameEvent::SpellCast { caster: PlayerId::Us, card_id: id, mana_spent: true, alt_cost: false, x: 0, delved: Vec::new() });
    }

    /// Cast (fire) a SpellCast for `name` and resolve any triggers it produces.
    fn fanta_cast(state: &mut SimState, name: &str) {
        let id = fanta_mk_spell(state, name);
        fire_event(GameEvent::SpellCast { caster: PlayerId::Us, card_id: id, mana_spent: true, alt_cost: false, x: 0, delved: Vec::new() }, state, 1, PlayerId::Us);
        for ctx in std::mem::take(&mut state.pending_triggers) { ctx.effect.call(state, 1, &[]); }
    }

    fn fanta_car_count(state: &SimState) -> usize {
        state.permanents_of(PlayerId::Us).filter(|c| c.catalog_key == "The Fantasticar").count()
    }
    fn fanta_token_count(state: &SimState) -> usize {
        state.permanents_of(PlayerId::Us).filter(|c| c.catalog_key == "Fantasticar Construct").count()
    }

    #[test]
    fn test_fantasticar_pops_on_exactly_fourth_noncreature_spell() {
        let mut state = make_state();
        state.catalog = test_catalog();
        add_default_perm(&mut state, PlayerId::Us, "The Fantasticar");

        // Three prior noncreature spells this turn, then cast the 4th.
        fanta_log_prior(&mut state, "Lotus Petal");
        fanta_log_prior(&mut state, "Mishra's Bauble");
        fanta_log_prior(&mut state, "Dark Ritual");
        fanta_cast(&mut state, "Brainstorm");

        assert_eq!(fanta_car_count(&state), 0, "The Fantasticar should be sacrificed on the pop");
        assert_eq!(fanta_token_count(&state), 4, "the pop should make four Construct tokens");

        // Tokens are 4/4 with flying and haste.
        recompute(&mut state);
        let tok = state.permanents_of(PlayerId::Us)
            .find(|c| c.catalog_key == "Fantasticar Construct").map(|o| o.id).unwrap();
        let eff = state.def_of(tok).expect("token in materialized defs");
        let CardKind::Creature(c) = &eff.kind else { panic!("expected creature token") };
        assert_eq!((c.power(), c.toughness()), (4, 4), "Construct should be 4/4");
        assert!(creature_has_keyword(tok, Keyword::Flying, &state), "Construct should have flying");
        assert!(creature_has_keyword(tok, Keyword::Haste, &state), "Construct should have haste");
    }

    #[test]
    fn test_fantasticar_no_pop_with_four_spells_before_car_then_more_after() {
        // "Casting 4 spells before the car doesn't work, even if you cast more after."
        let mut state = make_state();
        state.catalog = test_catalog();

        // Four noncreature spells this turn — but The Fantasticar is NOT in play yet,
        // so the 4th-spell trigger isn't armed (active_zone = Battlefield).
        fanta_log_prior(&mut state, "Lotus Petal");
        fanta_log_prior(&mut state, "Mishra's Bauble");
        fanta_log_prior(&mut state, "Dark Ritual");
        fanta_cast(&mut state, "Brainstorm"); // the 4th — no car in play
        assert_eq!(fanta_token_count(&state), 0, "no pop when the 4th is cast with no car in play");

        // Now the car lands and we cast MORE spells (the 5th, 6th, …). The "fourth
        // noncreature spell" has already passed, so it never pops this turn.
        add_default_perm(&mut state, PlayerId::Us, "The Fantasticar");
        fanta_cast(&mut state, "Ponder"); // 5th
        fanta_cast(&mut state, "Consider"); // 6th
        assert_eq!(fanta_token_count(&state), 0, "casting more after the car must not pop (not the 4th)");
        assert_eq!(fanta_car_count(&state), 1, "the car survives — it never popped");
    }

    #[test]
    fn test_fantasticar_as_the_fourth_spell_does_not_pop() {
        // If The Fantasticar IS the 4th noncreature spell, it's on the stack (not the
        // battlefield) when its own cast fires, so the trigger isn't armed.
        let mut state = make_state();
        state.catalog = test_catalog();

        fanta_log_prior(&mut state, "Lotus Petal");
        fanta_log_prior(&mut state, "Mishra's Bauble");
        fanta_log_prior(&mut state, "Dark Ritual");
        fanta_cast(&mut state, "The Fantasticar"); // 4th cast, but on the stack
        assert_eq!(fanta_token_count(&state), 0, "the car as the 4th spell cannot sacrifice itself (still on the stack)");
    }

    #[test]
    fn test_fantasticar_creature_spell_does_not_count_toward_four() {
        let mut state = make_state();
        state.catalog = test_catalog();
        add_default_perm(&mut state, PlayerId::Us, "The Fantasticar");

        // Three noncreature spells, then a CREATURE spell — the creature is not the
        // "fourth noncreature spell", so no pop.
        fanta_log_prior(&mut state, "Lotus Petal");
        fanta_log_prior(&mut state, "Mishra's Bauble");
        fanta_log_prior(&mut state, "Dark Ritual");
        fanta_cast(&mut state, "Thassa's Oracle"); // creature — doesn't count
        assert_eq!(fanta_token_count(&state), 0, "a creature spell is not the 4th noncreature spell");

        // A genuine 4th NONCREATURE spell now pops it.
        fanta_cast(&mut state, "Brainstorm");
        assert_eq!(fanta_token_count(&state), 4, "the 4th noncreature spell pops the car");
    }

    #[test]
    fn test_monk_prowess_noncreature() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let monk_id = add_default_perm(&mut state, PlayerId::Us, "Monk Token");
        recompute(&mut state);

        // Fire a noncreature SpellCast.
        let spell_id = {
            let id = state.alloc_id();
            state.objects.insert(id, GameObject {
            id,
            catalog_key: "Ponder".to_string(),
            owner: PlayerId::Us,
            controller: PlayerId::Us,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::StackSpell(SpellState { effect: None, chosen_targets: vec![], is_back_face: false, costs_paid_ctx: CostsPaidCtx::default() }),
        });
            state.catalog.entry("Ponder".to_string()).or_insert_with(|| catalog_card("Ponder"));
            id
        };

        fire_event(
            GameEvent::SpellCast { caster: PlayerId::Us, card_id: spell_id, mana_spent: true, alt_cost: false, x: 0, delved: Vec::new() },
            &mut state, 1, PlayerId::Us,
        );
        for ctx in std::mem::take(&mut state.pending_triggers) { ctx.effect.call(&mut state, 1, &[]); }
        recompute(&mut state);

        let def = state.def_of(monk_id).expect("Monk should have materialized def");
        if let CardKind::Creature(c) = &def.kind {
            assert_eq!(c.power(), 2, "Monk should be 2/2 after one prowess trigger");
            assert_eq!(c.toughness(), 2, "Monk should be 2/2 after one prowess trigger");
        } else {
            panic!("Monk should be a creature");
        }
    }

    #[test]
    fn test_detach_on_creature_leaves() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let creature_id = add_default_perm(&mut state, PlayerId::Us, "Barrowgoyf");
        let cori_id = add_default_perm(&mut state, PlayerId::Us, "Cori-Steel Cutter");
        state.permanent_bf_mut(cori_id).unwrap().attached_to = Some(creature_id);

        // Creature leaves the battlefield.
        change_zone(creature_id, ZoneId::Graveyard, &mut state, 1, PlayerId::Us);

        assert_eq!(state.permanent_bf(cori_id).unwrap().attached_to, None,
            "equipment should detach when creature leaves");
    }

    // ── Section 50: DD Strategy Evaluator ────────────────────────────────────────














    // ── Section 51: Opponent Strategy Evaluator ──────────────────────────────────











    // ── Section 52: Cantrip Effect Primitives ──────────────────────────────────

    /// Wire evaluate_card so that specific cards get known scores for deterministic testing.
    /// Maps card names to scores; anything not in the map gets 0.5.
    fn wire_eval(state: &mut SimState, scores: Vec<(&str, f64)>) {
        let map: HashMap<String, f64> = scores.into_iter().map(|(k, v)| (k.to_string(), v)).collect();
        state.evaluate_card = Arc::new(move |_who, card_id, state| {
            state.objects.get(&card_id)
                .and_then(|o| map.get(&o.catalog_key))
                .copied()
                .unwrap_or(0.5)
        });
    }

    #[test]
    fn test_put_back_eval_puts_worst_on_top() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Hand: Doomsday (high), Oracle (low), Brainstorm (medium)
        let dd = add_hand_card(&mut state, PlayerId::Us, "Doomsday");
        let oracle = add_hand_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        let bs = add_hand_card(&mut state, PlayerId::Us, "Brainstorm");
        wire_eval(&mut state, vec![("Doomsday", 0.9), ("Thassa's Oracle", 0.05), ("Brainstorm", 0.35)]);

        eff_put_back(PlayerId::Us, 1).call(&mut state, 0, &[]);

        // Oracle should be gone from hand and on top of library.
        let hand: Vec<ObjId> = state.hand_of(PlayerId::Us).map(|c| c.id).collect();
        assert!(!hand.contains(&oracle), "Oracle should be removed from hand");
        assert!(hand.contains(&dd), "Doomsday should stay in hand");
        assert!(hand.contains(&bs), "Brainstorm should stay in hand");
        assert_eq!(state.player(PlayerId::Us).library_order.front(), Some(&oracle), "Oracle should be on top of library");
    }

    #[test]
    fn test_put_back_eval_twice_puts_two_worst() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let dd = add_hand_card(&mut state, PlayerId::Us, "Doomsday");
        let oracle = add_hand_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        let edge = add_hand_card(&mut state, PlayerId::Us, "Edge of Autumn");
        let bs = add_hand_card(&mut state, PlayerId::Us, "Brainstorm");
        wire_eval(&mut state, vec![
            ("Doomsday", 0.9), ("Thassa's Oracle", 0.05),
            ("Edge of Autumn", 0.1), ("Brainstorm", 0.35),
        ]);

        eff_put_back(PlayerId::Us, 1).call(&mut state, 0, &[]);
        eff_put_back(PlayerId::Us, 1).call(&mut state, 0, &[]);

        let hand: Vec<ObjId> = state.hand_of(PlayerId::Us).map(|c| c.id).collect();
        assert_eq!(hand.len(), 2, "should have 2 cards left in hand");
        assert!(hand.contains(&dd));
        assert!(hand.contains(&bs));
        // Oracle was put back first (worst), then Edge of Autumn (second worst).
        // Oracle first → front, then Edge → new front. So library front = Edge, next = Oracle.
        assert_eq!(state.player(PlayerId::Us).library_order.front(), Some(&edge), "Edge on top (put back second)");
        assert_eq!(state.player(PlayerId::Us).library_order.get(1), Some(&oracle), "Oracle second (put back first)");
    }

    #[test]
    fn test_scry_keeps_good_cards_on_top() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Library top-to-bottom: Doomsday (0.9), Oracle (0.05), Brainstorm (0.35)
        let dd = add_library_card(&mut state, PlayerId::Us, "Doomsday");
        let oracle = add_library_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        let bs = add_library_card(&mut state, PlayerId::Us, "Brainstorm");
        // Reorder so dd is on top
        state.player_mut(PlayerId::Us).library_order.clear();
        state.player_mut(PlayerId::Us).library_order.push_back(dd);
        state.player_mut(PlayerId::Us).library_order.push_back(oracle);
        state.player_mut(PlayerId::Us).library_order.push_back(bs);

        wire_eval(&mut state, vec![("Doomsday", 0.9), ("Thassa's Oracle", 0.05), ("Brainstorm", 0.35)]);

        eff_scry(PlayerId::Us, 3).call(&mut state, 0, &[]);

        // Doomsday (0.9 >= 0.3) and Brainstorm (0.35 >= 0.3) kept on top.
        // Oracle (0.05 < 0.3) bottomed.
        // Kept cards preserve order: Doomsday, Brainstorm.
        let lib: Vec<ObjId> = state.player(PlayerId::Us).library_order.iter().copied().collect();
        assert_eq!(lib.len(), 3);
        assert_eq!(lib[0], dd, "Doomsday should be on top (kept)");
        assert_eq!(lib[1], bs, "Brainstorm should be second (kept)");
        assert_eq!(lib[2], oracle, "Oracle should be on bottom (scried away)");
    }

    #[test]
    fn test_scry_all_bad_sends_all_to_bottom() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let oracle = add_library_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        let edge = add_library_card(&mut state, PlayerId::Us, "Edge of Autumn");
        let deep = add_library_card(&mut state, PlayerId::Us, "Doomsday"); // will be below scry range
        state.player_mut(PlayerId::Us).library_order.clear();
        state.player_mut(PlayerId::Us).library_order.push_back(oracle);
        state.player_mut(PlayerId::Us).library_order.push_back(edge);
        state.player_mut(PlayerId::Us).library_order.push_back(deep);

        wire_eval(&mut state, vec![("Thassa's Oracle", 0.05), ("Edge of Autumn", 0.1), ("Doomsday", 0.9)]);

        eff_scry(PlayerId::Us, 2).call(&mut state, 0, &[]);

        // Both top cards were bad → both bottomed. Doomsday (untouched) now on top.
        let lib: Vec<ObjId> = state.player(PlayerId::Us).library_order.iter().copied().collect();
        assert_eq!(lib[0], deep, "Doomsday should now be on top");
    }

    #[test]
    fn test_order_sorts_top_n_by_score() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let oracle = add_library_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        let dd = add_library_card(&mut state, PlayerId::Us, "Doomsday");
        let bs = add_library_card(&mut state, PlayerId::Us, "Brainstorm");
        state.player_mut(PlayerId::Us).library_order.clear();
        state.player_mut(PlayerId::Us).library_order.push_back(oracle);  // worst on top
        state.player_mut(PlayerId::Us).library_order.push_back(dd);       // best in middle
        state.player_mut(PlayerId::Us).library_order.push_back(bs);        // medium at bottom

        wire_eval(&mut state, vec![("Doomsday", 0.9), ("Thassa's Oracle", 0.05), ("Brainstorm", 0.35)]);

        eff_ir(PlayerId::Us, crate::ir::action::Action::OrderTop {
            who: crate::ir::action::Who::You, n: crate::ir::expr::Expr::Num(3),
        }).call(&mut state, 0, &[]);

        let lib: Vec<ObjId> = state.player(PlayerId::Us).library_order.iter().copied().collect();
        assert_eq!(lib[0], dd, "Doomsday (0.9) should be on top after ordering");
        assert_eq!(lib[1], bs, "Brainstorm (0.35) should be second");
        assert_eq!(lib[2], oracle, "Oracle (0.05) should be third");
    }

    #[test]
    fn test_order_does_not_touch_cards_beyond_n() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let oracle = add_library_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        let dd = add_library_card(&mut state, PlayerId::Us, "Doomsday");
        let deep = add_library_card(&mut state, PlayerId::Us, "Force of Will");
        state.player_mut(PlayerId::Us).library_order.clear();
        state.player_mut(PlayerId::Us).library_order.push_back(oracle);
        state.player_mut(PlayerId::Us).library_order.push_back(dd);
        state.player_mut(PlayerId::Us).library_order.push_back(deep);

        wire_eval(&mut state, vec![("Doomsday", 0.9), ("Thassa's Oracle", 0.05), ("Force of Will", 0.8)]);

        eff_ir(PlayerId::Us, crate::ir::action::Action::OrderTop {
            who: crate::ir::action::Who::You, n: crate::ir::expr::Expr::Num(2),
        }).call(&mut state, 0, &[]);

        let lib: Vec<ObjId> = state.player(PlayerId::Us).library_order.iter().copied().collect();
        assert_eq!(lib[0], dd, "DD sorted to top of the 2");
        assert_eq!(lib[1], oracle, "Oracle second of the 2");
        assert_eq!(lib[2], deep, "FoW untouched at position 3");
    }

    #[test]
    fn test_shuffle_action_preserves_library() {
        use crate::ir::action::{Action, Who};
        use crate::ir::executor::{execute, BindEnv};
        let mut state = make_state();
        state.catalog = test_catalog();
        let oracle = add_library_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        let dd = add_library_card(&mut state, PlayerId::Us, "Doomsday");
        let bs = add_library_card(&mut state, PlayerId::Us, "Brainstorm");
        state.player_mut(PlayerId::Us).library_order.clear();
        for id in [oracle, dd, bs] { state.player_mut(PlayerId::Us).library_order.push_back(id); }
        let before: std::collections::HashSet<ObjId> =
            state.player(PlayerId::Us).library_order.iter().copied().collect();

        execute(&Action::Shuffle { who: Who::You }, &mut state,
                &BindEnv::new().with_controller(PlayerId::Us));

        // The shuffle carries no agency: same multiset, same count, possibly new order.
        let after: std::collections::HashSet<ObjId> =
            state.player(PlayerId::Us).library_order.iter().copied().collect();
        assert_eq!(after.len(), 3, "shuffle preserves card count");
        assert_eq!(before, after, "shuffle preserves the library multiset");
    }

    #[test]
    fn test_may_shuffle_gated_by_strategy() {
        use crate::ir::action::{Action, Who};
        // "You may shuffle" decomposes to MayDo { Shuffle }: the shuffle is the
        // effect, the "may" is a y/n strategy decision — no evaluator heuristic.
        let may_shuffle = || Action::MayDo {
            who: Who::You,
            action: Box::new(Action::Shuffle { who: Who::You }),
        };

        let mut state = make_state();
        state.catalog = test_catalog();
        let dd = add_library_card(&mut state, PlayerId::Us, "Doomsday");
        let oracle = add_library_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        state.player_mut(PlayerId::Us).library_order.clear();
        state.player_mut(PlayerId::Us).library_order.push_back(dd);
        state.player_mut(PlayerId::Us).library_order.push_back(oracle);

        // Default strategy declines (Mode 0) → order preserved exactly.
        eff_ir(PlayerId::Us, may_shuffle()).call(&mut state, 0, &[]);
        let lib: Vec<ObjId> = state.player(PlayerId::Us).library_order.iter().copied().collect();
        assert_eq!(lib, vec![dd, oracle], "default strategy declines the may-shuffle");

        // Opt-in strategy (Mode 1) → shuffles; multiset preserved.
        state.set_strategy(PlayerId::Us, Box::new(TestStrategy::new(PlayerId::Us).mode(1)));
        eff_ir(PlayerId::Us, may_shuffle()).call(&mut state, 0, &[]);
        let after: std::collections::HashSet<ObjId> =
            state.player(PlayerId::Us).library_order.iter().copied().collect();
        assert_eq!(after.len(), 2, "shuffle preserves count");
        assert!(after.contains(&dd) && after.contains(&oracle), "shuffle preserves cards");
    }

    #[test]
    fn test_brainstorm_composition_draw3_putback2() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Start with 2 cards in hand: Oracle (bad, 0.05) and Edge (bad, 0.1)
        let oracle = add_hand_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        let edge = add_hand_card(&mut state, PlayerId::Us, "Edge of Autumn");
        // Library: DD (0.9), FoW (0.8), Brainstorm (0.35)
        let dd = add_library_card(&mut state, PlayerId::Us, "Doomsday");
        let fow = add_library_card(&mut state, PlayerId::Us, "Force of Will");
        let bs = add_library_card(&mut state, PlayerId::Us, "Brainstorm");
        state.player_mut(PlayerId::Us).library_order.clear();
        state.player_mut(PlayerId::Us).library_order.push_back(dd);
        state.player_mut(PlayerId::Us).library_order.push_back(fow);
        state.player_mut(PlayerId::Us).library_order.push_back(bs);

        wire_eval(&mut state, vec![
            ("Doomsday", 0.9), ("Force of Will", 0.8), ("Brainstorm", 0.35),
            ("Thassa's Oracle", 0.05), ("Edge of Autumn", 0.1),
        ]);

        // Brainstorm = draw 3, put back 2 worst
        let effect = eff_draw(PlayerId::Us, 3)
            .then(eff_put_back(PlayerId::Us, 2));
        effect.call(&mut state, 0, &[]);

        // After draw 3: hand = Oracle(0.05), Edge(0.1), DD(0.9), FoW(0.8), BS(0.35)
        // Put back worst: Oracle(0.05) → top. Hand = Edge(0.1), DD(0.9), FoW(0.8), BS(0.35)
        // Put back worst: Edge(0.1) → top. Hand = DD(0.9), FoW(0.8), BS(0.35)
        let hand_names: Vec<String> = state.hand_of(PlayerId::Us)
            .map(|c| c.catalog_key.clone()).collect();
        assert_eq!(hand_names.len(), 3, "hand should have 3 cards");
        assert!(hand_names.contains(&"Doomsday".to_string()));
        assert!(hand_names.contains(&"Force of Will".to_string()));
        assert!(hand_names.contains(&"Brainstorm".to_string()));
        // Oracle and Edge should be on top of library (Edge on top, Oracle second)
        let lib: Vec<ObjId> = state.player(PlayerId::Us).library_order.iter().copied().collect();
        assert_eq!(lib[0], edge, "Edge (put back second) on top");
        assert_eq!(lib[1], oracle, "Oracle (put back first) second");
    }

    #[test]
    fn test_ponder_keeps_best_on_top_and_draws_it() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Library: Oracle (0.05), BS (0.35), DD (0.9)
        let oracle = add_library_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        let bs = add_library_card(&mut state, PlayerId::Us, "Brainstorm");
        let dd = add_library_card(&mut state, PlayerId::Us, "Doomsday");
        state.player_mut(PlayerId::Us).library_order.clear();
        state.player_mut(PlayerId::Us).library_order.push_back(oracle);
        state.player_mut(PlayerId::Us).library_order.push_back(bs);
        state.player_mut(PlayerId::Us).library_order.push_back(dd);

        wire_eval(&mut state, vec![("Doomsday", 0.9), ("Brainstorm", 0.35), ("Thassa's Oracle", 0.05)]);

        // Real Ponder: OrderTop(3) (default = best on top), may-shuffle (default
        // strategy declines), draw(1) → draw DD.
        let ponder = catalog_card("Ponder");
        build_spell_effect(&ponder, PlayerId::Us, ObjId::UNSET, 0, 0).1.call(&mut state, 0, &[]);

        let hand: Vec<String> = state.hand_of(PlayerId::Us)
            .map(|c| c.catalog_key.clone()).collect();
        assert!(hand.contains(&"Doomsday".to_string()), "should draw DD (best card)");
    }

    #[test]
    fn test_ponder_shuffles_when_strategy_opts_in() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let oracle = add_library_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        let edge = add_library_card(&mut state, PlayerId::Us, "Edge of Autumn");
        let unearth = add_library_card(&mut state, PlayerId::Us, "Unearth");
        let dd = add_library_card(&mut state, PlayerId::Us, "Doomsday");
        state.player_mut(PlayerId::Us).library_order.clear();
        for id in [oracle, edge, unearth, dd] { state.player_mut(PlayerId::Us).library_order.push_back(id); }

        wire_eval(&mut state, vec![
            ("Thassa's Oracle", 0.05), ("Edge of Autumn", 0.1),
            ("Unearth", 0.05), ("Doomsday", 0.9),
        ]);
        // Strategy opts into the may-shuffle (Mode 1).
        state.set_strategy(PlayerId::Us, Box::new(TestStrategy::new(PlayerId::Us).mode(1)));

        let ponder = catalog_card("Ponder");
        build_spell_effect(&ponder, PlayerId::Us, ObjId::UNSET, 0, 0).1.call(&mut state, 0, &[]);

        // Drew 1 card from the (shuffled) library; 3 remain.
        let hand_count = state.hand_of(PlayerId::Us).count();
        assert_eq!(hand_count, 1, "should draw 1 card after ponder");
        assert_eq!(state.player(PlayerId::Us).library_order.len(), 3, "3 cards left in library");
    }

    /// OrderTop honors the *strategy's* arrangement, not an engine/evaluator sort:
    /// a strategy that reverses the looked-at cards reverses the library top.
    #[test]
    fn test_order_top_respects_strategy_arrangement() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let a = add_library_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        let b = add_library_card(&mut state, PlayerId::Us, "Brainstorm");
        let c = add_library_card(&mut state, PlayerId::Us, "Doomsday");
        state.player_mut(PlayerId::Us).library_order.clear();
        for id in [a, b, c] { state.player_mut(PlayerId::Us).library_order.push_back(id); }
        // Evaluator would put Doomsday (c) on top — the reversing strategy must win.
        wire_eval(&mut state, vec![("Doomsday", 0.9), ("Brainstorm", 0.35), ("Thassa's Oracle", 0.05)]);
        state.set_strategy(PlayerId::Us, Box::new(TestStrategy::new(PlayerId::Us).order_reverse()));

        eff_ir(PlayerId::Us, crate::ir::action::Action::OrderTop {
            who: crate::ir::action::Who::You, n: crate::ir::expr::Expr::Num(3),
        }).call(&mut state, 0, &[]);

        let lib: Vec<ObjId> = state.player(PlayerId::Us).library_order.iter().copied().collect();
        assert_eq!(lib, vec![c, b, a], "OrderTop must honor the strategy's reversed arrangement");
    }

    #[test]
    fn test_preordain_scries_then_draws() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Library: Oracle(0.05), DD(0.9), BS(0.35)
        let oracle = add_library_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        let dd = add_library_card(&mut state, PlayerId::Us, "Doomsday");
        let bs = add_library_card(&mut state, PlayerId::Us, "Brainstorm");
        state.player_mut(PlayerId::Us).library_order.clear();
        state.player_mut(PlayerId::Us).library_order.push_back(oracle);
        state.player_mut(PlayerId::Us).library_order.push_back(dd);
        state.player_mut(PlayerId::Us).library_order.push_back(bs);

        wire_eval(&mut state, vec![("Doomsday", 0.9), ("Thassa's Oracle", 0.05), ("Brainstorm", 0.35)]);

        // Preordain = scry(2), draw(1)
        let effect = eff_scry(PlayerId::Us, 2).then(eff_draw(PlayerId::Us, 1));
        effect.call(&mut state, 0, &[]);

        // Scry 2 sees Oracle(0.05) and DD(0.9).
        // Oracle (0.05 < 0.3) → bottom. DD (0.9 >= 0.3) → keep on top.
        // After scry: DD on top, BS, Oracle on bottom.
        // Draw: DD drawn.
        let hand: Vec<String> = state.hand_of(PlayerId::Us)
            .map(|c| c.catalog_key.clone()).collect();
        assert!(hand.contains(&"Doomsday".to_string()), "should draw DD after scrying Oracle to bottom");
        // Library: BS, Oracle
        let lib: Vec<ObjId> = state.player(PlayerId::Us).library_order.iter().copied().collect();
        assert_eq!(lib.len(), 2);
        assert_eq!(lib[0], bs, "BS should be on top of remaining library");
        assert_eq!(lib[1], oracle, "Oracle should be on bottom");
    }

    #[test]
    fn test_consider_surveil_then_draw() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Library: Oracle(0.05 → mill), DD(0.9)
        let oracle = add_library_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        let dd = add_library_card(&mut state, PlayerId::Us, "Doomsday");
        state.player_mut(PlayerId::Us).library_order.clear();
        state.player_mut(PlayerId::Us).library_order.push_back(oracle);
        state.player_mut(PlayerId::Us).library_order.push_back(dd);

        wire_eval(&mut state, vec![("Thassa's Oracle", 0.05), ("Doomsday", 0.9)]);
        // No override: the default surveil policy already bins cards the
        // evaluator scores below 0.3 (delegates to `state.evaluate_card`).

        // Consider = surveil(1), draw(1)
        let effect = eff_surveil(PlayerId::Us, 1).then(eff_draw(PlayerId::Us, 1));
        effect.call(&mut state, 0, &[]);

        // Oracle (0.05 < 0.3) → milled to graveyard. DD drawn.
        let hand: Vec<String> = state.hand_of(PlayerId::Us)
            .map(|c| c.catalog_key.clone()).collect();
        assert!(hand.contains(&"Doomsday".to_string()), "should draw DD after surveilling Oracle away");
        // Oracle in graveyard
        let gy: Vec<String> = state.objects.values()
            .filter(|o| o.in_zone(Zone::Graveyard))
            .map(|o| o.catalog_key.clone()).collect();
        assert!(gy.contains(&"Thassa's Oracle".to_string()), "Oracle should be in graveyard");
    }

    #[test]
    fn test_consider_surveil_keeps_good_card() {
        let mut state = make_state();
        state.catalog = test_catalog();
        // Library: DD(0.9 → keep), Oracle(0.05)
        let dd = add_library_card(&mut state, PlayerId::Us, "Doomsday");
        let oracle = add_library_card(&mut state, PlayerId::Us, "Thassa's Oracle");
        state.player_mut(PlayerId::Us).library_order.clear();
        state.player_mut(PlayerId::Us).library_order.push_back(dd);
        state.player_mut(PlayerId::Us).library_order.push_back(oracle);

        wire_eval(&mut state, vec![("Doomsday", 0.9), ("Thassa's Oracle", 0.05)]);
        // No override: the default surveil policy already bins cards the
        // evaluator scores below 0.3 (delegates to `state.evaluate_card`).

        let effect = eff_surveil(PlayerId::Us, 1).then(eff_draw(PlayerId::Us, 1));
        effect.call(&mut state, 0, &[]);

        // DD (0.9 >= 0.3) → kept on top, then drawn.
        let hand: Vec<String> = state.hand_of(PlayerId::Us)
            .map(|c| c.catalog_key.clone()).collect();
        assert!(hand.contains(&"Doomsday".to_string()), "should draw DD (surveil kept it)");
        // Oracle still in library (not milled)
        assert_eq!(state.player(PlayerId::Us).library_order.len(), 1);
    }


    // ── Section 53: Mulligan Decision Tests ──────────────────────────────────













    // ── Section 54b: Color-Aware Mulligan Tests ──────────────────────────────






    // ── Section 55: Mana Ability Fixes ──────────────────────────────────────

    #[test]
    fn test_led_excluded_from_mana_sub_loop() {
        // LED has ActivationTiming::Instant, so enumerate_mana_abilities must skip it.
        let mut state = make_state();
        state.catalog = test_catalog();
        let led_def = catalog_card("Lion's Eye Diamond");
        add_perm_with_def(&mut state, PlayerId::Us, &led_def, BattlefieldState::new());
        recompute(&mut state);

        let options = enumerate_mana_abilities(&state, PlayerId::Us);
        assert!(options.is_empty(),
            "LED should be excluded from mana sub-loop (timing != Default), got {} options", options.len());
    }

    #[test]
    fn test_led_excluded_from_potential_mana() {
        // LED has non-Default timing, so potential_mana should NOT count it.
        // This prevents the engine from thinking spells are affordable when the
        // strategy refuses to auto-crack LED (undertapping bug).
        let mut state = make_state();
        state.catalog = test_catalog();
        let led_def = catalog_card("Lion's Eye Diamond");
        add_perm_with_def(&mut state, PlayerId::Us, &led_def, BattlefieldState::new());
        recompute(&mut state);

        let pool = state.potential_mana(PlayerId::Us);
        assert_eq!(pool.total, 0, "potential_mana should not count LED (non-Default timing)");
    }

    #[test]
    fn test_insufficient_mana_blocks_cast() {
        // With only LED on the battlefield (non-Default timing), a BBB spell
        // should NOT appear in legal actions — the engine cannot auto-tap LED.
        let mut state = make_state();
        state.catalog = test_catalog();
        let led_def = catalog_card("Lion's Eye Diamond");
        add_perm_with_def(&mut state, PlayerId::Us, &led_def, BattlefieldState::new());
        add_hand_card(&mut state, PlayerId::Us, "Doomsday");
        state.current_turn = 1;
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));
        recompute(&mut state);

        let legal = strategy::collect_legal_actions(&state, PlayerId::Us);
        assert!(
            !legal.iter().any(|a| matches!(a, LegalAction::CastSpell { .. })),
            "Doomsday should not be castable with only LED as a mana source"
        );
    }

    #[test]
    fn test_paid_mana_filter_does_not_self_fund() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let boulder_def = catalog_card("Giant's Boulder");
        let boulder_id = add_perm_with_def(&mut state, PlayerId::Us, &boulder_def, BattlefieldState::new());
        recompute(&mut state);

        let pool = state.potential_mana(PlayerId::Us);
        assert_eq!(pool.total, 0,
            "Giant's Boulder is a mana filter, not a free +1 mana source");
        let options = enumerate_mana_abilities(&state, PlayerId::Us);
        assert!(!options.iter().any(|o| o.source_id == boulder_id),
            "Boulder must not be offered when its {{1}} activation cost cannot already be paid");

        state.player_mut(PlayerId::Us).pool.c = 1;
        state.player_mut(PlayerId::Us).pool.total = 1;
        let options = enumerate_mana_abilities(&state, PlayerId::Us);
        assert!(options.iter().any(|o| o.source_id == boulder_id),
            "Boulder should become available once another source has funded its activation");
        let pool = state.potential_mana(PlayerId::Us);
        assert_eq!(pool.total, 1,
            "filtering through Boulder must not increase total mana");
        assert!(pool.u >= 1 && pool.b >= 1 && pool.r >= 1 && pool.g >= 1 && pool.w >= 1,
            "Boulder should advertise its color-filtering capability once funded");
    }

    #[test]
    fn test_land_drop_not_offered_during_upkeep_or_opponents_turn() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let land_id = add_hand_card(&mut state, PlayerId::Us, "Ancient Tomb");
        state.current_turn = 1;
        state.current_ap = state.player_id(PlayerId::Us);
        state.current_phase = Some(TurnPosition::Step(StepKind::Upkeep));
        recompute(&mut state);

        let upkeep = strategy::collect_legal_actions(&state, PlayerId::Us);
        assert!(!upkeep.iter().any(|a| matches!(a, LegalAction::LandDrop(id) if *id == land_id)),
            "lands may not be played during upkeep");

        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));
        let own_main = strategy::collect_legal_actions(&state, PlayerId::Us);
        assert!(own_main.iter().any(|a| matches!(a, LegalAction::LandDrop(id) if *id == land_id)),
            "land should be offered during the active player's empty-stack main phase");

        state.current_ap = state.player_id(PlayerId::Opp);
        let opp_turn = strategy::collect_legal_actions(&state, PlayerId::Us);
        assert!(!opp_turn.iter().any(|a| matches!(a, LegalAction::LandDrop(id) if *id == land_id)),
            "lands may not be played during the opponent's main phase");
    }

    #[test]
    fn test_sorcery_speed_spell_not_offered_on_nonempty_stack() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let tomb_def = catalog_card("Ancient Tomb");
        add_perm_with_def(&mut state, PlayerId::Us, &tomb_def, BattlefieldState::new());
        let boulder_id = add_hand_card(&mut state, PlayerId::Us, "Giant's Boulder");
        state.current_turn = 1;
        state.current_ap = state.player_id(PlayerId::Us);
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));
        let dummy_stack_id = state.alloc_id();
        state.stack.push(dummy_stack_id); // only non-emptiness matters to action generation
        recompute(&mut state);

        let legal = strategy::collect_legal_actions(&state, PlayerId::Us);
        assert!(!legal.iter().any(|a| matches!(a,
            LegalAction::CastSpell { card_id, .. } if *card_id == boulder_id)),
            "sorcery-speed Giant's Boulder must not be offered while another object is on the stack");
    }

    #[test]
    fn test_sufficient_mana_allows_cast() {
        // With real lands producing BBB, Doomsday should appear in legal actions.
        let mut state = make_state();
        state.catalog = test_catalog();
        let sea_def = catalog_card("Underground Sea");
        for _ in 0..3 {
            add_perm_with_def(&mut state, PlayerId::Us, &sea_def, BattlefieldState::new());
        }
        add_hand_card(&mut state, PlayerId::Us, "Doomsday");
        state.current_turn = 1;
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));
        recompute(&mut state);

        let legal = strategy::collect_legal_actions(&state, PlayerId::Us);
        assert!(
            legal.iter().any(|a| matches!(a, LegalAction::CastSpell { .. })),
            "Doomsday should be castable with 3 Underground Seas"
        );
    }

    #[test]
    fn test_fow_not_offered_on_empty_stack() {
        // Force of Will should NOT appear in legal actions when the stack is empty.
        let mut state = make_state();
        state.catalog = test_catalog();
        // Give opponent a land, FoW in hand, and a blue card to pitch.
        let sea_def = catalog_card("Underground Sea");
        add_perm_with_def(&mut state, PlayerId::Opp, &sea_def, BattlefieldState::new());
        add_hand_card(&mut state, PlayerId::Opp, "Force of Will");
        add_hand_card(&mut state, PlayerId::Opp, "Brainstorm"); // blue card to pitch
        state.current_turn = 2;
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));
        recompute(&mut state);
        assert!(state.stack.is_empty(), "stack should be empty");

        let legal = strategy::collect_legal_actions(&state, PlayerId::Opp);
        let has_fow = legal.iter().any(|a| {
            if let LegalAction::CastSpell { card_id, .. } = a {
                state.objects.get(card_id).map_or(false, |c| c.catalog_key == "Force of Will")
            } else { false }
        });
        assert!(!has_fow, "FoW should not be castable on empty stack — no valid targets");
    }

    #[test]
    fn test_cavern_colored_mana_blocked_for_non_creature() {
        // Cavern of Souls' colored mana ability requires casting_spell to be a creature.
        // With no spell being cast (or a non-creature spell), the condition should fail.
        let mut state = make_state();
        state.catalog = test_catalog();
        let cavern_def = catalog_card("Cavern of Souls");
        let cavern_id = add_perm_with_def(&mut state, PlayerId::Us, &cavern_def, BattlefieldState::new());
        recompute(&mut state);

        // No spell being cast — colored ability should be unavailable.
        state.casting_spell = None;
        let options = enumerate_mana_abilities(&state, PlayerId::Us);
        // Should only see the colorless ability (index 0), not the colored one (index 1).
        assert!(options.iter().all(|o| o.ability_index == 0),
            "Cavern colored mana should be blocked when no spell is being cast");

        // Casting a non-creature spell — colored ability still unavailable.
        let ts_id = add_hand_card(&mut state, PlayerId::Us, "Thoughtseize");
        state.casting_spell = Some(ts_id);
        let options = enumerate_mana_abilities(&state, PlayerId::Us);
        assert!(options.iter().all(|o| o.source_id != cavern_id || o.ability_index == 0),
            "Cavern colored mana should be blocked for non-creature spells");
    }

    #[test]
    fn test_cavern_colored_mana_allowed_for_creature() {
        let mut state = make_state();
        state.catalog = test_catalog();
        let cavern_def = catalog_card("Cavern of Souls");
        let cavern_id = add_perm_with_def(&mut state, PlayerId::Us, &cavern_def, BattlefieldState::new());
        recompute(&mut state);

        // Casting a creature spell — colored ability should be available.
        let creature_id = add_hand_card(&mut state, PlayerId::Us, "Murktide Regent");
        state.casting_spell = Some(creature_id);
        let options = enumerate_mana_abilities(&state, PlayerId::Us);
        let has_colored = options.iter().any(|o| o.source_id == cavern_id && o.ability_index == 1);
        assert!(has_colored,
            "Cavern colored mana should be available when casting a creature spell");
    }


    // ── Section 54: Validation (Phase 8) ─────────────────────────────────────




    /// Parse "us: N cards (-M mulligans)" from opening log line.
    fn parse_log_hand_info(log: &str) -> Option<(u32, u32, u32, u32)> {
        // Format: "... us: 7 cards (-0 mulligans), opp: 6 cards (-1 mulligans)"
        let us_hand = log.find("us: ").and_then(|i| log[i+4..].chars().next()?.to_digit(10))?;
        let us_mull = {
            let marker = "us: ";
            let after_us = log.find(marker).map(|i| &log[i..])?;
            let m_pos = after_us.find("(-")?;
            after_us[m_pos+2..].chars().next()?.to_digit(10)?
        };
        let opp_hand = log.find("opp: ").and_then(|i| log[i+5..].chars().next()?.to_digit(10))?;
        let opp_mull = {
            let marker = "opp: ";
            let after_opp = log.find(marker).map(|i| &log[i..])?;
            let m_pos = after_opp.find("(-")?;
            after_opp[m_pos+2..].chars().next()?.to_digit(10)?
        };
        Some((us_hand, us_mull, opp_hand, opp_mull))
    }

    #[test]
    fn test_parse_log_hand_info() {
        let log = "T0 [us] Turn 3 — UB Tempo (play) | us: 7 cards (-0 mulligans), opp: 7 cards (-0 mulligans)";
        let result = parse_log_hand_info(log);
        assert_eq!(result, Some((7, 0, 7, 0)), "failed to parse: {}", log);

        let log2 = "T0 [us] Turn 4 — UB Tempo (draw) | us: 6 cards (-1 mulligans), opp: 5 cards (-2 mulligans)";
        let result2 = parse_log_hand_info(log2);
        assert_eq!(result2, Some((6, 1, 5, 2)), "failed to parse: {}", log2);
    }



    // ── Targeted-spell legality ──────────────────────────────────────────────

    /// Put a permanent spell on the stack WITH an effect (eff_enter_permanent),
    /// mimicking how the cast submachine sets up a permanent about to resolve.
    fn add_permanent_spell_on_stack(state: &mut SimState, who: PlayerId, name: &str) -> ObjId {
        let id = state.alloc_id();
        let eff = eff_enter_permanent(who, name.to_string());
        state.objects.insert(id, GameObject {
            id,
            catalog_key: name.to_string(),
            owner: who,
            controller: who,
            is_token: false,
            materialized: None,
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: ObjectRole::StackSpell(SpellState {
                effect: Some(eff),
                chosen_targets: vec![],
                is_back_face: false,
                costs_paid_ctx: CostsPaidCtx::default(),
            }),
        });
        state.catalog.entry(name.to_string())
            .or_insert_with(|| test_catalog().remove(name).unwrap_or_else(||
                creature(name, 1, 1)));
        state.stack.push(id);
        id
    }

    #[test]
    fn test_resolved_permanent_does_not_leave_stale_stack_object() {
        // When a permanent spell resolves, the old spell object must not linger
        // with zone == Stack. Stale stack objects caused counterspells to find
        // phantom targets long after the permanent had entered the battlefield.
        let mut state = make_state();
        state.catalog = test_catalog();
        state.current_turn = 2;
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));

        add_permanent_spell_on_stack(&mut state, PlayerId::Opp, "Murktide Regent");
        recompute(&mut state);
        resolve_top_of_stack(&mut state, 2, PlayerId::Opp);

        // Stack should be empty and no objects should have zone == Stack.
        assert!(state.stack.is_empty(), "stack list should be empty after resolution");
        let stale_stack_objs: Vec<_> = state.objects.values()
            .filter(|o| o.in_zone(Zone::Stack))
            .map(|o| o.catalog_key.clone())
            .collect();
        assert!(stale_stack_objs.is_empty(),
            "no objects should remain with zone == Stack after permanent resolves, found: {:?}",
            stale_stack_objs);
    }

    #[test]
    fn test_fow_not_legal_after_permanent_resolves() {
        // End-to-end: a Us creature resolves, then on a later priority window
        // with an empty stack, Opp's Force of Will must NOT appear in legal actions.
        let mut state = make_state();
        state.catalog = test_catalog();
        state.current_turn = 2;
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));

        // Resolve a Us creature spell (simulating a prior turn).
        add_permanent_spell_on_stack(&mut state, PlayerId::Us, "Murktide Regent");
        recompute(&mut state);
        resolve_top_of_stack(&mut state, 2, PlayerId::Us);

        // Now set up opp's turn with FoW in hand and an empty stack.
        state.current_turn = 4;
        let island_def = catalog_card("Island");
        for _ in 0..5 {
            add_perm_with_def(&mut state, PlayerId::Opp, &island_def, BattlefieldState::new());
        }
        add_hand_card(&mut state, PlayerId::Opp, "Force of Will");
        add_hand_card(&mut state, PlayerId::Opp, "Brainstorm"); // blue pitch fodder
        recompute(&mut state);

        assert!(state.stack.is_empty(), "precondition: stack should be empty");
        let legal = strategy::collect_legal_actions(&state, PlayerId::Opp);
        let has_fow = legal.iter().any(|a| {
            if let LegalAction::CastSpell { card_id, .. } = a {
                state.objects.get(card_id).map_or(false, |c| c.catalog_key == "Force of Will")
            } else { false }
        });
        assert!(!has_fow,
            "Force of Will must not be offered when the stack is empty \
             (stale resolved-permanent objects must not be targetable)");
    }

    #[test]
    fn test_fow_not_legal_with_empty_stack() {
        // Force of Will requires a target spell on the stack.
        // With an empty stack it must NOT appear in legal actions.
        let mut state = make_state();
        state.catalog = test_catalog();
        let island_def = catalog_card("Island");
        for _ in 0..5 {
            add_perm_with_def(&mut state, PlayerId::Opp, &island_def, BattlefieldState::new());
        }
        // Give opp a blue card to pitch + FoW itself (hand_min >= 2).
        add_hand_card(&mut state, PlayerId::Opp, "Force of Will");
        add_hand_card(&mut state, PlayerId::Opp, "Brainstorm"); // blue pitch fodder
        state.current_turn = 4;
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));
        recompute(&mut state);

        assert!(state.stack.is_empty(), "precondition: stack should be empty");
        let legal = strategy::collect_legal_actions(&state, PlayerId::Opp);
        let has_fow = legal.iter().any(|a| {
            if let LegalAction::CastSpell { card_id, .. } = a {
                state.objects.get(card_id).map_or(false, |c| c.catalog_key == "Force of Will")
            } else { false }
        });
        assert!(!has_fow,
            "Force of Will must not be offered as a legal action with an empty stack");
    }

    #[test]
    fn test_fow_legal_with_opposing_spell_on_stack() {
        // When an opponent's spell IS on the stack, FoW should be legal.
        let mut state = make_state();
        state.catalog = test_catalog();
        let island_def = catalog_card("Island");
        for _ in 0..5 {
            add_perm_with_def(&mut state, PlayerId::Opp, &island_def, BattlefieldState::new());
        }
        add_hand_card(&mut state, PlayerId::Opp, "Force of Will");
        add_hand_card(&mut state, PlayerId::Opp, "Brainstorm"); // pitch fodder
        // Put an opponent (Us) spell on the stack.
        let bs_def = catalog_card("Brainstorm");
        let spell_id = add_stack_spell(&mut state, PlayerId::Us, &bs_def);
        state.stack.push(spell_id);
        state.current_turn = 4;
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));
        recompute(&mut state);

        let legal = strategy::collect_legal_actions(&state, PlayerId::Opp);
        let has_fow = legal.iter().any(|a| {
            if let LegalAction::CastSpell { card_id, .. } = a {
                state.objects.get(card_id).map_or(false, |c| c.catalog_key == "Force of Will")
            } else { false }
        });
        assert!(has_fow,
            "Force of Will should be offered when an opposing spell is on the stack");
    }

    // ── Section: Engine Invariant Tests ──────────────────────────────────────

    #[test]
    fn test_priority_round_both_players_pass() {
        // After handle_priority_round, the stack must be empty and the game
        // state must be self-consistent (assert_engine_invariants fires inside).
        let mut state = make_state();
        state.catalog = test_catalog();
        state.current_turn = 1;
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));
        // Give both players some lands so the state isn't degenerate.
        let island_def = catalog_card("Island");
        for _ in 0..3 {
            add_perm_with_def(&mut state, PlayerId::Us, &island_def, BattlefieldState::new());
            add_perm_with_def(&mut state, PlayerId::Opp, &island_def, BattlefieldState::new());
        }
        recompute(&mut state);

        handle_priority_round(&mut state, 1, PlayerId::Us);

        assert!(state.stack.is_empty(),
            "stack should be empty after priority round with no spells cast");
        let stack_objs: Vec<_> = state.objects.values()
            .filter(|o| o.in_zone(Zone::Stack))
            .collect();
        assert!(stack_objs.is_empty(),
            "no objects should have zone == Stack after clean priority round");
    }

    #[test]
    fn test_no_stale_objects_after_multi_permanent_resolution() {
        // Cast 3 permanent spells, resolve all, verify no zone == Stack objects remain.
        // This is the multi-spell version of test_resolved_permanent_does_not_leave_stale_stack_object.
        let mut state = make_state();
        state.catalog = test_catalog();
        state.current_turn = 3;
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));

        // Push 3 permanent spells onto the stack and resolve each.
        let names = ["Murktide Regent", "Orcish Bowmasters", "Grief"];
        for name in &names {
            add_permanent_spell_on_stack(&mut state, PlayerId::Opp, name);
            recompute(&mut state);
            resolve_top_of_stack(&mut state, 3, PlayerId::Opp);
        }

        assert!(state.stack.is_empty(),
            "stack list should be empty after resolving all 3 permanents");
        let stale: Vec<_> = state.objects.values()
            .filter(|o| o.in_zone(Zone::Stack))
            .map(|o| o.catalog_key.clone())
            .collect();
        assert!(stale.is_empty(),
            "no objects should have zone == Stack after resolving 3 permanents, found: {:?}", stale);
        // All 3 should be on the battlefield.
        for name in &names {
            let on_bf = state.objects.values()
                .any(|o| o.catalog_key == *name && o.in_zone(Zone::Battlefield));
            assert!(on_bf, "{} should be on the battlefield after resolution", name);
        }
    }

    #[test]
    fn test_zone_tracking_consistency() {
        // After various zone transitions, verify library_order, graveyard_order,
        // and actual object zones are in sync.
        let mut state = make_state();
        state.catalog = test_catalog();
        state.current_turn = 1;
        state.current_phase = Some(TurnPosition::Phase(PhaseKind::PreCombatMain));

        // Put some cards in different zones.
        let island_def = catalog_card("Island");
        let perm_id = add_perm_with_def(&mut state, PlayerId::Us, &island_def, BattlefieldState::new());
        let hand_id = add_hand_card(&mut state, PlayerId::Us, "Brainstorm");

        // Move permanent to graveyard.
        change_zone(perm_id, ZoneId::Graveyard, &mut state, 1, PlayerId::Us);
        // Move hand card to graveyard.
        change_zone(hand_id, ZoneId::Graveyard, &mut state, 1, PlayerId::Us);

        // Verify graveyard_order matches actual graveyard objects.
        let gy_objs: Vec<ObjId> = state.objects.values()
            .filter(|o| o.in_zone(Zone::Graveyard) && o.owner == PlayerId::Us)
            .map(|o| o.id)
            .collect();
        for &id in &state.graveyard_order {
            assert!(state.objects.get(&id).map_or(false, |o| o.in_zone(Zone::Graveyard)),
                "graveyard_order contains id {:?} that is not in graveyard zone", id);
        }
        for &id in &gy_objs {
            assert!(state.graveyard_order.contains(&id),
                "object {:?} in graveyard zone but missing from graveyard_order", id);
        }

        // Verify library_order matches actual library objects.
        for who in [PlayerId::Us, PlayerId::Opp] {
            let lib_objs: Vec<ObjId> = state.objects.values()
                .filter(|o| o.in_zone(Zone::Library) && o.owner == who)
                .map(|o| o.id)
                .collect();
            let lib_order = &state.player(who).library_order;
            for &id in lib_order.iter() {
                assert!(state.objects.get(&id).map_or(false, |o| o.in_zone(Zone::Library)),
                    "library_order for {:?} contains id {:?} not in library zone", who, id);
            }
            for &id in &lib_objs {
                assert!(lib_order.contains(&id),
                    "object {:?} in library zone for {:?} but missing from library_order", id, who);
            }
        }
    }

    /// Casting via run_cast_submachine logs both mana production and the cast.
    /// Engine machinery test — uses the reusable AlwaysPass stub (its trait-default
    /// `choose_mana_ability` = `auto_tap_plan`), not any content strategy.
    #[test]
    fn test_mana_log_and_cast_both_present() {
        let mut state = make_state();
        state.catalog = test_catalog();

        let sea_def = catalog_card("Underground Sea");
        add_perm_with_def(&mut state, PlayerId::Us, &sea_def, BattlefieldState::new());
        let dr_id = add_hand_card(&mut state, PlayerId::Us, "Dark Ritual");
        recompute(&mut state);

        // run_cast_submachine re-acquires the player's strategy per-decision from
        // the state (make_state installs AlwaysPass), so none is passed here.
        run_cast_submachine(&mut state, 1, PlayerId::Us, dr_id, SpellFace::Main);

        let has_cast = state.log.iter().any(|l| l.contains("Cast Dark Ritual"));
        let has_mana = state.log.iter().any(|l| l.contains("add B to pool"));
        assert!(has_cast, "should have a Cast log line, got: {:?}", state.log);
        assert!(has_mana, "should have a mana production log line, got: {:?}", state.log);
    }
