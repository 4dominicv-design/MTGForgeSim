#![allow(dead_code)]
//! Castability layer.
//!
//! `enumerate_playable` returns the typed surface of "things `who` can do
//! right now": cast a hand card, activate an ability on a permanent. Each
//! `PlayableAction` carries a `CostSchema` derived from the source's IR cost
//! tree (`build_schema` over the stored `CostBody::Ir(Action)`).
//!
//! This is the surface `Strategy::propose_announcement` consumes: one
//! structured plan per playable action, answering every announcement-time
//! decision (targets, modes, cost bindings) in one call.

use crate::ir::ability::{AbilityKind, CostBody};
use crate::ir::action::Action;
use crate::ir::cost::CostSchema;
use crate::ir::cost_exec::build_schema;
use crate::{
    parse_mana_cost, ActivationTiming, ObjId, PlayerId, SimState, SourceZone,
};

/// What kind of playable action this is. Activated abilities reference the
/// ability inside the card's `abilities()` slice; casting is just "the spell."
#[derive(Clone, Debug)]
pub enum PlayableKind {
    Cast,
    Activate { ability_index: usize },
}

/// One thing `who` can do right now. Schema is `Some` when the cost tree was
/// translatable (IR-cost cards, plus the subset of legacy patterns the shim
/// handles); `None` when the cost shape is outside the shim's scope and the
/// caller must fall back to the legacy per-decision callbacks.
#[derive(Clone)]
pub struct PlayableAction {
    pub source: ObjId,
    pub kind: PlayableKind,
    pub schema: Option<CostSchema>,
}

/// Enumerate every playable action `who` has at this moment. Pure read — no
/// mutation of `state`. The list is unordered; callers that care about
/// priority sort it themselves.
pub(crate) fn enumerate_playable(state: &SimState, who: PlayerId) -> Vec<PlayableAction> {
    let mut out = Vec::new();

    for card in state.hand_of(who) {
        let Some(def) = state.def_of(card.id) else { continue };
        if def.is_land() {
            continue;
        }
        if !def.castable {
            continue;
        }
        let cost = parse_mana_cost(def.mana_cost());
        if !state.potential_mana(who).can_pay(&cost) {
            continue;
        }
        let mana_action = Action::PayMana(cost);
        let schema = build_schema(&mana_action, state, who, card.id);
        out.push(PlayableAction {
            source: card.id,
            kind: PlayableKind::Cast,
            schema,
        });
    }

    for card in state.permanents_of(who) {
        let Some(def) = state.def_of(card.id) else { continue };
        for (idx, ability) in def.abilities().iter().enumerate() {
            if !ability.activatable {
                continue;
            }
            if !matches!(ability.source_zone, SourceZone::Battlefield) {
                continue;
            }
            if ability.timing == ActivationTiming::Sorcery && !state.stack.is_empty() {
                continue;
            }
            let _source_untapped = card.bf().map_or(false, |bf| !bf.tapped);
            let crate::ir::ability::CostBody::Ir(action) = &ability.costs;
            let schema = build_schema(action, state, who, card.id);
            if schema.is_none() {
                continue;
            }
            out.push(PlayableAction {
                source: card.id,
                kind: PlayableKind::Activate { ability_index: idx },
                schema,
            });
        }
    }

    out
}

/// Schema-build for an activated ability's `CostBody`.
pub(crate) fn schema_for_ability_cost(
    body: &CostBody,
    state: &SimState,
    who: PlayerId,
    source: ObjId,
) -> Option<CostSchema> {
    let CostBody::Ir(a) = body;
    build_schema(a, state, who, source)
}

/// Like `enumerate_playable` but matches `AbilityKind` cost bodies (used by IR
/// abilities living under `AbilityKind::Activated`). Phase 2's existing
/// `AbilityDef` enumeration above is the catalog/legacy path; once the
/// catalog and IR ability storage merge, this is the surviving entry point.
pub(crate) fn enumerate_ir_abilities(
    abilities: &[AbilityKind],
    state: &SimState,
    who: PlayerId,
    source: ObjId,
) -> Vec<PlayableAction> {
    let mut out = Vec::new();
    for (i, a) in abilities.iter().enumerate() {
        let AbilityKind::Activated { cost, .. } = a else { continue };
        let schema = schema_for_ability_cost(cost, state, who, source);
        out.push(PlayableAction {
            source,
            kind: PlayableKind::Activate { ability_index: i },
            schema,
        });
    }
    out
}
