#![allow(dead_code)]
//! Generic interpreter. Stage-2 implementation: real `eval_expr` and
//! `matches` over `SimState`; `execute` is still stubbed.
//!
//! Design:
//! - `eval_expr` — pure query, no state mutation
//! - `matches` — filter-style `Expr<Bool>` over a candidate
//! - `execute` — dispatches `Action` variants (Stage-2 slice 2)
//! - `deps_of`/`writes_of` — auto-derived CE dependency axes (Stage-2 slice 3)

use crate::ir::action::{Action, Who};
use crate::ir::ce::CEMod;
use crate::ir::context::{Ctx, GameCtx};
use crate::ir::expr::{Expr, Filter, Value, ZoneKindSel, ZoneSel};
use crate::{
    change_zone, destroy_one, CardKind, CardType, Zone, ChoiceRequest, ChoiceResult, Keyword,
    ObjId, PlayerId, SimState, ZoneId,
};
use std::collections::HashMap;

// ── BindEnv ──────────────────────────────────────────────────────────────────

/// The "invocation frame": who's the source, who's "you", what event (if any)
/// triggered us, plus per-step user bindings.
#[derive(Clone, Default)]
pub struct BindEnv {
    pub(crate) source: Option<ObjId>,
    pub(crate) controller: Option<PlayerId>,
    pub(crate) subj: Option<Value>,
    pub(crate) bindings: HashMap<&'static str, Value>,
    /// Color hint chosen at activation time for `Action::AddMana` with
    /// `ManaSpec::AnyOneColor`. Set by the activated-ability dispatch when
    /// the strategy picks a color; consumed by the AddMana executor arm.
    pub(crate) chosen_color: Option<crate::Color>,
    /// The event that triggered/replaced us, projected by `Ctx::Triggering`.
    /// Captured at match time (so it survives to a triggered ability's deferred
    /// resolution) and during a replacement's synchronous body. `ThisCast` does
    /// not use this — it scans the log for the source's own `SpellCast`.
    pub(crate) triggering_event: Option<crate::GameEvent>,
}

impl BindEnv {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn with_source(mut self, id: ObjId) -> Self {
        self.source = Some(id);
        self
    }

    pub(crate) fn with_controller(mut self, p: PlayerId) -> Self {
        self.controller = Some(p);
        self
    }

    pub(crate) fn with_var(mut self, name: &'static str, value: Value) -> Self {
        self.bindings.insert(name, value);
        self
    }

    pub(crate) fn with_subj(mut self, value: Value) -> Self {
        self.subj = Some(value);
        self
    }

    pub(crate) fn with_chosen_color(mut self, c: Option<crate::Color>) -> Self {
        self.chosen_color = c;
        self
    }

    pub(crate) fn with_triggering_event(mut self, e: crate::GameEvent) -> Self {
        self.triggering_event = Some(e);
        self
    }

    pub(crate) fn get(&self, name: &str) -> Option<&Value> {
        self.bindings.get(name)
    }
}

// ── Execute (Stage-2 slice 2, still stubbed) ────────────────────────────────

pub enum ExecResult {
    Ok,
    Unimplemented(&'static str),
    /// `Action::PayMana` reached with insufficient pool. The cost driver should
    /// yield control to the strategy so it can activate mana abilities (each a
    /// PlayableAction in its own right) and then resume the cost. CR 601.2g
    /// mana sub-loop is realised by this loop, not by a separate primitive.
    ManaShortage(crate::ManaCost),
}

pub(crate) fn execute(action: &Action, state: &mut SimState, env: &BindEnv) -> ExecResult {
    let mut env = env.clone();
    execute_mut(action, state, &mut env)
}

pub(crate) fn execute_mut(action: &Action, state: &mut SimState, env: &mut BindEnv) -> ExecResult {
    let t = state.current_turn as u8;
    let actor = env.controller.unwrap_or(PlayerId::Us);
    match action {
        Action::Noop => ExecResult::Ok,

        Action::Draw { who, n } => {
            let who = resolve_who(who, state, env, actor);
            let n = expect_num(eval_expr(n, state, env)) as usize;
            for _ in 0..n {
                crate::sim_draw(state, who, t, false);
            }
            ExecResult::Ok
        }

        Action::GainLife { who, amount } => {
            let who = resolve_who(who, state, env, actor);
            let n = expect_num(eval_expr(amount, state, env)) as i32;
            state.gain_life(who, n);
            ExecResult::Ok
        }

        Action::PayLife { who, amount } => {
            let who = resolve_who(who, state, env, actor);
            let n = expect_num(eval_expr(amount, state, env)) as i32;
            state.lose_life(who, n);
            ExecResult::Ok
        }

        Action::DealDamage { source, target, amount } => {
            let src = match eval_expr(source, state, env) {
                Value::Obj(o) => Some(o),
                _ => None,
            };
            let tgt = eval_expr(target, state, env);
            let n = expect_num(eval_expr(amount, state, env)) as i32;
            match tgt {
                Value::Player(p) => {
                    if !state.player(p).protection_from_everything { state.lose_life(p, n); }
                },
                // A player is an object too: damage to a player-object is life loss.
                Value::Obj(id) if state.is_player(id) => {
                    let p = state.who_pid(id);
                    if !state.player(p).protection_from_everything { state.lose_life(p, n); }
                },
                Value::Obj(id) => {
                    // CR 702.16e: a permanent with protection from the source's
                    // quality can't be damaged by it. Enforced here (not just at
                    // targeting) so "deal damage to each" effects respect it too.
                    let protected = src.map_or(false,
                        |s| crate::predicates::is_protected_from(id, s, state));
                    if !protected {
                        if let Some(bf) = state.permanent_bf_mut(id) {
                            bf.damage += n;
                        }
                    }
                }
                _ => {}
            }
            ExecResult::Ok
        }

        Action::PutCounters { on, kind, n } => {
            let id = expect_obj(eval_expr(on, state, env));
            let n = expect_num(eval_expr(n, state, env));
            // +1/+1 and loyalty counters are zone-scoped and folded into a stat,
            // so they live on BattlefieldState (counters / loyalty) rather than
            // the cross-zone generic map. All other kinds use the generic map.
            match kind {
                crate::CounterType::PlusOnePlusOne => {
                    if let Some(bf) = state.permanent_bf_mut(id) {
                        bf.counters += n as i32;
                    }
                }
                crate::CounterType::Loyalty => {
                    if let Some(bf) = state.permanent_bf_mut(id) {
                        bf.loyalty += n as i32;
                    }
                }
                crate::CounterType::Stun => {
                    if let Some(bf) = state.permanent_bf_mut(id) {
                        bf.stun_counters += n as u32;
                    }
                }
                _ => {
                    if let Some(obj) = state.objects.get_mut(&id) {
                        *obj.counters.entry(*kind).or_insert(0) += n as u32;
                    }
                }
            }
            ExecResult::Ok
        }

        Action::RemoveCounters { from, kind, n } => {
            let id = expect_obj(eval_expr(from, state, env));
            let n = expect_num(eval_expr(n, state, env));
            match kind {
                crate::CounterType::PlusOnePlusOne => {
                    if let Some(bf) = state.permanent_bf_mut(id) {
                        bf.counters = (bf.counters - n as i32).max(0);
                    }
                }
                crate::CounterType::Loyalty => {
                    if let Some(bf) = state.permanent_bf_mut(id) {
                        bf.loyalty = (bf.loyalty - n as i32).max(0);
                    }
                }
                crate::CounterType::Stun => {
                    if let Some(bf) = state.permanent_bf_mut(id) {
                        bf.stun_counters = bf.stun_counters.saturating_sub(n as u32);
                    }
                }
                _ => {
                    if let Some(obj) = state.objects.get_mut(&id) {
                        if let Some(c) = obj.counters.get_mut(kind) {
                            *c = c.saturating_sub(n as u32);
                        }
                    }
                }
            }
            ExecResult::Ok
        }

        Action::Destroy { target } => {
            let id = expect_obj(eval_expr(target, state, env));
            destroy_one(id, state, t, actor);
            ExecResult::Ok
        }

        Action::Tap { target } => {
            let ids = obj_ids_of(eval_expr(target, state, env));
            for id in ids {
                if let Some(bf) = state.permanent_bf_mut(id) {
                    bf.tapped = true;
                }
            }
            ExecResult::Ok
        }

        Action::Untap { target } => {
            let ids = obj_ids_of(eval_expr(target, state, env));
            for id in ids {
                if let Some(bf) = state.permanent_bf_mut(id) {
                    bf.tapped = false;
                }
            }
            ExecResult::Ok
        }

        Action::Transform { target } => {
            let id = expect_obj(eval_expr(target, state, env));
            // Only a permanent can transform; off-battlefield is a no-op.
            let Some(cur_face) = state.permanent_bf(id).map(|bf| bf.active_face) else {
                return ExecResult::Ok;
            };
            let new_face = 1 - cur_face; // DFCs have exactly two faces (CR 712.2)
            let controller = state.objects.get(&id).map(|o| o.controller).unwrap_or(actor);
            // Starting loyalty if the new face is a planeswalker (CR 711.3c).
            let new_loyalty = state.def_of(id).and_then(|d| {
                let face = if new_face == 1 { d.back.as_deref() } else { Some(d) };
                face.and_then(|fd| match &fd.kind {
                    crate::CardKind::Planeswalker(p) => Some(p.loyalty),
                    _ => None,
                })
            });
            if let Some(bf) = state.permanent_bf_mut(id) {
                bf.active_face = new_face;
                if let Some(loy) = new_loyalty {
                    bf.loyalty = loy;
                }
            }
            let name = state.objects.get(&id).map(|o| o.catalog_key.clone()).unwrap_or_default();
            state.log(t, controller, format!("{} transforms", name));
            crate::fire_event(crate::GameEvent::Transformed { id, controller }, state, t, controller);
            ExecResult::Ok
        }

        Action::Attach { what, to } => {
            let attachment = expect_obj(eval_expr(what, state, env));
            let target = expect_obj(eval_expr(to, state, env));
            let controller = state.objects.get(&attachment).map(|o| o.controller).unwrap_or(actor);
            if let Some(bf) = state.permanent_bf_mut(attachment) {
                bf.attached_to = Some(target);
            } else {
                return ExecResult::Ok; // attachment not on the battlefield; no-op
            }
            let aname = state.objects.get(&attachment).map(|o| o.catalog_key.clone()).unwrap_or_default();
            let tname = state.permanent_name(target).unwrap_or_default();
            state.log(t, controller, format!("{} attached to {}", aname, tname));
            crate::fire_event(
                crate::GameEvent::BecameAttached { attachment, target, controller },
                state, t, controller,
            );
            ExecResult::Ok
        }

        Action::Exile { target, bind_as } => {
            let v = eval_expr(target, state, env);
            if let Value::Obj(id) = v {
                change_zone(id, ZoneId::Exile, state, t, actor);
                if let Some(name) = bind_as {
                    env.bindings.insert(name, Value::Obj(id));
                }
            }
            ExecResult::Ok
        }

        Action::Return { what, to, bind_as } => {
            let v = eval_expr(what, state, env);
            if let Value::Obj(id) = v {
                let zone = zone_id_from_kind(*to);
                change_zone(id, zone, state, t, actor);
                if let Some(name) = bind_as {
                    env.bindings.insert(name, Value::Obj(id));
                }
            }
            ExecResult::Ok
        }

        Action::Mill { who, count } => {
            let who = resolve_who(who, state, env, actor);
            let n = expect_num(eval_expr(count, state, env)) as usize;
            let top: Vec<ObjId> = state.library_of(who).take(n).map(|o| o.id).collect();
            for id in top {
                change_zone(id, ZoneId::Graveyard, state, t, who);
            }
            ExecResult::Ok
        }

        Action::Shuffle { who } => {
            let who = resolve_who(who, state, env, actor);
            state.shuffle_library(who);
            ExecResult::Ok
        }

        // Control flow
        Action::Sequence(actions) => {
            for a in actions {
                match execute_mut(a, state, env) {
                    ExecResult::Ok => continue,
                    other => return other,
                }
            }
            ExecResult::Ok
        }

        Action::IfThen { cond, then, else_ } => {
            if expect_bool(eval_expr(cond, state, env)) {
                execute(then, state, env);
            } else if let Some(e) = else_ {
                execute(e, state, env);
            }
            ExecResult::Ok
        }

        Action::ForEach { over, bind, body } => {
            match eval_expr(over, state, env) {
                Value::ObjSet(ids) => {
                    for id in ids {
                        let sub_env = env.clone().with_var(bind, Value::Obj(id));
                        execute(body, state, &sub_env);
                    }
                }
                Value::PlayerSet(players) => {
                    for player in players {
                        let sub_env = env.clone().with_var(bind, Value::Player(player));
                        execute(body, state, &sub_env);
                    }
                }
                _ => {}
            }
            ExecResult::Ok
        }

        // ── Agency-gated actions ─────────────────────────────────────────
        Action::Counter { target } => {
            let id = expect_obj(eval_expr(target, state, env));
            crate::effects::counter_one(id, state, t, actor);
            ExecResult::Ok
        }

        Action::Ward { cost } => {
            // Resolves a ward trigger: the targeting spell (triggered_obj) and its
            // caster (triggered_actor) come from the trigger's bind env; the warded
            // permanent is the ability source, the holder is its controller. The
            // helper runs the opponent's pay-or-counter decision.
            let (Some(Value::Obj(spell)), Some(Value::Player(caster))) =
                (env.get("triggered_obj").cloned(), env.get("triggered_actor").cloned())
            else {
                return ExecResult::Ok;
            };
            let ward_source = env.source.unwrap_or(ObjId::UNSET);
            let ward_holder = env.controller.unwrap_or(actor);
            crate::effects::ward_pay_or_counter(
                ward_source, cost, spell, caster, ward_holder, state, t,
            );
            ExecResult::Ok
        }

        Action::CopySpell { what, n, new_targets } => {
            let spell_id = expect_obj(eval_expr(what, state, env));
            let n = expect_num(eval_expr(n, state, env)) as usize;
            if n == 0 {
                return ExecResult::Ok;
            }
            // Resolve original spell's CardDef, controller, chosen_x/mode,
            // target_spec, and current targets.
            let Some(spell_obj) = state.objects.get(&spell_id) else {
                return ExecResult::Ok;
            };
            let controller = spell_obj.controller;
            let key = spell_obj.catalog_key.clone();
            let Some(spell_state) = spell_obj.spell() else {
                return ExecResult::Ok;
            };
            let original_targets = spell_state.chosen_targets.clone();
            let chosen_x = spell_state.costs_paid_ctx.chosen_x;
            let chosen_mode = spell_state.costs_paid_ctx.chosen_mode;
            let Some(def) = state.catalog.get(&key).cloned() else {
                return ExecResult::Ok;
            };
            let (copy_target_spec, _) =
                crate::catalog::build_spell_effect(&def, controller, spell_id, chosen_x, chosen_mode);
            let name = def.name.clone();

            // Target selection: if `new_targets`, prefer legal targets not
            // already in `original_targets`; fall back to the originals.
            let all_legal = if matches!(copy_target_spec, crate::TargetSpec::None) {
                Vec::new()
            } else {
                crate::predicates::legal_targets(&copy_target_spec, controller, spell_id, state)
            };
            let mut unused: Vec<ObjId> = if *new_targets {
                all_legal
                    .iter()
                    .filter(|id| !original_targets.contains(id))
                    .copied()
                    .collect()
            } else {
                Vec::new()
            };

            for _ in 0..n {
                let targets = if matches!(copy_target_spec, crate::TargetSpec::None) {
                    Vec::new()
                } else if !unused.is_empty() {
                    vec![unused.remove(0)]
                } else if !original_targets.is_empty() {
                    vec![original_targets[0]]
                } else {
                    break;
                };
                let copy_id = state.alloc_id();
                let (_, copy_eff) =
                    crate::catalog::build_spell_effect(&def, controller, copy_id, chosen_x, chosen_mode);
                state.insert_stack_ability(copy_id, name.clone(), controller, crate::AbilityState {
                    effect: copy_eff,
                    chosen_targets: targets.clone(),
                    costs_paid_ctx: crate::CostsPaidCtx::default(),
                    is_triggered: false,
                    counterable: true,
                    choice_spec: None,
                });
                let tgt_label = targets.first()
                    .map(|&id| state.stack_item_display_name(id).to_string())
                    .unwrap_or_default();
                state.log(t, controller, format!("Copy → {} (targeting {})", name, tgt_label));
            }
            ExecResult::Ok
        }

        Action::Sacrifice { who, filter, count, bind_as: _ } => {
            let who = resolve_who(who, state, env, actor);
            let n = expect_num(eval_expr(count, state, env)) as usize;
            for _ in 0..n {
                let candidates: Vec<ObjId> = state
                    .permanents_of(who)
                    .map(|o| o.id)
                    .filter(|id| matches(filter, *id, state, env))
                    .collect();
                if candidates.is_empty() {
                    break;
                }
                let chosen = state.with_strategy(who, |s, st| s.sacrifice_choice(who, &candidates, st));
                if let Some(id) = chosen {
                    change_zone(id, ZoneId::Graveyard, state, t, actor);
                } else {
                    break;
                }
            }
            ExecResult::Ok
        }

        Action::MoveByChoice { who, from: _, to, verb: _, filter, count, bind_as } => {
            // Generalised "player picks K objects matching `filter` from
            // `from` zone, moves to `to` zone." Consumes the strategy's
            // BindEnv answer directly (no callback) — `bind_as` names the
            // binding the schema decision was emitted under, and the
            // strategy's `propose_announcement` filled it in.
            //
            // The `verb` tag is informational at this layer until sac/discard
            // triggers actually consume the distinction in code. The (from,
            // to) pair drives the zone change; the verb survives to fire
            // event-family-specific triggers when those land.
            let who = resolve_who(who, state, env, actor);
            let n = expect_num(eval_expr(count, state, env)) as usize;
            if n == 0 {
                return ExecResult::Ok;
            }
            let Some(name) = bind_as else {
                return ExecResult::Unimplemented(
                    "MoveByChoice without bind_as (binding-driven selection requires a name)",
                );
            };
            let chosen = match env.get(name) {
                Some(crate::ir::expr::Value::ObjSet(ids)) => ids.clone(),
                Some(crate::ir::expr::Value::Obj(id)) => vec![*id],
                _ => return ExecResult::Unimplemented(
                    "MoveByChoice: binding missing or wrong shape",
                ),
            };
            let dest = zone_id_from_kind(*to);
            for id in chosen.into_iter().take(n) {
                if !matches(filter, id, state, env) {
                    continue;
                }
                change_zone(id, dest, state, t, who);
            }
            ExecResult::Ok
        }

        Action::Discard { who, count, at_random: _, filter } => {
            use rand::Rng;
            let who = resolve_who(who, state, env, actor);
            let n = expect_num(eval_expr(count, state, env)) as usize;
            for _ in 0..n {
                let candidates: Vec<ObjId> = state
                    .hand_of(who)
                    .map(|o| o.id)
                    .filter(|id| match filter {
                        Some(f) => matches(f, *id, state, env),
                        None => true,
                    })
                    .collect();
                if candidates.is_empty() {
                    break;
                }
                let pick = state.rng.gen_range(0..candidates.len());
                let id = candidates[pick];
                change_zone(id, ZoneId::Graveyard, state, t, who);
            }
            ExecResult::Ok
        }

        Action::Reveal { who: _, what } => {
            let ids = obj_ids_of(eval_expr(what, state, env));
            for id in ids {
                if let Some(obj) = state.objects.get_mut(&id) {
                    if obj.in_zone(Zone::Hand { known: false }) {
                        obj.set_zone(Zone::Hand { known: true });
                    }
                }
            }
            ExecResult::Ok
        }

        Action::Scry { who, n } => {
            let who = resolve_who(who, state, env, actor);
            let n = expect_num(eval_expr(n, state, env)) as usize;
            let top: Vec<ObjId> = state.library_of(who).take(n).map(|o| o.id).collect();
            if top.is_empty() {
                return ExecResult::Ok;
            }
            // Keep/bottom + the kept order is a player decision (CR 701.18).
            let (keep, bottom) = state.with_strategy(who, |s, st| s.scry(&top, st));
            // Sanitize: only looked-at ids, deduped; any omitted card stays on top.
            let mut seen: std::collections::HashSet<ObjId> = Default::default();
            let mut keep: Vec<ObjId> =
                keep.into_iter().filter(|id| top.contains(id) && seen.insert(*id)).collect();
            let bottom: Vec<ObjId> =
                bottom.into_iter().filter(|id| top.contains(id) && seen.insert(*id)).collect();
            for &id in &top {
                if !keep.contains(&id) && !bottom.contains(&id) {
                    keep.push(id);
                }
            }
            let lib = &mut state.player_mut(who).library_order;
            for _ in 0..top.len().min(lib.len()) {
                lib.pop_front();
            }
            for &id in keep.iter().rev() {
                lib.push_front(id);
            }
            for &id in &bottom {
                lib.push_back(id);
            }
            // The kept cards on top were looked at and ordered by the controller.
            state.player_mut(who).known_top_len = keep.len();
            ExecResult::Ok
        }

        Action::Surveil { who, n } => {
            let who = resolve_who(who, state, env, actor);
            let n = expect_num(eval_expr(n, state, env)) as usize;
            for _ in 0..n {
                let Some(id) = state.library_of(who).next().map(|o| o.id) else {
                    break;
                };
                if state.with_strategy(who, |s, st| s.surveil_choice(id, st)) {
                    change_zone(id, ZoneId::Graveyard, state, t, who);
                }
            }
            // Surveil reorders/removes the top; conservatively forget the known prefix
            // (under-claim is safe — it can only cost a recognition, never cheat).
            state.player_mut(who).known_top_len = 0;
            ExecResult::Ok
        }

        Action::OrderTop { who, n } => {
            let who = resolve_who(who, state, env, actor);
            let n = expect_num(eval_expr(n, state, env)) as usize;
            let top: Vec<ObjId> = state.library_of(who).take(n).map(|o| o.id).collect();
            if top.len() < 2 {
                return ExecResult::Ok; // 0 or 1 card — nothing to arrange.
            }
            // The arrangement is the player's decision.
            let ordered = state.with_strategy(who, |s, st| s.order_top_library(&top, st));
            // Guard against a misbehaving strategy: keep only the looked-at cards,
            // and append any it dropped, so the library can't be corrupted.
            let mut final_order: Vec<ObjId> =
                ordered.into_iter().filter(|id| top.contains(id)).collect();
            for &id in &top {
                if !final_order.contains(&id) { final_order.push(id); }
            }
            let lib = &mut state.player_mut(who).library_order;
            for _ in 0..top.len().min(lib.len()) { lib.pop_front(); }
            for &id in final_order.iter().rev() { lib.push_front(id); }
            // The controller looked at and ordered all of these — the top is known.
            state.player_mut(who).known_top_len = final_order.len();
            ExecResult::Ok
        }

        Action::Look { who, zone, n } => {
            let who = resolve_who(who, state, env, actor);
            let n = expect_num(eval_expr(n, state, env)) as usize;
            if matches!(zone, ZoneKindSel::Library) {
                let ids: Vec<ObjId> = state.library_of(who).take(n).map(|o| o.id).collect();
                for id in ids {
                    if let Some(obj) = state.objects.get_mut(&id) {
                        if obj.in_zone(Zone::Hand { known: false }) {
                            obj.set_zone(Zone::Hand { known: true });
                        }
                    }
                }
            }
            ExecResult::Ok
        }

        Action::Search { who, zone, filter, count, dest, to_top, shuffle, bind_as: _ } => {
            let who = resolve_who(who, state, env, actor);
            let n = expect_num(eval_expr(count, state, env)) as usize;
            let dest_zone = zone_id_from_kind(*dest);
            let src = env.source.unwrap_or_default();
            let mut found: Vec<ObjId> = Vec::new();
            for _ in 0..n {
                let candidates: Vec<ObjId> = enumerate_kind_for_player(state, *zone, who)
                    .into_iter()
                    .filter(|id| matches(filter, *id, state, env))
                    .collect();
                if candidates.is_empty() {
                    break;
                }
                // Searching the library is a player choice (CR 701.19), not random —
                // the strategy picks which card matching the filter to find.
                let id = state
                    .with_strategy(who, |s, st| s.choose_for_effect(src, &candidates, st))
                    .unwrap_or(candidates[0]);
                change_zone(id, dest_zone, state, t, who);
                found.push(id);
            }
            // Order matters: shuffle FIRST, then place on top — e.g. Personal
            // Tutor / Vampiric Tutor are "shuffle, then put the card on top."
            // Doing it the other way would scatter the just-placed card.
            if *shuffle {
                state.shuffle_library(who);
            }
            if *to_top && matches!(*dest, ZoneKindSel::Library) {
                for &id in found.iter().rev() {
                    let lib = &mut state.player_mut(who).library_order;
                    if let Some(pos) = lib.iter().position(|&x| x == id) {
                        lib.remove(pos);
                        lib.push_front(id);
                    }
                }
                // We now KNOW the cards we just placed on top (a preceding shuffle, if
                // any, already cleared the rest), so the top `found.len()` is known.
                state.player_mut(who).known_top_len = found.len();
            }
            ExecResult::Ok
        }

        Action::Dig { who, n, take } => {
            let who = resolve_who(who, state, env, actor);
            let n = expect_num(eval_expr(n, state, env)).max(0) as usize;
            let take = expect_num(eval_expr(take, state, env)).max(0) as usize;
            // The looked-at cards are the top `n`, in library order.
            let top: Vec<ObjId> = state
                .player(who)
                .library_order
                .iter()
                .take(n)
                .copied()
                .collect();
            if top.is_empty() {
                return ExecResult::Ok;
            }
            let src = env.source.unwrap_or_default();
            // The player chooses which of the looked-at cards to keep (CR: a choice).
            let mut remaining = top.clone();
            let mut to_hand: Vec<ObjId> = Vec::new();
            for _ in 0..take.min(top.len()) {
                let pick = state
                    .with_strategy(who, |s, st| s.choose_for_effect(src, &remaining, st))
                    .unwrap_or(remaining[0]);
                remaining.retain(|&x| x != pick);
                to_hand.push(pick);
            }
            // Kept cards enter hand via a plain zone move — NOT a draw (no Draw
            // event), so draw-triggers like Orcish Bowmasters don't fire.
            for &id in &to_hand {
                change_zone(id, ZoneId::Hand, state, t, who);
            }
            // The rest go to the bottom of the library, in any order.
            for &id in &remaining {
                let lib = &mut state.player_mut(who).library_order;
                if let Some(pos) = lib.iter().position(|&x| x == id) {
                    lib.remove(pos);
                    lib.push_back(id);
                }
            }
            ExecResult::Ok
        }

        Action::MayDo { who, action } => {
            let who = resolve_who(who, state, env, actor);
            let src = env.source.unwrap_or(ObjId::default());
            let choice = state.with_strategy(who, |s, st| s.resolve_choice(src, &ChoiceRequest::Mode(2), st));
            let said_yes = matches!(choice, ChoiceResult::Mode(1));
            if said_yes {
                let sub_env = env.clone().with_controller(who);
                execute(action, state, &sub_env);
            }
            ExecResult::Ok
        }

        Action::CreateEmblem { abilities } => {
            // "You get an emblem with …" (CR 114). Register a command-zone emblem
            // controlled by the resolving player; recompute gathers its static
            // abilities each cycle and it never expires.
            let id = state.alloc_id();
            let timestamp = state.next_ci_timestamp();
            state.emblems.push(crate::EmblemInstance {
                id,
                controller: actor,
                abilities: abilities.clone(),
                timestamp,
            });
            ExecResult::Ok
        }

        Action::SimultaneousPut { who, from, filter, optional } => {
            use crate::ir::expr::ZoneKindSel;
            // Collect every selected player's choice (APNAP order) before placing
            // anything (CR 101.4): no player sees another's card first, and the
            // deferred pending_triggers fire together after this resolves.
            let players: Vec<PlayerId> = match who {
                Who::You => vec![actor],
                Who::Opponent | Who::EachOpponent => vec![actor.opp()],
                Who::Each => vec![actor, actor.opp()],
                Who::Player(e) => match eval_expr(e, state, env) {
                    Value::Player(p) => vec![p],
                    _ => Vec::new(),
                },
            };
            let mut to_place: Vec<(ObjId, PlayerId)> = Vec::new();
            for player in players {
                let in_zone: Vec<ObjId> = match from {
                    ZoneKindSel::Hand => state.hand_of(player).map(|c| c.id).collect(),
                    ZoneKindSel::Graveyard => state.graveyard_of(player).map(|c| c.id).collect(),
                    _ => Vec::new(),
                };
                let penv = BindEnv::new().with_controller(player);
                let candidates: Vec<ObjId> = in_zone
                    .into_iter()
                    .filter(|&id| matches(filter, id, state, &penv))
                    .collect();
                if candidates.is_empty() {
                    continue;
                }
                let chosen: Option<ObjId> = if *optional {
                    // "may" — the player chooses one or declines (CR 101.4).
                    let req = crate::ChoiceRequest::MayPutOnBattlefield { candidates: candidates.clone() };
                    match state.with_strategy(player, |s, st| s.resolve_choice(ObjId::UNSET, &req, st)) {
                        crate::ChoiceResult::OptionalObject(Some(id)) if candidates.contains(&id) => Some(id),
                        _ => None,
                    }
                } else {
                    // Mandatory — the player must put one if able.
                    state
                        .with_strategy(player, |s, st| s.choose_for_effect(ObjId::UNSET, &candidates, st))
                        .filter(|id| candidates.contains(id))
                        .or_else(|| candidates.first().copied())
                };
                if let Some(id) = chosen {
                    to_place.push((id, player));
                }
            }
            // Simultaneous placement — back-to-back change_zones whose triggers are
            // batched in pending_triggers (no triggers/SBAs fire between them).
            for (id, player) in to_place {
                crate::change_zone(id, crate::ZoneId::Battlefield, state, t, player);
            }
            ExecResult::Ok
        }

        Action::NinjutsuEnter => {
            // CR 702.49: put the ninja (Source, in hand) onto the battlefield
            // tapped and attacking, taking over the returned attacker's combat
            // slot. The returned creature's pre-move `attack_target` was captured
            // into CostsPaidCtx (it has since left the battlefield).
            let Some(source_id) = env.source else { return ExecResult::Ok };
            if !state.objects.contains_key(&source_id) { return ExecResult::Ok; }
            let attack_target = state
                .resolving_costs_ctx
                .returned_attack_targets
                .first()
                .copied()
                .flatten();
            crate::change_zone(source_id, crate::ZoneId::Battlefield, state, t, actor);
            if let Some(bf) = state.permanent_bf_mut(source_id) {
                bf.tapped = true;
                bf.entered_this_turn = true;
                bf.attacking = true;
                bf.unblocked = true;
                bf.attack_target = attack_target;
            }
            state.combat_attackers.push(source_id);
            ExecResult::Ok
        }

        Action::RecordEtbChoice { kind } => {
            // "As ~ enters, choose ..." (CR 614.12): ask the source's controller
            // and record the result on the source permanent's `etb_choice`, read
            // back by `Expr::ChosenColor`/`ChosenName`. Runs after the entry Move,
            // so the permanent is on the battlefield to store on.
            use crate::ir::action::EtbChoiceKind;
            if let Some(src) = env.source {
                let request = match kind {
                    EtbChoiceKind::Color => ChoiceRequest::Color,
                    EtbChoiceKind::CreatureType => ChoiceRequest::CreatureType,
                    EtbChoiceKind::CardName => ChoiceRequest::CardName,
                };
                let result = state.with_strategy(actor, |s, st| s.resolve_choice(src, &request, st));
                if let Some(bf) = state.permanent_bf_mut(src) {
                    bf.etb_choice = Some(result);
                }
            }
            ExecResult::Ok
        }

        Action::Choose { who, prompt: _, options, bind_as } => {
            let who = resolve_who(who, state, env, actor);
            let src = env.source.unwrap_or(ObjId::default());
            // Cost context (CR 601.2b): the branch was pre-decided at
            // announcement and lives in the BindEnv under `bind_as`. Run the
            // chosen option's action against the *same* env so its nested cost
            // decisions (e.g. which card to discard) resolve. The effect-
            // resolution path below (resolve_choice) is used only when there
            // is no pre-bound branch.
            if let Some(name) = bind_as {
                if let Some(crate::ir::expr::Value::Num(i)) = env.get(name).cloned() {
                    if let Some(opt) = options.get(i as usize) {
                        if let Some(c) = &opt.cost {
                            let _ = crate::pay_ir_cost(state, t, who, src, c, false);
                        }
                        return execute_mut(&opt.action, state, env);
                    }
                }
            }
            // Filter out options whose cost the chooser cannot pay (CR 118.4).
            // Free options (cost = None) always remain legal.
            let legal: Vec<usize> = options
                .iter()
                .enumerate()
                .filter(|(_, o)| match &o.cost {
                    None => true,
                    Some(c) => {
                        let schema_ok = crate::ir::cost_exec::build_schema(c, state, who, src).is_some();
                        let mana_ok = match crate::ir::ability::first_pay_mana_in_action(c) {
                            Some(mc) => state.potential_mana(who).can_pay(&mc),
                            None => true,
                        };
                        schema_ok && mana_ok
                    }
                })
                .map(|(i, _)| i)
                .collect();
            if legal.is_empty() {
                return ExecResult::Ok;
            }
            let nlegal = legal.len();
            let choice = state.with_strategy(who, |s, st| s.resolve_choice(src, &ChoiceRequest::Mode(nlegal), st));
            let picked = match choice {
                ChoiceResult::Mode(i) if i < nlegal => legal[i],
                _ => legal[0],
            };
            // Pay the option's cost (if any) before running its action.
            if let Some(c) = &options[picked].cost {
                let _ = crate::pay_ir_cost(state, t, who, src, c, false);
            }
            let sub_env = env.clone().with_controller(who);
            execute(&options[picked].action, state, &sub_env);
            ExecResult::Ok
        }

        Action::CreateToken { who, spec, n, bind_as } => {
            let who = resolve_who(who, state, env, actor);
            let n = expect_num(eval_expr(n, state, env)) as usize;
            let mut last = None;
            for _ in 0..n {
                last = Some(crate::do_create_token(spec.name, who, state, t));
            }
            if let (Some(name), Some(id)) = (bind_as, last) {
                env.bindings.insert(name, Value::Obj(id));
            }
            ExecResult::Ok
        }

        Action::Move { what, to, to_owner, bind_as: _ } => {
            let ids = obj_ids_of(eval_expr(what, state, env));
            let zone = zone_id_from_kind(*to);
            // Optional controller override (e.g. "return under owner's control").
            let new_controller = to_owner
                .as_ref()
                .map(|e| expect_player(eval_expr(e, state, env)));
            for id in ids {
                if let Some(c) = new_controller {
                    if let Some(obj) = state.objects.get_mut(&id) {
                        obj.controller = c;
                    }
                }
                let mover = new_controller.unwrap_or(actor);
                change_zone(id, zone, state, t, mover);
            }
            ExecResult::Ok
        }

        Action::PutOnLibrary { who, count, from, top } => {
            let who = resolve_who(who, state, env, actor);
            let n = expect_num(eval_expr(count, state, env)) as usize;
            if !matches!(*from, ZoneKindSel::Hand) {
                return ExecResult::Ok; // only hand source is modelled
            }
            // WHICH cards (and their order) is a player decision (Brainstorm:
            // "two cards from your hand ... in any order"), routed through Strategy.
            let candidates: Vec<ObjId> = state.hand_of(who).map(|c| c.id).collect();
            let chosen = state.with_strategy(who, |s, st| s.put_on_library(n, &candidates, *top, st));
            // Sanitize: only hand ids, deduped, at most `count`.
            let mut seen: std::collections::HashSet<ObjId> = Default::default();
            let chosen: Vec<ObjId> = chosen
                .into_iter()
                .filter(|id| candidates.contains(id) && seen.insert(*id))
                .take(n)
                .collect();
            for &id in &chosen {
                change_zone(id, ZoneId::Library, state, t, who);
            }
            // Place on top with `chosen[0]` closest to the top (drawn first).
            if *top {
                let lib = &mut state.player_mut(who).library_order;
                for &id in chosen.iter().rev() {
                    if let Some(pos) = lib.iter().position(|&x| x == id) {
                        lib.remove(pos);
                        lib.push_front(id);
                    }
                }
                // We chose exactly these cards for the top, so they're known. (Brainstorm
                // already drew off any prior known prefix, so the rest below is unknown.)
                state.player_mut(who).known_top_len = chosen.len();
            }
            let remaining: Vec<ObjId> = state.hand_of(who).map(|c| c.id).collect();
            for id in remaining {
                if let Some(card) = state.objects.get_mut(&id) {
                    card.set_zone(Zone::Hand { known: false });
                }
            }
            ExecResult::Ok
        }

        Action::ScheduleDelayedTrigger { fires, action } => {
            // Capture source/controller + the scheduling-time variable bindings
            // (so inner `Var("...")` references to the exiled card, chosen
            // target, etc. resolve correctly when the delayed trigger fires).
            let spec = fires.clone();
            let body = (**action).clone();
            let sched_bindings = env.bindings.clone();
            let source_id = env.source.unwrap_or(ObjId::default());
            let controller = env.controller.unwrap_or(actor);
            let source_name = state
                .objects
                .get(&source_id)
                .map(|o| o.catalog_key.clone())
                .unwrap_or_default();

            let check = std::sync::Arc::new(
                move |event: &crate::GameEvent,
                      src: ObjId,
                      ctlr: PlayerId,
                      state: &SimState,
                      pending: &mut Vec<crate::TriggerContext>| {
                    let Some(fired_env) = match_trigger(&spec, event, src, ctlr, state)
                    else {
                        return;
                    };
                    // Compose: sched-time bindings, then fired-event bindings
                    // override (so `triggered_obj` etc. take precedence if both
                    // name-collide, which they shouldn't in practice).
                    let mut final_env = BindEnv::new().with_source(src).with_controller(ctlr);
                    for (k, v) in &sched_bindings {
                        final_env.bindings.insert(k, v.clone());
                    }
                    for (k, v) in &fired_env.bindings {
                        final_env.bindings.insert(k, v.clone());
                    }
                    let body = body.clone();
                    let src_name = source_name.clone();
                    pending.push(crate::TriggerContext {
                        source_name: format!("{} (delayed)", src_name),
                        controller: ctlr,
                        target_spec: crate::TargetSpec::None,
                        effect: crate::effects::Effect(std::sync::Arc::new(
                            move |state, _t, _targets| {
                                execute(&body, state, &final_env);
                            },
                        )),
                    });
                },
            );

            state.trigger_instances.push(crate::TriggerInstance {
                source_id,
                controller,
                check,
                expiry: Some(crate::Expiry::OneShot),
            });
            ExecResult::Ok
        }

        Action::GrantCEToNextSpellCast { who, predicate, mods, expiry } => {
            let who_pid = resolve_who(who, state, env, actor);
            let source_id = env.source.unwrap_or(ObjId::UNSET);
            let engine_expiry = map_ir_expiry(expiry);
            let ts = state.next_ci_timestamp();

            // SpellPredicate — optional Filter over the candidate spell.
            let pred: crate::SpellPredicate = match predicate {
                None => std::sync::Arc::new(|_spell_id, _caster, _state| true),
                Some(f) => {
                    let f = f.clone();
                    let src = source_id;
                    let ctrl = who_pid;
                    std::sync::Arc::new(move |spell_id, _caster, state| {
                        let env = BindEnv::new()
                            .with_source(src)
                            .with_controller(ctrl);
                        matches(&f, spell_id, state, &env)
                    })
                }
            };

            // Compute engine reads/writes from the mods bundle. (Reads left
            // empty — current Uncounterable CEMod has no dependency edges.)
            let mut writes: Vec<crate::CeWrites> = Vec::new();
            for m in mods {
                for axis in writes_of(m) {
                    if let Some(w) = axis_to_cewrites(axis) {
                        if !writes.contains(&w) {
                            writes.push(w);
                        }
                    }
                }
            }
            let mods_cloned = mods.clone();
            let engine_expiry_for_ci = engine_expiry.clone();

            let make_ci: std::sync::Arc<
                dyn Fn(ObjId, PlayerId) -> ContinuousInstance + Send + Sync,
            > = std::sync::Arc::new(move |spell_id, spell_controller| {
                let mods_inner = mods_cloned.clone();
                ContinuousInstance {
                    source_id,
                    controller: spell_controller,
                    layer: ContinuousLayer::L6AbilityEffects,
                    reads: Vec::new(),
                    writes: writes.clone(),
                    timestamp: ts,
                    filter: std::sync::Arc::new(move |id, _ctr, _state| id == spell_id),
                    modifier: std::sync::Arc::new(move |def, _state| {
                        for m in &mods_inner {
                            apply_cemod_to_spell_def(def, m);
                        }
                    }),
                    expiry: engine_expiry_for_ci.clone(),
                }
            });

            state.latent_spell_mods.push(crate::LatentSpellMod {
                controller: who_pid,
                predicate: pred,
                make_ci,
                expiry: engine_expiry,
            });
            ExecResult::Ok
        }

        Action::RegisterContinuous { scope, mods, expiry } => {
            // Dynamic-filter sibling of ApplyCE: `scope` is re-evaluated each
            // recompute (so it catches objects matching later — e.g. attackers
            // declared after this resolves), with the given expiry.
            let source_id = env.source.unwrap_or(ObjId::UNSET);
            let ctrl = env.controller.unwrap_or(actor);
            let engine_expiry = map_ir_expiry(expiry);
            let scope_opt = Some(scope.clone());
            let mut built: Vec<ContinuousInstance> = Vec::new();
            for m in mods {
                let Some(build) = cemod_to_modifier(m, env, state) else { continue };
                let ts = state.next_ci_timestamp();
                built.push(ContinuousInstance {
                    source_id,
                    controller: ctrl,
                    layer: build.layer,
                    reads: build.reads,
                    writes: build.writes,
                    timestamp: ts,
                    filter: build_filter(source_id, ctrl, &scope_opt),
                    modifier: build.modifier,
                    expiry: engine_expiry.clone(),
                });
            }
            state.continuous_instances.extend(built);
            ExecResult::Ok
        }

        Action::ApplyCE { target, mods, expiry } => {
            // Target may be a single object (Mistrise/Dauthi: a chosen card) or a
            // set (Toxic Deluge: every creature). The affected id set is locked at
            // resolution (CR 611.2c — later-entering objects are not affected);
            // each mod becomes one CI sharing that membership filter, so each gets
            // its own correct CR-613 layer (via the shared `cemod_to_modifier`).
            let target_ids: Vec<ObjId> = obj_ids_of(eval_expr(target, state, env))
                .into_iter()
                .filter(|id| *id != ObjId::UNSET)
                .collect();
            if target_ids.is_empty() {
                return ExecResult::Ok;
            }
            let source_id = env.source.unwrap_or(ObjId::UNSET);
            let ctrl = env.controller.unwrap_or(actor);
            let engine_expiry = map_ir_expiry(expiry);

            // Build each CI up front (needs `&state` for `cemod_to_modifier`'s
            // Expr eval), then push — avoids borrowing `state` mutably and
            // immutably at once.
            let mut built: Vec<ContinuousInstance> = Vec::new();
            for m in mods {
                let Some(build) = cemod_to_modifier(m, env, state) else { continue };
                let ts = state.next_ci_timestamp();
                let filter_ids = target_ids.clone();
                built.push(ContinuousInstance {
                    source_id,
                    controller: ctrl,
                    layer: build.layer,
                    reads: build.reads,
                    writes: build.writes,
                    timestamp: ts,
                    filter: std::sync::Arc::new(move |id, _ctr, _state| filter_ids.contains(&id)),
                    modifier: build.modifier,
                    expiry: engine_expiry.clone(),
                });
            }
            state.continuous_instances.extend(built);
            ExecResult::Ok
        }

        // Remaining CE / stack plumbing deferred to later slice.
        Action::OfferCast { .. } => {
            ExecResult::Unimplemented("CE / stack plumbing")
        }

        Action::AddMana { who, count, spec } => {
            let who = resolve_who(who, state, env, actor);
            let n = expect_num(eval_expr(count, state, env)) as usize;
            // `AnyOneColor`: prefer the activation-time hint (mana-ability dispatch);
            // otherwise — an effect-body use like Tamiyo −3's green-card rider — ask
            // the producing player to choose the color (CR 106.1b).
            let color = env.chosen_color.or_else(|| {
                if matches!(spec, crate::ir::action::ManaSpec::AnyOneColor) {
                    let src = env.source.unwrap_or(ObjId::UNSET);
                    match state.with_strategy(who, |s, st| {
                        s.resolve_choice(src, &crate::ChoiceRequest::Color, st)
                    }) {
                        crate::ChoiceResult::Color(c) => Some(c),
                        _ => None,
                    }
                } else {
                    None
                }
            });
            let mana_spec = build_mana_spec_string(spec, n, color);
            crate::eff_mana(who, mana_spec).call(state, t, &[]);
            ExecResult::Ok
        }

        Action::PayMana(mc) => {
            let player = state.player_mut(actor);
            if !player.pool.can_pay(mc) {
                return ExecResult::ManaShortage(remaining_mana(&player.pool, mc));
            }
            player.pool.spend(mc);
            ExecResult::Ok
        }

        Action::PayManaX { generic } => {
            // Variable-X generic mana payment. The cost (`ManaCost{generic:n}`)
            // is built from the announced amount, then drained from the pool —
            // three distinct things: the demand (mc), the resource (pool), and
            // the announced scalar (n). Same pool/ManaShortage protocol as
            // `PayMana`.
            let n = expect_num(eval_expr(generic, state, env)).max(0) as i32;
            let mc = crate::ManaCost { generic: n, ..crate::ManaCost::default() };
            let player = state.player_mut(actor);
            if !player.pool.can_pay(&mc) {
                return ExecResult::ManaShortage(remaining_mana(&player.pool, &mc));
            }
            player.pool.spend(&mc);
            ExecResult::Ok
        }

        Action::LoyaltyAdjust(n) => {
            let source_id = match env.source {
                Some(id) => id,
                None => return ExecResult::Unimplemented("LoyaltyAdjust without env.source"),
            };
            if let Some(bf) = state.permanent_bf_mut(source_id) {
                bf.loyalty += *n;
                bf.pw_activated_this_turn = true;
            }
            ExecResult::Ok
        }

        // Replicate (CR 702.58) is meaningful only inside a cast cost tree —
        // the cost executor records the chosen replicate count and pushes the
        // copies after the spell lands on the stack. Outside a cost context
        // this is a no-op (legacy `CostComponent::Replicate` behaved the same).
        Action::Replicate(_) => ExecResult::Ok,
    }
}

/// Compute the residual `ManaCost` after applying `pool` to `cost`. Pays the
/// colored pips first (each color drains the matching pool field) and then
/// computes the generic shortfall from total. Used by `Action::PayMana` to
/// surface a precise `remaining` to the cost driver when the pool is short.
pub(crate) fn remaining_mana(pool: &crate::ManaPool, cost: &crate::ManaCost) -> crate::ManaCost {
    let mut r = crate::ManaCost::default();
    r.w = (cost.w - pool.w).max(0);
    r.u = (cost.u - pool.u).max(0);
    r.b = (cost.b - pool.b).max(0);
    r.r = (cost.r - pool.r).max(0);
    r.g = (cost.g - pool.g).max(0);
    r.c = (cost.c - pool.c).max(0);
    let pool_after_specifics = pool.total
        - (cost.w.min(pool.w)
            + cost.u.min(pool.u)
            + cost.b.min(pool.b)
            + cost.r.min(pool.r)
            + cost.g.min(pool.g)
            + cost.c.min(pool.c));
    r.generic = (cost.generic - pool_after_specifics).max(0);
    r
}

/// Lower a `ManaSpec` + count + chosen-color hint into the `eff_mana` spec
/// string format ('W'/'U'/'B'/'R'/'G'/'C').
pub(crate) fn build_mana_spec_string(
    spec: &crate::ir::action::ManaSpec,
    count: usize,
    hint: Option<crate::Color>,
) -> String {
    use crate::ir::action::ManaSpec;
    match spec {
        ManaSpec::Fixed(colors) => {
            let mut s = String::with_capacity(count);
            for c in colors {
                s.push(color_char(*c));
            }
            while s.len() < count {
                s.push('C');
            }
            s
        }
        ManaSpec::AnyOneColor => {
            let ch = hint.map(color_char).unwrap_or('C');
            std::iter::repeat(ch).take(count).collect()
        }
    }
}

/// Map the IR `Expiry` to the engine `Expiry`. Only variants that currently
/// map cleanly are defined; unsupported variants fall back to `EndOfTurn` (the
/// safest default for the latent-spell-mod use case).
fn map_ir_expiry(e: &crate::ir::action::Expiry) -> crate::Expiry {
    use crate::ir::action::Expiry as I;
    match e {
        I::EndOfTurn => crate::Expiry::EndOfTurn,
        I::WhileSourcePresent => crate::Expiry::WhileSourceOnBattlefield,
        I::Permanent => crate::Expiry::Never,
        I::UntilYourNextTurn => crate::Expiry::StartOfControllerNextTurn,
        I::EndOfCombat => crate::Expiry::EndOfTurn,
    }
}

/// Translate a CE-layer axis to the engine's `CeWrites` enum used by the
/// recompute dependency graph. Axes that don't map (Counters, Life, …) yield
/// `None` — those aren't part of the characteristic-axis write set.
fn axis_to_cewrites(axis: Axis) -> Option<crate::CeWrites> {
    use crate::CeWrites;
    match axis {
        Axis::Type => Some(CeWrites::CardTypes),
        Axis::Color => Some(CeWrites::Color),
        Axis::Abilities => Some(CeWrites::Abilities),
        Axis::PT => Some(CeWrites::PowerToughness),
        _ => None,
    }
}

/// Apply a non-P/T `CEMod` to a (cloned) `CardDef` via the CI modifier hook.
/// Handles the rules-text / cast-permission mods that need no env at apply time:
/// `Uncounterable` (Mistrise), `CastableFrom` + `AltCost` (Dauthi free-cast).
/// `PumpPT` is handled separately in the `ApplyCE` arm — its `Expr` deltas are
/// evaluated against the live env there and baked in, since this hook is env-less.
/// The remaining ~25 `CEMod` variants are still only honored by bespoke closure
/// `ContinuousInstance`s on cards, not by this IR path (see DESIGN.org 2026-06-20).
fn apply_cemod_to_spell_def(def: &mut CardDef, cemod: &CEMod) {
    use crate::catalog::{AlternateCost, ProhibitionDef};
    use crate::ir::ce::CostSpec;
    match cemod {
        CEMod::Uncounterable => {
            def.prohibition_defs.push(ProhibitionDef {
                check: std::sync::Arc::new(|event, source_id, _ctr, _state| {
                    matches!(
                        event,
                        crate::GameEvent::SpellBeingCountered { card_id, .. }
                            if *card_id == source_id
                    )
                }),
                active_when: crate::tp_on_stack(),
            });
        }
        CEMod::CastableFrom(_zone) => {
            // Permission to cast. CI `filter` already scopes to the right
            // object; the zone parameter is kept for docs/future analysis.
            def.castable = true;
        }
        CEMod::AltCost(spec) => match spec {
            CostSpec::Free => {
                def.alternate_costs.push(AlternateCost::default());
            }
            // Other CostSpec variants land when a card needs them.
            _ => {}
        },
        _ => {}
    }
}

/// The CEMod-intrinsic half of a continuous instance: which CR-613 sublayer it
/// lives in, what characteristic axes it reads/writes (for recompute dependency
/// ordering), and the modifier that mutates a materialised `CardDef`. The CI's
/// *extrinsic* half — filter (scope), expiry, timestamp, source, controller — is
/// supplied by the caller (the Static path, the `ApplyCE` arm, …).
pub(crate) struct CeBuild {
    pub layer: ContinuousLayer,
    pub reads: Vec<crate::CeReads>,
    pub writes: Vec<crate::CeWrites>,
    pub modifier: crate::ContinuousModFn,
}

/// Single source of truth: `CEMod` → `CeBuild`. Both IR continuous-effect entry
/// points (the `AbilityKind::Static` path via `cemod_to_ci`, and the one-shot
/// `Action::ApplyCE` arm) route through this, so a CEMod is implemented once and
/// every path gains it. `env`/`state` let value-carrying mods (e.g. `PumpPT`'s
/// `Expr` deltas — Toxic Deluge's −X/−X) evaluate against the live binding frame
/// and bake concrete amounts into the modifier; mods with no dynamic operand
/// ignore them. Returns `None` for CEMods with no recompute-time modifier yet
/// (the standing CE breadth gap — see DESIGN.org 2026-06-20).
pub(crate) fn cemod_to_modifier(
    cemod: &CEMod,
    env: &BindEnv,
    state: &SimState,
) -> Option<CeBuild> {
    use crate::ir::ce::BasicLandType;
    use crate::{CeReads, CeWrites};
    match cemod {
        CEMod::SetBasicLandType(kind) => {
            let kind: BasicLandType = *kind;
            Some(CeBuild {
                layer: ContinuousLayer::L4TypeEffects,
                reads: vec![CeReads::Supertypes],
                writes: vec![CeWrites::LandTypes, CeWrites::Abilities],
                modifier: std::sync::Arc::new(move |def, _state| {
                    apply_set_basic_land_type(def, kind);
                }),
            })
        }
        CEMod::AddBasicLandType(kind) => {
            let kind: BasicLandType = *kind;
            Some(CeBuild {
                layer: ContinuousLayer::L4TypeEffects,
                reads: vec![CeReads::LandTypes],
                writes: vec![CeWrites::LandTypes],
                modifier: std::sync::Arc::new(move |def, _state| {
                    apply_add_basic_land_type(def, kind);
                }),
            })
        }
        CEMod::BecomeCreature { power, toughness, subtypes, keywords } => {
            // Layer 4 (type) + the P/T it sets. Evaluate P/T now (env-less hook)
            // and bake them into the animate modifier.
            let p = expect_num(eval_expr(power, state, env)) as i32;
            let t = expect_num(eval_expr(toughness, state, env)) as i32;
            let subtypes = subtypes.clone();
            let keywords = keywords.clone();
            Some(CeBuild {
                layer: ContinuousLayer::L4TypeEffects,
                reads: vec![],
                writes: vec![CeWrites::CardTypes, CeWrites::PowerToughness, CeWrites::Abilities],
                modifier: std::sync::Arc::new(move |def, _state| {
                    apply_become_creature(def, p, t, &subtypes, &keywords);
                }),
            })
        }
        CEMod::PumpPT(p, t) => {
            // Evaluate the deltas now (env may hold an announced X); bake them in.
            let pv = expect_num(eval_expr(p, state, env)) as i32;
            let tv = expect_num(eval_expr(t, state, env)) as i32;
            Some(CeBuild {
                layer: ContinuousLayer::L7PowerToughness,
                reads: vec![],
                writes: vec![CeWrites::PowerToughness],
                modifier: std::sync::Arc::new(move |def, _state| {
                    if let CardKind::Creature(c) = &mut def.kind {
                        c.adjust_pt(pv, tv);
                    }
                }),
            })
        }
        CEMod::SetPT(p, t) => {
            // Evaluate against the current battlefield on every recompute. In
            // particular, a Saga Construct counts itself and all other artifacts
            // its controller currently owns, rather than remaining a 0/0.
            let p = p.clone();
            let t = t.clone();
            let env = env.clone();
            Some(CeBuild {
                layer: ContinuousLayer::L7PowerToughness,
                reads: vec![CeReads::CardTypes],
                writes: vec![CeWrites::PowerToughness],
                modifier: std::sync::Arc::new(move |def, state| {
                    let pv = expect_num(eval_expr(&p, state, &env)) as i32;
                    let tv = expect_num(eval_expr(&t, state, &env)) as i32;
                    if let CardKind::Creature(c) = &mut def.kind {
                        c.adjust_pt(pv - c.power(), tv - c.toughness());
                    }
                }),
            })
        }
        CEMod::AddColor(color_expr) => {
            // CR 613.4 layer 5: add a color to every object in scope. The color is
            // an Expr so it can be runtime-chosen (Painter's Servant reads its own
            // `ChosenColor`); evaluate it once here against the source's frame and
            // bake it in. No chosen color (Unit) → no CI, so nothing is added.
            let Value::Color(color) = eval_expr(color_expr, state, env) else {
                return None;
            };
            Some(CeBuild {
                layer: ContinuousLayer::L5ColorEffects,
                reads: vec![],
                writes: vec![CeWrites::Color],
                modifier: std::sync::Arc::new(move |def, _state| {
                    if !def.colors.contains(&color) {
                        def.colors.push(color);
                    }
                }),
            })
        }
        CEMod::AddKeyword(kw) => {
            // CR 613.1f layer 6: grant a keyword ability (trample, haste, …).
            let kw = *kw;
            Some(CeBuild {
                layer: ContinuousLayer::L6AbilityEffects,
                reads: vec![],
                writes: vec![CeWrites::Abilities],
                modifier: std::sync::Arc::new(move |def, _state| {
                    if let CardKind::Creature(c) = &mut def.kind {
                        c.keywords.insert(kw);
                    }
                }),
            })
        }
        CEMod::GrantAbility(ability) => {
            // CR 613.1f layer 6: grant a full IR ability (e.g. Ward) to objects in
            // scope. It lands on `granted_abilities` of the affected def, which
            // `fire_triggers` consults — the declarative analog of pushing a
            // `granted_trigger_def`.
            let ability = (**ability).clone();
            Some(CeBuild {
                layer: ContinuousLayer::L6AbilityEffects,
                reads: vec![],
                writes: vec![CeWrites::Abilities],
                modifier: std::sync::Arc::new(move |def, _state| {
                    def.granted_abilities.push(ability.clone());
                }),
            })
        }
        // Cost-to-cast surcharge (CR 601.2f / 614.12 "costs {N} more"): a
        // generic-mana add baked onto the matching def's casting cost. Scope
        // (which cards) lives in the static-ability filter; the modifier just
        // applies the delta. Disruptor Flute names a card → +3.
        CEMod::CastingCostPlus(amount) => {
            let amt = expect_num(eval_expr(amount, state, env)) as i32;
            Some(CeBuild {
                layer: ContinuousLayer::L3TextEffects,
                reads: vec![],
                writes: vec![],
                modifier: std::sync::Arc::new(move |def, _state| {
                    def.casting_cost_modifier += amt;
                }),
            })
        }
        // Cast-permission / rules-text mods (CR 613.1b layer 3): mutate the def
        // via the shared env-less hook (Uncounterable, CastableFrom, AltCost).
        CEMod::Uncounterable | CEMod::CastableFrom(_) | CEMod::AltCost(_) => {
            let cemod = cemod.clone();
            Some(CeBuild {
                layer: ContinuousLayer::L3TextEffects,
                reads: vec![],
                writes: vec![],
                modifier: std::sync::Arc::new(move |def, _state| {
                    apply_cemod_to_spell_def(def, &cemod);
                }),
            })
        }
        // Remaining CEMods have no recompute-time modifier yet — the standing CE
        // breadth gap. Each lands here as a new arm during the CE migration.
        _ => None,
    }
}

fn resolve_who(who: &Who, state: &SimState, env: &BindEnv, actor: PlayerId) -> PlayerId {
    match who {
        Who::You => env.controller.unwrap_or(actor),
        Who::Opponent | Who::EachOpponent => env.controller.unwrap_or(actor).opp(),
        Who::Player(e) => expect_player(eval_expr(e, state, env)),
        Who::Each => actor, // Each in single-player contexts is a no-op shortcut
    }
}

fn zone_id_from_kind(k: ZoneKindSel) -> ZoneId {
    match k {
        ZoneKindSel::Library => ZoneId::Library,
        ZoneKindSel::Hand => ZoneId::Hand,
        ZoneKindSel::Battlefield => ZoneId::Battlefield,
        ZoneKindSel::Graveyard => ZoneId::Graveyard,
        ZoneKindSel::Exile => ZoneId::Exile,
        ZoneKindSel::Stack => ZoneId::Stack,
        ZoneKindSel::Command => ZoneId::Library, // placeholder — no command-zone model yet
    }
}

// ── eval_expr ────────────────────────────────────────────────────────────────

/// Evaluate a pure expression.
pub(crate) fn eval_expr(expr: &Expr, state: &SimState, env: &BindEnv) -> Value {
    match expr {
        // literals
        Expr::Num(n) => Value::Num(*n),
        Expr::Bool(b) => Value::Bool(*b),
        Expr::TypeLit(t) => Value::Type(*t),
        Expr::SupertypeLit(s) => Value::Supertype(*s),
        Expr::SubtypeLit(s) => Value::Subtype(s.clone()),
        Expr::ColorLit(c) => Value::Color(*c),
        Expr::KeywordLit(k) => Value::Keyword(*k),
        Expr::NameLit(n) => Value::Name(n.clone()),

        // context
        Expr::Ctx(c) => eval_ctx(c, state, env),
        Expr::GameCtx(g) => eval_game_ctx(g, state),

        // property projections over object refs
        Expr::Types(e) => {
            // A single object → its types; an `ObjSet` → the deduped union of all
            // members' types. The latter powers aggregate reads like delirium:
            // `Count(Types(your graveyard)) ≥ 4`.
            let mut out: Vec<CardType> = Vec::new();
            for id in obj_ids_of(eval_expr(e, state, env)) {
                for ty in types_of_obj(state, id) {
                    if !out.contains(&ty) { out.push(ty); }
                }
            }
            Value::TypeSet(out)
        }
        Expr::Supertypes(e) => {
            let o = expect_obj(eval_expr(e, state, env));
            Value::SupertypeSet(
                card_def_of(state, o).map(|d| d.supertypes.clone()).unwrap_or_default(),
            )
        }
        Expr::Subtypes(e) => {
            let o = expect_obj(eval_expr(e, state, env));
            Value::SubtypeSet(subtypes_of_obj(state, o))
        }
        Expr::Colors(e) => {
            let o = expect_obj(eval_expr(e, state, env));
            Value::ColorSet(card_def_of(state, o).map(|d| d.colors.clone()).unwrap_or_default())
        }
        Expr::Keywords(e) => {
            let o = expect_obj(eval_expr(e, state, env));
            Value::KeywordSet(keywords_of_obj(state, o))
        }
        Expr::Power(e) => {
            let o = expect_obj(eval_expr(e, state, env));
            Value::Num(power_of_obj(state, o).unwrap_or(0) as i64)
        }
        Expr::Attacking(e) => {
            let o = expect_obj(eval_expr(e, state, env));
            Value::Bool(state.permanent_bf(o).map_or(false, |bf| bf.attacking))
        }
        Expr::AttachedTo(e) => {
            let o = expect_obj(eval_expr(e, state, env));
            match state.permanent_bf(o).and_then(|bf| bf.attached_to) {
                Some(id) => Value::Obj(id),
                None => Value::Unit,
            }
        }
        Expr::ChosenName(e) => {
            let o = expect_obj(eval_expr(e, state, env));
            match state.permanent_bf(o).and_then(|bf| bf.etb_choice.as_ref()) {
                Some(crate::ChoiceResult::CardName(n)) => Value::Name(n.clone()),
                _ => Value::Unit,
            }
        }
        Expr::ChosenColor(e) => {
            let o = expect_obj(eval_expr(e, state, env));
            match state.permanent_bf(o).and_then(|bf| bf.etb_choice.as_ref()) {
                Some(crate::ChoiceResult::Color(c)) => Value::Color(*c),
                _ => Value::Unit,
            }
        }
        Expr::ChosenTargets(e) => {
            let o = expect_obj(eval_expr(e, state, env));
            let ids = state.objects.get(&o)
                .and_then(|obj| obj.spell())
                .map(|s| s.chosen_targets.clone())
                .unwrap_or_default();
            Value::ObjSet(ids)
        }
        Expr::IsFrontFace(e) => {
            let o = expect_obj(eval_expr(e, state, env));
            Value::Bool(state.permanent_bf(o).map_or(false, |bf| bf.active_face == 0))
        }
        Expr::Unblocked(e) => {
            let o = expect_obj(eval_expr(e, state, env));
            Value::Bool(state.permanent_bf(o).map_or(false, |bf| bf.unblocked))
        }
        Expr::Toughness(e) => {
            let o = expect_obj(eval_expr(e, state, env));
            Value::Num(toughness_of_obj(state, o).unwrap_or(0) as i64)
        }
        Expr::Mv(e) => {
            let o = expect_obj(eval_expr(e, state, env));
            Value::Num(
                card_def_of(state, o)
                    .map(|d| crate::catalog::mana_value(d.mana_cost()))
                    .unwrap_or(0) as i64,
            )
        }
        Expr::Controller(e) => {
            let o = expect_obj(eval_expr(e, state, env));
            Value::Player(
                state
                    .objects
                    .get(&o)
                    .map(|g| g.controller)
                    .unwrap_or(PlayerId::Us),
            )
        }
        Expr::Owner(e) => {
            let o = expect_obj(eval_expr(e, state, env));
            Value::Player(
                state
                    .objects
                    .get(&o)
                    .map(|g| g.owner)
                    .unwrap_or(PlayerId::Us),
            )
        }
        Expr::ZoneOf(e) => {
            let o = expect_obj(eval_expr(e, state, env));
            Value::Zone(zone_id_of_obj(state, o).unwrap_or(ZoneId::Library))
        }
        Expr::ZoneLit(z) => Value::Zone(*z),
        Expr::ObjLit(id) => Value::Obj(*id),
        Expr::IsToken(e) => {
            let o = expect_obj(eval_expr(e, state, env));
            Value::Bool(state.objects.get(&o).map_or(false, |obj| obj.is_token))
        }
        Expr::IsAbility(e) => {
            let o = expect_obj(eval_expr(e, state, env));
            Value::Bool(state.objects.get(&o).map_or(false, |obj| obj.ability().is_some()))
        }
        Expr::AbilityIsTriggered(e) => {
            let o = expect_obj(eval_expr(e, state, env));
            Value::Bool(state.objects.get(&o)
                .and_then(|obj| obj.ability())
                .map_or(false, |a| a.is_triggered))
        }
        Expr::CountersOn(e, kind) => {
            let o = expect_obj(eval_expr(e, state, env));
            Value::Num(
                state
                    .objects
                    .get(&o)
                    .and_then(|g| g.counters.get(kind).copied())
                    .unwrap_or(0) as i64,
            )
        }
        Expr::Name(e) => {
            let o = expect_obj(eval_expr(e, state, env));
            // Fall back to the object's catalog_key if no def is available
            // (e.g. freshly-minted tokens whose CardDef isn't materialized or
            // registered in state.catalog yet). For tokens and normal cards
            // alike, catalog_key == the printed card name.
            let name = card_def_of(state, o)
                .map(|d| d.name.clone())
                .or_else(|| state.objects.get(&o).map(|obj| obj.catalog_key.clone()))
                .unwrap_or_default();
            Value::Name(name)
        }

        // player projections
        Expr::LoyaltyOf(e) => {
            let o = expect_obj(eval_expr(e, state, env));
            Value::Num(state.permanent_bf(o).map_or(0, |bf| bf.loyalty) as i64)
        }
        Expr::Life(e) => {
            let p = expect_player(eval_expr(e, state, env));
            Value::Num(crate::SimState::life_of(state, p) as i64)
        }
        Expr::HandSize(e) => {
            let p = expect_player(eval_expr(e, state, env));
            Value::Num(state.hand_of(p).count() as i64)
        }
        Expr::LibrarySize(e) => {
            let p = expect_player(eval_expr(e, state, env));
            Value::Num(state.library_size(p) as i64)
        }
        Expr::Opponents(e) => {
            let p = expect_player(eval_expr(e, state, env));
            Value::PlayerSet(vec![p.opp()])
        }

        // zone projections (top N)
        Expr::Top { zone, n } => {
            let n = expect_num(eval_expr(n, state, env)) as usize;
            Value::ObjSet(top_n(state, env, zone, n))
        }

        // boolean / arithmetic
        Expr::And(a, b) => {
            let va = expect_bool(eval_expr(a, state, env));
            Value::Bool(va && expect_bool(eval_expr(b, state, env)))
        }
        Expr::Or(a, b) => {
            let va = expect_bool(eval_expr(a, state, env));
            Value::Bool(va || expect_bool(eval_expr(b, state, env)))
        }
        Expr::Not(a) => Value::Bool(!expect_bool(eval_expr(a, state, env))),
        Expr::Eq(a, b) => {
            // Bridge: a player and that player's object are the same entity, so a
            // `Player` and an `Obj`-that-is-a-player compare equal (e.g. comparing
            // a land's `Controller` to a bound player object in Price of Progress).
            let norm = |v: Value| match v {
                Value::Obj(id) if state.is_player(id) => Value::Player(state.who_pid(id)),
                other => other,
            };
            Value::Bool(values_eq(
                &norm(eval_expr(a, state, env)),
                &norm(eval_expr(b, state, env)),
            ))
        }
        Expr::Lt(a, b) => Value::Bool(
            expect_num(eval_expr(a, state, env)) < expect_num(eval_expr(b, state, env)),
        ),
        Expr::Le(a, b) => Value::Bool(
            expect_num(eval_expr(a, state, env)) <= expect_num(eval_expr(b, state, env)),
        ),
        Expr::Gt(a, b) => Value::Bool(
            expect_num(eval_expr(a, state, env)) > expect_num(eval_expr(b, state, env)),
        ),
        Expr::Ge(a, b) => Value::Bool(
            expect_num(eval_expr(a, state, env)) >= expect_num(eval_expr(b, state, env)),
        ),
        Expr::Contains(needle, set) => Value::Bool(contains(
            &eval_expr(needle, state, env),
            &eval_expr(set, state, env),
        )),
        Expr::Add(a, b) => Value::Num(
            expect_num(eval_expr(a, state, env)) + expect_num(eval_expr(b, state, env)),
        ),
        Expr::Sub(a, b) => Value::Num(
            expect_num(eval_expr(a, state, env)) - expect_num(eval_expr(b, state, env)),
        ),
        Expr::Div(a, b) => Value::Num({
            let denom = expect_num(eval_expr(b, state, env));
            if denom == 0 { 0 } else { expect_num(eval_expr(a, state, env)) / denom }
        }),
        Expr::Mul(a, b) => Value::Num(
            expect_num(eval_expr(a, state, env)) * expect_num(eval_expr(b, state, env)),
        ),
        Expr::Players => Value::ObjSet(vec![state.us_id, state.opp_id]),
        Expr::ActivePlayer => {
            if state.current_ap == ObjId::UNSET {
                Value::Unit
            } else {
                Value::Player(state.who_pid(state.current_ap))
            }
        }
        Expr::Neg(a) => Value::Num(-expect_num(eval_expr(a, state, env))),
        Expr::Min(a, b) => Value::Num(std::cmp::min(
            expect_num(eval_expr(a, state, env)),
            expect_num(eval_expr(b, state, env)),
        )),
        Expr::Max(a, b) => Value::Num(std::cmp::max(
            expect_num(eval_expr(a, state, env)),
            expect_num(eval_expr(b, state, env)),
        )),

        // set-builders / folds
        Expr::AllObjects { zone, bind, filter } => {
            let candidates = enumerate_zone(state, env, zone);
            let mut out = Vec::with_capacity(candidates.len());
            for id in candidates {
                let sub_env = env
                    .clone()
                    .with_var(bind, Value::Obj(id))
                    .with_subj(Value::Obj(id));
                if expect_bool(eval_expr(filter, state, &sub_env)) {
                    out.push(id);
                }
            }
            Value::ObjSet(out)
        }
        Expr::Count(e) => Value::Num(match eval_expr(e, state, env) {
            Value::ObjSet(v) => v.len() as i64,
            Value::PlayerSet(v) => v.len() as i64,
            Value::TypeSet(v) => v.len() as i64,
            Value::SupertypeSet(v) => v.len() as i64,
            Value::ColorSet(v) => v.len() as i64,
            Value::KeywordSet(v) => v.len() as i64,
            Value::SubtypeSet(v) => v.len() as i64,
            _ => 0,
        }),
        Expr::SumManaValue(e) => Value::Num(match eval_expr(e, state, env) {
            Value::ObjSet(ids) => ids.into_iter().map(|id| {
                card_def_of(state, id)
                    .map(|d| crate::catalog::mana_value(d.mana_cost()) as i64)
                    .unwrap_or(0)
            }).sum(),
            _ => 0,
        }),
        Expr::CountWhere { set, bind, body } => Value::Num(count_set(state, env, set, bind, body)),
        Expr::Any { set, bind, body } => Value::Bool(fold_set(
            state,
            env,
            set,
            bind,
            body,
            /*is_all=*/ false,
        )),
        Expr::All { set, bind, body } => Value::Bool(fold_set(
            state,
            env,
            set,
            bind,
            body,
            /*is_all=*/ true,
        )),

        Expr::Let { name, value, body } => {
            let v = eval_expr(value, state, env);
            let sub_env = env.clone().with_var(name, v);
            eval_expr(body, state, &sub_env)
        }

        Expr::Bound(name) => Value::Bool(
            env.bindings
                .get(*name)
                .map_or(false, |v| !matches!(v, Value::Unit)),
        ),

        Expr::EventCount { window, filter } => {
            let n = state
                .event_log
                .count(*window, |logged| match_event(filter, logged, state, env))
                as i64;
            Value::Num(n)
        }
    }
}

/// Match a logged event against an `EventFilter`. Each variant is a small,
/// direct predicate — no recursion through `Expr` other than resolving the
/// typed-scalar selectors (e.g. the `caster` player filter).
fn match_event(
    filter: &crate::ir::expr::EventFilter,
    logged: &crate::ir::event_log::LoggedEvent,
    state: &SimState,
    env: &BindEnv,
) -> bool {
    use crate::ir::expr::EventFilter;
    match filter {
        EventFilter::SpellCast { caster, card, spell_filter, alt_cost } => {
            let crate::GameEvent::SpellCast { caster: c, card_id, alt_cost: ac, .. } = &logged.event
            else {
                return false;
            };
            if let Some(expr) = caster {
                if !matches!(eval_expr(expr, state, env), Value::Player(p) if p == *c) {
                    return false;
                }
            }
            if let Some(expr) = card {
                if !matches!(eval_expr(expr, state, env), Value::Obj(o) if o == *card_id) {
                    return false;
                }
            }
            // The cast object persists post-resolution (moved to graveyard/exile but
            // still in `state.objects` with its catalog_key), so its types resolve.
            if let Some(filter) = spell_filter {
                if !matches(filter, *card_id, state, env) {
                    return false;
                }
            }
            if let Some(want) = alt_cost {
                if *want != *ac {
                    return false;
                }
            }
            true
        }
        EventFilter::Draw { who } => {
            let crate::GameEvent::Draw { controller: drawer, .. } = &logged.event else {
                return false;
            };
            match who {
                None => true,
                Some(expr) => matches!(eval_expr(expr, state, env), Value::Player(p) if p == *drawer),
            }
        }
        EventFilter::LifeLost { who } => {
            let crate::GameEvent::LifeLost { who: loser, .. } = &logged.event else {
                return false;
            };
            match who {
                None => true,
                Some(expr) => matches!(eval_expr(expr, state, env), Value::Player(p) if p == *loser),
            }
        }
    }
}

/// Test whether a filter matches a candidate object.
pub(crate) fn matches(
    filter: &Filter,
    subj: ObjId,
    state: &SimState,
    env: &BindEnv,
) -> bool {
    let env = env.clone().with_subj(Value::Obj(subj));
    expect_bool(eval_expr(&filter.0, state, &env))
}

/// Is the player action `kind` on `subject_id` forbidden by an active
/// `AbilityKind::Restriction` (CR 101.2 "can't")? Walks battlefield sources (a
/// restriction is active while its source is on the battlefield) and matches each
/// `Restriction { action: kind, subject }` against `subject_id` (bound as `Ctx::It`,
/// with the restriction source/controller in scope so "opponent's" works). The legal-
/// option producers call this as an AND-NOT gate over *permission*, making "can't
/// beats can" order-independent. The action analogue of the event `Prohibition` walk
/// in `fire_event` Stage 1.
///
/// Use this for casts and **non-mana** activated abilities — the subject is
/// evaluated with `activating_mana_ability = false`. The mana sub-loop uses
/// [`mana_ability_restricted`] (binds `true`). Which of the two a caller uses
/// follows from the ability's CR-605.1a classification (mana abilities live in
/// `mana_abilities()`, non-mana in `abilities()`) — there is no mana-ness tag to
/// pass, and a restriction's "unless they're mana abilities" rider is a clause in
/// its own `subject`.
pub(crate) fn action_restricted(
    state: &SimState,
    kind: crate::ir::ability::ActionKind,
    subject_id: ObjId,
) -> bool {
    restriction_hits(state, kind, subject_id, false)
}

/// Is activating a **mana ability** (CR 605.1a) of `subject_id` forbidden? Like
/// [`action_restricted`] for `Activate`, but binds `activating_mana_ability = true`
/// — so a restriction whose subject excludes mana abilities (Pithing Needle /
/// Disruptor Flute "… unless they're mana abilities") won't match, while Null Rod /
/// Karn (subjects with no such clause) still bite here.
pub(crate) fn mana_ability_restricted(state: &SimState, subject_id: ObjId) -> bool {
    restriction_hits(state, crate::ir::ability::ActionKind::Activate, subject_id, true)
}

/// Shared walk for the two restriction queries. `for_mana_ability` is bound into
/// the subject's eval env as `activating_mana_ability` (CR 605.1a), so a "… unless
/// they're mana abilities" rider is expressed as a subject clause rather than a
/// flag on the variant — see `AbilityKind::Restriction`.
fn restriction_hits(
    state: &SimState,
    kind: crate::ir::ability::ActionKind,
    subject_id: ObjId,
    for_mana_ability: bool,
) -> bool {
    use crate::ir::ability::AbilityKind;
    state.objects.iter().any(|(id, obj)| {
        if !matches!(obj.zone(), Some(Zone::Battlefield)) {
            return false;
        }
        state.catalog.get(&obj.catalog_key).map_or(false, |card_def| {
            card_def.abilities.iter().any(|ab| {
                if let AbilityKind::Restriction { action, subject } = &ab.kind {
                    *action == kind && {
                        let env = BindEnv::new()
                            .with_source(*id)
                            .with_controller(obj.controller)
                            .with_var("activating_mana_ability", Value::Bool(for_mana_ability));
                        matches(subject, subject_id, state, &env)
                    }
                } else {
                    false
                }
            })
        })
    })
}

/// Test whether a filter matches a candidate player. `Ctx::It` binds to the
/// player; used for trigger actor matching (e.g., "whenever an opponent …").
pub(crate) fn matches_player(
    filter: &Filter,
    subj: PlayerId,
    state: &SimState,
    env: &BindEnv,
) -> bool {
    let env = env.clone().with_subj(Value::Player(subj));
    expect_bool(eval_expr(&filter.0, state, &env))
}

// ── Trigger matching ─────────────────────────────────────────────────────────

/// Match an event against an IR TriggerSpec. If the trigger fires, returns a
/// `BindEnv` populated with source/controller plus any triggering-event
/// bindings (`triggered_obj`, `triggered_actor`).
pub(crate) fn match_trigger(
    spec: &crate::ir::ability::TriggerSpec,
    event: &crate::GameEvent,
    source_id: ObjId,
    controller: PlayerId,
    state: &SimState,
) -> Option<BindEnv> {
    use crate::ir::ability::{StepScope, TriggerSpec};
    let env = BindEnv::new()
        .with_source(source_id)
        .with_controller(controller);
    match spec {
        TriggerSpec::When { pattern, condition } => {
            let env = match_event_pattern(pattern, event, &env, state)?;
            if let Some(cond) = condition {
                if !expect_bool(eval_expr(cond, state, &env)) {
                    return None;
                }
            }
            Some(env)
        }
        TriggerSpec::AtStep { step, who, condition } => {
            let crate::GameEvent::EnteredStep {
                step: s,
                active_player,
            } = event
            else {
                return None;
            };
            if *s != *step {
                return None;
            }
            let fires = match who {
                StepScope::ActivePlayer => true,
                StepScope::EachPlayer => true,
                StepScope::You => *active_player == controller,
                StepScope::EachOpponent => *active_player != controller,
            };
            if !fires {
                return None;
            }
            if let Some(cond) = condition {
                if !expect_bool(eval_expr(cond, state, &env)) {
                    return None;
                }
            }
            Some(env)
        }
    }
}

pub(crate) fn match_event_pattern(
    pattern: &crate::ir::ability::EventPattern,
    event: &crate::GameEvent,
    env: &BindEnv,
    state: &SimState,
) -> Option<BindEnv> {
    use crate::ir::ability::EventPattern;
    // Carry the matched event so `Ctx::Triggering` can project its fields; every
    // arm builds its result from this enriched base via `env.clone()`.
    let enriched = env.clone().with_triggering_event(event.clone());
    let env = &enriched;
    match pattern {
        EventPattern::Any => Some(env.clone()),

        EventPattern::EntersZone {
            obj_filter,
            zone_kind,
        } => {
            let crate::GameEvent::ZoneChange { id, to, from, .. } = event else {
                return None;
            };
            if zone_id_from_kind(zone_kind.clone()) != *to {
                return None;
            }
            if !matches(obj_filter, *id, state, env) {
                return None;
            }
            // `triggered_from` lets a condition test the source zone (e.g. Containment
            // Priest's "wasn't cast" = entered the battlefield from a zone other than
            // the stack).
            Some(
                env.clone()
                    .with_var("triggered_obj", Value::Obj(*id))
                    .with_var("triggered_from", Value::Zone(*from)),
            )
        }

        EventPattern::LeavesZone {
            obj_filter,
            zone_kind,
        } => {
            let crate::GameEvent::ZoneChange { id, from, .. } = event else {
                return None;
            };
            if zone_id_from_kind(zone_kind.clone()) != *from {
                return None;
            }
            if !matches(obj_filter, *id, state, env) {
                return None;
            }
            Some(env.clone().with_var("triggered_obj", Value::Obj(*id)))
        }

        EventPattern::ZoneChange {
            obj_filter,
            from,
            to,
            actor_filter,
        } => {
            let crate::GameEvent::ZoneChange {
                id,
                from: ef,
                to: et,
                actor,
                ..
            } = event
            else {
                return None;
            };
            if zone_id_from_kind(from.clone()) != *ef {
                return None;
            }
            if zone_id_from_kind(to.clone()) != *et {
                return None;
            }
            if !matches(obj_filter, *id, state, env) {
                return None;
            }
            if let Some(af) = actor_filter {
                if !matches_player(af, *actor, state, env) {
                    return None;
                }
            }
            Some(
                env.clone()
                    .with_var("triggered_obj", Value::Obj(*id))
                    .with_var("triggered_actor", Value::Player(*actor)),
            )
        }

        EventPattern::Dies { obj_filter } => {
            let crate::GameEvent::ZoneChange {
                id,
                from: ZoneId::Battlefield,
                to: ZoneId::Graveyard,
                ..
            } = event
            else {
                return None;
            };
            // Only creatures "die" (CR 700.4).
            let is_creature = state
                .objects
                .get(id)
                .and_then(|o| state.catalog.get(o.catalog_key.as_str()))
                .map_or(false, |d| d.is_creature());
            if !is_creature {
                return None;
            }
            if !matches(obj_filter, *id, state, env) {
                return None;
            }
            Some(env.clone().with_var("triggered_obj", Value::Obj(*id)))
        }

        EventPattern::SpellCast { spell_filter } => {
            let crate::GameEvent::SpellCast {
                card_id, caster, mana_spent, ..
            } = event
            else {
                return None;
            };
            if !matches(spell_filter, *card_id, state, env) {
                return None;
            }
            Some(
                env.clone()
                    .with_var("triggered_obj", Value::Obj(*card_id))
                    .with_var("triggered_actor", Value::Player(*caster))
                    .with_var("triggered_mana_spent", Value::Bool(*mana_spent)),
            )
        }

        EventPattern::SpellBeingCountered { spell_filter } => {
            let crate::GameEvent::SpellBeingCountered { card_id, caster } = event else {
                return None;
            };
            if !matches(spell_filter, *card_id, state, env) {
                return None;
            }
            Some(
                env.clone()
                    .with_var("triggered_obj", Value::Obj(*card_id))
                    .with_var("triggered_actor", Value::Player(*caster)),
            )
        }

        EventPattern::Draw { who } => {
            let crate::GameEvent::Draw {
                controller: drawer,
                is_natural,
                ..
            } = event
            else {
                return None;
            };
            if !matches_player(who, *drawer, state, env) {
                return None;
            }
            Some(
                env.clone()
                    .with_var("triggered_actor", Value::Player(*drawer))
                    .with_var("triggered_is_natural", Value::Bool(*is_natural)),
            )
        }

        EventPattern::Attacks { attacker_filter } => {
            let crate::GameEvent::CreatureAttacked { attacker_id, .. } = event else {
                return None;
            };
            if !matches(attacker_filter, *attacker_id, state, env) {
                return None;
            }
            Some(
                env.clone()
                    .with_var("triggered_obj", Value::Obj(*attacker_id)),
            )
        }

        EventPattern::And(ps) => {
            let mut e = env.clone();
            for p in ps {
                e = match_event_pattern(p, event, &e, state)?;
            }
            Some(e)
        }

        EventPattern::Or(ps) => ps
            .iter()
            .find_map(|p| match_event_pattern(p, event, env, state)),

        EventPattern::LandPlayed { who, land_filter } => {
            let crate::GameEvent::LandPlayed { id, controller: player } = event else {
                return None;
            };
            if !matches_player(who, *player, state, env) {
                return None;
            }
            if !matches(land_filter, *id, state, env) {
                return None;
            }
            Some(
                env.clone()
                    .with_var("triggered_obj", Value::Obj(*id))
                    .with_var("triggered_actor", Value::Player(*player)),
            )
        }

        // Stage-3+ patterns — not wired yet.
        EventPattern::DamageDealt { .. } | EventPattern::Blocks { .. } => None,
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn eval_ctx(c: &Ctx, state: &SimState, env: &BindEnv) -> Value {
    match c {
        Ctx::Source => env
            .source
            .map(Value::Obj)
            .unwrap_or(Value::Unit),
        Ctx::Controller => env
            .controller
            .map(Value::Player)
            .unwrap_or(Value::Unit),
        Ctx::It => env.subj.clone().unwrap_or(Value::Unit),
        Ctx::Var(name) => env.get(name).cloned().unwrap_or(Value::Unit),
        // The event that fired this ability (carried in the env from match time).
        Ctx::Triggering(field) => env
            .triggering_event
            .as_ref()
            .map(|e| project_event_field(e, *field))
            .unwrap_or(Value::Unit),
        // This spell's own cast: the most recent `SpellCast` in the log whose
        // card_id is the source object (an object keeps its id stack→battlefield,
        // so an ETB replacement on the entering permanent finds its own cast).
        Ctx::ThisCast(field) => {
            let Some(src) = env.source else { return Value::Unit };
            state
                .event_log
                .entries
                .iter()
                .rev()
                .find_map(|logged| match &logged.event {
                    crate::GameEvent::SpellCast { card_id, .. } if *card_id == src => {
                        Some(project_event_field(&logged.event, *field))
                    }
                    _ => None,
                })
                .unwrap_or(Value::Unit)
        }
    }
}

/// Project a single `EventField` off a logged `GameEvent` (CR-meaningful cast /
/// event properties). Unmapped (event, field) pairs return `Unit`; new arms are
/// added demand-driven as cards need them.
fn project_event_field(event: &crate::GameEvent, field: crate::ir::context::EventField) -> Value {
    use crate::ir::context::EventField as F;
    use crate::GameEvent as E;
    match (event, field) {
        // Spell-cast properties.
        (E::SpellCast { mana_spent, .. }, F::ManaSpent) => Value::Bool(*mana_spent),
        (E::SpellCast { alt_cost, .. }, F::AltCost) => Value::Bool(*alt_cost),
        (E::SpellCast { x, .. }, F::X) => Value::Num(*x as i64),
        (E::SpellCast { delved, .. }, F::DelvedExiled) => Value::ObjSet(delved.clone()),
        // Zone-change properties (useful to `Triggering` in replacement bodies).
        (E::ZoneChange { id, .. }, F::ObjMoved) => Value::Obj(*id),
        (E::ZoneChange { from, .. }, F::ZoneFrom) => Value::Zone(*from),
        (E::ZoneChange { to, .. }, F::ZoneTo) => Value::Zone(*to),
        _ => Value::Unit,
    }
}

fn eval_game_ctx(g: &GameCtx, state: &SimState) -> Value {
    // Layer C designations not yet wired into SimState — Monarch/Initiative/
    // DayNight/CityBlessing/RingTempted land with their host cards. Return
    // a neutral default; tests referring to these will set it up explicitly.
    match g {
        GameCtx::Monarch => Value::Unit,
        GameCtx::Initiative => Value::Unit,
        GameCtx::DayNight => Value::Unit,
        GameCtx::CityBlessing => Value::Bool(false),
        GameCtx::RingTempted => Value::Num(0),
        GameCtx::CastingSpell => Value::Obj(state.casting_spell.unwrap_or(ObjId::UNSET)),
    }
}

fn expect_obj(v: Value) -> ObjId {
    if let Value::Obj(o) = v {
        o
    } else {
        ObjId::default()
    }
}

/// Flatten a `Value` into a list of object ids. Accepts either a single
/// `Obj` or an `ObjSet`; anything else yields an empty list.
fn obj_ids_of(v: Value) -> Vec<ObjId> {
    match v {
        Value::Obj(id) => vec![id],
        Value::ObjSet(ids) => ids,
        _ => Vec::new(),
    }
}

/// Evaluator-driven scry: mirrors `effects::eff_scry`. Scores each of the top
/// `n` library cards for `who`; scores ≥ 0.3 stay on top in order, the rest go
/// to the bottom.
fn expect_player(v: Value) -> PlayerId {
    if let Value::Player(p) = v {
        p
    } else {
        PlayerId::Us
    }
}

fn expect_num(v: Value) -> i64 {
    match v {
        Value::Num(n) => n,
        Value::Bool(true) => 1,
        Value::Bool(false) => 0,
        _ => 0,
    }
}

fn expect_bool(v: Value) -> bool {
    match v {
        Value::Bool(b) => b,
        Value::Num(n) => n != 0,
        _ => false,
    }
}

fn values_eq(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Num(x), Value::Num(y)) => x == y,
        (Value::Bool(x), Value::Bool(y)) => x == y,
        (Value::Obj(x), Value::Obj(y)) => x == y,
        (Value::Player(x), Value::Player(y)) => x == y,
        (Value::Zone(x), Value::Zone(y)) => x == y,
        (Value::Type(x), Value::Type(y)) => x == y,
        (Value::Supertype(x), Value::Supertype(y)) => x == y,
        (Value::Color(x), Value::Color(y)) => x == y,
        (Value::Keyword(x), Value::Keyword(y)) => x == y,
        (Value::Counter(x), Value::Counter(y)) => x == y,
        (Value::Name(x), Value::Name(y)) => x == y,
        (Value::Subtype(x), Value::Subtype(y)) => x == y,
        _ => false,
    }
}

fn contains(needle: &Value, set: &Value) -> bool {
    match (needle, set) {
        (Value::Type(t), Value::TypeSet(s)) => s.contains(t),
        (Value::Color(c), Value::ColorSet(s)) => s.contains(c),
        (Value::Keyword(k), Value::KeywordSet(s)) => s.contains(k),
        (Value::Subtype(t), Value::SubtypeSet(s)) => s.iter().any(|x| x == t),
        (Value::Supertype(st), Value::SupertypeSet(s)) => s.contains(st),
        (Value::Obj(o), Value::ObjSet(s)) => s.contains(o),
        (Value::Player(p), Value::PlayerSet(s)) => s.contains(p),
        _ => false,
    }
}

fn fold_set(
    state: &SimState,
    env: &BindEnv,
    set: &Expr,
    bind: &'static str,
    body: &Expr,
    is_all: bool,
) -> bool {
    let elements = match eval_expr(set, state, env) {
        Value::ObjSet(v) => v.into_iter().map(Value::Obj).collect::<Vec<_>>(),
        Value::PlayerSet(v) => v.into_iter().map(Value::Player).collect::<Vec<_>>(),
        _ => return is_all, // empty fold: All → true, Any → false
    };
    if elements.is_empty() {
        return is_all;
    }
    for elem in elements {
        let sub_env = env.clone().with_var(bind, elem.clone()).with_subj(elem);
        let ok = expect_bool(eval_expr(body, state, &sub_env));
        if is_all && !ok {
            return false;
        }
        if !is_all && ok {
            return true;
        }
    }
    is_all
}

/// Count elements of `set` for which `body` holds (`bind` names the element).
/// The counting sibling of `fold_set`.
fn count_set(
    state: &SimState,
    env: &BindEnv,
    set: &Expr,
    bind: &'static str,
    body: &Expr,
) -> i64 {
    let elements = match eval_expr(set, state, env) {
        Value::ObjSet(v) => v.into_iter().map(Value::Obj).collect::<Vec<_>>(),
        Value::PlayerSet(v) => v.into_iter().map(Value::Player).collect::<Vec<_>>(),
        _ => return 0,
    };
    elements
        .into_iter()
        .filter(|elem| {
            let sub_env = env.clone().with_var(bind, elem.clone()).with_subj(elem.clone());
            expect_bool(eval_expr(body, state, &sub_env))
        })
        .count() as i64
}

fn enumerate_zone(state: &SimState, env: &BindEnv, zone: &ZoneSel) -> Vec<ObjId> {
    match zone {
        ZoneSel::Id(_) => Vec::new(), // stage-2 slice does not use absolute ZoneIds yet
        ZoneSel::Global(kind) => enumerate_kind_all_players(state, *kind),
        ZoneSel::Scoped { zone_kind, owner } => {
            let owner_val = eval_expr(owner, state, env);
            let owner = expect_player(owner_val);
            enumerate_kind_for_player(state, *zone_kind, owner)
        }
    }
}

fn enumerate_kind_for_player(state: &SimState, kind: ZoneKindSel, who: PlayerId) -> Vec<ObjId> {
    match kind {
        ZoneKindSel::Battlefield => state.permanents_of(who).map(|o| o.id).collect(),
        ZoneKindSel::Hand => state.hand_of(who).map(|o| o.id).collect(),
        ZoneKindSel::Graveyard => state.graveyard_of(who).map(|o| o.id).collect(),
        ZoneKindSel::Exile => state.exile_of(who).map(|o| o.id).collect(),
        ZoneKindSel::Library => state.library_of(who).map(|o| o.id).collect(),
        ZoneKindSel::Stack | ZoneKindSel::Command => {
            state.objects.values().filter(|o| o.controller == who && obj_in_kind(o, kind)).map(|o| o.id).collect()
        }
    }
}

fn enumerate_kind_all_players(state: &SimState, kind: ZoneKindSel) -> Vec<ObjId> {
    let mut out = enumerate_kind_for_player(state, kind, PlayerId::Us);
    out.extend(enumerate_kind_for_player(state, kind, PlayerId::Opp));
    out
}

pub(crate) fn obj_in_kind(o: &crate::GameObject, kind: ZoneKindSel) -> bool {
    match (kind, o.zone()) {
        (ZoneKindSel::Stack, Some(Zone::Stack)) => true,
        (ZoneKindSel::Hand, Some(Zone::Hand { .. })) => true,
        (ZoneKindSel::Library, Some(Zone::Library)) => true,
        (ZoneKindSel::Battlefield, Some(Zone::Battlefield)) => true,
        (ZoneKindSel::Graveyard, Some(Zone::Graveyard)) => true,
        (ZoneKindSel::Exile, Some(Zone::Exile { .. })) => true,
        _ => false,
    }
}

fn top_n(state: &SimState, env: &BindEnv, zone: &ZoneSel, n: usize) -> Vec<ObjId> {
    // Only library has a meaningful "top" for now; others fall back to empty.
    let owner = match zone {
        ZoneSel::Scoped { owner, .. } => expect_player(eval_expr(owner, state, env)),
        _ => return Vec::new(),
    };
    state.library_of(owner).take(n).map(|o| o.id).collect()
}

// ── Projections from SimState/CardDef ────────────────────────────────────────

/// Resolve an ObjId to its CardDef. Prefers the materialized CE-applied snapshot
/// (only present for battlefield objects), then falls back to the base catalog
/// entry — needed for objects in hand/graveyard/exile/stack.
fn card_def_of<'a>(state: &'a SimState, id: ObjId) -> Option<&'a crate::catalog::CardDef> {
    // A card-less object (ability on the stack) has no CardDef — don't fall back
    // to the catalog by its (synthetic) catalog_key.
    if state.objects.get(&id).map_or(false, |o| o.ability().is_some()) {
        return None;
    }
    state.def_of(id).or_else(|| {
        state
            .objects
            .get(&id)
            .and_then(|o| state.catalog.get(o.catalog_key.as_str()))
    })
}

fn types_of_obj(state: &SimState, id: ObjId) -> Vec<CardType> {
    card_def_of(state, id).map(|d| d.types.clone()).unwrap_or_default()
}

fn subtypes_of_obj(state: &SimState, id: ObjId) -> Vec<String> {
    let Some(d) = card_def_of(state, id) else {
        return Vec::new();
    };
    match &d.kind {
        CardKind::Creature(c) => c.creature_subtypes.clone(),
        CardKind::Artifact(a) => a.subtypes.clone(),
        CardKind::Instant(s) | CardKind::Sorcery(s) => s.subtypes.clone(),
        CardKind::Land(l) => {
            let mut out = land_type_strings(&l.land_types);
            for s in &l.nonbasic_subtypes {
                if !out.iter().any(|x| x.eq_ignore_ascii_case(s)) { out.push(s.to_lowercase()); }
            }
            out
        },
        _ => Vec::new(),
    }
}

fn land_type_strings(lt: &crate::catalog::LandTypes) -> Vec<String> {
    lt.iter().map(|t| t.as_lower().to_string()).collect()
}

fn keywords_of_obj(state: &SimState, id: ObjId) -> Vec<Keyword> {
    let Some(def) = card_def_of(state, id) else {
        return Vec::new();
    };
    let kws = match &def.kind {
        CardKind::Creature(c) => c.keywords,
        _ => return Vec::new(),
    };
    const ALL: &[Keyword] = &[
        Keyword::Flying,
        Keyword::Haste,
        Keyword::Shadow,
        Keyword::Lifelink,
        Keyword::Vigilance,
        Keyword::Deathtouch,
        Keyword::Annihilator6,
        Keyword::FirstStrike,
        Keyword::DoubleStrike,
        Keyword::Trample,
        Keyword::Flash,
        Keyword::Hexproof,
        Keyword::Reach,
        Keyword::Unblockable,
    ];
    ALL.iter().copied().filter(|k| kws.contains(*k)).collect()
}

fn power_of_obj(state: &SimState, id: ObjId) -> Option<i32> {
    card_def_of(state, id).and_then(|d| d.as_creature()).map(|c| c.power())
}

fn toughness_of_obj(state: &SimState, id: ObjId) -> Option<i32> {
    card_def_of(state, id).and_then(|d| d.as_creature()).map(|c| c.toughness())
}

fn zone_id_of_obj(state: &SimState, id: ObjId) -> Option<ZoneId> {
    let obj = state.objects.get(&id)?;
    Some(match obj.zone()? {
        Zone::Library => ZoneId::Library,
        Zone::Hand { .. } => ZoneId::Hand,
        Zone::Stack => ZoneId::Stack,
        Zone::Battlefield => ZoneId::Battlefield,
        Zone::Graveyard => ZoneId::Graveyard,
        Zone::Exile { .. } => ZoneId::Exile,
    })
}

// ── Dependency axes (Stage-2 slice 3) ────────────────────────────────────────
//
// Single shared vocabulary for both CE reads (Expr projections) and CE writes
// (CEMod variants). The CR 613 layer ordering falls out of the axis labels:
// a CE that reads X and a CE that writes X have a dependency edge.
//
// `deps_of(Expr)` walks the expression tree and returns the set of axes it
// reads. `writes_of(CEMod)` is hard-coded per variant — each variant knows
// which layer it modifies, and we simply list them.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Axis {
    /// Layer 1: full-copy effects. Writes to Copy implicitly invalidate all
    /// downstream characteristic axes, so `writes_of(CopyOf)` expands to the
    /// full characteristic set rather than a bare `Copy`.
    Copy,
    /// Layer 2: controller.
    Control,
    /// Layer 3: text-changing effects. No current CEMod writes to this axis,
    /// but the slot is reserved for future "change-the-text" cards.
    Text,
    /// Layer 4: card types / supertypes / subtypes.
    Type,
    /// Layer 5: colors.
    Color,
    /// Layer 6: abilities (keywords, granted abilities, protection, can't-X).
    Abilities,
    /// Layer 7: power / toughness.
    PT,
    /// Zone membership (battlefield, library, graveyard, …).
    Zone,
    /// Counters on objects (+1/+1, void, charge, …).
    Counters,
    /// Player life totals.
    Life,
    /// Player hand sizes (read by "target player discards a card"-type
    /// inspections; written by `MaxHandSize` via RuleMod).
    HandSize,
    /// Layer-C designations (monarch, initiative, day/night, city's blessing,
    /// the Ring-tempted level).
    GameCtx,
    /// Structured game-event history. Read by `Ctx::Triggering` /
    /// `Ctx::ThisCast` projections; written implicitly by the engine when it
    /// logs events (no CEMod writes here).
    EventLog,
    /// Who-can-cast-what / from-where (flashback, cascade, madness, etc.).
    CastPermission,
    /// Cost-to-cast modifiers (increase/decrease by N).
    CostMod,
    /// Rule-level overrides (can't lose, max hand size, extra land drops,
    /// skip-step permissions).
    RuleMod,
}

#[derive(Debug, Default, Clone)]
pub struct CeDeps {
    pub reads: Vec<Axis>,
    pub writes: Vec<Axis>,
}

/// Read axes of a pure expression. Walks the tree and collects the axes that
/// every projection touches. Duplicates are deduped at the end; order is not
/// meaningful.
pub(crate) fn deps_of(expr: &Expr) -> CeDeps {
    let mut reads = Vec::new();
    walk_reads(expr, &mut reads);
    dedup(&mut reads);
    CeDeps { reads, writes: Vec::new() }
}

fn walk_reads(expr: &Expr, out: &mut Vec<Axis>) {
    match expr {
        // ── leaves with no axis dependency ────────────────────────────────
        Expr::Num(_) | Expr::Bool(_) => {}
        Expr::TypeLit(_) | Expr::SupertypeLit(_) | Expr::SubtypeLit(_)
        | Expr::ColorLit(_) | Expr::KeywordLit(_) | Expr::NameLit(_) => {}
        Expr::Ctx(c) => match c {
            Ctx::Triggering(_) | Ctx::ThisCast(_) => out.push(Axis::EventLog),
            // Source / Controller / It / Var reference the bind-env; the
            // resolved value may later be fed into a projection whose axis is
            // recorded at that projection site.
            _ => {}
        },
        Expr::GameCtx(_) => out.push(Axis::GameCtx),

        // ── characteristic projections (CR 613 layers) ────────────────────
        Expr::Types(e) | Expr::Supertypes(e) | Expr::Subtypes(e) => {
            out.push(Axis::Type);
            walk_reads(e, out);
        }
        Expr::Colors(e) => {
            out.push(Axis::Color);
            walk_reads(e, out);
        }
        Expr::Keywords(e) => {
            out.push(Axis::Abilities);
            walk_reads(e, out);
        }
        Expr::Power(e) | Expr::Toughness(e) => {
            out.push(Axis::PT);
            walk_reads(e, out);
        }
        Expr::Attacking(e) | Expr::Unblocked(e) | Expr::AttachedTo(e)
        | Expr::ChosenName(e) | Expr::ChosenColor(e) | Expr::ChosenTargets(e)
        | Expr::IsFrontFace(e) => {
            // Battlefield-/stack-state projection — no CE axis applies (combat /
            // attachment / ETB-choice / targeting state isn't a continuous-effect
            // characteristic surface). Walk the operand but emit no axis push.
            walk_reads(e, out);
        }
        // Mana cost is a printed characteristic; layer 1 copy is the only
        // CE that currently changes it — treat MV as Copy-dependent.
        Expr::Mv(e) => {
            out.push(Axis::Copy);
            walk_reads(e, out);
        }
        Expr::Name(e) => {
            out.push(Axis::Copy);
            walk_reads(e, out);
        }
        Expr::Controller(e) => {
            out.push(Axis::Control);
            walk_reads(e, out);
        }
        // Owner is immutable; no axis dependency.
        Expr::Owner(e) => walk_reads(e, out),
        Expr::ZoneOf(e) => {
            out.push(Axis::Zone);
            walk_reads(e, out);
        }
        Expr::ZoneLit(_) => {}
        Expr::ObjLit(_) => {}
        Expr::IsToken(e) => walk_reads(e, out),
        Expr::IsAbility(e) => walk_reads(e, out),
        Expr::AbilityIsTriggered(e) => walk_reads(e, out),
        Expr::CountersOn(e, _) => {
            out.push(Axis::Counters);
            walk_reads(e, out);
        }
        // Loyalty is a counter (CR 306.5b); active player is global game state.
        Expr::LoyaltyOf(e) => {
            out.push(Axis::Counters);
            walk_reads(e, out);
        }
        Expr::ActivePlayer => out.push(Axis::GameCtx),

        // ── player projections ────────────────────────────────────────────
        Expr::Life(e) => {
            out.push(Axis::Life);
            walk_reads(e, out);
        }
        Expr::HandSize(e) => {
            out.push(Axis::HandSize);
            walk_reads(e, out);
        }
        Expr::LibrarySize(e) => {
            out.push(Axis::Zone);
            walk_reads(e, out);
        }
        Expr::Opponents(e) => walk_reads(e, out),

        // ── zone walkers ──────────────────────────────────────────────────
        Expr::Top { zone: _, n } => {
            out.push(Axis::Zone);
            walk_reads(n, out);
        }
        Expr::AllObjects { zone: _, bind: _, filter } => {
            out.push(Axis::Zone);
            walk_reads(filter, out);
        }

        // ── boolean / arithmetic ──────────────────────────────────────────
        Expr::And(a, b) | Expr::Or(a, b) | Expr::Eq(a, b) | Expr::Lt(a, b)
        | Expr::Le(a, b) | Expr::Gt(a, b) | Expr::Ge(a, b)
        | Expr::Contains(a, b) | Expr::Add(a, b) | Expr::Sub(a, b)
        | Expr::Mul(a, b) | Expr::Div(a, b) | Expr::Min(a, b) | Expr::Max(a, b) => {
            walk_reads(a, out);
            walk_reads(b, out);
        }
        Expr::Not(a) | Expr::Neg(a) => walk_reads(a, out),
        Expr::Players => {}

        // ── folds / binding ───────────────────────────────────────────────
        Expr::Count(a) => walk_reads(a, out),
        Expr::SumManaValue(a) => {
            out.push(Axis::Copy);
            walk_reads(a, out);
        },
        Expr::CountWhere { set, bind: _, body }
        | Expr::Any { set, bind: _, body }
        | Expr::All { set, bind: _, body } => {
            walk_reads(set, out);
            walk_reads(body, out);
        }
        Expr::Let { name: _, value, body } => {
            walk_reads(value, out);
            walk_reads(body, out);
        }
        // Bound is a pure env-lookup, no axis dependency.
        Expr::Bound(_) => {}

        // Event-log folds read the logged-event stream.
        Expr::EventCount { window: _, filter } => {
            out.push(Axis::EventLog);
            walk_event_filter(filter, out);
        }
    }
}

/// Read axes referenced by an `EventFilter`. The log itself is `Axis::EventLog`
/// (added at the `EventCount` site); this walk covers the sub-expressions each
/// filter threads through (e.g. the `caster` player expression).
fn walk_event_filter(filter: &crate::ir::expr::EventFilter, out: &mut Vec<Axis>) {
    use crate::ir::expr::EventFilter;
    match filter {
        EventFilter::SpellCast { caster, card, spell_filter, alt_cost: _ } => {
            if let Some(e) = caster { walk_reads(e, out); }
            if let Some(e) = card { walk_reads(e, out); }
            if let Some(f) = spell_filter { walk_reads(&f.0, out); }
        }
        EventFilter::Draw { who } => {
            if let Some(e) = who { walk_reads(e, out); }
        }
        EventFilter::LifeLost { who } => {
            if let Some(e) = who { walk_reads(e, out); }
        }
    }
}

fn dedup(v: &mut Vec<Axis>) {
    let mut seen = std::collections::HashSet::new();
    v.retain(|a| seen.insert(*a));
}

/// Write axes of a CE modification. Hard-coded per variant — we know which
/// layer each CEMod touches at construction time.
///
/// `CopyOf` is the odd one: layer 1 overrides every downstream characteristic,
/// so it writes the whole characteristic bundle rather than bare `Copy`. That
/// way any CE reading Type / Color / Abilities / PT correctly orders after
/// copy effects.
pub(crate) fn writes_of(cemod: &CEMod) -> Vec<Axis> {
    match cemod {
        CEMod::CopyOf(_) => vec![
            Axis::Copy, Axis::Type, Axis::Color, Axis::Abilities, Axis::PT,
        ],

        CEMod::OverrideTypes(_)
        | CEMod::AddType(_)
        | CEMod::AddSubtype(_)
        | CEMod::RemoveSubtype(_) => vec![Axis::Type],

        // Animate: sets type, P/T, and the carried ability set.
        CEMod::BecomeCreature { .. } => vec![Axis::Type, Axis::PT, Axis::Abilities],

        // CR 305.7: also strips abilities generated from rules text.
        CEMod::SetBasicLandType(_) => vec![Axis::Type, Axis::Abilities],

        // "is a <type> in addition to its other land types": adds a subtype
        // without touching abilities.
        CEMod::AddBasicLandType(_) => vec![Axis::Type],

        CEMod::SetColors(_) | CEMod::AddColor(_) => vec![Axis::Color],

        CEMod::AddKeyword(_)
        | CEMod::RemoveKeyword(_)
        | CEMod::GrantAbility(_)
        | CEMod::Uncounterable
        | CEMod::SetProtection(_)
        | CEMod::CantAttack
        | CEMod::CantBlock
        | CEMod::CantBeTargeted(_) => vec![Axis::Abilities],

        CEMod::PumpPT(_, _)
        | CEMod::SetPT(_, _)
        | CEMod::SetPower(_)
        | CEMod::SetToughness(_) => vec![Axis::PT],

        CEMod::AllowLoss(_)
        | CEMod::MaxHandSize(_)
        | CEMod::NoMaxHandSize
        | CEMod::ExtraLandDrops(_)
        | CEMod::SkipStep(_) => vec![Axis::RuleMod],

        CEMod::CastableFrom(_)
        | CEMod::AltCost(_)
        | CEMod::AnyColorMana
        | CEMod::GrantFlash
        | CEMod::OnResolveExile => vec![Axis::CastPermission],

        CEMod::CastingCostPlus(_)
        | CEMod::SpellsCostMore { .. }
        | CEMod::SpellsCostLess { .. } => vec![Axis::CostMod],
    }
}

// ── IR static → ContinuousInstance bridge ────────────────────────────────────
//
// Translates an `AbilityKind::Static { mods, scope, condition }` block into the legacy
// `ContinuousInstance` records that `recompute` already consumes. One CI per
// CEMod — each CEMod has a single CR-613 layer.
//
// Not every CEMod is a recompute-time effect (cast-permission / cost-mod / rule
// variants live at cast or state-check sites). `ir_static_to_cis` skips those;
// they are wired where they're actually consumed.

use crate::ir::ability::{Ability, AbilityKind};
use crate::ir::ce::BasicLandType;
use crate::{CardDef, ContinuousInstance, ContinuousLayer, Expiry};

pub(crate) fn ir_static_to_cis(
    source_id: ObjId,
    controller: PlayerId,
    ability: &Ability,
    state: &SimState,
) -> Vec<ContinuousInstance> {
    let AbilityKind::Static { mods, scope, condition } = &ability.kind else {
        return Vec::new();
    };
    // CR 613 "as long as …" gate: when the condition is false this recompute,
    // the whole block contributes nothing (Decision 4 — block condition, not a
    // per-object filter). Evaluated against the source's binding frame.
    if let Some(cond) = condition {
        let env = BindEnv::new().with_source(source_id).with_controller(controller);
        if !expect_bool(eval_expr(cond, state, &env)) {
            return Vec::new();
        }
    }
    mods.iter()
        .filter_map(|m| cemod_to_ci(source_id, controller, m, scope, state))
        .collect()
}

/// Wrap a `CEMod` into a *static-ability* `ContinuousInstance` (CR 613): the
/// CEMod-intrinsic half (layer/reads/writes/modifier) comes from the shared
/// `cemod_to_modifier`; the static half adds the scope filter and the
/// "while the source is on the battlefield" expiry. `timestamp` is assigned by
/// the recompute caller.
fn cemod_to_ci(
    source_id: ObjId,
    controller: PlayerId,
    cemod: &CEMod,
    scope: &Option<Filter>,
    state: &SimState,
) -> Option<ContinuousInstance> {
    let env = BindEnv::new().with_source(source_id).with_controller(controller);
    let build = cemod_to_modifier(cemod, &env, state)?;
    Some(ContinuousInstance {
        source_id,
        controller,
        layer: build.layer,
        reads: build.reads,
        writes: build.writes,
        timestamp: 0, // assigned by recompute caller
        filter: build_filter(source_id, controller, scope),
        modifier: build.modifier,
        expiry: Expiry::WhileSourceOnBattlefield,
    })
}

/// Build a `ContinuousFilterFn` from an optional scope `Filter`. `None`
/// matches all objects; otherwise the filter expr is evaluated per candidate
/// with `Ctx::It` bound to the candidate object.
fn build_filter(
    source_id: ObjId,
    controller: PlayerId,
    scope: &Option<Filter>,
) -> crate::ContinuousFilterFn {
    match scope {
        None => std::sync::Arc::new(|_id, _ctr, _state| true),
        Some(filter) => {
            let filter = filter.clone();
            std::sync::Arc::new(move |id, _ctr, state| {
                let env = BindEnv::new()
                    .with_source(source_id)
                    .with_controller(controller);
                matches(&filter, id, state, &env)
            })
        }
    }
}

/// Apply `SetBasicLandType` per CR 305.6/305.7:
/// - Non-lands: no-op.
/// - Basic lands: no-op (supertype Basic is preserved; the rule only changes
///   nonbasic lands per the usual "nonbasic lands are …" wording; scope filter
///   is expected to have excluded basics, but we double-check for robustness).
/// - Otherwise: replace `land_types` with the single chosen basic, swap the
///   intrinsic mana ability for the matching `{T}: Add {C}`, and strip all
///   rules-text abilities (both legacy closure-based and IR-based).
fn apply_set_basic_land_type(def: &mut CardDef, kind: BasicLandType) {
    use crate::Supertype;
    if !matches!(def.kind, CardKind::Land(_)) {
        return;
    }
    if def.supertypes.contains(&Supertype::Basic) {
        return;
    }
    if let CardKind::Land(ref mut land) = def.kind {
        land.land_types = crate::LandTypes::new();
        land.land_types.insert(kind);
        land.mana_abilities = vec![mana_for_basic_land(kind)];
        land.abilities.clear();
    }
    // CR 305.7: strip all non-mana abilities — both legacy closure-based and
    // IR-authored abilities on the source def.
    def.static_ability_defs.clear();
    def.abilities.clear();
}

/// Apply `AddBasicLandType` per CR 305.6 "is a <type> in addition to its
/// other land types". No-op on non-lands or when the land already has this
/// subtype (the idempotency check avoids pushing the intrinsic mana ability
/// twice when two Yavimayas / Urborgs share the battlefield).
fn apply_add_basic_land_type(def: &mut CardDef, kind: BasicLandType) {
    if !matches!(def.kind, crate::CardKind::Land(_)) {
        return;
    }
    if let crate::CardKind::Land(ref mut land) = def.kind {
        if land.land_types.contains(kind) {
            return;
        }
        land.land_types.insert(kind);
        land.mana_abilities.push(mana_for_basic_land(kind));
    }
}

/// Animate `def` as a creature (CR 613.1c layer 4): give it the base P/T, creature
/// subtypes, and keywords, carrying its existing activated abilities over (so a
/// planeswalker's loyalty abilities stay usable). Per the CR ruling for Kaito-style
/// "he's a … creature" wording, an animated planeswalker stops being a planeswalker.
fn apply_become_creature(
    def: &mut CardDef,
    power: i32,
    toughness: i32,
    subtypes: &[String],
    keywords: &[crate::Keyword],
) {
    use crate::{CardKind, CardType, CreatureData};
    let was_legendary = def.legendary();
    def.types.retain(|ty| *ty != CardType::Planeswalker);
    if !def.types.contains(&CardType::Creature) {
        def.types.push(CardType::Creature);
    }
    // Carry existing activated abilities over (loyalty abilities stay activatable).
    let abilities = match &def.kind {
        CardKind::Planeswalker(pw) => pw.abilities.clone(),
        CardKind::Creature(c) => c.abilities.clone(),
        _ => Vec::new(),
    };
    let mana_cost = def.mana_cost().to_string();
    let mut c = CreatureData::new(&mana_cost, power, toughness);
    c.legendary = was_legendary;
    c.creature_subtypes = subtypes.to_vec();
    for kw in keywords {
        c.keywords.insert(*kw);
    }
    c.abilities = abilities;
    def.kind = CardKind::Creature(c);
}

fn mana_for_basic_land(kind: BasicLandType) -> crate::ManaAbility {
    let color = kind.mana_color();
    let color_owned = color.to_string();
    crate::ManaAbility {
        // IR `Tap source` — same shape as `ir_tap_mana` in card_defs. The
        // infinite-draw loop that previously surfaced when migrating this
        // was tracked to the strategy's castability check skipping mana
        // availability for IR costs; fixed in strategy.rs::ability_available.
        costs: crate::ir::ability::CostBody::Ir(crate::ir::action::Action::Tap {
            target: crate::ir::expr::Expr::Ctx(crate::ir::context::Ctx::Source),
        }),
        produces: crate::produces_colors(color),
        produces_count: 1,
        make_effect: std::sync::Arc::new(move |who, _| crate::eff_mana(who, color_owned.clone())),
        ..Default::default()
    }
}

// ── IR replacement dispatch bridge ───────────────────────────────────────────
//
// Turns an `AbilityKind::Replacement` into the (targets, Effect) pair the
// `fire_event` replacement loop expects. Mirrors legacy `replacement_defs`
// dispatch: `check` returns `Some(targets)` when the pattern fires, and the
// built `Effect` runs the `ReplacementBody`. For `Replace(Action)`, the
// action body runs through the standard executor with bindings carrying the
// matched event's `triggered_obj` so it can compose `Move` + `Tap` /
// `PutCounters` / etc. from the generic Action vocabulary.

use crate::ir::ability::ReplacementBody;

/// Check whether an IR Replacement ability matches the given event. Returns
/// the object-id list to pass through to the effect (mirrors legacy
/// `ReplacementCheckFn`), or None if the ability doesn't apply.
pub(crate) fn ir_replacement_match(
    ability: &crate::ir::ability::Ability,
    event: &crate::GameEvent,
    source_id: ObjId,
    controller: PlayerId,
    state: &SimState,
) -> Option<Vec<ObjId>> {
    let AbilityKind::Replacement { matches: pattern, condition, .. } = &ability.kind else {
        return None;
    };
    let env = BindEnv::new()
        .with_source(source_id)
        .with_controller(controller);
    let matched = match_event_pattern(pattern, event, &env, state)?;
    if let Some(cond) = condition {
        if !expect_bool(eval_expr(cond, state, &matched)) {
            return None;
        }
    }
    let triggered_obj = match matched.bindings.get("triggered_obj") {
        Some(Value::Obj(id)) => *id,
        _ => source_id,
    };
    Some(vec![triggered_obj])
}

/// Build an `Effect` that, when called with the matched targets, runs the IR
/// `ReplacementBody`. The body's `Action` sees the matched event via
/// bindings populated by `match_event_pattern` (`triggered_obj`,
/// `triggered_actor`, etc.). CR 614.5 self-loop guard is engine-enforced.
pub(crate) fn ir_replacement_effect(
    ability: &crate::ir::ability::Ability,
    source_id: ObjId,
    controller: PlayerId,
    event: &crate::GameEvent,
) -> Option<crate::effects::Effect> {
    let AbilityKind::Replacement { body, .. } = &ability.kind else {
        return None;
    };
    match body {
        ReplacementBody::Replace(action) => {
            let action = action.clone();
            // A replacement runs *on* the event it replaces, so the body reads
            // that event's fields via `Ctx::Triggering`.
            let trig_event = event.clone();
            Some(crate::effects::Effect(std::sync::Arc::new(
                move |state, _t, targets| {
                    let Some(&id) = targets.first() else {
                        return;
                    };
                    let mut env = BindEnv::new()
                        .with_source(source_id)
                        .with_controller(controller)
                        .with_triggering_event(trig_event.clone())
                        .with_var("triggered_obj", Value::Obj(id));
                    execute_mut(&action, state, &mut env);
                },
            )))
        }
        ReplacementBody::Prevent => None,
    }
}

// ── IR activated-ability bridge ──────────────────────────────────────────────
//
// Synthesizes a legacy `AbilityDef` from an IR `AbilityKind::Activated`. Used
// by the catalog build to append IR-authored activated abilities to the
// kind-specific ability list (`LandData.abilities`, `ArtifactData.abilities`,
// etc.) so the existing `collect_legal_actions` / `run_activate_submachine`
// pipeline picks them up transparently. Body is carried via
// `AbilityDef.ir_body`, which `build_ability_effect` already handles.

pub(crate) fn ir_activated_as_legacy(
    ability: &crate::ir::ability::Ability,
) -> Option<crate::AbilityDef> {
    let AbilityKind::Activated {
        cost,
        target_spec,
        choice_spec,
        body,
        timing,
        active_zone,
        activation_condition: _, // TODO Stage-4-cleanup: lower to ObjPredicate
    } = &ability.kind
    else {
        return None;
    };
    // Mana abilities are routed via `ir_activated_as_mana_ability_legacy` —
    // see `is_mana_ability`. This returns `None` so the caller skips them.
    if is_mana_ability(ability) {
        return None;
    }
    let source_zone = match active_zone {
        ZoneKindSel::Battlefield => crate::SourceZone::Battlefield,
        ZoneKindSel::Hand => crate::SourceZone::Hand,
        _ => crate::SourceZone::Battlefield,
    };
    Some(crate::AbilityDef {
        source_zone,
        // Pass-through: the bridge clones the IR `CostBody` directly into
        // the synthesized legacy struct. Downstream consumers (mana sub-loop,
        // pay_ability_cost, planner, strategy castability check) all
        // dispatch on `CostBody` natively now — no lowering needed.
        costs: cost.clone(),
        target_spec: target_spec.clone(),
        choice_spec: choice_spec.clone(),
        ability_factory: None,
        ir_body: Some(body.clone()),
        activatable: true,
        timing: *timing,
    })
}

// ── CR 605.1a mana-ability classification ───────────────────────────────────
//
// "An activated ability is a mana ability if it has all of the following
// properties: it doesn't have a target, it isn't a loyalty ability, and it
// could add mana to a player's mana pool when it resolves."
//
// `body_can_produce_mana` walks the action tree pessimistically — any branch
// reaching `AddMana` qualifies (CR's "could add mana" wording).

pub(crate) fn body_can_produce_mana(action: &Action) -> bool {
    match action {
        Action::AddMana { .. } => true,
        Action::Sequence(actions) => actions.iter().any(body_can_produce_mana),
        Action::IfThen { then, else_, .. } => {
            body_can_produce_mana(then)
                || else_.as_ref().map_or(false, |e| body_can_produce_mana(e))
        }
        Action::MayDo { action, .. } => body_can_produce_mana(action),
        Action::ForEach { body, .. } => body_can_produce_mana(body),
        Action::Choose { options, .. } => {
            options.iter().any(|o| body_can_produce_mana(&o.action))
        }
        _ => false,
    }
}

pub(crate) fn is_mana_ability(ability: &crate::ir::ability::Ability) -> bool {
    let AbilityKind::Activated { target_spec, body, .. } = &ability.kind else {
        return false;
    };
    matches!(target_spec, crate::TargetSpec::None) && body_can_produce_mana(body)
}

/// Find the first `AddMana` action reachable from `action`. Used by the
/// bridge to extract `produces` and `produces_count` for the legacy
/// `ManaAbility` struct (which the affordability predictor reads).
fn find_first_add_mana(
    action: &Action,
) -> Option<(&crate::ir::action::Who, &Expr, &crate::ir::action::ManaSpec)> {
    match action {
        Action::AddMana { who, count, spec } => Some((who, count, spec)),
        Action::Sequence(actions) => actions.iter().find_map(find_first_add_mana),
        Action::IfThen { then, else_, .. } => find_first_add_mana(then)
            .or_else(|| else_.as_ref().and_then(|e| find_first_add_mana(e))),
        Action::MayDo { action, .. } => find_first_add_mana(action),
        Action::ForEach { body, .. } => find_first_add_mana(body),
        Action::Choose { options, .. } => options.iter().find_map(|o| find_first_add_mana(&o.action)),
        _ => None,
    }
}

// ── IR mana-ability bridge ───────────────────────────────────────────────────
//
// Synthesizes a legacy `ManaAbility` from an IR `AbilityKind::Activated` that
// classifies as a mana ability per CR 605.1a (`is_mana_ability`). The mana
// sub-loop (`CR 605.3b` — no stack) still drives execution; this bridge feeds
// the synthesized `ManaAbility` so the existing `permitted_mana_of` /
// affordability predictor / sub-loop pipeline pick it up. The `make_effect`
// closure runs `execute(body, ...)` over the IR body — so `Action::AddMana`
// (and any side-effect actions like Ancient Tomb's PayLife) all dispatch
// through the standard interpreter, no separate path.
//
// `produces` and `produces_count` for the legacy struct are extracted by
// walking the body for the first `AddMana`. Static-Expr counts only for now;
// dynamic counts (Cabal Coffers etc.) require predictor extension when those
// cards land.

pub(crate) fn ir_activated_as_mana_ability_legacy(
    ability: &crate::ir::ability::Ability,
) -> Option<crate::ManaAbility> {
    use crate::ir::action::ManaSpec;
    if !is_mana_ability(ability) {
        return None;
    }
    let AbilityKind::Activated {
        cost,
        body,
        timing,
        active_zone,
        activation_condition,
        ..
    } = &ability.kind
    else {
        return None;
    };

    let source_zone = match active_zone {
        ZoneKindSel::Battlefield => crate::SourceZone::Battlefield,
        ZoneKindSel::Hand => crate::SourceZone::Hand,
        _ => return None,
    };

    // Extract metadata from the first reachable AddMana for the predictor.
    let (_who, count_expr, spec) = find_first_add_mana(body)?;
    let count = match count_expr {
        Expr::Num(n) => *n as usize,
        _ => return None, // dynamic counts not yet supported by the predictor
    };
    let produces_vec = match spec {
        ManaSpec::Fixed(cs) => cs.clone(),
        ManaSpec::AnyOneColor => vec![
            crate::Color::White,
            crate::Color::Blue,
            crate::Color::Black,
            crate::Color::Red,
            crate::Color::Green,
        ],
    };

    // The mana-sub-loop calls `make_effect(who, hint)` to build the Effect.
    // We close over the body and dispatch through `execute()` with the hint
    // wired into BindEnv — every action (AddMana + side-effects) runs through
    // the standard interpreter.
    let body = body.clone();
    let make_effect: crate::ManaEffectFactory =
        std::sync::Arc::new(move |who, hint| {
            let body = body.clone();
            crate::Effect(std::sync::Arc::new(move |state, _t, _targets| {
                let env = BindEnv::new()
                    .with_controller(who)
                    .with_chosen_color(hint);
                let _ = execute(&body, state, &env);
            }))
        });

    // The activation gate (e.g. Mox Opal metalcraft) is just the IR condition
    // Expr wrapped as a `Filter` — checked via `obj_matches` at the legacy
    // affordability sites.
    let cond_pred: Option<crate::ir::expr::Filter> =
        activation_condition.as_ref().map(|expr| crate::ir::expr::Filter(expr.clone()));

    Some(crate::ManaAbility {
        source_zone,
        // Pass-through: the bridge clones the IR `CostBody` directly into
        // the synthesized legacy struct. Downstream consumers (mana sub-loop,
        // pay_ability_cost, planner, strategy castability check) all
        // dispatch on `CostBody` natively now — no lowering needed.
        costs: cost.clone(),
        produces: produces_vec,
        produces_count: count,
        make_effect,
        condition: cond_pred,
        activatable: true,
        timing: *timing,
    })
}

fn color_char(c: crate::Color) -> char {
    match c {
        crate::Color::White => 'W',
        crate::Color::Blue => 'U',
        crate::Color::Black => 'B',
        crate::Color::Red => 'R',
        crate::Color::Green => 'G',
    }
}
