//! Stage-1 smoke tests. Confirms the IR types compile and construct.
//! Real evaluator / executor tests land in Stage 2.

use super::ability::*;
use super::action::*;
use super::ce::*;
use super::context::*;
use super::event_log::*;
use super::executor::*;
use super::expr::*;

#[test]
fn expr_construction_smoke() {
    // "Controller of source" — a trivial expr tree, confirms enum variants compose.
    let _ = Expr::Controller(Box::new(Expr::Ctx(Ctx::Source)));

    // "Source has power >= 3"
    let _ = Expr::Ge(
        Box::new(Expr::Power(Box::new(Expr::Ctx(Ctx::Source)))),
        Box::new(Expr::Num(3)),
    );
}

#[test]
fn scoped_binder_smoke() {
    // "All creatures you control" — scoped AllObjects with a bind + filter.
    let _ = Expr::AllObjects {
        zone: ZoneSel::Global(ZoneKindSel::Battlefield),
        bind: "c",
        filter: Box::new(Expr::And(
            Box::new(Expr::Contains(
                Box::new(Expr::TypeLit(crate::CardType::Creature)),
                Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::Var("c"))))),
            )),
            Box::new(Expr::Eq(
                Box::new(Expr::Controller(Box::new(Expr::Ctx(Ctx::Var("c"))))),
                Box::new(Expr::Ctx(Ctx::Controller)),
            )),
        )),
    };
}

#[test]
fn action_construction_smoke() {
    let _ = Action::Draw {
        who: Who::You,
        n: Expr::Num(3),
    };

    let _ = Action::DealDamage {
        source: Expr::Ctx(Ctx::Source),
        target: Expr::Ctx(Ctx::Var("target")),
        amount: Expr::Num(3),
    };
}

#[test]
fn ability_construction_smoke() {
    // Trivial triggered ability: "When ~ enters, draw a card."
    let _ = Ability {
        text: Some("When ~ enters, draw a card."),
        kind: AbilityKind::Triggered {
            spec: TriggerSpec::When {
                pattern: EventPattern::EntersZone {
                    obj_filter: Filter(Expr::Eq(
                        Box::new(Expr::Ctx(Ctx::It)),
                        Box::new(Expr::Ctx(Ctx::Source)),
                    )),
                    zone_kind: ZoneKindSel::Battlefield,
                },
                condition: None,
            },
            target_spec: crate::TargetSpec::None,
            body: Action::Draw {
                who: Who::You,
                n: Expr::Num(1),
            },
            active_zone: ZoneKindSel::Battlefield,
        },
    };
}

#[test]
fn event_log_smoke() {
    let log = EventLog::new();
    assert_eq!(log.count(Window::ThisTurn, |_| true), 0);
    assert_eq!(log.count(Window::ThisGame, |_| true), 0);
    assert!(!log.any(Window::ThisTurn, |_| true));
}

#[test]
fn bind_env_smoke() {
    let env = BindEnv::new()
        .with_var("x", Value::Num(5))
        .with_subj(Value::Num(10));
    assert!(matches!(env.get("x"), Some(Value::Num(5))));
    assert!(env.get("missing").is_none());
}

#[test]
fn ce_mod_smoke() {
    let _ = CEMod::PumpPT(Expr::Num(2), Expr::Num(2));
    let _ = CEMod::GrantFlash;
    let _ = CEMod::AltCost(CostSpec::Free);
}

#[test]
fn game_ctx_smoke() {
    let _ = Expr::GameCtx(GameCtx::Monarch);
    let _ = Ctx::Triggering(EventField::DamageAmount);
    let _ = Ctx::ThisCast(EventField::DelvedExiled);
}

#[test]
fn axis_smoke() {
    // Sanity-check the shared dep vocabulary: reads and writes share one enum.
    let reads = vec![Axis::PT, Axis::Type];
    let writes = vec![Axis::PT];
    assert!(reads.contains(&Axis::PT) && writes.contains(&Axis::PT));
}

// ── Dependency-axis tests ───────────────────────────────────────────────────
//
// `deps_of(Expr)` walks the tree and returns the axes the expression reads.
// `writes_of(CEMod)` is hard-coded per variant. Together they let the engine
// derive CR 613 CE ordering without per-card annotations.

mod deps {
    use super::*;
    use crate::ir::ce::{CEMod, CostSpec};
    use crate::{CardType, Color, CounterType, Keyword, Supertype};
    use std::collections::HashSet;

    fn reads_of(e: &Expr) -> HashSet<Axis> {
        deps_of(e).reads.into_iter().collect()
    }

    #[test]
    fn power_reads_pt() {
        let e = Expr::Power(Box::new(Expr::Ctx(Ctx::It)));
        assert_eq!(reads_of(&e), HashSet::from([Axis::PT]));
    }

    #[test]
    fn types_reads_type() {
        let e = Expr::Types(Box::new(Expr::Ctx(Ctx::It)));
        assert_eq!(reads_of(&e), HashSet::from([Axis::Type]));
    }

    #[test]
    fn supertypes_and_subtypes_both_read_type_layer() {
        for e in [
            Expr::Supertypes(Box::new(Expr::Ctx(Ctx::It))),
            Expr::Subtypes(Box::new(Expr::Ctx(Ctx::It))),
        ] {
            assert_eq!(reads_of(&e), HashSet::from([Axis::Type]));
        }
    }

    #[test]
    fn colors_reads_color() {
        let e = Expr::Colors(Box::new(Expr::Ctx(Ctx::It)));
        assert_eq!(reads_of(&e), HashSet::from([Axis::Color]));
    }

    #[test]
    fn keywords_reads_abilities() {
        let e = Expr::Keywords(Box::new(Expr::Ctx(Ctx::It)));
        assert_eq!(reads_of(&e), HashSet::from([Axis::Abilities]));
    }

    #[test]
    fn mv_and_name_read_copy_layer() {
        // MV derives from the printed mana cost; only layer-1 copy rewrites it.
        for e in [
            Expr::Mv(Box::new(Expr::Ctx(Ctx::It))),
            Expr::Name(Box::new(Expr::Ctx(Ctx::It))),
        ] {
            assert_eq!(reads_of(&e), HashSet::from([Axis::Copy]));
        }
    }

    #[test]
    fn controller_reads_control_but_owner_is_free() {
        let ctrl = Expr::Controller(Box::new(Expr::Ctx(Ctx::It)));
        assert_eq!(reads_of(&ctrl), HashSet::from([Axis::Control]));
        // Owner never changes at runtime — no axis.
        let owner = Expr::Owner(Box::new(Expr::Ctx(Ctx::It)));
        assert_eq!(reads_of(&owner), HashSet::new());
    }

    #[test]
    fn zone_and_counters_and_life() {
        let z = Expr::ZoneOf(Box::new(Expr::Ctx(Ctx::It)));
        assert_eq!(reads_of(&z), HashSet::from([Axis::Zone]));
        let c = Expr::CountersOn(Box::new(Expr::Ctx(Ctx::It)), CounterType::Void);
        assert_eq!(reads_of(&c), HashSet::from([Axis::Counters]));
        let l = Expr::Life(Box::new(Expr::Ctx(Ctx::Controller)));
        assert_eq!(reads_of(&l), HashSet::from([Axis::Life]));
        let h = Expr::HandSize(Box::new(Expr::Ctx(Ctx::Controller)));
        assert_eq!(reads_of(&h), HashSet::from([Axis::HandSize]));
    }

    #[test]
    fn game_ctx_and_triggering_event() {
        assert_eq!(
            reads_of(&Expr::GameCtx(GameCtx::Monarch)),
            HashSet::from([Axis::GameCtx]),
        );
        assert_eq!(
            reads_of(&Expr::Ctx(Ctx::Triggering(EventField::DamageAmount))),
            HashSet::from([Axis::EventLog]),
        );
    }

    #[test]
    fn composed_predicate_unions_axes() {
        // "creature with power >= 3" — reads Type and PT.
        let e = Expr::And(
            Box::new(Expr::Contains(
                Box::new(Expr::TypeLit(CardType::Creature)),
                Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::It)))),
            )),
            Box::new(Expr::Ge(
                Box::new(Expr::Power(Box::new(Expr::Ctx(Ctx::It)))),
                Box::new(Expr::Num(3)),
            )),
        );
        assert_eq!(reads_of(&e), HashSet::from([Axis::Type, Axis::PT]));
    }

    #[test]
    fn all_objects_reads_zone_and_filter_axes() {
        // "Count creatures with flying on the battlefield" — Zone + Type + Abilities.
        let filter = Expr::And(
            Box::new(Expr::Contains(
                Box::new(Expr::TypeLit(CardType::Creature)),
                Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::Var("c"))))),
            )),
            Box::new(Expr::Contains(
                Box::new(Expr::KeywordLit(Keyword::Flying)),
                Box::new(Expr::Keywords(Box::new(Expr::Ctx(Ctx::Var("c"))))),
            )),
        );
        let e = Expr::Count(Box::new(Expr::AllObjects {
            zone: ZoneSel::Global(ZoneKindSel::Battlefield),
            bind: "c",
            filter: Box::new(filter),
        }));
        assert_eq!(
            reads_of(&e),
            HashSet::from([Axis::Zone, Axis::Type, Axis::Abilities]),
        );
    }

    #[test]
    fn dedup_removes_repeated_axes() {
        // Two power reads should collapse to one PT entry.
        let e = Expr::Add(
            Box::new(Expr::Power(Box::new(Expr::Ctx(Ctx::Var("a"))))),
            Box::new(Expr::Power(Box::new(Expr::Ctx(Ctx::Var("b"))))),
        );
        let d = deps_of(&e);
        assert_eq!(d.reads, vec![Axis::PT]);
    }

    // ── writes_of for each CEMod variant ────────────────────────────────────

    #[test]
    fn pt_writers() {
        for m in [
            CEMod::PumpPT(Expr::Num(1), Expr::Num(1)),
            CEMod::SetPT(Expr::Num(2), Expr::Num(2)),
            CEMod::SetPower(Expr::Num(0)),
            CEMod::SetToughness(Expr::Num(0)),
        ] {
            assert_eq!(writes_of(&m), vec![Axis::PT]);
        }
    }

    #[test]
    fn type_writers() {
        for m in [
            CEMod::OverrideTypes(vec![CardType::Creature]),
            CEMod::AddType(CardType::Artifact),
            CEMod::AddSubtype("Bear".into()),
            CEMod::RemoveSubtype("Bear".into()),
        ] {
            assert_eq!(writes_of(&m), vec![Axis::Type]);
        }
    }

    #[test]
    fn color_writers() {
        assert_eq!(writes_of(&CEMod::SetColors(vec![Color::Red])), vec![Axis::Color]);
        assert_eq!(writes_of(&CEMod::AddColor(Expr::ColorLit(Color::Blue))), vec![Axis::Color]);
    }

    #[test]
    fn abilities_writers() {
        for m in [
            CEMod::AddKeyword(Keyword::Flying),
            CEMod::RemoveKeyword(Keyword::Flying),
            CEMod::CantAttack,
            CEMod::CantBlock,
            CEMod::SetProtection(Expr::ColorLit(Color::White)),
        ] {
            assert_eq!(writes_of(&m), vec![Axis::Abilities]);
        }
    }

    #[test]
    fn copy_writer_covers_all_characteristic_layers() {
        // Layer 1 copy rewrites everything downstream — any characteristic
        // reader must order after copy.
        let w: HashSet<Axis> = writes_of(&CEMod::CopyOf(Expr::Ctx(Ctx::Source)))
            .into_iter()
            .collect();
        assert!(w.contains(&Axis::Copy));
        assert!(w.contains(&Axis::Type));
        assert!(w.contains(&Axis::Color));
        assert!(w.contains(&Axis::Abilities));
        assert!(w.contains(&Axis::PT));
    }

    #[test]
    fn rule_mod_writers() {
        for m in [
            CEMod::AllowLoss(Expr::Bool(true)),
            CEMod::MaxHandSize(Expr::Num(7)),
            CEMod::ExtraLandDrops(Expr::Num(1)),
            CEMod::SkipStep(crate::StepKind::Untap),
        ] {
            assert_eq!(writes_of(&m), vec![Axis::RuleMod]);
        }
    }

    #[test]
    fn cast_permission_writers() {
        for m in [
            CEMod::CastableFrom(ZoneKindSel::Graveyard),
            CEMod::AltCost(CostSpec::Free),
            CEMod::AnyColorMana,
            CEMod::GrantFlash,
            CEMod::OnResolveExile,
        ] {
            assert_eq!(writes_of(&m), vec![Axis::CastPermission]);
        }
    }

    #[test]
    fn cost_mod_writers() {
        for m in [
            CEMod::CastingCostPlus(Expr::Num(2)),
            CEMod::SpellsCostMore {
                filter: Filter(Expr::Bool(true)),
                amount: Expr::Num(1),
            },
            CEMod::SpellsCostLess {
                filter: Filter(Expr::Bool(true)),
                amount: Expr::Num(1),
            },
        ] {
            assert_eq!(writes_of(&m), vec![Axis::CostMod]);
        }
    }

    // ── End-to-end dep edge ─────────────────────────────────────────────────

    #[test]
    fn ce_edge_pump_then_read_power() {
        // A CE pumps P/T; another reads power. There must be a PT edge.
        let writer = CEMod::PumpPT(Expr::Num(1), Expr::Num(1));
        let reader = Expr::Power(Box::new(Expr::Ctx(Ctx::It)));
        let w: HashSet<Axis> = writes_of(&writer).into_iter().collect();
        let r: HashSet<Axis> = deps_of(&reader).reads.into_iter().collect();
        assert!(
            !w.is_disjoint(&r),
            "expected overlap between writer {:?} and reader {:?}", w, r
        );
    }

    #[test]
    fn ce_edge_copy_then_read_type() {
        // Copy writes to Type; a reader of Types must order after copy.
        let writer = CEMod::CopyOf(Expr::Ctx(Ctx::Source));
        let reader = Expr::Types(Box::new(Expr::Ctx(Ctx::It)));
        let w: HashSet<Axis> = writes_of(&writer).into_iter().collect();
        let r: HashSet<Axis> = deps_of(&reader).reads.into_iter().collect();
        assert!(w.contains(&Axis::Type));
        assert!(!w.is_disjoint(&r));
    }

    #[test]
    fn ce_edge_no_overlap_is_independent() {
        // Life reader is independent of a PT writer — no ordering edge.
        let writer = CEMod::PumpPT(Expr::Num(1), Expr::Num(1));
        let reader = Expr::Life(Box::new(Expr::Ctx(Ctx::Controller)));
        let w: HashSet<Axis> = writes_of(&writer).into_iter().collect();
        let r: HashSet<Axis> = deps_of(&reader).reads.into_iter().collect();
        assert!(
            w.is_disjoint(&r),
            "expected no overlap; got w={:?} r={:?}", w, r
        );
    }

    // Silence unused import warnings for the supertype import (kept for parity
    // with sibling modules using the same import pattern).
    #[allow(dead_code)]
    fn _touch(_: Supertype) {}
}

// ── Parity tests: IR filter vs closure predicate ────────────────────────────
//
// For each predicate in `predicates.rs`, build an equivalent IR Filter and
// verify they agree on a small fixture. Every new predicate variant added to
// the closure API should ship with a matching IR case here.

mod parity {
    use super::*;
    use crate::catalog::{ArtifactData, CreatureData, LandData, LandTypes, BasicLandType};
    use crate::{
        CardDef, CardKind, CardLayout, CardType, Color,
        CounterType, GameObject, Keyword, ObjId, PlayerId, PlayerState, SimState,
        Supertype,
    };
    use std::collections::HashMap;

    fn make_empty_state() -> SimState {
        let us = PlayerState::new("us_deck");
        let opp = PlayerState::new("opp_deck");
        SimState::new(us, opp)
    }

    /// Insert an object with a materialized def — query-side needs `def_of` to work.
    fn insert_with_def(state: &mut SimState, owner: PlayerId, def: CardDef) -> ObjId {
        let id = state.alloc_id();
        state.objects.insert(
            id,
            GameObject {
            id,
            catalog_key: def.name.clone(),
            owner,
            controller: owner,
            is_token: false,
            materialized: Some(def.clone()),
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: crate::ObjectRole::Battlefield(crate::BattlefieldState::new()),
        },
        );
        state.catalog.entry(def.name.clone()).or_insert(def);
        id
    }

    fn make_creature(name: &str, mana: &str, colors: Vec<Color>, power: i32, toughness: i32,
                     legendary: bool, keywords: &[Keyword], subtypes: &[&str]) -> CardDef {
        let mut c = CreatureData::new(mana, power, toughness);
        c.keywords = crate::catalog::Keywords::from_slice(keywords);
        c.creature_subtypes = subtypes.iter().map(|s| (*s).into()).collect();
        c.legendary = legendary;
        let supers = if legendary { vec![Supertype::Legendary] } else { vec![] };
        CardDef::new(name, CardKind::Creature(c), colors, None, supers,
                     CardLayout::Normal, None, vec![], vec![], vec![], vec![])
    }

    fn make_island(name: &str) -> CardDef {
        let land = LandData {
            land_types: LandTypes::from_types(&[BasicLandType::Island]),
            nonbasic_subtypes: vec![],
            abilities: vec![],
            mana_abilities: vec![],
        };
        CardDef::new(name, CardKind::Land(land), vec![], None, vec![Supertype::Basic],
                     CardLayout::Normal, None, vec![], vec![], vec![], vec![])
    }

    fn make_equipment(name: &str, mana: &str) -> CardDef {
        let a = ArtifactData {
            mana_cost: mana.into(),
            abilities: vec![],
            mana_abilities: vec![],
            subtypes: vec!["Equipment".into()],
        };
        CardDef::new(name, CardKind::Artifact(a), vec![], None, vec![],
                     CardLayout::Normal, None, vec![], vec![], vec![], vec![])
    }

    /// Build the IR equivalent of `pred_type_eq(t)`.
    fn ir_type_is(t: CardType) -> Filter {
        Filter(Expr::Contains(
            Box::new(Expr::TypeLit(t)),
            Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::It)))),
        ))
    }

    fn ir_has_supertype(s: Supertype) -> Filter {
        Filter(Expr::Contains(
            Box::new(Expr::SupertypeLit(s)),
            Box::new(Expr::Supertypes(Box::new(Expr::Ctx(Ctx::It)))),
        ))
    }

    fn ir_has_color(c: Color) -> Filter {
        Filter(Expr::Contains(
            Box::new(Expr::ColorLit(c)),
            Box::new(Expr::Colors(Box::new(Expr::Ctx(Ctx::It)))),
        ))
    }

    fn ir_mana_value_le(n: i32) -> Filter {
        Filter(Expr::Le(
            Box::new(Expr::Mv(Box::new(Expr::Ctx(Ctx::It)))),
            Box::new(Expr::Num(n as i64)),
        ))
    }

    fn ir_mana_value_eq(n: i32) -> Filter {
        Filter(Expr::Eq(
            Box::new(Expr::Mv(Box::new(Expr::Ctx(Ctx::It)))),
            Box::new(Expr::Num(n as i64)),
        ))
    }

    fn ir_toughness_le(n: i32) -> Filter {
        Filter(Expr::And(
            Box::new(ir_type_is(CardType::Creature).0),
            Box::new(Expr::Le(
                Box::new(Expr::Toughness(Box::new(Expr::Ctx(Ctx::It)))),
                Box::new(Expr::Num(n as i64)),
            )),
        ))
    }

    fn ir_has_keyword(kw: Keyword) -> Filter {
        Filter(Expr::Contains(
            Box::new(Expr::KeywordLit(kw)),
            Box::new(Expr::Keywords(Box::new(Expr::Ctx(Ctx::It)))),
        ))
    }

    fn ir_has_subtype(st: &str) -> Filter {
        Filter(Expr::Contains(
            Box::new(Expr::SubtypeLit(st.into())),
            Box::new(Expr::Subtypes(Box::new(Expr::Ctx(Ctx::It)))),
        ))
    }

    fn ir_land_subtype(st: &str) -> Filter {
        // "is a land AND has the land subtype"
        Filter(Expr::And(
            Box::new(ir_type_is(CardType::Land).0),
            Box::new(Expr::Contains(
                Box::new(Expr::SubtypeLit(st.into())),
                Box::new(Expr::Subtypes(Box::new(Expr::Ctx(Ctx::It)))),
            )),
        ))
    }

    fn ir_has_counter(ct: CounterType) -> Filter {
        Filter(Expr::Gt(
            Box::new(Expr::CountersOn(Box::new(Expr::Ctx(Ctx::It)), ct)),
            Box::new(Expr::Num(0)),
        ))
    }


    #[test]
    fn scoped_binder_enumeration() {
        // "All creatures on the battlefield" — AllObjects walks both players' battlefields.
        let mut s = make_empty_state();
        let _ = insert_with_def(&mut s, PlayerId::Us,
            make_creature("A", "{G}", vec![Color::Green], 1, 1, false, &[], &[]));
        let _ = insert_with_def(&mut s, PlayerId::Opp,
            make_creature("B", "{G}", vec![Color::Green], 1, 1, false, &[], &[]));
        let _ = insert_with_def(&mut s, PlayerId::Us, make_island("Island"));

        let all_creatures = Expr::AllObjects {
            zone: ZoneSel::Global(ZoneKindSel::Battlefield),
            bind: "c",
            filter: Box::new(ir_type_is(CardType::Creature).0),
        };
        let env = BindEnv::new();
        match eval_expr(&all_creatures, &s, &env) {
            Value::ObjSet(v) => assert_eq!(v.len(), 2, "expected 2 creatures, got {:?}", v),
            other => panic!("expected ObjSet, got {:?}", other),
        }

        // Count(AllObjects(creatures)) — should be 2
        let count_expr = Expr::Count(Box::new(all_creatures));
        let v = eval_expr(&count_expr, &s, &env);
        assert!(matches!(v, Value::Num(2)), "expected 2, got {:?}", v);
    }
}

// ── Parity tests: IR Action vs closure Effect ───────────────────────────────
//
// For each primitive in `effects.rs`, run the closure path and the IR path on
// independent clones of the same SimState; assert the game-relevant state
// (life, hand, zones, counters, battlefield damage) converges. Logs diverge by
// design — IR does not emit `state.log` yet — so we compare structural fields.

mod execute_parity {
    use super::*;
    use crate::catalog::{ArtifactData, CreatureData, LandData, LandTypes, BasicLandType};
    use crate::{
        effects as E, CardDef, CardKind, CardLayout, Zone, Color, CounterType,
        GameObject, Keyword, ObjId, PlayerId, PlayerState, SimState, Supertype,
    };
    use std::collections::HashMap;

    fn make_state() -> SimState {
        let us = PlayerState::new("us");
        let opp = PlayerState::new("opp");
        SimState::new(us, opp)
    }

    fn make_creature(name: &str, mana: &str, power: i32, toughness: i32) -> CardDef {
        let c = CreatureData::new(mana, power, toughness);
        CardDef::new(
            name,
            CardKind::Creature(c),
            vec![Color::Green],
            None,
            vec![],
            CardLayout::Normal,
            None,
            vec![],
            vec![],
            vec![],
            vec![],
        )
    }

    fn make_artifact(name: &str) -> CardDef {
        let a = ArtifactData {
            mana_cost: "{2}".into(),
            abilities: vec![],
            mana_abilities: vec![],
            subtypes: vec![],
        };
        CardDef::new(
            name,
            CardKind::Artifact(a),
            vec![],
            None,
            vec![],
            CardLayout::Normal,
            None,
            vec![],
            vec![],
            vec![],
            vec![],
        )
    }

    fn make_land(name: &str) -> CardDef {
        let land = LandData {
            land_types: LandTypes::from_types(&[BasicLandType::Island]),
            nonbasic_subtypes: vec![],
            abilities: vec![],
            mana_abilities: vec![],
        };
        CardDef::new(
            name,
            CardKind::Land(land),
            vec![],
            None,
            vec![Supertype::Basic],
            CardLayout::Normal,
            None,
            vec![],
            vec![],
            vec![],
            vec![],
        )
    }

    /// Insert an object without a zone yet; caller sets the zone via `set_card_zone`.
    fn insert_obj(state: &mut SimState, owner: PlayerId, def: CardDef) -> ObjId {
        let id = state.alloc_id();
        state.objects.insert(
            id,
            GameObject {
            id,
            catalog_key: def.name.clone(),
            owner,
            controller: owner,
            is_token: false,
            materialized: Some(def.clone()),
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: crate::ObjectRole::Library,
        },
        );
        state.catalog.entry(def.name.clone()).or_insert(def);
        // default: freshly-allocated objects land in library_order for the player
        state.player_mut(owner).library_order.push_back(id);
        id
    }

    fn put_on_bf(state: &mut SimState, id: ObjId) {
        state.set_card_zone(id, Zone::Battlefield);
        if let Some(obj) = state.objects.get_mut(&id) {
            obj.role = crate::ObjectRole::Battlefield(crate::BattlefieldState {
                tapped: false,
                damage: 0,
                entered_this_turn: false,
                counters: 0,
                power_mod: 0,
                toughness_mod: 0,
                loyalty: 0,
                pw_activated_this_turn: false,
                attacking: false,
                unblocked: false,
                attack_target: None,
                active_face: 0,
                etb_choice: None,
                attached_to: None,
                stun_counters: 0,
            });
        }
    }

    // ── Draw ─────────────────────────────────────────────────────────────────
    // SimState is not Clone, so each parity test defines a `setup` closure
    // producing a fresh state with deterministic object IDs (alloc_id just
    // increments a counter), then runs the closure path and the IR path on
    // two independently-constructed states.

    #[test]
    fn draw_parity() {
        let setup = || {
            let mut s = make_state();
            insert_obj(&mut s, PlayerId::Us, make_creature("A", "{G}", 1, 1));
            insert_obj(&mut s, PlayerId::Us, make_creature("B", "{G}", 1, 1));
            insert_obj(&mut s, PlayerId::Us, make_creature("C", "{G}", 1, 1));
            s
        };

        let mut closure_state = setup();
        let mut ir_state = setup();

        E::eff_draw(PlayerId::Us, 2).call(&mut closure_state, 0, &[]);
        execute(
            &Action::Draw { who: Who::You, n: Expr::Num(2) },
            &mut ir_state,
            &BindEnv::new().with_controller(PlayerId::Us),
        );

        let closure_hand: std::collections::HashSet<ObjId> =
            closure_state.hand_of(PlayerId::Us).map(|o| o.id).collect();
        let ir_hand: std::collections::HashSet<ObjId> =
            ir_state.hand_of(PlayerId::Us).map(|o| o.id).collect();
        assert_eq!(closure_hand, ir_hand, "hand diverged after Draw");

        let closure_lib: Vec<ObjId> =
            closure_state.player(PlayerId::Us).library_order.iter().copied().collect();
        let ir_lib: Vec<ObjId> =
            ir_state.player(PlayerId::Us).library_order.iter().copied().collect();
        assert_eq!(closure_lib, ir_lib, "library diverged after Draw");
    }

    // ── PayLife / GainLife ───────────────────────────────────────────────────

    #[test]
    fn pay_life_parity() {
        let setup = || make_state();

        let mut closure_state = setup();
        let mut ir_state = setup();
        let start = closure_state.life_of(PlayerId::Us);

        E::eff_life_loss(PlayerId::Us, 3).call(&mut closure_state, 0, &[]);
        execute(
            &Action::PayLife { who: Who::You, amount: Expr::Num(3) },
            &mut ir_state,
            &BindEnv::new().with_controller(PlayerId::Us),
        );

        assert_eq!(closure_state.life_of(PlayerId::Us), start - 3);
        assert_eq!(ir_state.life_of(PlayerId::Us), start - 3);
    }

    // ── DealDamage to a player ───────────────────────────────────────────────

    #[test]
    fn damage_player_parity() {
        let setup = || make_state();

        let mut closure_state = setup();
        let mut ir_state = setup();
        let opp_id = closure_state.player(PlayerId::Opp).id;
        let start = closure_state.life_of(PlayerId::Opp);

        // Closure path: zero source id avoids protection checks and models
        // "generic damage source, player target".
        E::eff_damage_target(PlayerId::Us, 3, ObjId::UNSET).call(
            &mut closure_state,
            0,
            &[opp_id],
        );
        execute(
            &Action::DealDamage {
                source: Expr::Ctx(Ctx::Source),
                target: Expr::Ctx(Ctx::Var("tgt")),
                amount: Expr::Num(3),
            },
            &mut ir_state,
            &BindEnv::new()
                .with_controller(PlayerId::Us)
                .with_var("tgt", Value::Player(PlayerId::Opp)),
        );

        assert_eq!(closure_state.life_of(PlayerId::Opp), start - 3);
        assert_eq!(ir_state.life_of(PlayerId::Opp), start - 3);
    }

    // ── DealDamage to a permanent ────────────────────────────────────────────

    #[test]
    fn damage_permanent_parity() {
        let setup = || {
            let mut s = make_state();
            let cid = insert_obj(&mut s, PlayerId::Opp, make_creature("Bear", "{1}{G}", 2, 3));
            put_on_bf(&mut s, cid);
            (s, cid)
        };

        let (mut closure_state, c_id) = setup();
        let (mut ir_state, i_id) = setup();
        assert_eq!(c_id, i_id, "setup must be deterministic across runs");

        E::eff_damage_target(PlayerId::Us, 2, ObjId::UNSET).call(
            &mut closure_state,
            0,
            &[c_id],
        );
        execute(
            &Action::DealDamage {
                source: Expr::Ctx(Ctx::Source),
                target: Expr::Ctx(Ctx::Var("tgt")),
                amount: Expr::Num(2),
            },
            &mut ir_state,
            &BindEnv::new()
                .with_controller(PlayerId::Us)
                .with_var("tgt", Value::Obj(i_id)),
        );

        let c_dmg = closure_state.objects.get(&c_id).and_then(|o| o.bf()).map(|bf| bf.damage);
        let i_dmg = ir_state.objects.get(&i_id).and_then(|o| o.bf()).map(|bf| bf.damage);
        assert_eq!(c_dmg, Some(2));
        assert_eq!(i_dmg, Some(2));
    }

    // ── Destroy ──────────────────────────────────────────────────────────────

    #[test]
    fn destroy_parity() {
        let setup = || {
            let mut s = make_state();
            let cid = insert_obj(&mut s, PlayerId::Us, make_creature("Bear", "{1}{G}", 2, 2));
            put_on_bf(&mut s, cid);
            (s, cid)
        };

        let (mut closure_state, c_id) = setup();
        let (mut ir_state, i_id) = setup();

        E::eff_destroy_target(PlayerId::Us).call(&mut closure_state, 0, &[c_id]);
        execute(
            &Action::Destroy { target: Expr::Ctx(Ctx::Var("t")) },
            &mut ir_state,
            &BindEnv::new()
                .with_controller(PlayerId::Us)
                .with_var("t", Value::Obj(i_id)),
        );

        let c_zone = closure_state.objects.get(&c_id).and_then(|o| o.zone());
        let i_zone = ir_state.objects.get(&i_id).and_then(|o| o.zone());
        assert!(matches!(c_zone, Some(Zone::Graveyard)));
        assert_eq!(c_zone, i_zone);
    }

    // ── Exile ────────────────────────────────────────────────────────────────

    #[test]
    fn exile_parity() {
        let setup = || {
            let mut s = make_state();
            let cid = insert_obj(&mut s, PlayerId::Us, make_artifact("Relic"));
            put_on_bf(&mut s, cid);
            (s, cid)
        };

        let (mut closure_state, c_id) = setup();
        let (mut ir_state, i_id) = setup();

        E::eff_exile_target(PlayerId::Us).call(&mut closure_state, 0, &[c_id]);
        execute(
            &Action::Exile { target: Expr::Ctx(Ctx::Var("t")), bind_as: None },
            &mut ir_state,
            &BindEnv::new()
                .with_controller(PlayerId::Us)
                .with_var("t", Value::Obj(i_id)),
        );

        let c_zone = closure_state.objects.get(&c_id).and_then(|o| o.zone());
        let i_zone = ir_state.objects.get(&i_id).and_then(|o| o.zone());
        assert!(matches!(c_zone, Some(Zone::Exile { .. })));
        assert!(matches!(i_zone, Some(Zone::Exile { .. })));
    }

    // ── Return (bounce to hand) ─────────────────────────────────────────────

    #[test]
    fn bounce_parity() {
        let setup = || {
            let mut s = make_state();
            let cid = insert_obj(&mut s, PlayerId::Us, make_creature("Bear", "{G}", 2, 2));
            put_on_bf(&mut s, cid);
            (s, cid)
        };

        let (mut closure_state, c_id) = setup();
        let (mut ir_state, i_id) = setup();

        E::eff_bounce_target(PlayerId::Us).call(&mut closure_state, 0, &[c_id]);
        execute(
            &Action::Return {
                what: Expr::Ctx(Ctx::Var("t")),
                to: ZoneKindSel::Hand,
                bind_as: None,
            },
            &mut ir_state,
            &BindEnv::new()
                .with_controller(PlayerId::Us)
                .with_var("t", Value::Obj(i_id)),
        );

        let c_zone = closure_state.objects.get(&c_id).and_then(|o| o.zone());
        let i_zone = ir_state.objects.get(&i_id).and_then(|o| o.zone());
        assert!(matches!(c_zone, Some(Zone::Hand { .. })));
        assert!(matches!(i_zone, Some(Zone::Hand { .. })));
    }

    // ── Counters ─────────────────────────────────────────────────────────────
    //
    // `effects.rs` has no counter primitive; the baseline is a direct
    // `obj.counters.entry(...).or_insert(0) += n`, matching how card code
    // manipulates counters today.

    #[test]
    fn put_counters_parity() {
        let setup = || {
            let mut s = make_state();
            let cid = insert_obj(&mut s, PlayerId::Us, make_creature("Bear", "{G}", 2, 2));
            put_on_bf(&mut s, cid);
            (s, cid)
        };

        let (mut baseline, b_id) = setup();
        let (mut ir_state, i_id) = setup();

        *baseline.objects.get_mut(&b_id).unwrap().counters.entry(CounterType::Void).or_insert(0) += 2;
        execute(
            &Action::PutCounters {
                on: Expr::Ctx(Ctx::Var("t")),
                kind: CounterType::Void,
                n: Expr::Num(2),
            },
            &mut ir_state,
            &BindEnv::new()
                .with_controller(PlayerId::Us)
                .with_var("t", Value::Obj(i_id)),
        );

        let b = baseline.objects.get(&b_id).unwrap().counters.get(&CounterType::Void).copied();
        let i = ir_state.objects.get(&i_id).unwrap().counters.get(&CounterType::Void).copied();
        assert_eq!(b, Some(2));
        assert_eq!(i, Some(2));
    }

    #[test]
    fn remove_counters_saturates_at_zero() {
        let mut s = make_state();
        let cid = insert_obj(&mut s, PlayerId::Us, make_creature("Bear", "{G}", 2, 2));
        put_on_bf(&mut s, cid);
        s.objects.get_mut(&cid).unwrap().counters.insert(CounterType::Void, 1);

        execute(
            &Action::RemoveCounters {
                from: Expr::Ctx(Ctx::Var("t")),
                kind: CounterType::Void,
                n: Expr::Num(5),
            },
            &mut s,
            &BindEnv::new()
                .with_controller(PlayerId::Us)
                .with_var("t", Value::Obj(cid)),
        );

        let n = s.objects.get(&cid).unwrap().counters.get(&CounterType::Void).copied();
        assert_eq!(n, Some(0), "remove should saturate at 0, got {:?}", n);
    }

    // ── Mill ─────────────────────────────────────────────────────────────────
    //
    // No closure equivalent; baseline is manual change_zone calls matching
    // what `execute` should do.

    #[test]
    fn mill_parity_against_manual_baseline() {
        let setup = || {
            let mut s = make_state();
            let a = insert_obj(&mut s, PlayerId::Us, make_creature("A", "{G}", 1, 1));
            let b = insert_obj(&mut s, PlayerId::Us, make_creature("B", "{G}", 1, 1));
            let _c = insert_obj(&mut s, PlayerId::Us, make_creature("C", "{G}", 1, 1));
            (s, a, b)
        };

        let (mut baseline, ba, bb) = setup();
        let (mut ir_state, _ia, _ib) = setup();

        let top: Vec<ObjId> = baseline.library_of(PlayerId::Us).take(2).map(|o| o.id).collect();
        assert_eq!(top, vec![ba, bb]);
        for id in top {
            crate::change_zone(id, crate::ZoneId::Graveyard, &mut baseline, 0, PlayerId::Us);
        }

        execute(
            &Action::Mill { who: Who::You, count: Expr::Num(2) },
            &mut ir_state,
            &BindEnv::new().with_controller(PlayerId::Us),
        );

        let baseline_gy: std::collections::HashSet<ObjId> =
            baseline.graveyard_of(PlayerId::Us).map(|o| o.id).collect();
        let ir_gy: std::collections::HashSet<ObjId> =
            ir_state.graveyard_of(PlayerId::Us).map(|o| o.id).collect();
        assert_eq!(baseline_gy, ir_gy);

        let baseline_lib: Vec<ObjId> =
            baseline.player(PlayerId::Us).library_order.iter().copied().collect();
        let ir_lib: Vec<ObjId> =
            ir_state.player(PlayerId::Us).library_order.iter().copied().collect();
        assert_eq!(baseline_lib, ir_lib);
    }

    #[test]
    fn attach_sets_attached_to_and_detaches_on_ltb() {
        // Action::Attach is type-agnostic — it points attached_to at whatever is on the
        // battlefield. Two permanents stand in for attachment/host here.
        let mut s = make_state();
        let equip = insert_obj(&mut s, PlayerId::Us, make_creature("Sword", "{1}", 0, 0));
        let host = insert_obj(&mut s, PlayerId::Us, make_creature("Bear", "{G}", 2, 2));
        put_on_bf(&mut s, equip);
        put_on_bf(&mut s, host);

        execute(
            &Action::Attach { what: Expr::ObjLit(equip), to: Expr::ObjLit(host) },
            &mut s,
            &BindEnv::new().with_controller(PlayerId::Us),
        );
        assert_eq!(s.objects[&equip].bf().unwrap().attached_to, Some(host),
            "Attach sets attached_to");

        // Host leaves the battlefield → the attachment detaches (CR 704.5q).
        crate::change_zone(host, crate::ZoneId::Graveyard, &mut s, 0, PlayerId::Us);
        assert_eq!(s.objects[&equip].bf().unwrap().attached_to, None,
            "attachment detaches when its host leaves");
    }

    // ── Control flow ─────────────────────────────────────────────────────────

    #[test]
    fn sequence_runs_in_order() {
        let mut s = make_state();
        let start = s.life_of(PlayerId::Us);
        execute(
            &Action::Sequence(vec![
                Action::PayLife { who: Who::You, amount: Expr::Num(2) },
                Action::PayLife { who: Who::You, amount: Expr::Num(3) },
            ]),
            &mut s,
            &BindEnv::new().with_controller(PlayerId::Us),
        );
        assert_eq!(s.life_of(PlayerId::Us), start - 5);
    }

    #[test]
    fn if_then_branches_correctly() {
        // true branch
        let mut s = make_state();
        let start = s.life_of(PlayerId::Us);
        execute(
            &Action::IfThen {
                cond: Expr::Bool(true),
                then: Box::new(Action::PayLife { who: Who::You, amount: Expr::Num(1) }),
                else_: Some(Box::new(Action::PayLife { who: Who::You, amount: Expr::Num(10) })),
            },
            &mut s,
            &BindEnv::new().with_controller(PlayerId::Us),
        );
        assert_eq!(s.life_of(PlayerId::Us), start - 1);

        // false branch
        let mut s = make_state();
        let start = s.life_of(PlayerId::Us);
        execute(
            &Action::IfThen {
                cond: Expr::Bool(false),
                then: Box::new(Action::PayLife { who: Who::You, amount: Expr::Num(1) }),
                else_: Some(Box::new(Action::PayLife { who: Who::You, amount: Expr::Num(10) })),
            },
            &mut s,
            &BindEnv::new().with_controller(PlayerId::Us),
        );
        assert_eq!(s.life_of(PlayerId::Us), start - 10);
    }

    #[test]
    fn for_each_damages_all_creatures() {
        // Deal 1 damage to each creature on the battlefield.
        let mut s = make_state();
        let a = insert_obj(&mut s, PlayerId::Us, make_creature("A", "{G}", 1, 2));
        put_on_bf(&mut s, a);
        let b = insert_obj(&mut s, PlayerId::Opp, make_creature("B", "{G}", 1, 2));
        put_on_bf(&mut s, b);
        let land = insert_obj(&mut s, PlayerId::Us, make_land("Island"));
        put_on_bf(&mut s, land);

        execute(
            &Action::ForEach {
                over: Expr::AllObjects {
                    zone: ZoneSel::Global(ZoneKindSel::Battlefield),
                    bind: "c",
                    filter: Box::new(Expr::Contains(
                        Box::new(Expr::TypeLit(crate::CardType::Creature)),
                        Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::Var("c"))))),
                    )),
                },
                bind: "c",
                body: Box::new(Action::DealDamage {
                    source: Expr::Ctx(Ctx::Source),
                    target: Expr::Ctx(Ctx::Var("c")),
                    amount: Expr::Num(1),
                }),
            },
            &mut s,
            &BindEnv::new().with_controller(PlayerId::Us),
        );

        assert_eq!(s.objects.get(&a).unwrap().bf().unwrap().damage, 1);
        assert_eq!(s.objects.get(&b).unwrap().bf().unwrap().damage, 1);
        assert_eq!(s.objects.get(&land).unwrap().bf().unwrap().damage, 0);
    }

    // ── Agency actions ───────────────────────────────────────────────────────

    use crate::ir::expr::Filter;
    use rand::SeedableRng;

    fn seeded_rng(seed: u64) -> Box<dyn rand::RngCore + Send> {
        Box::new(rand::rngs::StdRng::seed_from_u64(seed))
    }

    #[test]
    fn sacrifice_parity() {
        // Strategy picks the candidate with the smallest ObjId — deterministic
        // regardless of HashMap iteration order in `permanents_of`.
        let setup = || {
            let mut s = make_state();
            let a = insert_obj(&mut s, PlayerId::Us, make_creature("A", "{G}", 1, 1));
            let b = insert_obj(&mut s, PlayerId::Us, make_creature("B", "{G}", 2, 2));
            put_on_bf(&mut s, a);
            put_on_bf(&mut s, b);
            s.set_strategy(PlayerId::Us,
                Box::new(crate::strategy::TestStrategy::new(PlayerId::Us).sacrifice_min_id()));
            (s, a, b)
        };

        let (mut closure_state, a1, _b1) = setup();
        let (mut ir_state, a2, _b2) = setup();

        E::eff_sacrifice(
            PlayerId::Us,
            crate::effects::Who::Actor,
            Filter(Expr::Bool(true)),
        )
        .call(&mut closure_state, 0, &[]);

        let creature_filter = Filter(Expr::Contains(
            Box::new(Expr::TypeLit(crate::CardType::Creature)),
            Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::It)))),
        ));
        execute(
            &Action::Sacrifice {
                who: Who::You,
                filter: creature_filter,
                count: Expr::Num(1),
                bind_as: None,
            },
            &mut ir_state,
            &BindEnv::new().with_controller(PlayerId::Us),
        );

        // Both paths should have sacrificed the smallest-id candidate (a).
        assert!(matches!(
            closure_state.objects.get(&a1).unwrap().zone(), Some(Zone::Graveyard)
        ));
        assert!(matches!(
            ir_state.objects.get(&a2).unwrap().zone(), Some(Zone::Graveyard)
        ));
    }

    #[test]
    fn surveil_parity() {
        // surveil_choice always returns true → top card always goes to graveyard.
        let setup = || {
            let mut s = make_state();
            insert_obj(&mut s, PlayerId::Us, make_creature("A", "{G}", 1, 1));
            insert_obj(&mut s, PlayerId::Us, make_creature("B", "{G}", 1, 1));
            insert_obj(&mut s, PlayerId::Us, make_creature("C", "{G}", 1, 1));
            s.set_strategy(PlayerId::Us,
                Box::new(crate::strategy::TestStrategy::new(PlayerId::Us).surveil(true)));
            s
        };

        let mut closure_state = setup();
        let mut ir_state = setup();

        E::eff_surveil(PlayerId::Us, 2).call(&mut closure_state, 0, &[]);
        execute(
            &Action::Surveil { who: Who::You, n: Expr::Num(2) },
            &mut ir_state,
            &BindEnv::new().with_controller(PlayerId::Us),
        );

        let closure_gy: usize = closure_state.graveyard_of(PlayerId::Us).count();
        let ir_gy: usize = ir_state.graveyard_of(PlayerId::Us).count();
        assert_eq!(closure_gy, ir_gy, "graveyard size diverged after Surveil");
        assert_eq!(closure_gy, 2);
    }

    #[test]
    fn scry_parity() {
        // evaluate_card: card at index 0 scores 0.9 (keep), others score 0.1 (bottom).
        let setup = || {
            let mut s = make_state();
            let a = insert_obj(&mut s, PlayerId::Us, make_creature("A", "{G}", 1, 1));
            let b = insert_obj(&mut s, PlayerId::Us, make_creature("B", "{G}", 1, 1));
            let c = insert_obj(&mut s, PlayerId::Us, make_creature("C", "{G}", 1, 1));
            let keep = a; // captured into evaluator
            s.evaluate_card = std::sync::Arc::new(move |_who, id, _s| {
                if id == keep { 0.9 } else { 0.1 }
            });
            (s, a, b, c)
        };

        let (mut closure_state, _, _, _) = setup();
        let (mut ir_state, _, _, _) = setup();

        E::eff_scry(PlayerId::Us, 3).call(&mut closure_state, 0, &[]);
        execute(
            &Action::Scry { who: Who::You, n: Expr::Num(3) },
            &mut ir_state,
            &BindEnv::new().with_controller(PlayerId::Us),
        );

        let closure_lib: Vec<ObjId> =
            closure_state.player(PlayerId::Us).library_order.iter().copied().collect();
        let ir_lib: Vec<ObjId> =
            ir_state.player(PlayerId::Us).library_order.iter().copied().collect();
        assert_eq!(closure_lib, ir_lib, "library order diverged after Scry");
    }

    #[test]
    fn counter_parity() {
        // Put a spell on the stack via direct manipulation; compare counter paths.
        let setup = || {
            let mut s = make_state();
            let spell = insert_obj(&mut s, PlayerId::Opp, make_creature("X", "{2}", 2, 2));
            // Move onto stack.
            s.set_card_zone(spell, Zone::Stack);
            s.stack.push(spell);
            (s, spell)
        };

        let (mut closure_state, spell1) = setup();
        let (mut ir_state, spell2) = setup();

        crate::effects::counter_one(spell1, &mut closure_state, 0, PlayerId::Us);
        execute(
            &Action::Counter { target: Expr::Num(spell2.0 as i64) },
            &mut ir_state,
            &BindEnv::new().with_controller(PlayerId::Us),
        );
        // The above uses Num for target; but IR expects an Obj value. Rerun
        // with the right path: bind the spell into the env instead.
        let (mut ir_state, spell3) = setup();
        let env = BindEnv::new()
            .with_controller(PlayerId::Us)
            .with_var("tgt", crate::ir::expr::Value::Obj(spell3));
        execute(
            &Action::Counter { target: Expr::Ctx(Ctx::Var("tgt")) },
            &mut ir_state,
            &env,
        );

        // Closure path: stack empty, spell in graveyard.
        assert!(closure_state.stack.is_empty());
        assert!(matches!(
            closure_state.objects.get(&spell1).unwrap().zone(), Some(Zone::Graveyard)
        ));
        // IR path: same.
        assert!(ir_state.stack.is_empty());
        assert!(matches!(
            ir_state.objects.get(&spell3).unwrap().zone(), Some(Zone::Graveyard)
        ));
    }

    #[test]
    fn may_do_respects_strategy_yes() {
        let mut s = make_state();
        let start = s.life_of(PlayerId::Us);
        s.set_strategy(PlayerId::Us,
            Box::new(crate::strategy::TestStrategy::new(PlayerId::Us).mode(1)));
        execute(
            &Action::MayDo {
                who: Who::You,
                action: Box::new(Action::PayLife { who: Who::You, amount: Expr::Num(2) }),
            },
            &mut s,
            &BindEnv::new().with_controller(PlayerId::Us),
        );
        assert_eq!(s.life_of(PlayerId::Us), start - 2);
    }

    #[test]
    fn may_do_respects_strategy_no() {
        let mut s = make_state();
        let start = s.life_of(PlayerId::Us);
        // Default resolve_choice returns Mode(0) → "no"
        execute(
            &Action::MayDo {
                who: Who::You,
                action: Box::new(Action::PayLife { who: Who::You, amount: Expr::Num(2) }),
            },
            &mut s,
            &BindEnv::new().with_controller(PlayerId::Us),
        );
        assert_eq!(s.life_of(PlayerId::Us), start);
    }

    #[test]
    fn choose_picks_strategy_index() {
        let mut s = make_state();
        let start = s.life_of(PlayerId::Us);
        s.set_strategy(PlayerId::Us,
            Box::new(crate::strategy::TestStrategy::new(PlayerId::Us).mode(1)));
        execute(
            &Action::Choose {
                who: Who::You,
                prompt: "test",
                options: vec![
                    crate::ir::action::ChoiceOption {
                        label: "pay 1",
                        cost: None,
                        action: Box::new(Action::PayLife {
                            who: Who::You,
                            amount: Expr::Num(1),
                        }),
                    },
                    crate::ir::action::ChoiceOption {
                        label: "pay 5",
                        cost: None,
                        action: Box::new(Action::PayLife {
                            who: Who::You,
                            amount: Expr::Num(5),
                        }),
                    },
                ],
                bind_as: None,
            },
            &mut s,
            &BindEnv::new().with_controller(PlayerId::Us),
        );
        assert_eq!(s.life_of(PlayerId::Us), start - 5);
    }

    #[test]
    fn tap_and_untap_flip_battlefield_state() {
        let mut s = make_state();
        let id = insert_obj(&mut s, PlayerId::Us, make_creature("A", "{G}", 1, 1));
        put_on_bf(&mut s, id);
        let env = BindEnv::new()
            .with_controller(PlayerId::Us)
            .with_var("t", crate::ir::expr::Value::Obj(id));

        assert!(!s.permanent_bf(id).unwrap().tapped);
        execute(
            &Action::Tap { target: Expr::Ctx(Ctx::Var("t")) },
            &mut s,
            &env,
        );
        assert!(s.permanent_bf(id).unwrap().tapped, "Action::Tap sets tapped=true");
        execute(
            &Action::Untap { target: Expr::Ctx(Ctx::Var("t")) },
            &mut s,
            &env,
        );
        assert!(!s.permanent_bf(id).unwrap().tapped, "Action::Untap clears tapped");
    }

    #[test]
    fn move_changes_zone() {
        let mut s = make_state();
        let id = insert_obj(&mut s, PlayerId::Us, make_creature("A", "{G}", 1, 1));
        put_on_bf(&mut s, id);
        let env = BindEnv::new()
            .with_controller(PlayerId::Us)
            .with_var("t", crate::ir::expr::Value::Obj(id));

        execute(
            &Action::Move {
                what: Expr::Ctx(Ctx::Var("t")),
                to: ZoneKindSel::Hand,
                to_owner: None,
                bind_as: None,
            },
            &mut s,
            &env,
        );
        assert!(matches!(
            s.objects.get(&id).unwrap().zone(), Some(Zone::Hand { .. })
        ));
    }

    #[test]
    fn reveal_marks_hand_known() {
        let mut s = make_state();
        let id = insert_obj(&mut s, PlayerId::Us, make_creature("A", "{G}", 1, 1));
        s.set_card_zone(id, Zone::Hand { known: false });

        let env = BindEnv::new()
            .with_controller(PlayerId::Us)
            .with_var("t", crate::ir::expr::Value::Obj(id));

        execute(
            &Action::Reveal {
                who: Who::You,
                what: Expr::Ctx(Ctx::Var("t")),
            },
            &mut s,
            &env,
        );
        assert_eq!(
            s.objects.get(&id).unwrap().zone(), Some(Zone::Hand { known: true })
        );
    }

    #[test]
    fn discard_sends_random_hand_card_to_graveyard() {
        let mut s = make_state();
        s.rng = seeded_rng(42);
        let a = insert_obj(&mut s, PlayerId::Us, make_creature("A", "{G}", 1, 1));
        let b = insert_obj(&mut s, PlayerId::Us, make_creature("B", "{G}", 1, 1));
        s.set_card_zone(a, Zone::Hand { known: false });
        s.set_card_zone(b, Zone::Hand { known: false });

        execute(
            &Action::Discard {
                who: Who::You,
                count: Expr::Num(1),
                at_random: true,
                filter: None,
            },
            &mut s,
            &BindEnv::new().with_controller(PlayerId::Us),
        );

        // One of the two should now be in the graveyard.
        let gy: usize = s.graveyard_of(PlayerId::Us).count();
        assert_eq!(gy, 1);
        let hand: usize = s.hand_of(PlayerId::Us).count();
        assert_eq!(hand, 1);
    }

    #[test]
    fn search_picks_matching_card() {
        // Seed rng so the pick is reproducible; three candidates in library.
        let mut s = make_state();
        s.rng = seeded_rng(7);
        let a = insert_obj(&mut s, PlayerId::Us, make_creature("A", "{G}", 1, 1));
        let b = insert_obj(&mut s, PlayerId::Us, make_creature("B", "{G}", 1, 1));
        let land = insert_obj(&mut s, PlayerId::Us, make_land("Island"));

        // Search for a creature — should land in hand.
        let creature_filter = Filter(Expr::Contains(
            Box::new(Expr::TypeLit(crate::CardType::Creature)),
            Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::It)))),
        ));
        execute(
            &Action::Search {
                who: Who::You,
                zone: ZoneKindSel::Library,
                filter: creature_filter,
                count: Expr::Num(1),
                dest: ZoneKindSel::Hand,
                to_top: false,
                shuffle: true,
                bind_as: None,
            },
            &mut s,
            &BindEnv::new().with_controller(PlayerId::Us),
        );

        // Exactly one of {a, b} ends up in hand; land stays in library.
        let in_hand: Vec<ObjId> = s.hand_of(PlayerId::Us).map(|o| o.id).collect();
        assert_eq!(in_hand.len(), 1);
        assert!(in_hand[0] == a || in_hand[0] == b);
        assert!(!matches!(
            s.objects.get(&land).unwrap().zone(), Some(Zone::Hand { .. })
        ));
    }

    fn make_sorcery(name: &str) -> CardDef {
        let sp = crate::catalog::SpellData { mana_cost: "{B}".into(), ..Default::default() };
        CardDef::new(
            name, CardKind::Sorcery(sp), vec![Color::Black], None, vec![],
            CardLayout::Normal, None, vec![], vec![], vec![], vec![],
        )
    }

    // Personal Tutor pattern: search → shuffle → put the chosen card on TOP.
    #[test]
    fn search_to_top_after_shuffle_puts_chosen_sorcery_on_top() {
        let mut s = make_state();
        s.rng = seeded_rng(7);
        // One sorcery among non-sorceries in the library.
        let sorc = insert_obj(&mut s, PlayerId::Us, make_sorcery("Doomsday"));
        let _l1 = insert_obj(&mut s, PlayerId::Us, make_land("Island"));
        let _l2 = insert_obj(&mut s, PlayerId::Us, make_land("Island"));
        let _c = insert_obj(&mut s, PlayerId::Us, make_creature("A", "{G}", 1, 1));

        let sorcery_filter = Filter(Expr::Contains(
            Box::new(Expr::TypeLit(crate::CardType::Sorcery)),
            Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::It)))),
        ));
        execute(
            &Action::Search {
                who: Who::You,
                zone: ZoneKindSel::Library,
                filter: sorcery_filter,
                count: Expr::Num(1),
                dest: ZoneKindSel::Library,
                to_top: true,
                shuffle: true,
                bind_as: None,
            },
            &mut s,
            &BindEnv::new().with_controller(PlayerId::Us),
        );

        // The chosen sorcery sits on top (shuffle happened first, then it was lifted).
        assert_eq!(s.player(PlayerId::Us).library_order.front(), Some(&sorc));
        // Still four cards in the library — nothing left the zone.
        assert_eq!(s.player(PlayerId::Us).library_order.len(), 4);
    }

    fn make_instant(name: &str) -> CardDef {
        let sp = crate::catalog::SpellData { mana_cost: "{U}".into(), ..Default::default() };
        CardDef::new(
            name, CardKind::Instant(sp), vec![Color::Blue], None, vec![],
            CardLayout::Normal, None, vec![], vec![], vec![], vec![],
        )
    }

    // Dig: look at top N, keep K to hand (NOT a draw), rest to the bottom.
    #[test]
    fn dig_keeps_chosen_and_bottoms_the_rest() {
        let mut s = make_state();
        s.rng = seeded_rng(1);
        let t1 = insert_obj(&mut s, PlayerId::Us, make_creature("T1", "{G}", 1, 1));
        let t2 = insert_obj(&mut s, PlayerId::Us, make_land("L2"));
        let t3 = insert_obj(&mut s, PlayerId::Us, make_creature("T3", "{G}", 1, 1));
        let below = insert_obj(&mut s, PlayerId::Us, make_land("Below"));

        // Look at the top 3 [t1,t2,t3]; keep 1 (default strategy keeps the first, t1).
        execute(
            &Action::Dig { who: Who::You, n: Expr::Num(3), take: Expr::Num(1) },
            &mut s,
            &BindEnv::new().with_controller(PlayerId::Us),
        );
        assert_eq!(s.hand_of(PlayerId::Us).map(|o| o.id).collect::<Vec<_>>(), vec![t1]);
        // The un-looked card rises to the top; the two un-kept go to the bottom.
        assert_eq!(
            s.player(PlayerId::Us).library_order.iter().copied().collect::<Vec<_>>(),
            vec![below, t2, t3],
        );
    }

    // Flow State keeps two only when both an instant AND a sorcery are in the graveyard.
    #[test]
    fn flow_state_keeps_two_only_with_instant_and_sorcery_in_graveyard() {
        let cat = crate::card_defs::build_catalog();
        let fs = cat.get("Flow State").expect("Flow State registered in catalog");
        let crate::ir::ability::AbilityKind::OnResolve { modes } = &fs
            .abilities
            .iter()
            .find(|a| matches!(a.kind, crate::ir::ability::AbilityKind::OnResolve { .. }))
            .unwrap()
            .kind
        else {
            unreachable!()
        };
        let body = modes[0].body.clone();

        let run = |inst: bool, sorc: bool| -> usize {
            let mut s = make_state();
            s.rng = seeded_rng(2);
            for nm in ["A", "B", "C", "D"] {
                insert_obj(&mut s, PlayerId::Us, make_land(nm));
            }
            if inst {
                let id = insert_obj(&mut s, PlayerId::Us, make_instant("Bolt"));
                s.set_card_zone(id, Zone::Graveyard);
            }
            if sorc {
                let id = insert_obj(&mut s, PlayerId::Us, make_sorcery("Sorc"));
                s.set_card_zone(id, Zone::Graveyard);
            }
            execute(&body, &mut s, &BindEnv::new().with_controller(PlayerId::Us));
            s.hand_of(PlayerId::Us).count()
        };
        assert_eq!(run(false, false), 1, "no graveyard spells → keep 1");
        assert_eq!(run(true, false), 1, "only an instant → keep 1");
        assert_eq!(run(false, true), 1, "only a sorcery → keep 1");
        assert_eq!(run(true, true), 2, "instant AND sorcery → keep 2");
    }

    // Silence unused imports (Keyword/Supertype appear only in sibling module).
    #[allow(dead_code)]
    fn _touch_unused_imports(_: Keyword, _: Supertype) {}
}

// ── Phase 1 cost-IR tests ────────────────────────────────────────────────────
//
// Per-variant unit tests for `Action::PayMana`, `LoyaltyAdjust`, `Replicate`,
// plus the `cost_exec::build_schema` / `pay` round-trip and equivalence
// crosschecks against the legacy `pay_costs` for shapes both can express.
mod cost_phase1 {
    use crate::catalog::CreatureData;
    use crate::ir::action::{Action, Who};
    use crate::ir::cost::{DecisionKind, NumberKind, PayError};
    use crate::ir::cost_exec::{build_schema, pay};
    use crate::ir::executor::{execute_mut, BindEnv, ExecResult};
    use crate::ir::expr::{Expr, Filter};
    use crate::ir::context::Ctx;
    use crate::{
        parse_mana_cost, BattlefieldState, CardDef, CardKind, CardLayout, Zone, Color,
        GameObject, ObjId, PlayerId, PlayerState, SimState,
    };
    use std::collections::HashMap;

    pub(super) fn make_state() -> SimState {
        SimState::new(PlayerState::new("us"), PlayerState::new("opp"))
    }

    pub(super) fn make_creature(name: &str, mana: &str, p: i32, t: i32) -> CardDef {
        CardDef::new(
            name,
            CardKind::Creature(CreatureData::new(mana, p, t)),
            vec![Color::Green],
            None,
            vec![],
            CardLayout::Normal,
            None,
            vec![], vec![], vec![], vec![],
        )
    }

    pub(super) fn insert_obj(state: &mut SimState, owner: PlayerId, def: CardDef) -> ObjId {
        let id = state.alloc_id();
        state.objects.insert(id, GameObject {
            id,
            catalog_key: def.name.clone(),
            owner,
            controller: owner,
            is_token: false,
            materialized: Some(def.clone()),
            counters: HashMap::new(),
            ci_timestamp: 0,
            role: crate::ObjectRole::Library,
        });
        state.catalog.entry(def.name.clone()).or_insert(def);
        state.player_mut(owner).library_order.push_back(id);
        id
    }

    pub(super) fn put_on_bf(state: &mut SimState, id: ObjId) {
        state.set_card_zone(id, Zone::Battlefield);
        if let Some(obj) = state.objects.get_mut(&id) {
            obj.role = crate::ObjectRole::Battlefield(BattlefieldState {
                tapped: false, damage: 0, entered_this_turn: false,
                counters: 0, power_mod: 0, toughness_mod: 0, loyalty: 0,
                pw_activated_this_turn: false, attacking: false,
                unblocked: false, attack_target: None, active_face: 0,
                etb_choice: None, attached_to: None, stun_counters: 0,
            });
        }
    }

    // ── PayMana ─────────────────────────────────────────────────────────────

    #[test]
    fn pay_mana_drains_pool_when_payable() {
        let mut s = make_state();
        // Add U mana by parsing it through the standard helper.
        crate::eff_mana(PlayerId::Us, "U").call(&mut s, 0, &[]);
        let mc = parse_mana_cost("U");
        let mut env = BindEnv::new().with_controller(PlayerId::Us);
        let res = execute_mut(&Action::PayMana(mc), &mut s, &mut env);
        assert!(matches!(res, ExecResult::Ok));
        // Pool should be drained.
        assert!(!s.player(PlayerId::Us).pool.can_pay(&parse_mana_cost("U")));
    }

    #[test]
    fn pay_mana_returns_shortage_when_pool_empty() {
        let mut s = make_state();
        let mc = parse_mana_cost("U");
        let mut env = BindEnv::new().with_controller(PlayerId::Us);
        match execute_mut(&Action::PayMana(mc.clone()), &mut s, &mut env) {
            ExecResult::ManaShortage(rem) => {
                assert_eq!(rem.u, 1, "remaining should reflect 1 unpaid blue");
                assert_eq!(rem.generic, 0);
            }
            other => panic!("expected ManaShortage, got {:?}", debug_result(&other)),
        }
    }

    #[test]
    fn pay_mana_partial_pool_reports_residual() {
        let mut s = make_state();
        crate::eff_mana(PlayerId::Us, "U").call(&mut s, 0, &[]); // 1 blue
        let mc = parse_mana_cost("UU"); // need 2 blue
        let mut env = BindEnv::new().with_controller(PlayerId::Us);
        match execute_mut(&Action::PayMana(mc), &mut s, &mut env) {
            ExecResult::ManaShortage(rem) => {
                assert_eq!(rem.u, 1);
                assert_eq!(rem.generic, 0);
            }
            other => panic!("expected ManaShortage, got {:?}", debug_result(&other)),
        }
    }

    fn debug_result(r: &ExecResult) -> &'static str {
        match r {
            ExecResult::Ok => "Ok",
            ExecResult::ManaShortage(_) => "ManaShortage",
            ExecResult::Unimplemented(_) => "Unimplemented",
        }
    }

    // ── LoyaltyAdjust ──────────────────────────────────────────────────────

    #[test]
    fn loyalty_adjust_modifies_source_and_marks_activated() {
        let mut s = make_state();
        let pw = insert_obj(&mut s, PlayerId::Us, make_creature("Walker", "{2}{U}", 0, 0));
        put_on_bf(&mut s, pw);
        s.permanent_bf_mut(pw).unwrap().loyalty = 4;

        let mut env = BindEnv::new()
            .with_controller(PlayerId::Us)
            .with_source(pw);
        let res = execute_mut(&Action::LoyaltyAdjust(-2), &mut s, &mut env);
        assert!(matches!(res, ExecResult::Ok));

        let bf = s.permanent_bf(pw).unwrap();
        assert_eq!(bf.loyalty, 2, "loyalty -2");
        assert!(bf.pw_activated_this_turn, "pw_activated_this_turn flag set");
    }

    #[test]
    fn loyalty_adjust_positive_increases() {
        let mut s = make_state();
        let pw = insert_obj(&mut s, PlayerId::Us, make_creature("Walker", "{2}{U}", 0, 0));
        put_on_bf(&mut s, pw);
        s.permanent_bf_mut(pw).unwrap().loyalty = 3;

        let mut env = BindEnv::new()
            .with_controller(PlayerId::Us)
            .with_source(pw);
        let _ = execute_mut(&Action::LoyaltyAdjust(1), &mut s, &mut env);
        assert_eq!(s.permanent_bf(pw).unwrap().loyalty, 4);
    }

    // ── Replicate ──────────────────────────────────────────────────────────

    #[test]
    fn replicate_outside_cost_context_is_noop() {
        let mut s = make_state();
        let mut env = BindEnv::new().with_controller(PlayerId::Us);
        let res = execute_mut(&Action::Replicate(parse_mana_cost("1U")), &mut s, &mut env);
        assert!(matches!(res, ExecResult::Ok));
    }

    // ── Sequence short-circuits on ManaShortage ─────────────────────────────

    #[test]
    fn sequence_short_circuits_on_mana_shortage() {
        let mut s = make_state();
        let pw = insert_obj(&mut s, PlayerId::Us, make_creature("Walker", "{2}{U}", 0, 0));
        put_on_bf(&mut s, pw);
        s.permanent_bf_mut(pw).unwrap().loyalty = 4;

        // Sequence: PayMana(U) (will shortage) THEN LoyaltyAdjust(-1).
        // The loyalty adjustment must NOT run because the sequence short-circuits.
        let mut env = BindEnv::new()
            .with_controller(PlayerId::Us)
            .with_source(pw);
        let action = Action::Sequence(vec![
            Action::PayMana(parse_mana_cost("U")),
            Action::LoyaltyAdjust(-1),
        ]);
        match execute_mut(&action, &mut s, &mut env) {
            ExecResult::ManaShortage(_) => {}
            other => panic!("expected ManaShortage, got {:?}", debug_result(&other)),
        }
        // Loyalty unchanged because the second action never ran.
        assert_eq!(s.permanent_bf(pw).unwrap().loyalty, 4);
        assert!(!s.permanent_bf(pw).unwrap().pw_activated_this_turn);
    }

    // ── build_schema basics ────────────────────────────────────────────────

    #[test]
    fn build_schema_tap_source_no_decision() {
        let mut s = make_state();
        let id = insert_obj(&mut s, PlayerId::Us, make_creature("X", "{G}", 1, 1));
        put_on_bf(&mut s, id);

        let cost = Action::Tap { target: Expr::Ctx(Ctx::Source) };
        let schema = build_schema(&cost, &s, PlayerId::Us, id).expect("payable");
        assert_eq!(schema.decisions.len(), 0, "no decision for Tap source");
    }

    #[test]
    fn build_schema_pay_life_constant_no_decision() {
        let s = make_state();
        let cost = Action::PayLife { who: Who::You, amount: Expr::Num(2) };
        let schema = build_schema(&cost, &s, PlayerId::Us, ObjId::UNSET).expect("payable");
        assert_eq!(schema.decisions.len(), 0);
    }

    #[test]
    fn build_schema_pay_life_x_emits_number_decision() {
        let s = make_state();
        // Var(x) — non-constant; treated as XLife.
        let cost = Action::PayLife { who: Who::You, amount: Expr::Ctx(Ctx::Var("x")) };
        let schema = build_schema(&cost, &s, PlayerId::Us, ObjId::UNSET).expect("payable");
        assert_eq!(schema.decisions.len(), 1);
        match &schema.decisions[0].kind {
            DecisionKind::Number { kind: NumberKind::XLife, max } => {
                assert!(*max >= 1, "max should be at least 1 from default life");
            }
            other => panic!("wrong decision kind: {:?}", debug_decision(other)),
        }
    }

    fn debug_decision(d: &DecisionKind) -> String {
        match d {
            DecisionKind::Objects { count, candidates } =>
                format!("Objects(count={}, candidates={})", count, candidates.len()),
            DecisionKind::Branch { labels, payable, .. } =>
                format!("Branch(labels={}, payable={})", labels.len(), payable.len()),
            DecisionKind::Number { kind, max } =>
                format!("Number({:?}, max={})", kind, max),
        }
    }

    // ── PayLife(n) via the IR cost path ─────────────────────────────────────

    #[test]
    fn equiv_pay_life_2() {
        let mut ir_state = make_state();
        let start = ir_state.life_of(PlayerId::Us);
        let cost = Action::PayLife { who: Who::You, amount: Expr::Num(2) };
        let schema = build_schema(&cost, &ir_state, PlayerId::Us, ObjId::UNSET).expect("payable");
        let env = BindEnv::new().with_controller(PlayerId::Us);
        pay(&cost, &schema, &env, &mut ir_state, 0, PlayerId::Us, ObjId::UNSET)
            .expect("pay should succeed");
        assert_eq!(ir_state.life_of(PlayerId::Us), start - 2);
    }

    // ── TapSelf via the IR cost path ───────────────────────────────────────

    #[test]
    fn equiv_tap_self() {
        let mut ir_state = make_state();
        let ir_id = insert_obj(&mut ir_state, PlayerId::Us, make_creature("X", "{G}", 1, 1));
        put_on_bf(&mut ir_state, ir_id);
        let cost = Action::Tap { target: Expr::Ctx(Ctx::Source) };
        let schema = build_schema(&cost, &ir_state, PlayerId::Us, ir_id).expect("payable");
        assert_eq!(schema.decisions.len(), 0);
        let env = BindEnv::new().with_controller(PlayerId::Us);
        pay(&cost, &schema, &env, &mut ir_state, 0, PlayerId::Us, ir_id)
            .expect("pay should succeed");
        assert!(ir_state.permanent_bf(ir_id).unwrap().tapped);
    }

    // ── pay validates BindEnv against schema ────────────────────────────────

    #[test]
    fn pay_validates_missing_binding() {
        let s_setup = || {
            let mut s = make_state();
            // Build two artifacts so a Sacrifice with a generic filter has
            // multiple candidates and forces a decision.
            let a = insert_obj(&mut s, PlayerId::Us, make_creature("A", "{1}", 1, 1));
            let b = insert_obj(&mut s, PlayerId::Us, make_creature("B", "{1}", 1, 1));
            put_on_bf(&mut s, a);
            put_on_bf(&mut s, b);
            s
        };

        let mut s = s_setup();
        // Sacrifice 1 of {creatures you control}. Two candidates ⇒ Objects decision.
        let cost = Action::Sacrifice {
            who: Who::You,
            // Filter that matches the candidate object — uses Ctx::It semantics.
            // Empty Filter (always true) keeps the test focused on count > 0.
            filter: Filter(Expr::Bool(true)),
            count: Expr::Num(1),
            bind_as: None,
        };
        let schema = build_schema(&cost, &s, PlayerId::Us, ObjId::UNSET).expect("payable");
        assert_eq!(schema.decisions.len(), 1, "should emit one Objects decision");
        let env = BindEnv::new().with_controller(PlayerId::Us);
        // Strategy did not bind anything — pay should reject.
        let res = pay(&cost, &schema, &env, &mut s, 0, PlayerId::Us, ObjId::UNSET);
        assert!(matches!(res, Err(PayError::MissingBinding(_))));
    }
}

// ── Phase 2 castability tests ────────────────────────────────────────────────
//
// `enumerate_playable` returns the typed surface of "things `who` can do
// right now". Tests cover empty-board / no-payable cases, a single castable
// spell with affordable mana, and the `legacy_cost_as_ir` shim's ability to
// translate the simple cost shapes Phase 4 will rely on (TapSelf, Mana(mc),
// Life(n), CostAnd-of-the-above).
mod playable_phase2 {
    use super::cost_phase1::{insert_obj, make_creature, make_state};
    use crate::playable::{enumerate_playable, PlayableKind};
    use crate::{Zone, ObjId, PlayerId};

    fn move_to_hand(state: &mut crate::SimState, id: ObjId, _owner: PlayerId) {
        state.set_card_zone(id, Zone::Hand { known: true });
        // The CE materialization pass normally sets castable from zone — short
        // of running it here, set the bit manually so enumerate_playable sees
        // a castable hand card.
        if let Some(obj) = state.objects.get_mut(&id) {
            if let Some(def) = obj.materialized.as_mut() {
                def.castable = true;
            }
        }
    }

    #[test]
    fn empty_board_yields_no_playable() {
        let s = make_state();
        let actions = enumerate_playable(&s, PlayerId::Us);
        assert!(actions.is_empty(), "no cards anywhere ⇒ nothing playable");
    }

    #[test]
    fn unaffordable_spell_in_hand_not_playable() {
        let mut s = make_state();
        let id = insert_obj(&mut s, PlayerId::Us, make_creature("Big", "{5}{U}{U}", 6, 6));
        move_to_hand(&mut s, id, PlayerId::Us);
        // Pool is empty — unaffordable.
        let actions = enumerate_playable(&s, PlayerId::Us);
        assert!(
            actions.iter().all(|a| !matches!(a.kind, PlayableKind::Cast) || a.source != id),
            "unaffordable spell should not appear as Cast playable"
        );
    }

    #[test]
    fn affordable_spell_appears_with_schema() {
        let mut s = make_state();
        // Mana costs are formatted as e.g. "1", "UU", "3BB" — no braces.
        let id = insert_obj(&mut s, PlayerId::Us, make_creature("Cheap", "1", 1, 1));
        move_to_hand(&mut s, id, PlayerId::Us);
        // Give the player a single colorless to pay {1}. Setting the pool
        // directly bypasses the ManaProduced event path which isn't wired in
        // this bare test fixture.
        s.player_mut(PlayerId::Us).pool.c = 1;
        s.player_mut(PlayerId::Us).pool.total = 1;

        let actions = enumerate_playable(&s, PlayerId::Us);
        let cast: Vec<_> = actions
            .iter()
            .filter(|a| a.source == id && matches!(a.kind, PlayableKind::Cast))
            .collect();
        assert_eq!(cast.len(), 1, "exactly one Cast action for the affordable spell");
        // PayMana doesn't emit a decision — schema should be empty (Some, but
        // with zero decisions). The strategy's announcement plan is "answer
        // nothing" because mana is pool-based, not a Decision.
        let schema = cast[0].schema.as_ref().expect("schema present for IR PayMana");
        assert_eq!(schema.decisions.len(), 0);
    }

}

mod cost_phase3 {
    //! Phase 3 wiring: `pay_ir_cost` runs the IR cost executor with a
    //! schema-driven announcement plan; `default_announcement` answers each
    //! `DecisionKind` so strategies that don't override get sensible bindings.

    use super::cost_phase1::{insert_obj, make_creature, make_state};
    use crate::ir::action::{Action, Who};
    use crate::ir::context::Ctx;
    use crate::ir::cost::{Decision, DecisionKind, NumberKind};
    use crate::ir::expr::{Expr, Value};
    use crate::strategy::default_announcement;
    use crate::{ObjId, PlayerId};

    // ── pay_ir_cost wiring ─────────────────────────────────────────────────

    #[test]
    fn pay_ir_cost_drains_life_with_default_strategy() {
        let mut s = make_state();
        let id = insert_obj(&mut s, PlayerId::Us, make_creature("Free", "", 1, 1));
        s.player_mut(PlayerId::Us).life = 20;

        let action = Action::PayLife { who: Who::You, amount: Expr::Num(2) };
        let ctx = crate::pay_ir_cost(&mut s, 0, PlayerId::Us, id, &action, false)
            .expect("constant PayLife is payable with no strategy");

        assert_eq!(s.player(PlayerId::Us).life, 18, "2 life paid");
        assert!(ctx.objects_moved.is_empty(), "PayLife moves no objects");
    }

    #[test]
    fn pay_ir_cost_xlife_uses_default_strategy_binding() {
        // Var X amount → schema emits an XLife Number decision; default
        // strategy answers min(3, max). With max unbounded by the schema
        // (life-only), default picks 3.
        let mut s = make_state();
        let id = insert_obj(&mut s, PlayerId::Us, make_creature("Free", "", 1, 1));
        s.player_mut(PlayerId::Us).life = 20;

        let action = Action::PayLife { who: Who::You, amount: Expr::Ctx(Ctx::Var("$x")) };
        crate::pay_ir_cost(&mut s, 0, PlayerId::Us, id, &action, false)
            .expect("XLife with default strategy resolves to 3");

        assert_eq!(s.player(PlayerId::Us).life, 17, "3 life paid (default min(3,max))");
    }

    #[test]
    fn pay_ir_cost_unpayable_returns_none() {
        // PayLife with the player at 1 life; constant amount 5 → unpayable.
        let mut s = make_state();
        let id = insert_obj(&mut s, PlayerId::Us, make_creature("Free", "", 1, 1));
        s.player_mut(PlayerId::Us).life = 1;

        let action = Action::PayLife { who: Who::You, amount: Expr::Num(5) };
        let res = crate::pay_ir_cost(&mut s, 0, PlayerId::Us, id, &action, false);
        assert!(res.is_none(), "unpayable cost yields None");
        assert_eq!(s.player(PlayerId::Us).life, 1, "life unchanged on failure");
    }

    // ── default_announcement coverage of each DecisionKind ─────────────────

    #[test]
    fn default_announcement_picks_first_n_objects() {
        let candidates = vec![ObjId(1), ObjId(2), ObjId(3)];
        let schema = crate::ir::cost::CostSchema {
            decisions: vec![Decision {
                binding: "$pick",
                kind: DecisionKind::Objects { candidates: candidates.clone(), count: 2 },
            }],
        };
        let env = default_announcement(&schema);
        match env.bindings.get("$pick") {
            Some(Value::ObjSet(v)) => assert_eq!(v, &vec![ObjId(1), ObjId(2)]),
            other => panic!("expected ObjSet of first 2, got {:?}", other),
        }
    }

    #[test]
    fn default_announcement_single_object_uses_obj_value() {
        let schema = crate::ir::cost::CostSchema {
            decisions: vec![Decision {
                binding: "$one",
                kind: DecisionKind::Objects { candidates: vec![ObjId(7)], count: 1 },
            }],
        };
        let env = default_announcement(&schema);
        match env.bindings.get("$one") {
            Some(Value::Obj(id)) => assert_eq!(*id, ObjId(7)),
            other => panic!("expected Obj(7), got {:?}", other),
        }
    }

    #[test]
    fn default_announcement_branch_picks_first_payable() {
        let schema = crate::ir::cost::CostSchema {
            decisions: vec![Decision {
                binding: "$mode",
                kind: DecisionKind::Branch {
                    labels: vec!["a", "b", "c"],
                    payable: vec![1, 2],
                    branches: vec![
                        crate::ir::cost::CostSchema::empty(),
                        crate::ir::cost::CostSchema::empty(),
                        crate::ir::cost::CostSchema::empty(),
                    ],
                },
            }],
        };
        let env = default_announcement(&schema);
        match env.bindings.get("$mode") {
            Some(Value::Num(n)) => assert_eq!(*n, 1, "first payable index"),
            other => panic!("expected Num(1), got {:?}", other),
        }
    }

    #[test]
    fn default_announcement_xlife_picks_min_three_and_max() {
        // max=2 → default picks 2 (capped under 3).
        let schema = crate::ir::cost::CostSchema {
            decisions: vec![Decision {
                binding: "$x",
                kind: DecisionKind::Number { kind: NumberKind::XLife, max: 2 },
            }],
        };
        let env = default_announcement(&schema);
        match env.bindings.get("$x") {
            Some(Value::Num(n)) => assert_eq!(*n, 2),
            other => panic!("expected Num(2), got {:?}", other),
        }
    }

    #[test]
    fn default_announcement_replicate_picks_zero() {
        let schema = crate::ir::cost::CostSchema {
            decisions: vec![Decision {
                binding: "$rep",
                kind: DecisionKind::Number { kind: NumberKind::Replicate, max: 5 },
            }],
        };
        let env = default_announcement(&schema);
        match env.bindings.get("$rep") {
            Some(Value::Num(n)) => assert_eq!(*n, 0, "default replicate count is 0"),
            other => panic!("expected Num(0), got {:?}", other),
        }
    }
}

// Phase 4 — per-card IR cost storage regression guards.
//
// Now that the cost sub-language is fully on the IR (no `CostComponent`,
// no `Legacy` variant), these tests pin the stored cost shape of migrated
// cards so accidental reverts are caught:
//   1. Activated-ability costs (Lotus Petal sac, EE `{2}, Sac ~`).
//   2. Alternate costs (Daze bounce, FoN/FoW pitch, Snuff Out pay-life).
//   3. Basic lands' mana ability still produces mana end-to-end (CR 605.3b).
mod cost_phase4 {
    use crate::ir::ability::CostBody;
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::Expr;

    #[test]
    fn lotus_petal_storage_is_ir_sacrifice() {
        let cat = crate::card_defs::build_catalog();
        let petal = cat.get("Lotus Petal").expect("Lotus Petal in catalog");
        let ability = petal
            .abilities
            .iter()
            .find(|a| matches!(
                &a.kind,
                crate::ir::ability::AbilityKind::Activated { cost: CostBody::Ir(_), .. }
            ))
            .expect("Lotus Petal's ability stored as CostBody::Ir(_) after Phase 4 step 2");
        let crate::ir::ability::AbilityKind::Activated { cost, .. } = &ability.kind else {
            unreachable!()
        };
        let CostBody::Ir(action) = cost else { unreachable!() };
        assert!(
            matches!(action, Action::Sacrifice { count: Expr::Num(1), bind_as: None, .. }),
            "Lotus Petal cost is Action::Sacrifice {{ count: 1 }}"
        );
    }

    #[test]
    fn daze_alt_cost_storage_is_ir_move_by_choice_return() {
        // Phase 4 step 5 regression: Daze's alt cost is the first card to
        // actually run through `pay_ir_cost` at runtime (not the bridge).
        // Uses the unified `MoveByChoice` primitive with `MoveVerb::Return`
        // and the (Battlefield → Hand) transition.
        use crate::ir::action::MoveVerb;
        use crate::ir::expr::ZoneKindSel;
        let cat = crate::card_defs::build_catalog();
        let daze = cat.get("Daze").expect("Daze in catalog");
        let alts = daze.alternate_costs();
        assert_eq!(alts.len(), 1, "Daze has one alt cost");
        let CostBody::Ir(action) = &alts[0].costs else {
            panic!("Daze's alt cost is CostBody::Ir(_) after Phase 4 step 5")
        };
        match action {
            Action::MoveByChoice {
                from: ZoneKindSel::Battlefield,
                to: ZoneKindSel::Hand,
                verb: MoveVerb::Return,
                count: Expr::Num(1),
                bind_as: Some("$daze_island"),
                ..
            } => {}
            _ => panic!(
                "Daze's alt cost is MoveByChoice {{ from: Battlefield, to: Hand, verb: Return, count: 1, bind_as: Some(...) }}"
            ),
        }
    }

    #[test]
    fn force_of_negation_alt_cost_storage_is_ir_pitch() {
        use crate::ir::action::MoveVerb;
        use crate::ir::expr::ZoneKindSel;
        let cat = crate::card_defs::build_catalog();
        let fon = cat.get("Force of Negation").expect("FoN in catalog");
        let alts = fon.alternate_costs();
        assert_eq!(alts.len(), 1, "FoN has one alt cost");
        let CostBody::Ir(action) = &alts[0].costs else {
            panic!("FoN's alt cost is CostBody::Ir(_) after pitch migration")
        };
        match action {
            Action::MoveByChoice {
                from: ZoneKindSel::Hand,
                to: ZoneKindSel::Exile,
                verb: MoveVerb::Exile,
                count: Expr::Num(1),
                bind_as: Some("$fon_pitch"),
                ..
            } => {}
            _ => panic!(
                "FoN's pitch cost is MoveByChoice {{ from: Hand, to: Exile, verb: Exile, count: 1, bind_as: Some(...) }}"
            ),
        }
    }

    #[test]
    fn force_of_will_alt_cost_storage_is_ir_pitch_then_pay_life() {
        // FoW pitch is a Sequence: [MoveByChoice(Hand→Exile, blue), PayLife(1)].
        use crate::ir::action::MoveVerb;
        use crate::ir::expr::ZoneKindSel;
        let cat = crate::card_defs::build_catalog();
        let fow = cat.get("Force of Will").expect("FoW in catalog");
        let alts = fow.alternate_costs();
        assert_eq!(alts.len(), 1, "FoW has one alt cost");
        let CostBody::Ir(action) = &alts[0].costs else {
            panic!("FoW's alt cost is CostBody::Ir(_) after pitch migration")
        };
        let Action::Sequence(steps) = action else {
            panic!("FoW's pitch cost is Action::Sequence(...)")
        };
        assert_eq!(steps.len(), 2);
        match &steps[0] {
            Action::MoveByChoice {
                from: ZoneKindSel::Hand,
                to: ZoneKindSel::Exile,
                verb: MoveVerb::Exile,
                count: Expr::Num(1),
                bind_as: Some("$fow_pitch"),
                ..
            } => {}
            _ => panic!("step 0 is MoveByChoice(Hand→Exile, exile, blue, count=1)"),
        }
        match &steps[1] {
            Action::PayLife { amount: Expr::Num(1), .. } => {}
            _ => panic!("step 1 is PayLife(1)"),
        }
    }

    #[test]
    fn snuff_out_alt_cost_storage_is_ir_pay_life_4() {
        let cat = crate::card_defs::build_catalog();
        let snuff = cat.get("Snuff Out").expect("Snuff Out in catalog");
        let alts = snuff.alternate_costs();
        assert_eq!(alts.len(), 1);
        let CostBody::Ir(action) = &alts[0].costs else {
            panic!("Snuff Out alt cost is CostBody::Ir(_)")
        };
        match action {
            Action::PayLife { amount: Expr::Num(4), .. } => {}
            _ => panic!("Snuff Out alt cost is Action::PayLife {{ amount: Num(4) }}"),
        }
    }

    #[test]
    fn engineered_explosives_storage_is_ir_pay_mana_then_sacrifice() {
        // First end-to-end IR migration of an `AbilityDef` cost. EE is
        // `{2}, Sacrifice ~`. Cost shape: Sequence(PayMana(2), MoveByChoice
        // (Battlefield → Graveyard, verb=Sacrifice)).
        use crate::ir::action::MoveVerb;
        use crate::ir::expr::ZoneKindSel;
        let cat = crate::card_defs::build_catalog();
        let ee = cat.get("Engineered Explosives").expect("EE in catalog");
        let ab = ee
            .abilities()
            .iter()
            .find(|a| matches!(a.source_zone, crate::SourceZone::Battlefield))
            .expect("EE has a battlefield activated ability");
        let CostBody::Ir(action) = &ab.costs else {
            panic!("EE's activated ability cost is CostBody::Ir(_) post-migration")
        };
        let Action::Sequence(steps) = action else {
            panic!("EE cost is Action::Sequence(...)")
        };
        assert_eq!(steps.len(), 2);
        assert!(matches!(steps[0], Action::PayMana(_)));
        match &steps[1] {
            Action::MoveByChoice {
                from: ZoneKindSel::Battlefield,
                to: ZoneKindSel::Graveyard,
                verb: MoveVerb::Sacrifice,
                count: Expr::Num(1),
                bind_as: Some("$ee_self"),
                ..
            } => {}
            _ => panic!(
                "EE step 1 is MoveByChoice {{ from: Battlefield, to: Graveyard, verb: Sacrifice, count: 1, bind_as: Some(...) }}"
            ),
        }
    }

    #[test]
    fn engineered_explosives_pay_ir_cost_drains_mana_and_sacrifices() {
        // End-to-end runtime: pay EE's `{2}, Sacrifice ~` cost via
        // `pay_ir_cost`. Asserts pool drained by 2, EE in graveyard,
        // and `CostsPaidCtx.objects_moved` contains the EE id.
        use crate::ir::ability::CostBody;
        use crate::{Zone, PlayerId};
        use super::cost_phase1::{insert_obj, make_state};

        let mut state = make_state();
        let ee_def = crate::card_defs::build_catalog()
            .get("Engineered Explosives").expect("EE in catalog").clone();
        state.catalog.insert(ee_def.name.clone(), ee_def.clone());
        let ee_id = insert_obj(&mut state, PlayerId::Us, ee_def.clone());
        // Move EE to battlefield and give it a `bf` slot.
        state.set_card_zone(ee_id, Zone::Battlefield);
        state.objects.get_mut(&ee_id).unwrap().role = crate::ObjectRole::Battlefield(crate::BattlefieldState::new());
        // Fill the pool with {2}.
        state.player_mut(PlayerId::Us).pool.c = 2;
        state.player_mut(PlayerId::Us).pool.total = 2;

        // Locate the activated ability and pay via the IR cost path.
        let ab = ee_def.abilities().iter()
            .find(|a| matches!(a.source_zone, crate::SourceZone::Battlefield))
            .expect("battlefield ability");
        let CostBody::Ir(action) = &ab.costs else { panic!("expected Ir cost") };

        let ctx = crate::pay_ir_cost(&mut state, 1, PlayerId::Us, ee_id, action, false)
            .expect("pay_ir_cost succeeds for EE");

        assert_eq!(state.player(PlayerId::Us).pool.total, 0, "pool drained by 2");
        assert!(matches!(state.objects[&ee_id].zone(), Some(Zone::Graveyard)),
            "EE moved to graveyard");
        assert_eq!(ctx.objects_moved, vec![ee_id],
            "CostsPaidCtx.objects_moved records the sacrificed EE");
    }

    #[test]
    fn move_by_choice_walk_unpayable_with_no_candidates() {
        use crate::ir::action::{MoveVerb, Who};
        use crate::ir::cost_exec::build_schema;
        use crate::ir::expr::{Filter, ZoneKindSel};
        use crate::ObjId;
        use super::cost_phase1::make_state;

        let state = make_state();
        let action = Action::MoveByChoice {
            who: Who::You,
            from: ZoneKindSel::Battlefield,
            to: ZoneKindSel::Hand,
            verb: MoveVerb::Return,
            filter: Filter(Expr::Bool(true)),
            count: Expr::Num(1),
            bind_as: Some("$pick"),
        };
        // No permanents on the battlefield — schema build returns None.
        assert!(build_schema(&action, &state, crate::PlayerId::Us, ObjId::UNSET).is_none());
    }

    #[test]
    fn wasteland_storage_is_ir_sequence() {
        // Phase 4 step 3 regression: TapSelf+SacSelf migrated to a Sequence.
        let cat = crate::card_defs::build_catalog();
        let waste = cat.get("Wasteland").expect("Wasteland in catalog");
        let ability = waste
            .abilities
            .iter()
            .find(|a| matches!(
                &a.kind,
                crate::ir::ability::AbilityKind::Activated { cost: CostBody::Ir(_), .. }
            ))
            .expect("Wasteland's ability is CostBody::Ir(_) after step 3");
        let crate::ir::ability::AbilityKind::Activated { cost, .. } = &ability.kind else {
            unreachable!()
        };
        let CostBody::Ir(action) = cost else { unreachable!() };
        let Action::Sequence(steps) = action else {
            panic!("Wasteland cost is Action::Sequence")
        };
        assert_eq!(steps.len(), 2);
        assert!(matches!(steps[0], Action::Tap { .. }));
        assert!(matches!(steps[1], Action::Sacrifice { .. }));
    }

    #[test]
    fn ir_tap_mana_storage_is_ir() {
        // Regression guard: an island built via the basic_land factory uses
        // `ir_tap_mana`, whose cost is `CostBody::Ir(_)` (the only variant).
        let cat = crate::card_defs::build_catalog();
        let island = cat.get("Island").expect("Island in catalog");
        let ability = island
            .abilities
            .iter()
            .find(|a| matches!(
                &a.kind,
                crate::ir::ability::AbilityKind::Activated { cost: CostBody::Ir(_), .. }
            ))
            .expect("Island's mana ability stored as CostBody::Ir(_) after Phase 4 step 1");
        let crate::ir::ability::AbilityKind::Activated { cost, .. } = &ability.kind else {
            unreachable!()
        };
        let CostBody::Ir(action) = cost else {
            unreachable!("matched Ir(_) above")
        };
        assert!(
            matches!(action, Action::Tap { target: Expr::Ctx(Ctx::Source) }),
            "Island mana ability cost is Action::Tap {{ target: Source }}"
        );
    }
}

// ── Phase 6: XMana (PayManaX) + nested-Choose cost schema ────────────────────
//
// The last cost sub-language migrated off `CostComponent`. `PayManaX` is the
// variable-X mana payment (mirror of `PayLife { amount: Expr }`); the nested
// Choose schema lets a cost-tree branch carry its own object decision (e.g.
// Bitter Triumph's "discard a card or pay 3 life").
mod cost_xmana {
    use super::cost_phase1::make_state;
    use crate::ir::action::{Action, ChoiceOption, MoveVerb, Who};
    use crate::ir::context::Ctx;
    use crate::ir::cost::{DecisionKind, NumberKind};
    use crate::ir::cost_exec::{build_schema, pay};
    use crate::ir::executor::BindEnv;
    use crate::ir::expr::{Expr, Filter, Value, ZoneKindSel};
    use crate::{ObjId, PlayerId};

    #[test]
    fn pay_mana_x_var_emits_xmana_decision_bounded_by_potential() {
        let mut s = make_state();
        s.player_mut(PlayerId::Us).pool.c = 4;
        s.player_mut(PlayerId::Us).pool.total = 4;
        let cost = Action::PayManaX { generic: Expr::Ctx(Ctx::Var("$x")) };
        let schema = build_schema(&cost, &s, PlayerId::Us, ObjId::UNSET).expect("payable");
        assert_eq!(schema.decisions.len(), 1, "X mana emits exactly one decision");
        assert_eq!(schema.decisions[0].binding, "$x", "decision keyed by the variable's own name");
        match &schema.decisions[0].kind {
            DecisionKind::Number { kind: NumberKind::XMana, max } => {
                assert_eq!(*max, 4, "bound is the potential mana total (the resource)");
            }
            _ => panic!("expected an XMana Number decision"),
        }
    }

    #[test]
    fn pay_mana_x_constant_emits_no_decision() {
        let s = make_state();
        let cost = Action::PayManaX { generic: Expr::Num(2) };
        let schema = build_schema(&cost, &s, PlayerId::Us, ObjId::UNSET).expect("payable");
        assert_eq!(schema.decisions.len(), 0, "constant generic = pool checked at exec, no decision");
    }

    #[test]
    fn pay_mana_x_drains_pool_by_bound_amount() {
        let mut s = make_state();
        s.player_mut(PlayerId::Us).pool.c = 5;
        s.player_mut(PlayerId::Us).pool.total = 5;
        let cost = Action::PayManaX { generic: Expr::Ctx(Ctx::Var("$x")) };
        let schema = build_schema(&cost, &s, PlayerId::Us, ObjId::UNSET).expect("payable");
        let mut env = BindEnv::new().with_controller(PlayerId::Us);
        env.bindings.insert("$x", Value::Num(3));
        pay(&cost, &schema, &env, &mut s, 0, PlayerId::Us, ObjId::UNSET).expect("pay ok");
        assert_eq!(s.player(PlayerId::Us).pool.total, 2, "the paid amount (3) is drained from the pool");
    }

    // Bitter-Triumph-shaped cost: Choose([ discard-a-card | pay 3 life ]).
    fn bitter_choice() -> Action {
        Action::Choose {
            who: Who::You,
            prompt: "bt",
            options: vec![
                ChoiceOption {
                    label: "discard a card",
                    cost: None,
                    action: Box::new(Action::MoveByChoice {
                        who: Who::You,
                        from: ZoneKindSel::Hand,
                        to: ZoneKindSel::Graveyard,
                        verb: MoveVerb::Discard,
                        filter: Filter(Expr::Bool(true)),
                        count: Expr::Num(1),
                        bind_as: Some("$bt_discard"),
                    }),
                },
                ChoiceOption {
                    label: "pay 3 life",
                    cost: None,
                    action: Box::new(Action::PayLife { who: Who::You, amount: Expr::Num(3) }),
                },
            ],
            bind_as: Some("$bt_branch"),
        }
    }

    #[test]
    fn nested_choose_branch_carries_discard_object_decision() {
        // Hand has a card → discard branch is payable and its sub-schema holds
        // the "which card" Objects decision under the branch.
        use super::cost_phase1::{insert_obj, make_creature};
        let mut s = make_state();
        let card = insert_obj(&mut s, PlayerId::Us, make_creature("Spare", "1", 1, 1));
        s.set_card_zone(card, crate::Zone::Hand { known: true });

        let schema = build_schema(&bitter_choice(), &s, PlayerId::Us, ObjId::UNSET).expect("payable");
        assert_eq!(schema.decisions.len(), 1);
        match &schema.decisions[0].kind {
            DecisionKind::Branch { payable, branches, .. } => {
                assert_eq!(payable, &vec![0, 1], "both branches payable with a card in hand");
                assert_eq!(branches[0].decisions.len(), 1, "discard branch carries one object decision");
                assert_eq!(branches[1].decisions.len(), 0, "pay-life branch carries no decision");
            }
            _ => panic!("expected a Branch decision"),
        }
    }

    #[test]
    fn nested_choose_discard_branch_executes_chosen_card() {
        use super::cost_phase1::{insert_obj, make_creature};
        let mut s = make_state();
        let card = insert_obj(&mut s, PlayerId::Us, make_creature("Spare", "1", 1, 1));
        s.set_card_zone(card, crate::Zone::Hand { known: true });
        let start_life = s.life_of(PlayerId::Us);

        let cost = bitter_choice();
        let schema = build_schema(&cost, &s, PlayerId::Us, ObjId::UNSET).expect("payable");
        // Default plan: first payable branch (discard) + first candidate card.
        let env = crate::strategy::default_announcement(&schema);
        let ctx = pay(&cost, &schema, &env, &mut s, 0, PlayerId::Us, ObjId::UNSET).expect("pay ok");
        assert!(matches!(s.objects[&card].zone(), Some(crate::Zone::Graveyard)), "chosen card discarded");
        assert_eq!(s.life_of(PlayerId::Us), start_life, "discard branch taken → no life paid");
        assert_eq!(ctx.objects_moved, vec![card], "discarded card recorded in objects_moved");
    }

    #[test]
    fn nested_choose_falls_back_to_life_when_hand_empty() {
        // Empty hand → discard branch unpayable; only the life branch remains.
        let mut s = make_state();
        let start_life = s.life_of(PlayerId::Us);
        let cost = bitter_choice();
        let schema = build_schema(&cost, &s, PlayerId::Us, ObjId::UNSET).expect("payable via life");
        match &schema.decisions[0].kind {
            DecisionKind::Branch { payable, .. } => assert_eq!(payable, &vec![1], "only pay-life payable"),
            _ => panic!("expected Branch"),
        }
        let env = crate::strategy::default_announcement(&schema);
        pay(&cost, &schema, &env, &mut s, 0, PlayerId::Us, ObjId::UNSET).expect("pay ok");
        assert_eq!(s.life_of(PlayerId::Us), start_life - 3, "fell back to paying 3 life");
    }
}
