use std::collections::HashMap;
use std::sync::Arc;
use super::*;
use crate::ir::ability::CostBody;

// ── IR cost helpers ───────────────────────────────────────────────────────────
//
// Tight wrappers for the recurring `CostBody::Ir(Action::…)` shapes used by
// per-card migrations. Each helper builds one structural cost; compose via
// `Sequence` for conjunctions (see `ir_seq`).

fn ir_tap_source() -> CostBody {
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::Expr;
    CostBody::Ir(Action::Tap { target: Expr::Ctx(Ctx::Source) })
}

fn ir_pay_mana_str(s: &str) -> CostBody {
    use crate::ir::action::Action;
    CostBody::Ir(Action::PayMana(parse_mana_cost(s)))
}

fn ir_pay_life(n: i64) -> CostBody {
    use crate::ir::action::{Action, Who};
    use crate::ir::expr::Expr;
    CostBody::Ir(Action::PayLife { who: Who::You, amount: Expr::Num(n) })
}

fn ir_loyalty(n: i32) -> CostBody {
    use crate::ir::action::Action;
    CostBody::Ir(Action::LoyaltyAdjust(n))
}

/// `Sacrifice ~` cost shape — single source, MoveByChoice with `It == Source`.
#[allow(dead_code)]
fn ir_sac_self(bind: &'static str) -> CostBody {
    use crate::ir::action::{Action, MoveVerb, Who};
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, Filter, ZoneKindSel};
    CostBody::Ir(Action::MoveByChoice {
        who: Who::You,
        from: ZoneKindSel::Battlefield,
        to: ZoneKindSel::Graveyard,
        verb: MoveVerb::Sacrifice,
        filter: Filter(Expr::Eq(
            Box::new(Expr::Ctx(Ctx::It)),
            Box::new(Expr::Ctx(Ctx::Source)),
        )),
        count: Expr::Num(1),
        bind_as: Some(bind),
    })
}

/// Inner action for `Tap source` (no CostBody wrapper) — for use inside Sequence.
fn act_tap_source() -> crate::ir::action::Action {
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::Expr;
    Action::Tap { target: Expr::Ctx(Ctx::Source) }
}

/// Inner Sacrifice~ action.
fn act_sac_self(bind: &'static str) -> crate::ir::action::Action {
    use crate::ir::action::{Action, MoveVerb, Who};
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, Filter, ZoneKindSel};
    Action::MoveByChoice {
        who: Who::You,
        from: ZoneKindSel::Battlefield,
        to: ZoneKindSel::Graveyard,
        verb: MoveVerb::Sacrifice,
        filter: Filter(Expr::Eq(
            Box::new(Expr::Ctx(Ctx::It)),
            Box::new(Expr::Ctx(Ctx::Source)),
        )),
        count: Expr::Num(1),
        bind_as: Some(bind),
    }
}

fn act_pay_mana_str(s: &str) -> crate::ir::action::Action {
    use crate::ir::action::Action;
    Action::PayMana(parse_mana_cost(s))
}

fn act_pay_life(n: i64) -> crate::ir::action::Action {
    use crate::ir::action::{Action, Who};
    use crate::ir::expr::Expr;
    Action::PayLife { who: Who::You, amount: Expr::Num(n) }
}

/// Wrap a vec of actions in `CostBody::Ir(Action::Sequence(...))`.
fn ir_seq(actions: Vec<crate::ir::action::Action>) -> CostBody {
    use crate::ir::action::Action;
    CostBody::Ir(Action::Sequence(actions))
}

/// `additional_costs` shape for "as an additional cost, pay X generic mana"
/// (Engineered Explosives, Meltdown, Prismatic Ending convergence, etc.). The
/// X amount is the announced `chosen_x`, bound to `$x` at pay time and shared
/// with the spell's resolution effect.
fn ir_xmana_cost() -> CostBody {
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::Expr;
    CostBody::Ir(Action::PayManaX { generic: Expr::Ctx(Ctx::Var("$x")) })
}

/// `additional_costs` shape for "as an additional cost, pay X life"
/// (Toxic Deluge, etc.). Same `$x` binding as [`ir_xmana_cost`].
fn ir_xlife_cost() -> CostBody {
    use crate::ir::action::{Action, Who};
    use crate::ir::context::Ctx;
    use crate::ir::expr::Expr;
    CostBody::Ir(Action::PayLife { who: Who::You, amount: Expr::Ctx(Ctx::Var("$x")) })
}

// ── Public API ────────────────────────────────────────────────────────────────

/// Build the full card catalog used by the simulation engine.
pub fn build_catalog() -> HashMap<String, CardDef> {
    all_cards()
        .into_iter()
        .map(|mut c| {
            // Synthesize a legacy `AbilityDef` for each IR `AbilityKind::Activated`
            // and append it to the kind-specific ability list. This lets the
            // existing `collect_legal_actions` / `run_activate_submachine` pipeline
            // dispatch IR activated abilities without duplicate discovery logic.
            // Synthesized entries carry the IR body via `AbilityDef.ir_body`, which
            // `build_ability_effect` already honors.
            let synthesized: Vec<AbilityDef> = c
                .abilities
                .iter()
                .filter_map(crate::ir::executor::ir_activated_as_legacy)
                .collect();
            if !synthesized.is_empty() {
                if let Some(list) = c.abilities_vec_mut() {
                    list.extend(synthesized);
                }
            }
            // For each IR `AbilityKind::Activated` that classifies as a mana
            // ability per CR 605.1a (no target + body could produce mana),
            // synthesize a legacy `ManaAbility` entry so the existing
            // synchronous mana sub-loop picks it up. `ir_activated_as_legacy`
            // skips these (returns None for mana-classified activated
            // abilities), so each IR ability lands in exactly one list.
            let synthesized_mana: Vec<ManaAbility> = c
                .abilities
                .iter()
                .filter_map(crate::ir::executor::ir_activated_as_mana_ability_legacy)
                .collect();
            if !synthesized_mana.is_empty() {
                if let Some(list) = c.mana_abilities_vec_mut() {
                    list.extend(synthesized_mana);
                }
            }
            (c.name.clone(), c)
        })
        .collect()
}

fn all_cards() -> Vec<CardDef> {
    vec![
        // Lands — basics
        island(),
        swamp(),
        plains(),
        mountain(),
        forest(),
        wastes(),
        snow_covered_island(),
        snow_covered_swamp(),
        snow_covered_plains(),
        snow_covered_mountain(),
        snow_covered_forest(),
        snow_covered_wastes(),
        // Lands — ABU duals
        underground_sea(),
        tundra(),
        badlands(),
        taiga(),
        savannah(),
        scrubland(),
        volcanic_island(),
        bayou(),
        plateau(),
        tropical_island(),
        // Lands — MKM surveil duals (enter tapped)
        undercity_sewers(),
        meticulous_archive(),
        raucous_theater(),
        hedge_maze(),
        commercial_district(),
        lush_portico(),
        thundering_falls(),
        underground_mortuary(),
        elegant_parlor(),
        shadowy_backstreet(),
        // Lands — fetches
        polluted_delta(),
        flooded_strand(),
        misty_rainforest(),
        scalding_tarn(),
        marsh_flats(),
        bloodstained_mire(),
        windswept_heath(),
        wooded_foothills(),
        verdant_catacombs(),
        arid_mesa(),
        // Lands — other
        wasteland(),
        karakas(),
        ancient_tomb(),
        city_of_traitors(),
        cavern_of_souls(),
        urborg_tomb_of_yawgmoth(),
        yavimaya_cradle_of_growth(),
        mistrise_village(),
        great_furnace(),
        planar_nexus(),
        urzas_tower(),
        urzas_workshop(),
        // Artifacts
        lotus_petal(),
        lions_eye_diamond(),
        mox_opal(),
        grim_monolith(),
        basalt_monolith(),
        manifold_key(),
        mystic_forge(),
        the_one_ring(),
        relic_of_sauron(),
        giant_boulder(),
        paradox_engine(),
        soul_guide_lantern(),
        tormods_crypt(),
        mycosynth_lattice(),
        ensnaring_bridge(),
        portable_hole(),
        summon_bahamut(),
        ursas_saga(),
        engineered_explosives(),
        grafdiggers_cage(),
        mishras_bauble(),
        cori_steel_cutter(),
        the_fantasticar(),
        batterskull(),
        meteor_sword(),
        pre_war_formalwear(),
        cryptic_coat(),
        // Spells — instants
        brainstorm(),
        consider(),
        daze(),
        force_of_negation(),
        force_of_will(),
        dark_ritual(),
        cabal_ritual(),
        fatal_push(),
        snuff_out(),
        swords_to_plowshares(),
        bitter_triumph(),
        long_goodbye(),
        consign_to_memory(),
        surgical_extraction(),
        lightning_bolt(),
        abrade(),
        red_elemental_blast(),
        pyroblast(),
        blue_elemental_blast(),
        hydroblast(),
        sheoldreds_edict(),
        spell_pierce(),
        stifle(),
        flusterstorm(),
        mindbreak_trap(),
        // Spells — sorceries
        brotherhoods_end(),
        toxic_deluge(),
        doomsday(),
        stock_up(),
        preordain(),
        ponder(),
        thoughtseize(),
        unearth(),
        hymn_to_tourach(),
        edge_of_autumn(),
        personal_tutor(),
        flow_state(),
        green_suns_zenith(),
        show_and_tell(),
        transmute_artifact(),
        kozileks_command(),
        omniscience(),
        sneak_attack(),
        // Creatures
        thassas_oracle(),
        jace_wielder_of_mysteries(),
        street_wraith(),
        barrowgoyf(),
        ingenious_infiltrator(),
        kaito_bane_of_nightmares(),
        recruiter_of_the_guard(),
        stoneforge_mystic(),
        orcish_bowmasters(),
        murktide_regent(),
        dauthi_voidwalker(),
        lavinia_azorius_renegade(),
        phelia_exuberant_shepherd(),
        hexing_squelcher(),
        dragons_rage_channeler(),
        simian_spirit_guide(),
        fury(),
        quantum_riddler(),
        griselbrand(),
        emrakul_the_aeons_torn(),
        atraxa_grand_unifier(),
        // DFCs / split
        tamiyo_inquisitive_student(),
        brazen_borrower(),
        containment_priest(),
        delver_of_secrets(),
        // Spells — Izzet Delver
        unholy_heat(),
        price_of_progress(),
        meltdown(),
        rough_tumble(),
        prismatic_ending(),
        // Opponent archetypes / hate cards
        null_rod(),
        trinisphere(),
        karn_the_great_creator(),
        tezzeret_cruel_captain(),
        painters_servant(),
        leyline_of_the_void(),
        disruptor_flute(),
        blood_moon(),
        magus_of_the_moon(),
        // Tokens
        orc_army_token(),
        clue_token(),
        monk_token(),
        phyrexian_germ_token(),
        fantasticar_construct_token(),
        mysterious_creature_token(),
        construct_token(),
        eldrazi_spawn_token(),
    ]
}

// ── Local helpers ─────────────────────────────────────────────────────────────

/// `CardDef` with no supertypes, normal layout, no back, no triggers/replacements/statics.
fn simple(name: &str, kind: CardKind, colors: Vec<Color>, play_weight: Option<u32>) -> CardDef {
    CardDef::new(
        name, kind, colors, play_weight,
        vec![], CardLayout::Normal, None, vec![], vec![], vec![], vec![],
    )
}


/// IR resolution body for "counter target spell/ability unless its controller
/// pays `cost`" (Daze, Spell Pierce). The spell's controller is offered a choice:
/// pay the tax (the spell then resolves normally — `Noop`), or decline (the spell
/// is countered). The `Choose` executor filters the pay option out when it's
/// unaffordable, and the default strategy takes the first legal option — i.e. pay
/// whenever possible. Mana abilities are auto-tapped during the resolution-time
/// payment (see `pay_ir_cost`).
pub(crate) fn counter_unless_pays_body(cost: ManaCost) -> crate::ir::action::Action {
    use crate::ir::action::{Action, ChoiceOption, Who as IrWho};
    use crate::ir::context::Ctx;
    use crate::ir::expr::Expr;
    let target = || Expr::Ctx(Ctx::Var("target"));
    Action::Choose {
        who: IrWho::Player(Expr::Controller(Box::new(target()))),
        prompt: "Pay the tax or be countered",
        options: vec![
            ChoiceOption {
                label: "pay",
                cost: Some(Box::new(Action::PayMana(cost))),
                action: Box::new(Action::Noop),
            },
            ChoiceOption {
                label: "be countered",
                cost: None,
                action: Box::new(Action::Counter { target: target() }),
            },
        ],
        bind_as: None,
    }
}

fn color_to_mana_char(c: Color) -> &'static str {
    match c {
        Color::White => "W", Color::Blue => "U", Color::Black => "B",
        Color::Red => "R", Color::Green => "G",
    }
}

/// `ManaAbility` that taps self and produces the given mana string (e.g. `"U"`, `"B"`).
fn tap_produces(s: &str) -> ManaAbility {
    let s_owned = s.to_string();
    ManaAbility {
        costs: ir_tap_source(),
        produces: produces_colors(s),
        produces_count: 1,
        make_effect: std::sync::Arc::new(move |who, _color| eff_mana(who, s_owned.clone())),
        ..Default::default()
    }
}

/// IR mana ability: tap self to produce the mana listed in `s`
/// ("U" = one blue, "CC" = two colorless, "" = one colorless / Wastes).
/// Built as a no-target `AbilityKind::Activated` whose body is `Action::AddMana`;
/// the bridge classifies it as a mana ability via `is_mana_ability` (CR 605.1a).
fn ir_tap_mana(s: &str) -> crate::ir::ability::Ability {
    use crate::ir::ability::{Ability, AbilityKind};
    use crate::ir::action::{Action, ManaSpec, Who};
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel};
    let colors = produces_colors(s);
    let count = if s.is_empty() {
        1 // Wastes: one colorless
    } else {
        s.chars().count() as i64
    };
    Ability {
        kind: AbilityKind::Activated {
            // Tap-self mana ability cost on the IR grammar.
            cost: CostBody::Ir(Action::Tap { target: Expr::Ctx(Ctx::Source) }),
            target_spec: TargetSpec::None,
            choice_spec: None,
            body: Action::AddMana {
                who: Who::You,
                count: Expr::Num(count),
                spec: ManaSpec::Fixed(colors),
            },
            timing: ActivationTiming::Default,
            activation_condition: None,
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("{T}: Add mana."),
    }
}

/// `AbilityDef` for a fetch land: sacrifice self, pay 1 life, search → Battlefield.
/// Closure-free — the effect is an IR `Action::Search` body (the canonical lowered
/// form, run by `build_ability_effect`), not an `ability_factory`.
fn fetch_ability(filter: crate::ir::expr::Filter) -> AbilityDef {
    use crate::ir::action::{Action, Who as IrWho};
    use crate::ir::expr::{Expr, ZoneKindSel};
    AbilityDef {
        costs: ir_seq(vec![act_sac_self("$fetch_self"), act_pay_life(1)]),
        ir_body: Some(Action::Search {
            who: IrWho::You,
            zone: ZoneKindSel::Library,
            filter,
            count: Expr::Num(1),
            dest: ZoneKindSel::Battlefield,
            to_top: false,
            shuffle: true,
            bind_as: None,
        }),
        ..Default::default()
    }
}

/// Basic land (Island, Swamp, Plains, Mountain, Forest, Wastes).
fn basic_land(name: &str, land_types: LandTypes, mana: &str) -> CardDef {
    let mut def = CardDef::new(
        name, CardKind::Land(LandData {
            land_types,
            mana_abilities: vec![],
            ..Default::default()
        }),
        vec![], Some(25), vec![Supertype::Basic], CardLayout::Normal, None,
        vec![], vec![], vec![], vec![],
    );
    def.abilities.push(ir_tap_mana(mana));
    def
}

/// Basic snow land (Snow-Covered X).
fn snow_basic(name: &str, land_types: LandTypes, mana: &str) -> CardDef {
    let mut def = CardDef::new(
        name, CardKind::Land(LandData {
            land_types,
            mana_abilities: vec![],
            ..Default::default()
        }),
        vec![], Some(25), vec![Supertype::Basic, Supertype::Snow], CardLayout::Normal, None,
        vec![], vec![], vec![], vec![],
    );
    def.abilities.push(ir_tap_mana(mana));
    def
}

/// Dual land that always enters tapped (surveil lands, etc.).
/// MKM-style surveil dual: always enters tapped, triggers surveil 1 on ETB.
fn surveil_dual(name: &'static str, land_types: LandTypes, c1: &str, c2: &str) -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, EventPattern, ReplacementBody};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, Filter, ZoneKindSel};

    let trigger = etb_self_trigger(name, TargetSpec::None, move |_, controller| {
        eff_surveil(controller, 1)
    });
    let self_etb = Filter(Expr::Eq(
        Box::new(Expr::Ctx(Ctx::It)),
        Box::new(Expr::Ctx(Ctx::Source)),
    ));
    let enters_tapped = Ability {
        kind: AbilityKind::Replacement {
            matches: EventPattern::EntersZone {
                obj_filter: self_etb,
                zone_kind: ZoneKindSel::Battlefield,
            },
            condition: None,
            body: ReplacementBody::Replace(Action::Sequence(vec![
                Action::Move {
                    what: Expr::Ctx(Ctx::Var("triggered_obj")),
                    to: ZoneKindSel::Battlefield,
                    to_owner: None,
                    bind_as: None,
                },
                Action::Tap { target: Expr::Ctx(Ctx::Var("triggered_obj")) },
            ])),
            active_zone: None, // self-entry replacement
        },
        text: Some("~ enters tapped."),
    };
    let mut card = CardDef::new(
        name,
        CardKind::Land(LandData {
            land_types,
            mana_abilities: vec![tap_produces(c1), tap_produces(c2)],
            ..Default::default()
        }),
        vec![], None, vec![], CardLayout::Normal, None,
        vec![trigger],
        vec![],
        vec![],
        vec![],
    );
    card.abilities = vec![enters_tapped];
    card
}

// ── Lands ─────────────────────────────────────────────────────────────────────

/// ABU dual land: two basic land subtypes, two IR tap-mana abilities (one per color).
fn abu_dual(name: &str, a: BasicLandType, b: BasicLandType, ma: &str, mb: &str) -> CardDef {
    let mut def = simple(name, CardKind::Land(LandData {
        land_types: LandTypes::from_types(&[a, b]),
        mana_abilities: vec![],
        ..Default::default()
    }), vec![], None);
    def.abilities.push(ir_tap_mana(ma));
    def.abilities.push(ir_tap_mana(mb));
    def
}

fn underground_sea()  -> CardDef { abu_dual("Underground Sea",  BasicLandType::Island,   BasicLandType::Swamp,    "U", "B") }
fn tundra()           -> CardDef { abu_dual("Tundra",           BasicLandType::Plains,   BasicLandType::Island,   "W", "U") }
fn badlands()         -> CardDef { abu_dual("Badlands",         BasicLandType::Swamp,    BasicLandType::Mountain, "B", "R") }
fn taiga()            -> CardDef { abu_dual("Taiga",            BasicLandType::Mountain, BasicLandType::Forest,   "R", "G") }
fn savannah()         -> CardDef { abu_dual("Savannah",         BasicLandType::Forest,   BasicLandType::Plains,   "G", "W") }
fn scrubland()        -> CardDef { abu_dual("Scrubland",        BasicLandType::Plains,   BasicLandType::Swamp,    "W", "B") }
fn volcanic_island()  -> CardDef { abu_dual("Volcanic Island",  BasicLandType::Island,   BasicLandType::Mountain, "U", "R") }
fn bayou()            -> CardDef { abu_dual("Bayou",            BasicLandType::Swamp,    BasicLandType::Forest,   "B", "G") }
fn plateau()          -> CardDef { abu_dual("Plateau",          BasicLandType::Mountain, BasicLandType::Plains,   "R", "W") }
fn tropical_island()  -> CardDef { abu_dual("Tropical Island",  BasicLandType::Forest,   BasicLandType::Island,   "G", "U") }

fn swamp() -> CardDef {
    let mut def = CardDef::new(
        "Swamp",
        CardKind::Land(LandData {
            land_types: LandTypes::from_types(&[BasicLandType::Swamp]),
            mana_abilities: vec![],
            ..Default::default()
        }),
        vec![], Some(25), vec![Supertype::Basic], CardLayout::Normal, None,
        vec![], vec![], vec![], vec![],
    );
    def.abilities.push(ir_tap_mana("B"));
    def
}

fn island() -> CardDef {
    let mut def = CardDef::new(
        "Island",
        CardKind::Land(LandData {
            land_types: LandTypes::from_types(&[BasicLandType::Island]),
            mana_abilities: vec![],
            ..Default::default()
        }),
        vec![], Some(25), vec![Supertype::Basic], CardLayout::Normal, None,
        vec![], vec![], vec![], vec![],
    );
    def.abilities.push(ir_tap_mana("U"));
    def
}

fn plains() -> CardDef {
    basic_land("Plains", LandTypes::from_types(&[BasicLandType::Plains]), "W")
}

fn mountain() -> CardDef {
    basic_land("Mountain", LandTypes::from_types(&[BasicLandType::Mountain]), "R")
}

fn forest() -> CardDef {
    basic_land("Forest", LandTypes::from_types(&[BasicLandType::Forest]), "G")
}

/// Wastes: basic land with no subtype, produces {C}.
fn wastes() -> CardDef {
    basic_land("Wastes", LandTypes::default(), "")
}

fn snow_covered_island() -> CardDef {
    snow_basic("Snow-Covered Island", LandTypes::from_types(&[BasicLandType::Island]), "U")
}

fn snow_covered_swamp() -> CardDef {
    snow_basic("Snow-Covered Swamp", LandTypes::from_types(&[BasicLandType::Swamp]), "B")
}

fn snow_covered_plains() -> CardDef {
    snow_basic("Snow-Covered Plains", LandTypes::from_types(&[BasicLandType::Plains]), "W")
}

fn snow_covered_mountain() -> CardDef {
    snow_basic("Snow-Covered Mountain", LandTypes::from_types(&[BasicLandType::Mountain]), "R")
}

fn snow_covered_forest() -> CardDef {
    snow_basic("Snow-Covered Forest", LandTypes::from_types(&[BasicLandType::Forest]), "G")
}

fn snow_covered_wastes() -> CardDef {
    let mut def = CardDef::new(
        "Snow-Covered Wastes",
        CardKind::Land(LandData {
            land_types: LandTypes::default(),
            mana_abilities: vec![],
            ..Default::default()
        }),
        vec![], Some(25), vec![Supertype::Basic, Supertype::Snow], CardLayout::Normal, None,
        vec![], vec![], vec![], vec![],
    );
    def.abilities.push(ir_tap_mana(""));
    def
}

/// Enters tapped. CR 614.1 (replacement effect): replaces the ETB event to set tapped=true.
// ── MKM surveil lands (always enter tapped; surveil 1 on ETB) ─────────────────

fn undercity_sewers()     -> CardDef { surveil_dual("Undercity Sewers",     LandTypes::from_types(&[BasicLandType::Island, BasicLandType::Swamp]), "U", "B") }
fn meticulous_archive()   -> CardDef { surveil_dual("Meticulous Archive",   LandTypes::from_types(&[BasicLandType::Plains, BasicLandType::Island]), "W", "U") }
fn raucous_theater()      -> CardDef { surveil_dual("Raucous Theater",      LandTypes::from_types(&[BasicLandType::Swamp, BasicLandType::Mountain]), "B", "R") }
fn hedge_maze()           -> CardDef { surveil_dual("Hedge Maze",           LandTypes::from_types(&[BasicLandType::Mountain, BasicLandType::Forest]), "R", "G") }
fn commercial_district()  -> CardDef { surveil_dual("Commercial District",  LandTypes::from_types(&[BasicLandType::Forest, BasicLandType::Plains]), "G", "W") }
fn lush_portico()         -> CardDef { surveil_dual("Lush Portico",         LandTypes::from_types(&[BasicLandType::Plains, BasicLandType::Forest]), "W", "G") }
fn thundering_falls()     -> CardDef { surveil_dual("Thundering Falls",     LandTypes::from_types(&[BasicLandType::Island, BasicLandType::Mountain]), "U", "R") }
fn underground_mortuary() -> CardDef { surveil_dual("Underground Mortuary", LandTypes::from_types(&[BasicLandType::Swamp, BasicLandType::Forest]), "B", "G") }
fn elegant_parlor()       -> CardDef { surveil_dual("Elegant Parlor",       LandTypes::from_types(&[BasicLandType::Mountain, BasicLandType::Plains]), "R", "W") }
fn shadowy_backstreet()   -> CardDef { surveil_dual("Shadowy Backstreet",   LandTypes::from_types(&[BasicLandType::Plains, BasicLandType::Swamp]), "W", "B") }

/// {T}, Sacrifice: destroy target nonbasic land. CR 701.7.
fn wasteland() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, Filter};

    let nonbasic_land = ir_and(
        ir_type(CardType::Land),
        ir_not(ir_supertype(Supertype::Basic)),
    );
    let mut card = simple(
        "Wasteland",
        CardKind::Land(LandData::default()),
        vec![],
        None,
    );
    card.abilities = vec![Ability {
        kind: AbilityKind::Activated {
            // Phase 4 step 3: TapSelf+SacSelf conjunction migrated to IR.
            // Lowered back to legacy by the Sequence-aware shim arm.
            cost: CostBody::Ir(Action::Sequence(vec![
                Action::Tap { target: Expr::Ctx(Ctx::Source) },
                Action::Sacrifice {
                    who: crate::ir::action::Who::You,
                    filter: Filter(Expr::Eq(
                        Box::new(Expr::Ctx(Ctx::It)),
                        Box::new(Expr::Ctx(Ctx::Source)),
                    )),
                    count: Expr::Num(1),
                    bind_as: None,
                },
            ])),
            target_spec: TargetSpec::ObjectInZone {
                controller: Who::Opp,
                zone: ZoneId::Battlefield,
                filter: nonbasic_land,
            },
            choice_spec: None,
            body: Action::Destroy {
                target: Expr::Ctx(Ctx::Var("target")),
            },
            timing: ActivationTiming::Default,
            activation_condition: None,
            active_zone: crate::ir::expr::ZoneKindSel::Battlefield,
        },
        text: Some("{T}, Sacrifice Wasteland: Destroy target nonbasic land."),
    }];
    card
}

fn karakas() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel};
    let legend_creature = ir_and(
        ir_type(CardType::Creature),
        ir_supertype(Supertype::Legendary),
    );
    let mut card = CardDef::new(
        "Karakas",
        CardKind::Land(LandData {
            mana_abilities: vec![tap_produces("W")],
            abilities: vec![], // bounce is now an IR Activated ability (below)
            ..Default::default()
        }),
        vec![], None, vec![Supertype::Legendary], CardLayout::Normal, None,
        vec![], vec![], vec![], vec![],
    );
    // "{T}: Return target legendary creature to its owner's hand."
    card.abilities = vec![Ability {
        kind: AbilityKind::Activated {
            cost: ir_tap_source(),
            target_spec: TargetSpec::Union(vec![
                TargetSpec::ObjectInZone {
                    controller: Who::Actor,
                    zone: ZoneId::Battlefield,
                    filter: legend_creature.clone(),
                },
                TargetSpec::ObjectInZone {
                    controller: Who::Opp,
                    zone: ZoneId::Battlefield,
                    filter: legend_creature,
                },
            ]),
            choice_spec: None,
            body: Action::Move {
                what: Expr::Ctx(Ctx::Var("target")),
                to: ZoneKindSel::Hand,
                to_owner: None,
                bind_as: None,
            },
            timing: ActivationTiming::Default,
            activation_condition: None,
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("{T}: Return target legendary creature to its owner's hand."),
    }];
    card
}

fn ancient_tomb() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, CostBody};
    use crate::ir::action::{Action, ManaSpec, Who};
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel};
    let mut def = simple("Ancient Tomb", CardKind::Land(LandData {
        mana_abilities: vec![],
        ..Default::default()
    }), vec![], None);
    def.abilities.push(Ability {
        kind: AbilityKind::Activated {
            // Same cost shape as Underground Sea's tap-for-mana abilities
            // (`{T}: Add <X>`). Body differs — adds CC and pays 2 life as
            // a side-effect — but the cost is just TapSelf.
            cost: CostBody::Ir(Action::Tap { target: Expr::Ctx(Ctx::Source) }),
            target_spec: TargetSpec::None,
            choice_spec: None,
            body: Action::Sequence(vec![
                Action::AddMana {
                    who: Who::You,
                    count: Expr::Num(2),
                    spec: ManaSpec::Fixed(vec![]), // CC — pad with colorless
                },
                Action::PayLife { who: Who::You, amount: Expr::Num(2) },
            ]),
            timing: ActivationTiming::Default,
            activation_condition: None,
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("{T}: Add {C}{C}. Ancient Tomb deals 2 damage to you."),
    });
    def
}

fn city_of_traitors() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, EventPattern, TriggerSpec};
    use crate::ir::action::{Action, Who as IrWho};
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel};
    let mut card = CardDef::new(
        "City of Traitors",
        CardKind::Land(LandData {
            mana_abilities: vec![ManaAbility {
                costs: ir_tap_source(),
                produces_count: 2,
                make_effect: Arc::new(|who, _| eff_mana(who, "CC")),
                ..Default::default()
            }],
            ..Default::default()
        }),
        vec![], None,
        vec![], CardLayout::Normal, None,
        vec![], vec![], vec![], vec![],
    );
    // "When you play another land, sacrifice City of Traitors." `land_filter` is
    // "another land" (the played land ≠ this one); the condition scopes to "you".
    card.abilities = vec![Ability {
        kind: AbilityKind::Triggered {
            spec: TriggerSpec::When {
                pattern: EventPattern::LandPlayed {
                    who: ir_any(),
                    land_filter: ir_not(ir_self()),
                },
                condition: Some(Expr::Eq(
                    Box::new(Expr::Ctx(Ctx::Var("triggered_actor"))),
                    Box::new(Expr::Ctx(Ctx::Controller)),
                )),
            },
            target_spec: TargetSpec::None,
            body: Action::Sacrifice {
                who: IrWho::You,
                filter: ir_self(),
                count: Expr::Num(1),
                bind_as: None,
            },
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("When you play another land, sacrifice City of Traitors."),
    }];
    card
}

fn polluted_delta() -> CardDef {
    simple("Polluted Delta", CardKind::Land(LandData {
        abilities: vec![fetch_ability(ir_and(
            ir_type(CardType::Land),
            ir_or(ir_subtype("island"), ir_subtype("swamp")),
        ))],
        ..Default::default()
    }), vec![], Some(25))
}

fn flooded_strand() -> CardDef {
    simple("Flooded Strand", CardKind::Land(LandData {
        abilities: vec![fetch_ability(ir_and(
            ir_type(CardType::Land),
            ir_subtype("island"),
        ))],
        ..Default::default()
    }), vec![], Some(25))
}

fn misty_rainforest() -> CardDef {
    simple("Misty Rainforest", CardKind::Land(LandData {
        abilities: vec![fetch_ability(ir_and(
            ir_type(CardType::Land),
            ir_subtype("island"),
        ))],
        ..Default::default()
    }), vec![], Some(25))
}

fn scalding_tarn() -> CardDef {
    simple("Scalding Tarn", CardKind::Land(LandData {
        abilities: vec![fetch_ability(ir_and(
            ir_type(CardType::Land),
            ir_subtype("island"),
        ))],
        ..Default::default()
    }), vec![], Some(25))
}

fn marsh_flats() -> CardDef {
    simple("Marsh Flats", CardKind::Land(LandData {
        abilities: vec![fetch_ability(ir_and(
            ir_type(CardType::Land),
            ir_subtype("swamp"),
        ))],
        ..Default::default()
    }), vec![], Some(25))
}

fn bloodstained_mire() -> CardDef {
    simple("Bloodstained Mire", CardKind::Land(LandData {
        abilities: vec![fetch_ability(ir_and(
            ir_type(CardType::Land),
            ir_subtype("swamp"),
        ))],
        ..Default::default()
    }), vec![], Some(25))
}

fn windswept_heath() -> CardDef {
    simple("Windswept Heath", CardKind::Land(LandData {
        abilities: vec![fetch_ability(ir_and(
            ir_type(CardType::Land),
            ir_or(ir_subtype("forest"), ir_subtype("plains")),
        ))],
        ..Default::default()
    }), vec![], Some(25))
}

fn wooded_foothills() -> CardDef {
    simple("Wooded Foothills", CardKind::Land(LandData {
        abilities: vec![fetch_ability(ir_and(
            ir_type(CardType::Land),
            ir_or(ir_subtype("forest"), ir_subtype("mountain")),
        ))],
        ..Default::default()
    }), vec![], Some(25))
}

fn verdant_catacombs() -> CardDef {
    simple("Verdant Catacombs", CardKind::Land(LandData {
        abilities: vec![fetch_ability(ir_and(
            ir_type(CardType::Land),
            ir_or(ir_subtype("forest"), ir_subtype("swamp")),
        ))],
        ..Default::default()
    }), vec![], Some(25))
}

fn arid_mesa() -> CardDef {
    simple("Arid Mesa", CardKind::Land(LandData {
        abilities: vec![fetch_ability(ir_and(
            ir_type(CardType::Land),
            ir_or(ir_subtype("plains"), ir_subtype("mountain")),
        ))],
        ..Default::default()
    }), vec![], Some(25))
}

/// Produces generic mana only (no colored pips). CR 106.
/// Legendary land. ETB: choose a creature type (logged, used for future uncounterable modeling).
/// {T}: Add {C}.
/// {T}: Add one mana of any color (TODO: restrict to spells of the named type; mana is uncounterable).
fn cavern_of_souls() -> CardDef {
    let mut def = CardDef::new(
        "Cavern of Souls",
        CardKind::Land(LandData {
            // {T}: Add {C} — colorless
            // {T}: Add one mana of any color (type restriction and uncounterable not yet modeled)
            mana_abilities: vec![
                ManaAbility {
                    costs: ir_tap_source(),
                    make_effect: std::sync::Arc::new(|who, _| eff_mana(who, "C")),
                    ..Default::default()
                },
                ManaAbility {
                    costs: ir_tap_source(),
                    produces: produces_colors("WUBRG"),
                    make_effect: std::sync::Arc::new(|who, color| {
                        eff_mana(who, color.map(color_to_mana_char).unwrap_or("1"))
                    }),
                    // Colored mana only for creature spells of the named type (CR 106).
                    // Creature-type matching is coarsened to "is creature" since
                    // the sim doesn't track per-card creature subtypes. The
                    // gate is "the spell being cast is a creature" — a Filter
                    // over `GameCtx::CastingSpell`, not the source land.
                    condition: Some(crate::ir::expr::Filter(crate::ir::expr::Expr::Contains(
                        Box::new(crate::ir::expr::Expr::TypeLit(CardType::Creature)),
                        Box::new(crate::ir::expr::Expr::Types(Box::new(
                            crate::ir::expr::Expr::GameCtx(crate::ir::context::GameCtx::CastingSpell),
                        ))),
                    ))),
                    ..Default::default()
                },
            ],
            ..Default::default()
        }),
        vec![],
        Some(50),
        vec![Supertype::Legendary], CardLayout::Normal, None,
        vec![],
        vec![], // ETB creature-type choice is now an IR Replacement (below)
        vec![],
        vec![],
    );
    // ETB (CR 614.12 "as ~ enters, choose a creature type"). Stored in
    // `etb_choice`; the colored-mana ability's type gate is coarsened to
    // "is creature" today, but the named type is recorded for when it tightens.
    def.abilities = vec![etb_choice_replacement(
        crate::ir::action::EtbChoiceKind::CreatureType,
        "As Cavern of Souls enters the battlefield, choose a creature type.",
    )];
    def
}

// ── Artifacts ─────────────────────────────────────────────────────────────────

/// Sacrifice: add one mana of any color. CR 106.3.
fn lotus_petal() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, CostBody};
    use crate::ir::action::{Action, ManaSpec, Who};
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, Filter, ZoneKindSel};
    let mut def = simple("Lotus Petal", CardKind::Artifact(ArtifactData {
        mana_cost: "0".to_string(),
        mana_abilities: vec![],
        ..Default::default()
    }), vec![], Some(25));
    def.abilities.push(Ability {
        kind: AbilityKind::Activated {
            // Sacrifice-self cost on the IR grammar. Filter is `It == Source`
            // — the canonical "this object only" shape.
            cost: CostBody::Ir(Action::Sacrifice {
                who: Who::You,
                filter: Filter(Expr::Eq(
                    Box::new(Expr::Ctx(Ctx::It)),
                    Box::new(Expr::Ctx(Ctx::Source)),
                )),
                count: Expr::Num(1),
                bind_as: None,
            }),
            target_spec: TargetSpec::None,
            choice_spec: None,
            body: Action::AddMana {
                who: Who::You,
                count: Expr::Num(1),
                spec: ManaSpec::AnyOneColor,
            },
            timing: ActivationTiming::Default,
            activation_condition: None,
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("Sacrifice Lotus Petal: Add one mana of any color."),
    });
    def
}

/// Discard your hand, Sacrifice Lion's Eye Diamond: Add three mana of any one color.
/// Activate only as an instant. CR 605.3, CR 601.2g (excluded from mana sub-loop).
fn lions_eye_diamond() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind};
    use crate::ir::action::{Action, ManaSpec, Who};
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel};
    let mut def = simple("Lion's Eye Diamond", CardKind::Artifact(ArtifactData {
        mana_cost: "0".to_string(),
        mana_abilities: vec![],
        ..Default::default()
    }), vec![], Some(10));
    def.abilities.push(Ability {
        kind: AbilityKind::Activated {
            // "Discard your hand, Sacrifice ~". The discard side is modelled
            // as `Action::Discard` with `count = HandSize(Controller)` —
            // the "Discard(All)" composition (per
            // feedback_discard_hand_idiom.md): no separate `DiscardHand`
            // primitive, just a dynamic count over the canonical hand-size
            // expression. The walk emits no decision (dynamic count) and
            // the executor's loop sweeps the hand.
            cost: ir_seq(vec![
                Action::Discard {
                    who: Who::You,
                    count: Expr::HandSize(Box::new(Expr::Ctx(Ctx::Controller))),
                    at_random: false,
                    filter: None,
                },
                act_sac_self("$led_self"),
            ]),
            target_spec: TargetSpec::None,
            choice_spec: None,
            body: Action::AddMana {
                who: Who::You,
                count: Expr::Num(3),
                spec: ManaSpec::AnyOneColor,
            },
            timing: ActivationTiming::Instant,
            activation_condition: None,
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("Discard your hand, Sacrifice: Add three mana of any one color."),
    });
    def
}

/// Mox Opal — Legendary Artifact, {0}.
/// Metalcraft — {T}: Add one mana of any color. Activate only if you control three or more artifacts.
fn mox_opal() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind};
    use crate::ir::action::{Action, ManaSpec, Who};
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel, ZoneSel};
    // Metalcraft: count of artifacts controlled by source's controller >= 3.
    let metalcraft = Expr::Ge(
        Box::new(Expr::Count(Box::new(Expr::AllObjects {
            zone: ZoneSel::Global(ZoneKindSel::Battlefield),
            bind: "a",
            filter: Box::new(Expr::And(
                Box::new(Expr::Eq(
                    Box::new(Expr::Controller(Box::new(Expr::Ctx(Ctx::Var("a"))))),
                    Box::new(Expr::Ctx(Ctx::Controller)),
                )),
                Box::new(Expr::Contains(
                    Box::new(Expr::TypeLit(CardType::Artifact)),
                    Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::Var("a"))))),
                )),
            )),
        }))),
        Box::new(Expr::Num(3)),
    );
    let mut def = simple("Mox Opal", CardKind::Artifact(ArtifactData {
        mana_cost: "0".to_string(),
        mana_abilities: vec![],
        ..Default::default()
    }), vec![], Some(20));
    def.supertypes.push(Supertype::Legendary);
    def.abilities.push(Ability {
        kind: AbilityKind::Activated {
            cost: ir_tap_source(),
            target_spec: TargetSpec::None,
            choice_spec: None,
            body: Action::AddMana {
                who: Who::You,
                count: Expr::Num(1),
                spec: ManaSpec::AnyOneColor,
            },
            timing: ActivationTiming::Default,
            activation_condition: Some(metalcraft),
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("Metalcraft — {T}: Add one mana of any color."),
    });
    def
}

/// Summon: Bahamut — {9} 9/9 flying Enchantment Creature — Saga Dragon.
/// Chapters I/II destroy up to one nonland permanent, III draws two, and IV
/// deals damage equal to the total mana value of your other permanents to each
/// opponent. The generic Saga engine supplies lore timing and sacrifice.
fn summon_bahamut() -> CardDef {
    use crate::ir::action::{Action, Who as IrWho};
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel, ZoneSel};

    let mut creature = CreatureData::new("9", 9, 9);
    creature.creature_subtypes = vec!["Saga".into(), "Dragon".into()];
    creature.keywords = Keywords::from_slice(&[Keyword::Flying]);
    let mut def = simple("Summon: Bahamut", CardKind::Creature(creature), vec![], None);
    if !def.types.contains(&CardType::Enchantment) {
        def.types.push(CardType::Enchantment);
    }

    let nonland_target = TargetSpec::ObjectInZone {
        controller: Who::Any,
        zone: ZoneId::Battlefield,
        filter: ir_not(ir_type(CardType::Land)),
    };

    let your_other_permanents = Expr::AllObjects {
        zone: ZoneSel::Global(ZoneKindSel::Battlefield),
        bind: "bahamut_perm",
        filter: Box::new(Expr::And(
            Box::new(Expr::Eq(
                Box::new(Expr::Controller(Box::new(Expr::Ctx(Ctx::Var("bahamut_perm"))))),
                Box::new(Expr::Ctx(Ctx::Controller)),
            )),
            Box::new(Expr::Not(Box::new(Expr::Eq(
                Box::new(Expr::Ctx(Ctx::Var("bahamut_perm"))),
                Box::new(Expr::Ctx(Ctx::Source)),
            )))),
        )),
    };
    let mega_flare_amount = Expr::SumManaValue(Box::new(your_other_permanents));

    def.chapters = vec![
        Action::Destroy { target: Expr::Ctx(Ctx::Var("target")) },
        Action::Destroy { target: Expr::Ctx(Ctx::Var("target")) },
        Action::Draw { who: IrWho::You, n: Expr::Num(2) },
        Action::ForEach {
            over: Expr::Opponents(Box::new(Expr::Ctx(Ctx::Controller))),
            bind: "bahamut_opp",
            body: Box::new(Action::DealDamage {
                source: Expr::Ctx(Ctx::Source),
                target: Expr::Ctx(Ctx::Var("bahamut_opp")),
                amount: mega_flare_amount,
            }),
        },
    ];
    def.chapter_target_specs = vec![
        nonland_target.clone(),
        nonland_target,
        TargetSpec::None,
        TargetSpec::None,
    ];
    def
}

/// Urza's Saga (CR 714), an Enchantment Land, on the real Saga machinery: a lore
/// counter on entry and each precombat main fires chapters I → II → III, and the
/// SBA sacrifices it after chapter III.


/// Count lands you control with a particular (lowercase) land subtype.
fn controlled_land_subtype_count_expr(bind: &'static str, subtype: &str) -> crate::ir::expr::Expr {
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel, ZoneSel};
    Expr::Count(Box::new(Expr::AllObjects {
        zone: ZoneSel::Global(ZoneKindSel::Battlefield),
        bind,
        filter: Box::new(Expr::And(
            Box::new(Expr::Eq(
                Box::new(Expr::Controller(Box::new(Expr::Ctx(Ctx::Var(bind))))),
                Box::new(Expr::Ctx(Ctx::Controller)),
            )),
            Box::new(Expr::Contains(
                Box::new(Expr::SubtypeLit(subtype.to_ascii_lowercase())),
                Box::new(Expr::Subtypes(Box::new(Expr::Ctx(Ctx::Var(bind))))),
            )),
        )),
    }))
}

fn controlled_artifact_count_expr(bind: &'static str) -> crate::ir::expr::Expr {
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel, ZoneSel};
    Expr::Count(Box::new(Expr::AllObjects {
        zone: ZoneSel::Global(ZoneKindSel::Battlefield),
        bind,
        filter: Box::new(Expr::And(
            Box::new(Expr::Eq(
                Box::new(Expr::Controller(Box::new(Expr::Ctx(Ctx::Var(bind))))),
                Box::new(Expr::Ctx(Ctx::Controller)),
            )),
            Box::new(Expr::Contains(
                Box::new(Expr::TypeLit(CardType::Artifact)),
                Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::Var(bind))))),
            )),
        )),
    }))
}

/// Planar Nexus — all nonbasic land types; {T}: {C}; {1},{T}: one mana of any color.
/// The engine only needs the land types that can matter to implemented cards, but keeps
/// the current named nonbasic land-type family so Tower/Workshop interactions are exact.
fn planar_nexus() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind};
    use crate::ir::action::{Action, ManaSpec, Who};
    use crate::ir::expr::{Expr, ZoneKindSel};
    let mut def = simple(
        "Planar Nexus",
        CardKind::Land(LandData {
            nonbasic_subtypes: [
                "urza's", "mine", "power-plant", "tower", "desert", "gate",
                "lair", "locus", "cave", "sphere",
            ].into_iter().map(str::to_string).collect(),
            ..Default::default()
        }),
        vec![], None,
    );
    def.abilities = vec![
        ir_tap_mana(""),
        Ability {
            kind: AbilityKind::Activated {
                cost: ir_seq(vec![act_pay_mana_str("1"), act_tap_source()]),
                target_spec: TargetSpec::None,
                choice_spec: None,
                body: Action::AddMana { who: Who::You, count: Expr::Num(1), spec: ManaSpec::AnyOneColor },
                timing: ActivationTiming::Default,
                activation_condition: None,
                active_zone: ZoneKindSel::Battlefield,
            },
            text: Some("{1}, {T}: Add one mana of any color."),
        },
    ];
    def
}

/// Urza's Tower — {T}: {C}; if you control an Urza's Mine and Urza's Power-Plant,
/// the same tap produces {C}{C}{C} instead. Planar Nexus satisfies both subtype checks.
fn urzas_tower() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind};
    use crate::ir::action::{Action, ManaSpec, Who};
    use crate::ir::expr::{Expr, ZoneKindSel};
    let assembled = Expr::And(
        Box::new(Expr::Ge(
            Box::new(controlled_land_subtype_count_expr("mine_land", "mine")),
            Box::new(Expr::Num(1)),
        )),
        Box::new(Expr::Ge(
            Box::new(controlled_land_subtype_count_expr("plant_land", "power-plant")),
            Box::new(Expr::Num(1)),
        )),
    );
    let mut def = simple(
        "Urza's Tower",
        CardKind::Land(LandData {
            nonbasic_subtypes: vec!["urza's".into(), "tower".into()],
            ..Default::default()
        }),
        vec![], None,
    );
    def.abilities = vec![
        Ability {
            kind: AbilityKind::Activated {
                cost: ir_tap_source(),
                target_spec: TargetSpec::None,
                choice_spec: None,
                body: Action::AddMana { who: Who::You, count: Expr::Num(3), spec: ManaSpec::Fixed(vec![]) },
                timing: ActivationTiming::Default,
                activation_condition: Some(assembled.clone()),
                active_zone: ZoneKindSel::Battlefield,
            },
            text: Some("{T}: Add {C}{C}{C} if you control an Urza's Mine and an Urza's Power-Plant."),
        },
        Ability {
            kind: AbilityKind::Activated {
                cost: ir_tap_source(),
                target_spec: TargetSpec::None,
                choice_spec: None,
                body: Action::AddMana { who: Who::You, count: Expr::Num(1), spec: ManaSpec::Fixed(vec![]) },
                timing: ActivationTiming::Default,
                activation_condition: Some(Expr::Not(Box::new(assembled))),
                active_zone: ZoneKindSel::Battlefield,
            },
            text: Some("{T}: Add {C}."),
        },
    ];
    def
}

/// Urza's Workshop — ordinary {T}:{C}, plus its metalcraft mana ability.
/// Dynamic mana prediction is represented as mutually-exclusive fixed-count abilities;
/// resolution and the affordability predictor therefore agree on the amount produced.
fn urzas_workshop() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind};
    use crate::ir::action::{Action, ManaSpec, Who};
    use crate::ir::expr::{Expr, ZoneKindSel};
    let artifact_count = controlled_artifact_count_expr("workshop_artifact");
    let urza_count = controlled_land_subtype_count_expr("workshop_urza", "urza's");
    let metalcraft = Expr::Ge(Box::new(artifact_count), Box::new(Expr::Num(3)));

    let mut def = simple(
        "Urza's Workshop",
        CardKind::Land(LandData {
            nonbasic_subtypes: vec!["urza's".into()],
            ..Default::default()
        }),
        vec![], None,
    );
    let mut abilities = vec![ir_tap_mana("")];
    // Legacy decks cannot realistically control more than 30 Urza's lands in this
    // simulator; fixed branches keep the mana predictor exact without a new dynamic
    // predictor API. Count 0 cannot satisfy this ability meaningfully.
    for n in 1..=30i64 {
        let cond = Expr::And(
            Box::new(metalcraft.clone()),
            Box::new(Expr::Eq(Box::new(urza_count.clone()), Box::new(Expr::Num(n)))),
        );
        abilities.push(Ability {
            kind: AbilityKind::Activated {
                cost: ir_tap_source(),
                target_spec: TargetSpec::None,
                choice_spec: None,
                body: Action::AddMana { who: Who::You, count: Expr::Num(n), spec: ManaSpec::Fixed(vec![]) },
                timing: ActivationTiming::Default,
                activation_condition: Some(cond),
                active_zone: ZoneKindSel::Battlefield,
            },
            text: Some("Metalcraft — {T}: Add {C} for each Urza's land you control."),
        });
    }
    def.abilities = abilities;
    def
}

// ── Mystic Forge package ─────────────────────────────────────────────────────

/// Grim Monolith — {2}; does not untap during untap; {T}: {C}{C}{C}; {4}: untap.
fn grim_monolith() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel};
    let mut def = simple("Grim Monolith", CardKind::Artifact(ArtifactData {
        mana_cost: "2".into(), ..Default::default()
    }), vec![], None);
    def.untaps_during_untap_step = false;
    def.abilities = vec![
        ir_tap_mana("CCC"),
        Ability {
            kind: AbilityKind::Activated {
                cost: ir_pay_mana_str("4"),
                target_spec: TargetSpec::None,
                choice_spec: None,
                body: Action::Untap { target: Expr::Ctx(Ctx::Source) },
                timing: ActivationTiming::Default,
                activation_condition: None,
                active_zone: ZoneKindSel::Battlefield,
            },
            text: Some("{4}: Untap Grim Monolith."),
        },
    ];
    def
}

/// Basalt Monolith — {3}; does not untap during untap; {T}: {C}{C}{C}; {3}: untap.
fn basalt_monolith() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel};
    let mut def = simple("Basalt Monolith", CardKind::Artifact(ArtifactData {
        mana_cost: "3".into(), ..Default::default()
    }), vec![], None);
    def.untaps_during_untap_step = false;
    def.abilities = vec![
        ir_tap_mana("CCC"),
        Ability {
            kind: AbilityKind::Activated {
                cost: ir_pay_mana_str("3"),
                target_spec: TargetSpec::None,
                choice_spec: None,
                body: Action::Untap { target: Expr::Ctx(Ctx::Source) },
                timing: ActivationTiming::Default,
                activation_condition: None,
                active_zone: ZoneKindSel::Battlefield,
            },
            text: Some("{3}: Untap Basalt Monolith."),
        },
    ];
    def
}

/// Manifold Key — both activated abilities: untap another artifact, or make an
/// artifact creature unblockable for the turn.
fn manifold_key() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind};
    use crate::ir::action::{Action, Expiry as IrExpiry};
    use crate::ir::ce::CEMod;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, Filter, ZoneKindSel};
    let another_artifact = Filter(Expr::And(
        Box::new(ir_type(CardType::Artifact).0),
        Box::new(Expr::Not(Box::new(Expr::Eq(
            Box::new(Expr::Ctx(Ctx::It)),
            Box::new(Expr::Ctx(Ctx::Source)),
        )))),
    ));
    let artifact_creature = Filter(Expr::And(
        Box::new(ir_type(CardType::Artifact).0),
        Box::new(ir_type(CardType::Creature).0),
    ));
    let mut def = simple("Manifold Key", CardKind::Artifact(ArtifactData {
        mana_cost: "1".into(), ..Default::default()
    }), vec![], None);
    def.abilities = vec![
        Ability {
            kind: AbilityKind::Activated {
                cost: ir_seq(vec![act_pay_mana_str("1"), act_tap_source()]),
                target_spec: TargetSpec::ObjectInZone {
                    controller: Who::Any,
                    zone: ZoneId::Battlefield,
                    filter: another_artifact,
                },
                choice_spec: None,
                body: Action::Untap { target: Expr::Ctx(Ctx::Var("target")) },
                timing: ActivationTiming::Default,
                activation_condition: None,
                active_zone: ZoneKindSel::Battlefield,
            },
            text: Some("{1}, {T}: Untap another target artifact."),
        },
        Ability {
            kind: AbilityKind::Activated {
                cost: ir_seq(vec![act_pay_mana_str("3"), act_tap_source()]),
                target_spec: TargetSpec::ObjectInZone {
                    controller: Who::Actor,
                    zone: ZoneId::Battlefield,
                    filter: artifact_creature,
                },
                choice_spec: None,
                body: Action::ApplyCE {
                    target: Expr::Ctx(Ctx::Var("target")),
                    mods: vec![CEMod::AddKeyword(Keyword::Unblockable)],
                    expiry: IrExpiry::EndOfTurn,
                },
                timing: ActivationTiming::Default,
                activation_condition: None,
                active_zone: ZoneKindSel::Battlefield,
            },
            text: Some("{3}, {T}: Target artifact creature can't be blocked this turn."),
        },
    ];
    def
}

/// Mystic Forge — cast artifact/colorless spells from the top; tap/pay 1 life to exile top.
fn mystic_forge() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind};
    use crate::ir::action::Action;
    use crate::ir::ce::CEMod;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, Filter, ZoneKindSel, ZoneSel};
    let top_of_your_library = Expr::Contains(
        Box::new(Expr::Ctx(Ctx::It)),
        Box::new(Expr::Top {
            zone: ZoneSel::Scoped {
                zone_kind: ZoneKindSel::Library,
                owner: Box::new(Expr::Ctx(Ctx::Controller)),
            },
            n: Box::new(Expr::Num(1)),
        }),
    );
    let owned_library_card = Filter(Expr::And(
        Box::new(top_of_your_library),
        Box::new(Expr::Or(
            Box::new(ir_type(CardType::Artifact).0),
            Box::new(ir_colorless().0),
        )),
    ));
    let mut def = simple("Mystic Forge", CardKind::Artifact(ArtifactData {
        mana_cost: "4".into(), ..Default::default()
    }), vec![], None);
    def.abilities = vec![
        Ability {
            kind: AbilityKind::Static {
                mods: vec![CEMod::CastableFrom(ZoneKindSel::Library)],
                scope: Some(owned_library_card),
                condition: None,
            },
            text: Some("You may cast artifact spells and colorless spells from the top of your library."),
        },
        Ability {
            kind: AbilityKind::Activated {
                cost: ir_seq(vec![act_tap_source(), act_pay_life(1)]),
                target_spec: TargetSpec::None,
                choice_spec: None,
                body: Action::Move {
                    what: Expr::Top {
                        zone: ZoneSel::Scoped {
                            zone_kind: ZoneKindSel::Library,
                            owner: Box::new(Expr::Ctx(Ctx::Controller)),
                        },
                        n: Box::new(Expr::Num(1)),
                    },
                    to: ZoneKindSel::Exile,
                    to_owner: None,
                    bind_as: None,
                },
                timing: ActivationTiming::Default,
                activation_condition: None,
                active_zone: ZoneKindSel::Battlefield,
            },
            text: Some("{T}, Pay 1 life: Exile the top card of your library."),
        },
    ];
    def
}

/// The One Ring — indestructible, cast-ETB protection, burden upkeep, burden draw engine.
fn the_one_ring() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, StepScope, TriggerSpec};
    use crate::ir::action::{Action, Who as IrWho};
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel};

    let protection_trigger = etb_self_trigger("The One Ring", TargetSpec::None, |source_id, controller| {
        Effect(Arc::new(move |state, _t, _targets| {
            let was_cast = state.event_log.entries.iter().any(|e| {
                matches!(e.event, GameEvent::SpellCast { card_id, caster, .. }
                    if card_id == source_id && caster == controller)
            });
            if was_cast {
                state.player_mut(controller).protection_from_everything = true;
            }
        }))
    });

    let mut def = CardDef::new(
        "The One Ring",
        CardKind::Artifact(ArtifactData { mana_cost: "4".into(), ..Default::default() }),
        vec![], None, vec![Supertype::Legendary], CardLayout::Normal, None,
        vec![protection_trigger], vec![], vec![], vec![],
    );
    def.indestructible = true;
    def.abilities = vec![
        Ability {
            kind: AbilityKind::Triggered {
                spec: TriggerSpec::AtStep { step: StepKind::Upkeep, who: StepScope::You, condition: None },
                target_spec: TargetSpec::None,
                body: Action::PayLife {
                    who: IrWho::You,
                    amount: Expr::CountersOn(Box::new(Expr::Ctx(Ctx::Source)), CounterType::Burden),
                },
                active_zone: ZoneKindSel::Battlefield,
            },
            text: Some("At the beginning of your upkeep, you lose 1 life for each burden counter on The One Ring."),
        },
        Ability {
            kind: AbilityKind::Activated {
                cost: ir_tap_source(),
                target_spec: TargetSpec::None,
                choice_spec: None,
                body: Action::Sequence(vec![
                    Action::PutCounters { on: Expr::Ctx(Ctx::Source), kind: CounterType::Burden, n: Expr::Num(1) },
                    Action::Draw {
                        who: IrWho::You,
                        n: Expr::CountersOn(Box::new(Expr::Ctx(Ctx::Source)), CounterType::Burden),
                    },
                ]),
                timing: ActivationTiming::Default,
                activation_condition: None,
                active_zone: ZoneKindSel::Battlefield,
            },
            text: Some("{T}: Put a burden counter on The One Ring, then draw a card for each burden counter on it."),
        },
    ];
    def
}

/// Relic of Sauron — {T}: add two mana in any combination of U/B/R;
/// plus {3},{T}: draw two, discard one.
fn relic_of_sauron() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind};
    use crate::ir::action::{Action, Who as IrWho};
    use crate::ir::expr::{Expr, ZoneKindSel};

    fn fixed_relic_mana(spec: &'static str) -> ManaAbility {
        let colors = produces_colors(spec);
        ManaAbility {
            costs: ir_tap_source(),
            produces: colors,
            produces_count: 2,
            make_effect: Arc::new(move |who, _| eff_mana(who, spec)),
            ..Default::default()
        }
    }

    // Six fixed modes exactly cover "two mana in any combination of U/B/R".
    // The generic mana planner prefers the mode that satisfies the largest
    // number of currently-required colored pips.
    let mana_abilities = vec![
        fixed_relic_mana("UU"),
        fixed_relic_mana("UB"),
        fixed_relic_mana("UR"),
        fixed_relic_mana("BB"),
        fixed_relic_mana("BR"),
        fixed_relic_mana("RR"),
    ];

    let mut def = simple("Relic of Sauron", CardKind::Artifact(ArtifactData {
        mana_cost: "4".into(), mana_abilities, ..Default::default()
    }), vec![], None);
    def.abilities = vec![Ability {
        kind: AbilityKind::Activated {
            cost: ir_seq(vec![act_pay_mana_str("3"), act_tap_source()]),
            target_spec: TargetSpec::None,
            choice_spec: None,
            body: Action::Sequence(vec![
                Action::Draw { who: IrWho::You, n: Expr::Num(2) },
                Action::Discard { who: IrWho::You, count: Expr::Num(1), at_random: false, filter: None },
            ]),
            timing: ActivationTiming::Default,
            activation_condition: None,
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("{3}, {T}: Draw two cards, then discard a card."),
    }];
    def
}

/// Giant's Boulder — ETB scry 2; color fixing; expensive Vindicate activation.
fn giant_boulder() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind};
    use crate::ir::action::{Action, ManaSpec, Who as IrWho};
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel};
    let etb = etb_self_trigger("Giant's Boulder", TargetSpec::None, |_, controller| eff_scry(controller, 2));
    let mut def = CardDef::new(
        "Giant's Boulder",
        CardKind::Artifact(ArtifactData { mana_cost: "1".into(), ..Default::default() }),
        vec![], None, vec![], CardLayout::Normal, None,
        vec![etb], vec![], vec![], vec![],
    );
    def.abilities = vec![
        Ability {
            kind: AbilityKind::Activated {
                cost: ir_seq(vec![act_pay_mana_str("1"), act_tap_source()]),
                target_spec: TargetSpec::None,
                choice_spec: None,
                body: Action::AddMana { who: IrWho::You, count: Expr::Num(1), spec: ManaSpec::AnyOneColor },
                timing: ActivationTiming::Default,
                activation_condition: None,
                active_zone: ZoneKindSel::Battlefield,
            },
            text: Some("{1}, {T}: Add one mana of any color."),
        },
        Ability {
            kind: AbilityKind::Activated {
                cost: ir_seq(vec![act_pay_mana_str("7"), act_tap_source(), act_sac_self("$boulder")]),
                target_spec: TargetSpec::ObjectInZone { controller: Who::Any, zone: ZoneId::Battlefield, filter: ir_any() },
                choice_spec: None,
                body: Action::Destroy { target: Expr::Ctx(Ctx::Var("target")) },
                timing: ActivationTiming::Default,
                activation_condition: None,
                active_zone: ZoneKindSel::Battlefield,
            },
            text: Some("{7}, {T}, Sacrifice Giant's Boulder: Destroy target permanent."),
        },
    ];
    def
}

/// Paradox Engine — spell-cast trigger untaps all nonland permanents you control.
fn paradox_engine() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, EventPattern, TriggerSpec};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel, ZoneSel};
    let mut def = CardDef::new(
        "Paradox Engine",
        CardKind::Artifact(ArtifactData { mana_cost: "5".into(), ..Default::default() }),
        vec![], None, vec![Supertype::Legendary], CardLayout::Normal, None,
        vec![], vec![], vec![], vec![],
    );
    let your_spell = crate::ir::expr::Filter(Expr::Eq(
        Box::new(Expr::Controller(Box::new(Expr::Ctx(Ctx::It)))),
        Box::new(Expr::Ctx(Ctx::Controller)),
    ));
    let your_nonlands = Expr::AllObjects {
        zone: ZoneSel::Global(ZoneKindSel::Battlefield),
        bind: "pe",
        filter: Box::new(Expr::And(
            Box::new(Expr::Eq(
                Box::new(Expr::Controller(Box::new(Expr::Ctx(Ctx::Var("pe"))))),
                Box::new(Expr::Ctx(Ctx::Controller)),
            )),
            Box::new(Expr::Not(Box::new(Expr::Contains(
                Box::new(Expr::TypeLit(CardType::Land)),
                Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::Var("pe"))))),
            )))),
        )),
    };
    def.abilities = vec![Ability {
        kind: AbilityKind::Triggered {
            spec: TriggerSpec::When { pattern: EventPattern::SpellCast { spell_filter: your_spell }, condition: None },
            target_spec: TargetSpec::None,
            body: Action::Untap { target: your_nonlands },
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("Whenever you cast a spell, untap all nonland permanents you control."),
    }];
    def
}


/// Soul-Guide Lantern — ETB single-card exile plus the two sacrifice modes.
fn soul_guide_lantern() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind};
    use crate::ir::action::{Action, Who as IrWho};
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel, ZoneSel};

    let grave_target = TargetSpec::ObjectInZone {
        controller: Who::Any,
        zone: ZoneId::Graveyard,
        filter: ir_any(),
    };
    let etb = etb_self_trigger("Soul-Guide Lantern", grave_target, |_, _| {
        Effect(Arc::new(|state, t, targets| {
            if let Some(&id) = targets.first() {
                let ctrl = state.objects.get(&id).map(|o| o.controller).unwrap_or(PlayerId::Us);
                change_zone(id, ZoneId::Exile, state, t, ctrl);
            }
        }))
    });
    let mut def = CardDef::new(
        "Soul-Guide Lantern",
        CardKind::Artifact(ArtifactData { mana_cost: "1".into(), ..Default::default() }),
        vec![], None, vec![], CardLayout::Normal, None,
        vec![etb], vec![], vec![], vec![],
    );
    def.abilities = vec![
        Ability {
            kind: AbilityKind::Activated {
                cost: ir_seq(vec![act_tap_source(), act_sac_self("$lantern")]),
                target_spec: TargetSpec::None,
                choice_spec: None,
                body: Action::Move {
                    what: Expr::AllObjects {
                        zone: ZoneSel::Scoped {
                            zone_kind: ZoneKindSel::Graveyard,
                            owner: Box::new(Expr::Ctx(Ctx::Var("lantern_opp"))),
                        },
                        bind: "lantern_card",
                        filter: Box::new(Expr::Bool(true)),
                    },
                    to: ZoneKindSel::Exile,
                    to_owner: None,
                    bind_as: None,
                },
                timing: ActivationTiming::Default,
                activation_condition: None,
                active_zone: ZoneKindSel::Battlefield,
            },
            text: Some("{T}, Sacrifice Soul-Guide Lantern: Exile each opponent's graveyard."),
        },
        Ability {
            kind: AbilityKind::Activated {
                cost: ir_seq(vec![act_pay_mana_str("1"), act_tap_source(), act_sac_self("$lantern")]),
                target_spec: TargetSpec::None,
                choice_spec: None,
                body: Action::Draw { who: IrWho::You, n: Expr::Num(1) },
                timing: ActivationTiming::Default,
                activation_condition: None,
                active_zone: ZoneKindSel::Battlefield,
            },
            text: Some("{1}, {T}, Sacrifice Soul-Guide Lantern: Draw a card."),
        },
    ];
    // The first activated mode's opponent graveyard set needs the opponent player
    // binding, which the current generic Action environment does not inject. Keep
    // the exact exile-all behavior in a legacy ability alongside the draw mode.
    let exile_factory: AbilityFactory = Arc::new(|controller, _source_id| {
        Effect(Arc::new(move |state, t, _targets| {
            let opp = controller.opp();
            let ids: Vec<ObjId> = state.graveyard_of(opp).map(|o| o.id).collect();
            for id in ids { change_zone(id, ZoneId::Exile, state, t, controller); }
        }))
    });
    if let CardKind::Artifact(a) = &mut def.kind {
        a.abilities.push(AbilityDef {
            costs: ir_seq(vec![act_tap_source(), act_sac_self("$lantern_legacy")]),
            ability_factory: Some(exile_factory),
            ..Default::default()
        });
    }
    // Avoid exposing the unbound IR copy of the exile-all mode.
    def.abilities.remove(0);
    def
}

/// Tormod's Crypt — {T}, sacrifice: exile target player's graveyard.
fn tormods_crypt() -> CardDef {
    let factory: AbilityFactory = Arc::new(|controller, _source_id| {
        Effect(Arc::new(move |state, t, targets| {
            let target_player = targets.first().copied()
                .filter(|id| state.is_player(*id))
                .map(|id| state.who_pid(id))
                .unwrap_or(controller.opp());
            let ids: Vec<ObjId> = state.graveyard_of(target_player).map(|o| o.id).collect();
            for id in ids { change_zone(id, ZoneId::Exile, state, t, controller); }
        }))
    });
    simple(
        "Tormod's Crypt",
        CardKind::Artifact(ArtifactData {
            mana_cost: "0".into(),
            abilities: vec![AbilityDef {
                costs: ir_seq(vec![act_tap_source(), act_sac_self("$crypt")]),
                target_spec: TargetSpec::Union(vec![TargetSpec::Player(Who::Actor), TargetSpec::Player(Who::Opp)]),
                ability_factory: Some(factory),
                ..Default::default()
            }],
            ..Default::default()
        }),
        vec![], None,
    )
}

/// Mycosynth Lattice — the Forge/Karn-relevant part: every battlefield permanent
/// is an artifact in addition to its other types. This makes Karn's static ability
/// shut off activated abilities of opposing permanents as in the real lock.
fn mycosynth_lattice() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind};
    use crate::ir::ce::CEMod;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, Filter};
    let mut def = simple(
        "Mycosynth Lattice",
        CardKind::Artifact(ArtifactData { mana_cost: "6".into(), ..Default::default() }),
        vec![], None,
    );
    // Only permanents become artifacts.  Cards in other zones do *not* become
    // artifacts, which matters for Mystic Forge's top-of-library permission.
    let battlefield_only = Some(Filter(Expr::Eq(
        Box::new(Expr::ZoneOf(Box::new(Expr::Ctx(Ctx::It)))),
        Box::new(Expr::ZoneLit(ZoneId::Battlefield)),
    )));
    def.abilities = vec![
        Ability {
            kind: AbilityKind::Static {
                mods: vec![CEMod::AddType(CardType::Artifact)],
                scope: battlefield_only,
                condition: None,
            },
            text: Some("All permanents are artifacts in addition to their other types."),
        },
        Ability {
            kind: AbilityKind::Static {
                mods: vec![CEMod::SetColors(vec![])],
                scope: None,
                condition: None,
            },
            text: Some("All cards, spells, and permanents are colorless."),
        },
    ];
    def
}

/// Ensnaring Bridge — attackers with power greater than their controller's hand
/// size are prohibited at declaration time.
fn ensnaring_bridge() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, ActionKind};
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, Filter};
    let subject = Filter(Expr::Gt(
        Box::new(Expr::Power(Box::new(Expr::Ctx(Ctx::It)))),
        Box::new(Expr::HandSize(Box::new(Expr::Controller(Box::new(Expr::Ctx(Ctx::It)))))),
    ));
    let mut def = simple(
        "Ensnaring Bridge",
        CardKind::Artifact(ArtifactData { mana_cost: "3".into(), ..Default::default() }),
        vec![], None,
    );
    def.abilities = vec![Ability {
        kind: AbilityKind::Restriction { action: ActionKind::Attack, subject },
        text: Some("Creatures with power greater than the number of cards in their controller's hand can't attack."),
    }];
    def
}


/// Portable Hole — ETB exile of an opponent's nonland permanent with MV <= 2,
/// linked to this Hole so the card returns if the Hole leaves the battlefield.
fn portable_hole() -> CardDef {
    let filter = ir_and(
        ir_not(ir_type(CardType::Land)),
        ir_mv_le(2),
    );
    let etb = etb_self_trigger(
        "Portable Hole",
        TargetSpec::ObjectInZone {
            controller: Who::Opp,
            zone: ZoneId::Battlefield,
            filter,
        },
        |source_id, controller| Effect(Arc::new(move |state, t, targets| {
            if let Some(&id) = targets.first() {
                change_zone(id, ZoneId::Exile, state, t, controller);
                if state.objects.get(&id).map_or(false, |o| o.in_zone(Zone::Exile { on_adventure: false })) {
                    state.linked_exile.entry(source_id).or_default().push(id);
                }
            }
        })),
    );
    CardDef::new(
        "Portable Hole",
        CardKind::Artifact(ArtifactData { mana_cost: "W".into(), ..Default::default() }),
        parse_colors("W", true, false), None, vec![], CardLayout::Normal, None,
        vec![etb], vec![], vec![], vec![],
    )
}

/// Urza's Saga uses the generic Saga machinery. Chapter I grants {T}: Add {C};
/// chapter II grants the Construct-making ability; chapter III searches for a
/// colorless artifact with mana value 0 or 1. The Construct token has a live
/// artifact-count P/T characteristic, so later artifact changes update it.
fn ursas_saga() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, CostBody};
    use crate::ir::action::{Action, Expiry as IrExpiry, ManaSpec, Who as IrWho};
    use crate::ir::ce::CEMod;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel};
    let mut def = CardDef::new(
        "Urza's Saga",
        CardKind::Land(LandData::default()),
        vec![], // colorless
        None,
        vec![], CardLayout::Normal, None,
        vec![], vec![], vec![], vec![],
    );
    def.types = vec![CardType::Land, CardType::Enchantment];
    def.chapters = vec![
        // I: "Urza's Saga gains '{T}: Add {C}.'" Granted as a real activated
        // ability that classifies (CR 605.1a) as a mana ability, while it's on
        // the battlefield.
        Action::ApplyCE {
            target: Expr::Ctx(Ctx::Source),
            mods: vec![CEMod::GrantAbility(Box::new(Ability {
                kind: AbilityKind::Activated {
                    cost: CostBody::Ir(Action::Tap { target: Expr::Ctx(Ctx::Source) }),
                    target_spec: TargetSpec::None,
                    choice_spec: None,
                    body: Action::AddMana { who: IrWho::You, count: Expr::Num(1), spec: ManaSpec::Fixed(vec![]) },
                    timing: ActivationTiming::Default,
                    activation_condition: None,
                    active_zone: ZoneKindSel::Battlefield,
                },
                text: Some("{T}: Add {C}."),
            }))],
            expiry: IrExpiry::WhileSourcePresent,
        },
        // II: gains "{2}, {T}: Create a 0/0 colorless Construct artifact creature
        // token with 'This creature gets +1/+1 for each artifact you control.'"
        // The token's CDA is modeled on the token definition below, so it remains
        // live as artifact count changes rather than snapshotting at creation.
        Action::ApplyCE {
            target: Expr::Ctx(Ctx::Source),
            mods: vec![CEMod::GrantAbility(Box::new(Ability {
                kind: AbilityKind::Activated {
                    cost: ir_seq(vec![act_pay_mana_str("2"), act_tap_source()]),
                    target_spec: TargetSpec::None,
                    choice_spec: None,
                    body: Action::CreateToken {
                        who: IrWho::You,
                        spec: crate::ir::action::TokenSpec {
                            name: "Construct Token",
                            types: vec![CardType::Artifact, CardType::Creature],
                            subtypes: vec!["Construct"],
                            colors: vec![],
                            power: Some(0),
                            toughness: Some(0),
                            keywords: vec![],
                        },
                        n: Expr::Num(1),
                        bind_as: None,
                    },
                    timing: ActivationTiming::Default,
                    activation_condition: None,
                    active_zone: ZoneKindSel::Battlefield,
                },
                text: Some("{2}, {T}: Create a Construct artifact creature token."),
            }))],
            expiry: IrExpiry::WhileSourcePresent,
        },
        Action::Search {
            who: IrWho::You,
            zone: ZoneKindSel::Library,
            filter: ir_and(ir_type(CardType::Artifact), ir_and(ir_colorless(), ir_mv_le(1))),
            count: Expr::Num(1),
            dest: ZoneKindSel::Battlefield,
            to_top: false,
            shuffle: true,
            bind_as: None,
        },
    ];
    def
}

/// {1}. Static: creature cards in graveyards and libraries can't enter the battlefield.
/// Players can't cast spells from graveyards or libraries.
/// Two static effects while on the battlefield:
///   (a) CR 614.17 prohibition: creature cards from graveyards/libraries can't enter the BF.
///       Implemented as a `ProhibitionDef` — checked in `fire_event` before replacements.
///   (b) Static CE sets `castable = false` on all cards in graveyard/library zones.
/// Sunburst: enters with a charge counter for each distinct color of mana spent to cast it.
/// Modeled via the IR `PayManaX` additional cost (strategy declares the intended
/// distinct-color count as chosen_x; the engine pays that many generic mana). The
/// ETB replacement reads `resolving_costs_ctx.chosen_x` and places that many Charge counters.
/// {2}, Sacrifice: destroy each nonland permanent with MV equal to the charge count.
/// CR 702.43 sunburst, CR 701.7 destroy.
fn engineered_explosives() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, CostBody, EventPattern, ReplacementBody};
    use crate::ir::action::{Action, MoveVerb};
    use crate::ir::context::{Ctx, EventField};
    use crate::ir::expr::{Expr, Filter, ZoneKindSel};
    let mut def = CardDef::new(
        "Engineered Explosives",
        CardKind::Artifact(ArtifactData {
            mana_cost: "0".to_string(),
            abilities: vec![], // "{2}, Sacrifice ~" is now an IR Activated ability (below)
            ..Default::default()
        }),
        vec![],
        None,
        vec![], CardLayout::Normal, None,
        vec![],
        vec![], // sunburst ETB is now an IR Replacement (below)
        vec![],
        vec![],
    );
    def.additional_costs = ir_xmana_cost();
    // Sunburst (CR 702.43): EE enters with a charge counter for each color of
    // mana spent — modelled here as the announced X. A self-entry Replacement
    // re-does the entry (`Move`) then places the counters, reading X off EE's
    // own logged cast via `ThisCast(X)` (CR 614.1c "enters with counters").
    def.abilities = vec![Ability {
        kind: AbilityKind::Replacement {
            matches: EventPattern::EntersZone {
                obj_filter: ir_self(),
                zone_kind: ZoneKindSel::Battlefield,
            },
            condition: None,
            body: ReplacementBody::Replace(Action::Sequence(vec![
                Action::Move {
                    what: Expr::Ctx(Ctx::Var("triggered_obj")),
                    to: ZoneKindSel::Battlefield,
                    to_owner: None,
                    bind_as: None,
                },
                Action::PutCounters {
                    on: Expr::Ctx(Ctx::Var("triggered_obj")),
                    kind: CounterType::Charge,
                    n: Expr::Ctx(Ctx::ThisCast(EventField::X)),
                },
            ])),
            active_zone: None, // self-entry replacement
        },
        text: Some("Engineered Explosives enters with a charge counter on it for each color of mana spent to cast it."),
    }];
    // "{2}, Sacrifice Engineered Explosives: Destroy each nonland permanent with
    //  mana value equal to the number of charge counters on it." EE is sacrificed as
    //  a cost, so its charge counters persist in the objects map and are read at
    //  resolution via `CountersOn(Source)`.
    def.abilities.push(Ability {
        kind: AbilityKind::Activated {
            cost: CostBody::Ir(Action::Sequence(vec![
                Action::PayMana(parse_mana_cost("2")),
                Action::MoveByChoice {
                    who: crate::ir::action::Who::You,
                    from: ZoneKindSel::Battlefield,
                    to: ZoneKindSel::Graveyard,
                    verb: MoveVerb::Sacrifice,
                    filter: Filter(Expr::Eq(
                        Box::new(Expr::Ctx(Ctx::It)),
                        Box::new(Expr::Ctx(Ctx::Source)),
                    )),
                    count: Expr::Num(1),
                    bind_as: Some("$ee_self"),
                },
            ])),
            target_spec: TargetSpec::None,
            choice_spec: None,
            body: ir_for_each_on_battlefield(
                ir_and(
                    ir_not(ir_type(CardType::Land)),
                    ir_mv_eq_expr(Expr::CountersOn(
                        Box::new(Expr::Ctx(Ctx::Source)), CounterType::Charge)),
                ),
                Action::Destroy { target: Expr::Ctx(Ctx::Var("v")) },
            ),
            timing: ActivationTiming::Default,
            activation_condition: None,
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("{2}, Sacrifice Engineered Explosives: Destroy each nonland permanent with mana value equal to the number of charge counters on Engineered Explosives."),
    });
    def
}

fn grafdiggers_cage() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, ActionKind, EventPattern};
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, Filter, ZoneKindSel};

    let mut card = CardDef::new(
        "Grafdigger's Cage",
        CardKind::Artifact(ArtifactData {
            mana_cost: "1".to_string(),
            ..Default::default()
        }),
        vec![],
        Some(40),
        vec![], CardLayout::Normal, None,
        vec![], // no trigger_defs
        vec![], // no replacements
        vec![], // "creatures can't ETB from GY/library" is now an IR Prohibition (below)
        vec![], // "can't cast from GY/library" is now an IR Restriction (below)
    );

    // (a) "Creatures can't enter the battlefield from graveyards or libraries."
    // An IR Prohibition consulted in the event pipeline (fire_event Stage 1): it
    // matches a ZoneChange to the battlefield whose source zone is GY *or* library,
    // for a creature card, and suppresses it (CR 614.17 "can't"). `active_zone`
    // Battlefield gates it to while the Cage is in play; the `Or` keeps the two
    // source zones as one CR ability.
    let creature_obj = || Filter(Expr::Contains(
        Box::new(Expr::TypeLit(CardType::Creature)),
        Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::It)))),
    ));
    let cant_etb = Ability {
        kind: AbilityKind::Prohibition {
            matches: EventPattern::Or(vec![
                EventPattern::ZoneChange {
                    obj_filter: creature_obj(),
                    from: ZoneKindSel::Graveyard,
                    to: ZoneKindSel::Battlefield,
                    actor_filter: None,
                },
                EventPattern::ZoneChange {
                    obj_filter: creature_obj(),
                    from: ZoneKindSel::Library,
                    to: ZoneKindSel::Battlefield,
                    actor_filter: None,
                },
            ]),
            active_zone: Some(ZoneKindSel::Battlefield),
        },
        text: Some("Creatures can't enter the battlefield from graveyards or libraries."),
    };

    // (b) "Players can't cast spells from graveyards or libraries."
    // An action-Restriction consulted at legal-cast enumeration (AND-NOT over
    // castable → "can't beats can", CR 101.2). Zone-scoped subject: any card whose
    // current zone is GY or library — so a Dauthi exile-cast (exile ≠ GY/library)
    // falls out naturally, while flashback (GY) is correctly forbidden.
    let zone_of_it = || Expr::ZoneOf(Box::new(Expr::Ctx(Ctx::It)));
    let cant_cast = Ability {
        kind: AbilityKind::Restriction {
            action: ActionKind::Cast,
            subject: Filter(Expr::Or(
                Box::new(Expr::Eq(
                    Box::new(zone_of_it()),
                    Box::new(Expr::ZoneLit(ZoneId::Graveyard)),
                )),
                Box::new(Expr::Eq(
                    Box::new(zone_of_it()),
                    Box::new(Expr::ZoneLit(ZoneId::Library)),
                )),
            )),
        },
        text: Some("Players can't cast spells from graveyards or libraries."),
    };

    card.abilities = vec![cant_etb, cant_cast];
    card
}

// ── Instants ──────────────────────────────────────────────────────────────────

/// Draw 3, put back 2 (evaluator-driven: puts back the two worst cards).
/// CR 420 (draw), CR 701.26 (library manipulation).
fn brainstorm() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::{Action, Who as IrWho};
    use crate::ir::expr::{Expr, ZoneKindSel};

    let mut card = simple("Brainstorm", CardKind::Instant(SpellData {
        mana_cost: "U".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("U", false, false), None);
    card.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::None,
                body: Action::Sequence(vec![
                    Action::Draw { who: IrWho::You, n: Expr::Num(3) },
                    Action::PutOnLibrary {
                        who: IrWho::You,
                        count: Expr::Num(2),
                        from: ZoneKindSel::Hand,
                        top: true,
                    },
                ]),
            }],
        },
        text: Some("Draw three cards, then put two cards from your hand on top of your library in any order."),
    }];
    card
}

/// Surveil 1, then draw 1. CR 701.43 (surveil).
fn consider() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::{Action, Who as IrWho};
    use crate::ir::expr::Expr;
    let mut card = simple("Consider", CardKind::Instant(SpellData {
        mana_cost: "U".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("U", false, false), None);
    card.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::None,
                body: Action::Sequence(vec![
                    Action::Surveil { who: IrWho::You, n: Expr::Num(1) },
                    Action::Draw { who: IrWho::You, n: Expr::Num(1) },
                ]),
            }],
        },
        text: Some("Surveil 1, then draw a card."),
    }];
    card
}

/// Counter target spell. Alternate costs: bounce a blue-producing island (free),
/// or pay {1U} (20% probability). CR 701.5.
/// "Counter target spell unless its controller pays {1}."
fn daze() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::{Action, MoveVerb};
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, Filter, ZoneKindSel};
    let mut c = simple("Daze", CardKind::Instant(SpellData {
        mana_cost: "1U".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("1U", true, false), None);
    c.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                // Who::Any: real Daze counters "target spell" (any). The deck also self-targets
                // (Daze your own Lotus Petal) to advance the per-turn noncreature-spell count.
                target_spec: TargetSpec::ObjectInZone { controller: Who::Any, zone: ZoneId::Stack, filter: ir_spell() },
                body: counter_unless_pays_body(parse_mana_cost("1")),
            }],
        },
        text: Some("Counter target spell unless its controller pays {1}."),
    }];
    // Phase 4 step 5 (alt-cost migration): "Return an Island you control to
    // its owner's hand." First card to actually flow through `pay_ir_cost`
    // at runtime. The schema decision is bound to "$daze_island" and the
    // executor reads that binding to know which permanent to bounce.
    let island_filter = Filter(Expr::Contains(
        Box::new(Expr::SubtypeLit("island".to_string())),
        Box::new(Expr::Subtypes(Box::new(Expr::Ctx(Ctx::It)))),
    ));
    c.alternate_costs = vec![
        AlternateCost {
            costs: CostBody::Ir(Action::MoveByChoice {
                who: crate::ir::action::Who::You,
                from: ZoneKindSel::Battlefield,
                to: ZoneKindSel::Hand,
                verb: MoveVerb::Return,
                filter: island_filter,
                count: Expr::Num(1),
                bind_as: Some("$daze_island"),
            }),
            ..Default::default()
        },
    ];
    c
}

/// Filter: "a card of the given color in hand other than the source" — the
/// canonical pitch-cost shape used by FoW, FoN, Fury, and similar Modern
/// Horizons / Mercadian Masques pitch cards. Excluding `Source` enforces
/// "you can't pitch the spell to itself."
fn pitch_color_filter(color: Color) -> crate::ir::expr::Filter {
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, Filter};
    Filter(Expr::And(
        Box::new(Expr::Not(Box::new(Expr::Eq(
            Box::new(Expr::Ctx(Ctx::It)),
            Box::new(Expr::Ctx(Ctx::Source)),
        )))),
        Box::new(Expr::Contains(
            Box::new(Expr::ColorLit(color)),
            Box::new(Expr::Colors(Box::new(Expr::Ctx(Ctx::It)))),
        )),
    ))
}

fn pitch_blue_filter() -> crate::ir::expr::Filter {
    pitch_color_filter(Color::Blue)
}

/// Counter target noncreature spell. Pitch cost (exile a blue card) only available when it's
/// not your turn; the countered spell is exiled via a scoped replacement (CR 118.9b, 614.1a).
fn force_of_negation() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::{Action, MoveVerb};
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel};
    let mut c = simple("Force of Negation", CardKind::Instant(SpellData {
        mana_cost: "1UU".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("1UU", true, false), None);
    // "Counter target noncreature spell." This engine exiles the countered spell
    // instead of leaving it in the graveyard (deliberate model with tests):
    // `Counter` sends it to the graveyard, then `Exile` moves it on to exile.
    c.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::ObjectInZone {
                    controller: Who::Opp,
                    zone: ZoneId::Stack,
                    filter: ir_and(ir_spell(), ir_not(ir_type(CardType::Creature))),
                },
                body: Action::Sequence(vec![
                    Action::Counter { target: Expr::Ctx(Ctx::Var("target")) },
                    Action::Exile { target: Expr::Ctx(Ctx::Var("target")), bind_as: None },
                ]),
            }],
        },
        text: Some("Counter target noncreature spell. (Exiled rather than put into the graveyard.)"),
    }];
    // Phase 4 step 5 follow-up: pitch alt cost migrated to MoveByChoice
    // (Hand → Exile, verb=Exile). The hand_min and condition gates are
    // unchanged — those live on AlternateCost, not the cost tree.
    c.alternate_costs = vec![
        AlternateCost {
            costs: CostBody::Ir(Action::MoveByChoice {
                who: crate::ir::action::Who::You,
                from: ZoneKindSel::Hand,
                to: ZoneKindSel::Exile,
                verb: MoveVerb::Exile,
                filter: pitch_blue_filter(),
                count: Expr::Num(1),
                bind_as: Some("$fon_pitch"),
            }),
            hand_min: 2,
            condition: Some(std::sync::Arc::new(|caster, state| {
                state.current_ap != state.player_id(caster)
            })),
            ..Default::default()
        },
    ];
    c
}

/// Counter target spell. Alternate costs: exile a blue card from hand + pay 1 life (pitch),
/// or pay {3UU} (hard cost, rare). CR 702.14 (pitch cost), CR 701.5.
fn force_of_will() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::{Action, MoveVerb};
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel};
    let mut c = simple("Force of Will", CardKind::Instant(SpellData {
        mana_cost: "3UU".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("3UU", true, false), None);
    c.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::ObjectInZone {
                    // Who::Any: real Force of Will counters "target spell" (any), including
                    // your own (pitch-fix / advancing a per-turn spell count).
                    controller: Who::Any,
                    zone: ZoneId::Stack,
                    filter: ir_any(),
                },
                body: Action::Counter { target: Expr::Ctx(Ctx::Var("target")) },
            }],
        },
        text: Some("Counter target spell."),
    }];
    // Phase 4 step 5 follow-up: pitch alt cost migrated to a Sequence of
    // MoveByChoice (hand → exile) and PayLife(1).
    c.alternate_costs = vec![
        AlternateCost {
            costs: CostBody::Ir(Action::Sequence(vec![
                Action::MoveByChoice {
                    who: crate::ir::action::Who::You,
                    from: ZoneKindSel::Hand,
                    to: ZoneKindSel::Exile,
                    verb: MoveVerb::Exile,
                    filter: pitch_blue_filter(),
                    count: Expr::Num(1),
                    bind_as: Some("$fow_pitch"),
                },
                Action::PayLife {
                    who: crate::ir::action::Who::You,
                    amount: Expr::Num(1),
                },
            ])),
            hand_min: 2,
            ..Default::default()
        },
    ];
    c
}

/// Add {B}{B}{B}. CR 106.3.
fn dark_ritual() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::{Action, ManaSpec, Who as IrWho};
    use crate::ir::expr::Expr;
    let mut card = simple("Dark Ritual", CardKind::Instant(SpellData {
        mana_cost: "B".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("B", false, false), None);
    card.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::None,
                body: Action::AddMana {
                    who: IrWho::You,
                    count: Expr::Num(3),
                    spec: ManaSpec::Fixed(vec![Color::Black, Color::Black, Color::Black]),
                },
            }],
        },
        text: Some("Add {B}{B}{B}."),
    }];
    card
}

/// Add {B}{B}{B}. Threshold — Add {B}{B}{B}{B}{B} instead if seven or more cards are
/// in your graveyard. Cost is {1}{B}, so below threshold it nets only +1 (vs Dark
/// Ritual's +2). (Threshold is near-impossible comboing by T2–3 in the goldfish, but
/// modeled for fidelity in grindier lines.)
fn cabal_ritual() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::{Action, ManaSpec, Who as IrWho};
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel, ZoneSel};
    let mut card = simple("Cabal Ritual", CardKind::Instant(SpellData {
        mana_cost: "1B".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("B", false, false), None);

    // Threshold: 7+ cards in your graveyard (count ALL graveyard objects).
    let threshold = Expr::Ge(
        Box::new(Expr::Count(Box::new(Expr::AllObjects {
            zone: ZoneSel::Scoped {
                zone_kind: ZoneKindSel::Graveyard,
                owner: Box::new(Expr::Ctx(Ctx::Controller)),
            },
            bind: "cr_gy",
            filter: Box::new(Expr::Bool(true)),
        }))),
        Box::new(Expr::Num(7)),
    );
    let add_black = |n: usize| Action::AddMana {
        who: IrWho::You,
        count: Expr::Num(n as i64),
        spec: ManaSpec::Fixed(vec![Color::Black; n]),
    };
    card.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::None,
                body: Action::IfThen {
                    cond: threshold,
                    then: Box::new(add_black(5)),
                    else_: Some(Box::new(add_black(3))),
                },
            }],
        },
        text: Some(
            "Add {B}{B}{B}. Threshold — Add {B}{B}{B}{B}{B} instead if seven or more cards are in your graveyard.",
        ),
    }];
    card
}

/// Destroy target creature with MV ≤ 3. CR 701.7.
fn fatal_push() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::Expr;
    let mut card = simple("Fatal Push", CardKind::Instant(SpellData {
        mana_cost: "B".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("B", false, false), None);
    card.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::ObjectInZone {
                    controller: Who::Opp,
                    zone: ZoneId::Battlefield,
                    filter: ir_and(ir_type(CardType::Creature), ir_mv_le(3)),
                },
                body: Action::Destroy { target: Expr::Ctx(Ctx::Var("target")) },
            }],
        },
        text: Some("Destroy target creature with mana value 3 or less."),
    }];
    card
}

/// Destroy target non-black creature. Alternate cost: pay 4 life (free spell). CR 701.7.
fn snuff_out() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::Expr;
    let mut c = simple("Snuff Out", CardKind::Instant(SpellData {
        mana_cost: "3BB".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("3BB", false, true), None);
    c.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::ObjectInZone {
                    controller: Who::Opp,
                    zone: ZoneId::Battlefield,
                    filter: ir_and(ir_type(CardType::Creature), ir_not(ir_color(Color::Black))),
                },
                body: Action::Destroy { target: Expr::Ctx(Ctx::Var("target")) },
            }],
        },
        text: Some("Destroy target non-black creature."),
    }];
    // Phase 4 step 5 follow-up: the simplest IR alt cost — just PayLife(4).
    // No object decision, no schema entries; the executor drains life
    // directly and `cost_exec::pay` returns an empty CostsPaidCtx.
    c.alternate_costs = vec![
        AlternateCost {
            costs: CostBody::Ir(Action::PayLife {
                who: crate::ir::action::Who::You,
                amount: Expr::Num(4),
            }),
            ..Default::default()
        },
    ];
    c
}

/// Exile target creature. Its controller gains life equal to its power. CR 701.10.
fn swords_to_plowshares() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::{Action, Who as IrWho};
    use crate::ir::context::Ctx;
    use crate::ir::expr::Expr;
    let mut card = simple("Swords to Plowshares", CardKind::Instant(SpellData {
        mana_cost: "W".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("W", true, false), None);
    card.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::ObjectInZone {
                    controller: Who::Opp,
                    zone: ZoneId::Battlefield,
                    filter: ir_type(CardType::Creature),
                },
                // Gain life = power FIRST (read while the creature is still on the
                // battlefield), then exile it. Same outcome as the closure.
                body: Action::Sequence(vec![
                    Action::GainLife {
                        who: IrWho::Player(Expr::Controller(Box::new(Expr::Ctx(Ctx::Var("target"))))),
                        amount: Expr::Power(Box::new(Expr::Ctx(Ctx::Var("target")))),
                    },
                    Action::Exile { target: Expr::Ctx(Ctx::Var("target")), bind_as: None },
                ]),
            }],
        },
        text: Some("Exile target creature. Its controller gains life equal to its power."),
    }];
    card
}

/// Destroy target creature or planeswalker.
/// Additional cost: discard a card OR pay 3 life (CR 118.9d).
fn bitter_triumph() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::context::Ctx;
    let mut def = simple("Bitter Triumph", CardKind::Instant(SpellData {
        mana_cost: "1B".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("1B", false, false), None);
    def.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::ObjectInZone {
                    controller: Who::Opp,
                    zone: ZoneId::Battlefield,
                    filter: ir_or(ir_type(CardType::Creature), ir_type(CardType::Planeswalker)),
                },
                body: crate::ir::action::Action::Destroy {
                    target: crate::ir::expr::Expr::Ctx(Ctx::Var("target")),
                },
            }],
        },
        text: Some("Destroy target creature or planeswalker."),
    }];
    // "As an additional cost to cast this spell, discard a card or pay 3 life."
    // A cost-tree Choose (CR 601.2b): the chooser commits a branch at
    // announcement; the discard branch carries a nested object decision
    // (which card), answered via the same BindEnv. `It != Source` excludes the
    // spell itself, which is still in hand during the pre-cast feasibility check.
    def.additional_costs = {
        use crate::ir::action::{Action, ChoiceOption, MoveVerb, Who};
        use crate::ir::context::Ctx;
        use crate::ir::expr::{Expr, Filter, ZoneKindSel};
        CostBody::Ir(Action::Choose {
            who: Who::You,
            prompt: "Bitter Triumph: additional cost",
            options: vec![
                ChoiceOption {
                    label: "discard a card",
                    cost: None,
                    action: Box::new(Action::MoveByChoice {
                        who: Who::You,
                        from: ZoneKindSel::Hand,
                        to: ZoneKindSel::Graveyard,
                        verb: MoveVerb::Discard,
                        filter: Filter(Expr::Not(Box::new(Expr::Eq(
                            Box::new(Expr::Ctx(Ctx::It)),
                            Box::new(Expr::Ctx(Ctx::Source)),
                        )))),
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
        })
    };
    def
}

/// Destroy target creature or planeswalker with MV ≤ 3. This spell can't be countered (CR 608.2b).
fn long_goodbye() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::Expr;
    let mut card = CardDef::new(
        "Long Goodbye",
        CardKind::Instant(SpellData {
            mana_cost: "1B".to_string(),
            modes: None,
            ..Default::default()
        }),
        parse_colors("1B", false, false),
        None,
        vec![], // supertypes
        CardLayout::Normal, None,
        vec![], vec![],
        vec![], // "can't be countered" is now an IR Prohibition (card.abilities below)
        vec![],
    );
    card.abilities = vec![
        Ability {
            kind: AbilityKind::OnResolve {
                modes: vec![IrSpellMode {
                    target_spec: TargetSpec::ObjectInZone {
                        controller: Who::Opp,
                        zone: ZoneId::Battlefield,
                        filter: ir_and(
                            ir_or(ir_type(CardType::Creature), ir_type(CardType::Planeswalker)),
                            ir_mv_le(3),
                        ),
                    },
                    body: Action::Destroy { target: Expr::Ctx(Ctx::Var("target")) },
                }],
            },
            text: Some("Destroy target creature or planeswalker with mana value 3 or less."),
        },
        // "This spell can't be countered" — IR Prohibition (event pipeline).
        cant_be_countered_self(),
    ];
    card
}

/// Choose one — each opponent sacrifices a nontoken creature (mode 0), a creature token
/// (mode 1), or a planeswalker (mode 2) of their choice. CR 700.2, CR 701.16.
/// Mode chosen at cast time (CR 700.2a); sacrifice goes through `sacrifice_choice`.
fn sheoldreds_edict() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::{Action, Who as IrWho};
    use crate::ir::expr::Expr;
    // Each opponent sacrifices the chosen kind of permanent. Modeled as
    // `Action::Sacrifice { who: Opponent }` — the sacrificing player picks via
    // `Strategy::sacrifice_choice`, same as the old `eff_sacrifice`.
    let sac = |filter| IrSpellMode {
        target_spec: TargetSpec::None,
        body: Action::Sacrifice {
            who: IrWho::Opponent,
            filter,
            count: Expr::Num(1),
            bind_as: None,
        },
    };
    let mut card = simple("Sheoldred's Edict", CardKind::Instant(SpellData {
        mana_cost: "1B".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("1B", false, false), None);
    card.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![
                sac(ir_and(ir_not(ir_token()), ir_type(CardType::Creature))),
                sac(ir_token()),
                sac(ir_type(CardType::Planeswalker)),
            ],
        },
        text: Some("Choose one — each opponent sacrifices a creature; or a token; or a planeswalker."),
    }];
    card
}

/// Counter target noncreature spell unless its controller pays {2}. CR 700.2.
fn spell_pierce() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    let mut card = simple("Spell Pierce", CardKind::Instant(SpellData {
        mana_cost: "U".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("U", true, false), None);
    card.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::ObjectInZone {
                    controller: Who::Opp,
                    zone: ZoneId::Stack,
                    filter: ir_and(ir_spell(), ir_not(ir_type(CardType::Creature))),
                },
                body: counter_unless_pays_body(parse_mana_cost("2")),
            }],
        },
        text: Some("Counter target noncreature spell unless its controller pays {2}."),
    }];
    card
}

/// Counter target activated or triggered ability. (Mana abilities can't be targeted.)
/// Mana abilities never go on the stack (CR 605.3a), so `ir_ability()` already excludes them.
fn stifle() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::Expr;

    let mut card = simple("Stifle", CardKind::Instant(SpellData {
        mana_cost: "U".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("U", true, false), None);
    card.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::ObjectInZone {
                    controller: Who::Opp,
                    zone: ZoneId::Stack,
                    filter: ir_ability(),
                },
                body: Action::Counter { target: Expr::Ctx(Ctx::Var("target")) },
            }],
        },
        text: Some("Counter target activated or triggered ability."),
    }];
    card
}

/// Counter target instant or sorcery spell unless its controller pays {1}.
/// Storm (CR 702.40): when you cast this spell, copy it for each spell cast before it
/// this turn. Copies are counterable stack abilities targeting other legal targets.
///
/// IR structure (no `CounterUnlessPays` primitive):
/// - OnResolve: `Choose { who: Controller(target), pay {1} → Noop | else → Counter }`.
///   Payment-costed Choose options subsume the "unless X pays Y" idiom (CR 118.4
///   filters out unpayable options before the chooser sees them).
/// - Storm trigger (Triggered, active_zone: Stack): condition checks self-cast,
///   body copies the spell N-1 times where N = EventCount(ThisTurn, SpellCast
///   by controller). -1 excludes the triggering Flusterstorm cast itself (the
///   event log pushes *before* triggers fire).
fn flusterstorm() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, EventPattern, IrSpellMode, TriggerSpec};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::event_log::Window;
    use crate::ir::expr::{EventFilter, Expr, Filter, ZoneKindSel};

    let target_spec = TargetSpec::ObjectInZone {
        controller: Who::Opp,
        zone: ZoneId::Stack,
        filter: ir_or(
            ir_type(CardType::Instant),
            ir_type(CardType::Sorcery),
        ),
    };

    // "Counter unless its controller pays {1}" — shared with Daze / Spell Pierce.
    let on_resolve_body = counter_unless_pays_body(parse_mana_cost("1"));

    // Self-cast detection: the SpellCast pattern binds `triggered_obj` to the
    // cast card_id. Self-trigger ⇔ triggered_obj == Ctx::Source. Also require
    // caster == controller (defensive; storm is a "when you cast" trigger).
    let self_cast = Expr::And(
        Box::new(Expr::Eq(
            Box::new(Expr::Ctx(Ctx::Var("triggered_obj"))),
            Box::new(Expr::Ctx(Ctx::Source)),
        )),
        Box::new(Expr::Eq(
            Box::new(Expr::Ctx(Ctx::Var("triggered_actor"))),
            Box::new(Expr::Ctx(Ctx::Controller)),
        )),
    );

    // N = |SpellCast events this turn by controller| - 1.
    // The -1 excludes the Flusterstorm cast itself (already logged by fire_event).
    let storm_count = Expr::Sub(
        Box::new(Expr::EventCount {
            window: Window::ThisTurn,
            filter: Box::new(EventFilter::SpellCast {
                caster: Some(Box::new(Expr::Ctx(Ctx::Controller))),
                card: None,
                spell_filter: None,
                alt_cost: None,
            }),
        }),
        Box::new(Expr::Num(1)),
    );

    let storm_body = Action::CopySpell {
        what: Expr::Ctx(Ctx::Source),
        n: storm_count,
        new_targets: true,
    };

    let spell_data = SpellData {
        mana_cost: "U".to_string(),
        modes: None,
        ..Default::default()
    };
    let mut card = simple(
        "Flusterstorm",
        CardKind::Instant(spell_data),
        parse_colors("U", true, false),
        None,
    );
    card.abilities = vec![
        Ability {
            kind: AbilityKind::OnResolve {
                modes: vec![IrSpellMode {
                    target_spec,
                    body: on_resolve_body,
                }],
            },
            text: Some("Counter target instant or sorcery spell unless its controller pays {1}."),
        },
        Ability {
            kind: AbilityKind::Triggered {
                spec: TriggerSpec::When {
                    pattern: EventPattern::SpellCast {
                        spell_filter: Filter(Expr::Bool(true)),
                    },
                    condition: Some(self_cast),
                },
                target_spec: TargetSpec::None,
                body: storm_body,
                active_zone: ZoneKindSel::Stack,
            },
            text: Some("Storm (When you cast this spell, copy it for each spell cast before it this turn. You may choose new targets for the copies.)"),
        },
    ];
    card
}

/// Exile any number of target spells. If an opponent cast three or more spells this turn,
/// you may pay {0} rather than pay this spell's mana cost. CR 107.1c, CR 118.9.
fn mindbreak_trap() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, Filter, ZoneKindSel};

    let mut c = simple("Mindbreak Trap", CardKind::Instant(SpellData {
        mana_cost: "2UU".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("2UU", true, false), None);
    // "Exile any number of target spells" — a generic object filter over the
    // stack: exile each spell not controlled by the caster. (No strategic reason
    // to spare any, so the "any number" choice collapses to "all opposing spells".)
    let not_mine = Filter(Expr::Not(Box::new(Expr::Eq(
        Box::new(Expr::Controller(Box::new(Expr::Ctx(Ctx::It)))),
        Box::new(Expr::Ctx(Ctx::Controller)),
    ))));
    c.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::None,
                body: ir_for_each_obj(
                    ZoneKindSel::Stack,
                    ir_and(ir_spell(), not_mine),
                    Action::Exile { target: Expr::Ctx(Ctx::Var("v")), bind_as: None },
                ),
            }],
        },
        text: Some("Exile each spell an opponent controls. (Mindbreak Trap: exile any number of target spells.)"),
    }];
    c.alternate_costs = vec![
        AlternateCost {
            // Mindbreak Trap's alt cost is "{0}" — pay nothing — when the
            // condition is met. IR Action::Noop.
            costs: CostBody::Ir(crate::ir::action::Action::Noop),
            condition: Some(Arc::new(|caster, state| {
                // "If an opponent cast three or more spells this turn" — counted
                // from the event log (replaces the spells_cast_this_turn counter).
                let opp = caster.opp();
                state.event_log.count(
                    crate::ir::event_log::Window::ThisTurn,
                    |e| matches!(e.event, GameEvent::SpellCast { caster: c, .. } if c == opp),
                ) >= 3
            })),
            ..Default::default()
        },
    ];
    c
}

/// Counter target triggered ability or colorless spell.
/// Replicate {1} (CR 702.58): optional additional cost paid 0+ times; each payment
/// creates a copy of the spell targeting another triggered ability or colorless spell.
fn consign_to_memory() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::Expr;
    let mut def = simple("Consign to Memory", CardKind::Instant(SpellData {
        mana_cost: "U".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("U", false, false), None);
    def.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::ObjectInZone {
                    controller: Who::Opp,
                    zone: ZoneId::Stack,
                    filter: ir_or(
                        ir_triggered_ability(),
                        ir_and(ir_spell(), ir_colorless()),
                    ),
                },
                body: Action::Counter { target: Expr::Ctx(Ctx::Var("target")) },
            }],
        },
        text: Some("Counter target triggered ability or colorless spell."),
    }];
    def.additional_costs = CostBody::Ir(Action::Replicate(parse_mana_cost("1")));
    def
}

/// Exile target card in a graveyard (not basic land), then exile all cards with the
/// same name from that player's graveyard, hand, and library (CR 107.4f phyrexian mana).
/// {B/P}: pay {B} or pay 2 life.
fn surgical_extraction() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel, ZoneSel};
    let mut c = simple("Surgical Extraction", CardKind::Instant(SpellData {
        mana_cost: "B".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("B", false, false), None);
    // Exile the target plus every same-name card from its owner's graveyard,
    // hand, and library. One sweep per zone: for each object in that zone owned
    // by the target's owner whose name equals the target's name, exile it. The
    // target itself lives in the graveyard, so the graveyard sweep catches it.
    let sweep = |zone_kind: ZoneKindSel| Action::ForEach {
        over: Expr::AllObjects {
            zone: ZoneSel::Scoped {
                zone_kind,
                owner: Box::new(Expr::Owner(Box::new(Expr::Ctx(Ctx::Var("target"))))),
            },
            bind: "it",
            filter: Box::new(Expr::Eq(
                Box::new(Expr::Name(Box::new(Expr::Ctx(Ctx::Var("it"))))),
                Box::new(Expr::Name(Box::new(Expr::Ctx(Ctx::Var("target"))))),
            )),
        },
        bind: "v",
        body: Box::new(Action::Exile {
            target: Expr::Ctx(Ctx::Var("v")),
            bind_as: None,
        }),
    };
    c.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::ObjectInZone {
                    controller: Who::Opp,
                    zone: ZoneId::Graveyard,
                    filter: ir_not(ir_and(
                        ir_type(CardType::Land),
                        ir_supertype(Supertype::Basic),
                    )),
                },
                body: Action::Sequence(vec![
                    sweep(ZoneKindSel::Graveyard),
                    sweep(ZoneKindSel::Hand),
                    sweep(ZoneKindSel::Library),
                ]),
            }],
        },
        text: Some("Choose target card in a graveyard other than a basic land card. Exile it and all cards with the same name from that player's graveyard, hand, and library."),
    }];
    c.alternate_costs = vec![
        AlternateCost { costs: ir_pay_life(2), ..Default::default() },
    ];
    c
}

/// Build a TargetSpec for the modal color-hate instants: either a spell on the stack
/// or a permanent on the battlefield, both filtered to the given color.
fn color_hate_target_spec(c: Color) -> TargetSpec {
    TargetSpec::Union(vec![
        TargetSpec::ObjectInZone {
            controller: Who::Opp,
            zone: ZoneId::Stack,
            filter: ir_color(c),
        },
        TargetSpec::ObjectInZone {
            controller: Who::Opp,
            zone: ZoneId::Battlefield,
            filter: ir_color(c),
        },
    ])
}

/// Build a TargetSpec for the "if it's [color]" variant: targets ANY spell on the stack
/// or ANY permanent on the battlefield (targeting is unrestricted; the effect is conditional).
fn any_spell_or_permanent_target() -> TargetSpec {
    TargetSpec::Union(vec![
        TargetSpec::ObjectInZone {
            controller: Who::Opp,
            zone: ZoneId::Stack,
            filter: ir_spell(),
        },
        TargetSpec::ObjectInZone {
            controller: Who::Opp,
            zone: ZoneId::Battlefield,
            filter: ir_any(),
        },
    ])
}

/// IR body: counter `target` if it's on the stack (a spell), otherwise destroy
/// it (CR 701.5/701.7). Used by REB/BEB, where the color restriction lives on the
/// target spec rather than the effect. `target` is bound as `Ctx::Var("target")`.
fn ir_counter_or_destroy() -> crate::ir::action::Action {
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::Expr;
    let target = || Expr::Ctx(Ctx::Var("target"));
    Action::IfThen {
        cond: Expr::Eq(
            Box::new(Expr::ZoneOf(Box::new(target()))),
            Box::new(Expr::ZoneLit(ZoneId::Stack)),
        ),
        then: Box::new(Action::Counter { target: target() }),
        else_: Some(Box::new(Action::Destroy { target: target() })),
    }
}

/// IR body: counter-or-destroy `target`, but only if it's the given color.
/// Pyroblast/Hydroblast may target any spell/permanent; the effect applies only
/// "if it's [color]" (CR 608.2b — a legal target whose effect simply doesn't
/// apply otherwise). Colors are read materialized, so Painter's Servant naming a
/// color makes a once-off-color permanent a valid victim.
fn ir_counter_or_destroy_if_color(c: Color) -> crate::ir::action::Action {
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::Expr;
    Action::IfThen {
        cond: Expr::Contains(
            Box::new(Expr::ColorLit(c)),
            Box::new(Expr::Colors(Box::new(Expr::Ctx(Ctx::Var("target"))))),
        ),
        then: Box::new(ir_counter_or_destroy()),
        else_: None,
    }
}

/// Lightning Bolt deals 3 damage to any target. CR 120.2.
fn lightning_bolt() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::Expr;

    let mut card = simple("Lightning Bolt", CardKind::Instant(SpellData {
        mana_cost: "R".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("R", false, false), None);
    card.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::Union(vec![
                    TargetSpec::ObjectInZone { controller: Who::Opp, zone: ZoneId::Battlefield, filter: ir_type(CardType::Creature) },
                    TargetSpec::ObjectInZone { controller: Who::Opp, zone: ZoneId::Battlefield, filter: ir_type(CardType::Planeswalker) },
                    TargetSpec::Player(Who::Opp),
                ]),
                body: Action::DealDamage {
                    source: Expr::Ctx(Ctx::Source),
                    target: Expr::Ctx(Ctx::Var("target")),
                    amount: Expr::Num(3),
                },
            }],
        },
        text: Some("Lightning Bolt deals 3 damage to any target."),
    }];
    card
}

/// Choose one — Deal 3 damage to target creature; or destroy target artifact. CR 700.2, 701.7.
fn abrade() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::Expr;

    let mut card = simple("Abrade", CardKind::Instant(SpellData {
        mana_cost: "1R".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("1R", false, false), None);
    card.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![
                // Mode 0: deal 3 damage to target creature
                IrSpellMode {
                    target_spec: TargetSpec::ObjectInZone {
                        controller: Who::Opp,
                        zone: ZoneId::Battlefield,
                        filter: ir_type(CardType::Creature),
                    },
                    body: Action::DealDamage {
                        source: Expr::Ctx(Ctx::Source),
                        target: Expr::Ctx(Ctx::Var("target")),
                        amount: Expr::Num(3),
                    },
                },
                // Mode 1: destroy target artifact
                IrSpellMode {
                    target_spec: TargetSpec::ObjectInZone {
                        controller: Who::Opp,
                        zone: ZoneId::Battlefield,
                        filter: ir_type(CardType::Artifact),
                    },
                    body: Action::Destroy { target: Expr::Ctx(Ctx::Var("target")) },
                },
            ],
        },
        text: Some("Choose one — Abrade deals 3 damage to target creature; or destroy target artifact."),
    }];
    card
}

/// Helper: an `Instant` whose only resolution body is `body`, targeting `target_spec`.
fn ir_instant(name: &str, mana_cost: &str, target_spec: TargetSpec,
              body: crate::ir::action::Action, text: &'static str) -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    let mut card = simple(name, CardKind::Instant(SpellData {
        mana_cost: mana_cost.to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors(mana_cost, false, false), None);
    card.abilities = vec![Ability {
        kind: AbilityKind::OnResolve { modes: vec![IrSpellMode { target_spec, body }] },
        text: Some(text),
    }];
    card
}

/// Counter target blue spell, or destroy target blue permanent. CR 701.5, 701.7.
fn red_elemental_blast() -> CardDef {
    ir_instant("Red Elemental Blast", "R", color_hate_target_spec(Color::Blue),
        ir_counter_or_destroy(),
        "Counter target blue spell, or destroy target blue permanent.")
}

/// Counter target spell if it's blue; or destroy target permanent if it's blue.
/// Targets any opp spell/permanent; effect fizzles if the target is not blue. CR 701.5, 701.7.
fn pyroblast() -> CardDef {
    ir_instant("Pyroblast", "R", any_spell_or_permanent_target(),
        ir_counter_or_destroy_if_color(Color::Blue),
        "Choose one — Counter target spell if it's blue; or destroy target permanent if it's blue.")
}

/// Counter target red spell, or destroy target red permanent. CR 701.5, 701.7.
fn blue_elemental_blast() -> CardDef {
    ir_instant("Blue Elemental Blast", "U", color_hate_target_spec(Color::Red),
        ir_counter_or_destroy(),
        "Counter target red spell, or destroy target red permanent.")
}

/// Counter target spell if it's red; or destroy target permanent if it's red.
/// Targets any opp spell/permanent; effect fizzles if the target is not red. CR 701.5, 701.7.
fn hydroblast() -> CardDef {
    ir_instant("Hydroblast", "U", any_spell_or_permanent_target(),
        ir_counter_or_destroy_if_color(Color::Red),
        "Choose one — Counter target spell if it's red; or destroy target permanent if it's red.")
}

// ── Sorceries ─────────────────────────────────────────────────────────────────

/// All creatures get -X/-X until end of turn; additional cost: pay X life (CR 107.2).
/// The -X/-X is a Layer 7 ContinuousInstance; creatures with resulting toughness ≤ 0
/// die when the engine checks state-based actions before the next priority grant.
/// X is chosen by the strategy (default: 3) via `choose_x_for_spell`.
fn toxic_deluge() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::{Action, Expiry as IrExpiry};
    use crate::ir::ce::CEMod;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel, ZoneSel};
    let mut def = simple("Toxic Deluge", CardKind::Sorcery(SpellData {
        mana_cost: "2B".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("2B", false, false), None);
    // -X/-X to every creature until end of turn: one L7 P/T continuous effect over
    // the creatures present at resolution (`ApplyCE` over the `AllObjects` set →
    // a single CI; layer derived from the `PumpPT` write). X = announced X life
    // paid, read as `Ctx::Var("x")` and negated.
    let neg_x = Expr::Neg(Box::new(Expr::Ctx(Ctx::Var("x"))));
    def.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::None,
                body: Action::ApplyCE {
                    target: Expr::AllObjects {
                        zone: ZoneSel::Global(ZoneKindSel::Battlefield),
                        bind: "it",
                        filter: Box::new(Expr::Contains(
                            Box::new(Expr::TypeLit(CardType::Creature)),
                            Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::Var("it"))))),
                        )),
                    },
                    mods: vec![CEMod::PumpPT(neg_x.clone(), neg_x)],
                    expiry: IrExpiry::EndOfTurn,
                },
            }],
        },
        text: Some("All creatures get -X/-X until end of turn, where X is the amount of life paid."),
    }];
    def.additional_costs = ir_xlife_cost();
    def
}

/// "For each object in `zone` (across all players) matching `filter`, run `body`."
/// `body` refers to the current match as `Ctx::Var("v")`. The generic
/// object-set sweep: a board wipe is this over `Battlefield`, Mindbreak Trap is
/// this over `Stack`. Protection/indestructibility live in the leaf primitives
/// (`DealDamage` / `Destroy`), so this stays a pure iteration.
fn ir_for_each_obj(zone: crate::ir::expr::ZoneKindSel, filter: crate::ir::expr::Filter,
                   body: crate::ir::action::Action) -> crate::ir::action::Action {
    use crate::ir::action::Action;
    use crate::ir::expr::{Expr, ZoneSel};
    Action::ForEach {
        over: Expr::AllObjects {
            zone: ZoneSel::Global(zone),
            bind: "it",
            filter: Box::new(filter.0),
        },
        bind: "v",
        body: Box::new(body),
    }
}

/// `ir_for_each_obj` specialized to the battlefield — the common board-wide sweep.
fn ir_for_each_on_battlefield(filter: crate::ir::expr::Filter, body: crate::ir::action::Action) -> crate::ir::action::Action {
    ir_for_each_obj(crate::ir::expr::ZoneKindSel::Battlefield, filter, body)
}

/// Brotherhood's End — {1}{R}{R} sorcery. Choose one:
/// • Deal 3 damage to each creature and each planeswalker.
/// • Destroy all artifacts with mana value 3 or less.
fn brotherhoods_end() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::Expr;

    let mut card = simple("Brotherhood's End", CardKind::Sorcery(SpellData {
        mana_cost: "1RR".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("1RR", false, false), None);
    card.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![
                // Mode 0: 3 damage to each creature and each planeswalker.
                IrSpellMode {
                    target_spec: TargetSpec::None,
                    body: ir_for_each_on_battlefield(
                        ir_or(ir_type(CardType::Creature), ir_type(CardType::Planeswalker)),
                        Action::DealDamage {
                            source: Expr::Ctx(Ctx::Source),
                            target: Expr::Ctx(Ctx::Var("v")),
                            amount: Expr::Num(3),
                        },
                    ),
                },
                // Mode 1: destroy all artifacts with mana value 3 or less.
                IrSpellMode {
                    target_spec: TargetSpec::None,
                    body: ir_for_each_on_battlefield(
                        ir_and(ir_type(CardType::Artifact), ir_mv_le(3)),
                        Action::Destroy { target: Expr::Ctx(Ctx::Var("v")) },
                    ),
                },
            ],
        },
        text: Some("Choose one — 3 damage to each creature and planeswalker; or destroy each artifact with mana value 3 or less."),
    }];
    card
}

/// Win condition: set success=true. In full rules: opponent's library and graveyard become
/// their library; controller searches for exactly five cards. CR 101.1 (shortcut).
fn doomsday() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::Action;
    // "Doomsday" is a sentinel in this sim: resolving it is the stopping point
    // (the real pile-building is deferred to the human via the web UI). The card
    // body is a deliberate no-op — termination + life accounting are owned by the
    // application's objective (`objective::DoomsdayResolvedObjective`), which
    // observes the `SpellResolved` event. Not a real cast.
    let mut card = simple("Doomsday", CardKind::Sorcery(SpellData {
        mana_cost: "BBB".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("BBB", false, false), None);
    card.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::None,
                body: Action::Sequence(vec![]),
            }],
        },
        text: Some("(sim sentinel) Resolving Doomsday ends the simulation; the objective observes it."),
    }];
    card
}

/// Look at top 5, put two in hand, rest on bottom in any order. Modeled as draw:2. CR 701.26.
fn stock_up() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::{Action, Who as IrWho};
    use crate::ir::expr::Expr;
    let mut card = simple("Stock Up", CardKind::Sorcery(SpellData {
        mana_cost: "2U".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("U", false, false), None);
    card.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::None,
                // De-hacked: real "look at top 5, keep 2, rest to bottom" — a dig,
                // not a Draw N (which was a player-agency + too-specific shortcut).
                body: Action::Dig { who: IrWho::You, n: Expr::Num(5), take: Expr::Num(2) },
            }],
        },
        text: Some("Look at the top five cards of your library. Put two of them into your hand and the rest on the bottom of your library in any order."),
    }];
    card
}

/// Look at the top three cards; put one (two if there's an instant AND a sorcery
/// in your graveyard) into hand, rest on the bottom. CR 701.* (look/choose).
fn flow_state() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::{Action, Who as IrWho};
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel, ZoneSel};
    let mut card = simple("Flow State", CardKind::Sorcery(SpellData {
        mana_cost: "1U".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("U", false, false), None);

    // ">= 1 card of `ty` in your graveyard".
    let gy_has = |ty: CardType, bind: &'static str| -> Expr {
        Expr::Ge(
            Box::new(Expr::Count(Box::new(Expr::AllObjects {
                zone: ZoneSel::Scoped {
                    zone_kind: ZoneKindSel::Graveyard,
                    owner: Box::new(Expr::Ctx(Ctx::Controller)),
                },
                bind,
                filter: Box::new(Expr::Contains(
                    Box::new(Expr::TypeLit(ty)),
                    Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::Var(bind))))),
                )),
            }))),
            Box::new(Expr::Num(1)),
        )
    };
    let cond = Expr::And(
        Box::new(gy_has(CardType::Instant, "fs_i")),
        Box::new(gy_has(CardType::Sorcery, "fs_s")),
    );

    card.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::None,
                body: Action::IfThen {
                    cond,
                    then: Box::new(Action::Dig { who: IrWho::You, n: Expr::Num(3), take: Expr::Num(2) }),
                    else_: Some(Box::new(Action::Dig { who: IrWho::You, n: Expr::Num(3), take: Expr::Num(1) })),
                },
            }],
        },
        text: Some("Look at the top three cards of your library. Put one of them into your hand and the rest on the bottom of your library in any order. If there is an instant card and a sorcery card in your graveyard, instead put two of them into your hand and the rest on the bottom of your library in any order."),
    }];
    card
}

/// Scry 2, then draw a card. CR 701.18 (scry), CR 701.9 (draw).
fn preordain() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::{Action, Who as IrWho};
    use crate::ir::expr::Expr;
    let mut card = simple("Preordain", CardKind::Sorcery(SpellData {
        mana_cost: "U".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("U", false, false), None);
    card.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::None,
                body: Action::Sequence(vec![
                    Action::Scry { who: IrWho::You, n: Expr::Num(2) },
                    Action::Draw { who: IrWho::You, n: Expr::Num(1) },
                ]),
            }],
        },
        text: Some("Scry 2, then draw a card."),
    }];
    card
}

/// Look at top 3, arrange or shuffle, then draw. CR 701.26 (library manipulation).
/// "You may shuffle" decomposes to `MayDo { Shuffle }` — the shuffle is the
/// effect, the "may" is a y/n strategy decision (no heuristic baked in).
fn ponder() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::{Action, Who as IrWho};
    use crate::ir::expr::Expr;
    let mut card = simple("Ponder", CardKind::Sorcery(SpellData {
        mana_cost: "U".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("U", false, false), None);
    card.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::None,
                // Look at top 3, put back in any order (a player decision via
                // OrderTop → Strategy::order_top_library); you may shuffle; draw.
                body: Action::Sequence(vec![
                    Action::OrderTop { who: IrWho::You, n: Expr::Num(3) },
                    Action::MayDo {
                        who: IrWho::You,
                        action: Box::new(Action::Shuffle { who: IrWho::You }),
                    },
                    Action::Draw { who: IrWho::You, n: Expr::Num(1) },
                ]),
            }],
        },
        text: Some("Look at the top three cards of your library, then put them back in any order. You may shuffle. Draw a card."),
    }];
    card
}

/// Target opponent discards a nonland card; you lose 2 life. CR 701.8, CR 702.1.
fn thoughtseize() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::{Action, Who as IrWho};
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel, ZoneSel};
    let mut card = simple("Thoughtseize", CardKind::Sorcery(SpellData {
        mana_cost: "B".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("B", false, false), None);
    card.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::None,
                body: Action::Sequence(vec![
                    // Reveal the opponent's hand (every hand card not owned by us).
                    Action::Reveal {
                        who: IrWho::Opponent,
                        what: Expr::AllObjects {
                            zone: ZoneSel::Global(ZoneKindSel::Hand),
                            bind: "h",
                            filter: Box::new(Expr::Not(Box::new(Expr::Eq(
                                Box::new(Expr::Owner(Box::new(Expr::Ctx(Ctx::Var("h"))))),
                                Box::new(Expr::Ctx(Ctx::Controller)),
                            )))),
                        },
                    },
                    // Opponent discards a nonland card. (Random pick, as before.)
                    Action::Discard {
                        who: IrWho::Opponent,
                        count: Expr::Num(1),
                        at_random: true,
                        filter: Some(ir_not(ir_type(CardType::Land))),
                    },
                    // You lose 2 life.
                    Action::PayLife { who: IrWho::You, amount: Expr::Num(2) },
                ]),
            }],
        },
        text: Some("Target player reveals their hand. You choose a nonland card from it. That player discards that card. You lose 2 life."),
    }];
    card
}

/// Return target creature from your graveyard to play. CR 701.14.
/// Reanimation modeled with the generic `Move` (graveyard → battlefield), not a
/// dedicated reanimate primitive.
fn unearth() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel};
    let mut card = simple("Unearth", CardKind::Sorcery(SpellData {
        mana_cost: "B".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("B", false, false), None);
    card.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::ObjectInZone {
                    controller: Who::Actor,
                    zone: ZoneId::Graveyard,
                    filter: ir_type(CardType::Creature),
                },
                body: Action::Move {
                    what: Expr::Ctx(Ctx::Var("target")),
                    to: ZoneKindSel::Battlefield,
                    to_owner: None,
                    bind_as: None,
                },
            }],
        },
        text: Some("Return target creature card from your graveyard to the battlefield."),
    }];
    card
}

/// Target opponent discards 2 cards at random. CR 701.8.
fn hymn_to_tourach() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::{Action, Who as IrWho};
    use crate::ir::expr::Expr;
    let mut card = simple("Hymn to Tourach", CardKind::Sorcery(SpellData {
        mana_cost: "BB".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("BB", false, false), None);
    card.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::None,
                body: Action::Discard {
                    who: IrWho::Opponent,
                    count: Expr::Num(2),
                    at_random: true,
                    filter: None,
                },
            }],
        },
        text: Some("Target player discards two cards at random."),
    }];
    card
}

/// Cycling: discard this card, sacrifice a land you control → draw a card.
/// Modeled as a hand-zone activated ability. Cast cost {G}{W} rarely used.
fn edge_of_autumn() -> CardDef {
    simple("Edge of Autumn", CardKind::Sorcery(SpellData {
        mana_cost: "GW".to_string(),
        // Hand ability: discard self + sacrifice a land → draw 1.
        // Modeled via AbilityDef on SpellData is not standard; the TOML used `abilities`
        // at the top level. Since SpellData has no abilities field, this card has no
        // castable effects but the hand ability is registered via the cycling-like path
        // in strategy. Future work: add `abilities` to SpellData.
        ..Default::default()
    }), parse_colors("GW", false, false), None)
}

/// Search your library for a sorcery card, put it on top. CR 700.3, CR 701.19.
fn personal_tutor() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::{Action, Who as IrWho};
    use crate::ir::expr::{Expr, ZoneKindSel};
    let mut card = simple("Personal Tutor", CardKind::Sorcery(SpellData {
        mana_cost: "U".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("U", false, false), None);
    card.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::None,
                body: Action::Search {
                    who: IrWho::You,
                    zone: ZoneKindSel::Library,
                    filter: ir_type(CardType::Sorcery),
                    count: Expr::Num(1),
                    dest: ZoneKindSel::Library,
                    to_top: true,
                    shuffle: true,
                    bind_as: None,
                },
            }],
        },
        text: Some("Search your library for a sorcery card and reveal that card. Shuffle your library, then put the card on top of it."),
    }];
    card
}

/// Search your library for a green creature and put it onto the battlefield.
/// X not modeled; treated as {1G} (fixed cost). CR 700.3, CR 701.19.
fn green_suns_zenith() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::{Action, Who as IrWho};
    use crate::ir::expr::{Expr, ZoneKindSel};
    let mut card = simple("Green Sun's Zenith", CardKind::Sorcery(SpellData {
        mana_cost: "1G".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("1G", false, false), None);
    card.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::None,
                body: Action::Search {
                    who: IrWho::You,
                    zone: ZoneKindSel::Library,
                    filter: ir_and(ir_color(Color::Green), ir_type(CardType::Creature)),
                    count: Expr::Num(1),
                    dest: ZoneKindSel::Battlefield,
                    to_top: false,
                    shuffle: true,
                    bind_as: None,
                },
            }],
        },
        text: Some("Search your library for a green creature card, put it onto the battlefield, then shuffle."),
    }];
    card
}

/// Each player may put an artifact, creature, enchantment, or land card from their
/// hand onto the battlefield. Both placements are simultaneous (CR 101.4).
fn show_and_tell() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::{Action, Who as IrWho};
    use crate::ir::expr::ZoneKindSel;
    let mut card = simple("Show and Tell", CardKind::Sorcery(SpellData {
        mana_cost: "2U".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("U", false, false), None);
    // "Each player may put an artifact, creature, enchantment, or land card from
    // their hand onto the battlefield." Each player chooses before any card
    // enters, so the placements are simultaneous (CR 101.4).
    card.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::None,
                body: Action::SimultaneousPut {
                    who: IrWho::Each,
                    from: ZoneKindSel::Hand,
                    filter: ir_or(
                        ir_or(ir_type(CardType::Artifact), ir_type(CardType::Creature)),
                        ir_or(ir_type(CardType::Enchantment), ir_type(CardType::Land)),
                    ),
                    optional: true,
                },
            }],
        },
        text: Some("Each player may put an artifact, creature, enchantment, or land card from their hand onto the battlefield."),
    }];
    card
}

fn omniscience() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind};
    use crate::ir::ce::{CEMod, CostSpec};
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, Filter};

    let mut card = CardDef::new(
        "Omniscience",
        CardKind::Enchantment(EnchantmentData::default()),
        parse_colors("UUUUUUUUU", false, false),  // blue; {7}{U}{U}{U}
        None,
        vec![], CardLayout::Normal, None,
        vec![], vec![], vec![],
        vec![], // effect is an IR Static ability (below)
    );
    // "You may cast spells from your hand without paying their mana costs."
    // An L3 alt-cost CE: push a free `AlternateCost` onto each non-land card the
    // Omniscience controller controls. Scope is controller-keyed but *zone-agnostic*
    // on purpose — a spell moves hand→stack at CR 601.2a, before its cost is paid, so
    // a hand-only scope would drop the alt cost mid-cast. (Only hand casts consume it
    // in practice.) `AltCost(Free)` is translated by `cemod_to_modifier`.
    let non_land_of_controller = Filter(Expr::And(
        Box::new(Expr::Eq(
            Box::new(Expr::Controller(Box::new(Expr::Ctx(Ctx::It)))),
            Box::new(Expr::Ctx(Ctx::Controller)),
        )),
        Box::new(Expr::Not(Box::new(Expr::Contains(
            Box::new(Expr::TypeLit(CardType::Land)),
            Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::It)))),
        )))),
    ));
    card.abilities = vec![Ability {
        kind: AbilityKind::Static {
            mods: vec![CEMod::AltCost(CostSpec::Free)],
            scope: Some(non_land_of_controller),
            condition: None,
        },
        text: Some("You may cast spells from your hand without paying their mana costs."),
    }];
    card
}

fn sneak_attack() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, StepScope, TriggerSpec};
    use crate::ir::action::{Action, Expiry as IrExpiry, Who as IrWho};
    use crate::ir::ce::CEMod;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, Filter, ZoneKindSel};

    // The creature picked from hand (CR 701.10 resolution choice) is bound as
    // `Ctx::Var("target")` by `build_ability_effect`.
    let chosen = || Expr::Ctx(Ctx::Var("target"));

    // "{R}: You may put a creature card from your hand onto the battlefield. That
    //  creature gains haste. Sacrifice it at the beginning of the next end step."
    let body = Action::Sequence(vec![
        // Put the chosen creature onto the battlefield (full ETB pipeline).
        Action::Move {
            what: chosen(),
            to: ZoneKindSel::Battlefield,
            to_owner: None,
            bind_as: None,
        },
        // It gains haste. EndOfTurn duration suffices — it's sacrificed this turn,
        // and the printed creature has no haste once the effect ends.
        Action::ApplyCE {
            target: chosen(),
            mods: vec![CEMod::AddKeyword(Keyword::Haste)],
            expiry: IrExpiry::EndOfTurn,
        },
        // Delayed: at the beginning of the next end step, sacrifice it. The
        // `target` binding is captured at schedule time, so it resolves to this
        // creature when the trigger fires (matches nothing if it has since left).
        Action::ScheduleDelayedTrigger {
            fires: TriggerSpec::AtStep { step: StepKind::End, who: StepScope::EachPlayer, condition: None },
            action: Box::new(Action::Sacrifice {
                who: IrWho::You,
                filter: Filter(Expr::Eq(Box::new(Expr::Ctx(Ctx::It)), Box::new(chosen()))),
                count: Expr::Num(1),
                bind_as: None,
            }),
        },
    ]);

    let mut card = CardDef::new(
        "Sneak Attack",
        CardKind::Enchantment(EnchantmentData::default()),
        parse_colors("3R", false, false),
        None,
        vec![], CardLayout::Normal, None,
        vec![], vec![], vec![],
        vec![],
    );
    card.abilities = vec![Ability {
        kind: AbilityKind::Activated {
            cost: ir_pay_mana_str("R"),
            target_spec: TargetSpec::None,
            choice_spec: Some(ChoiceSpec {
                controller: Who::Actor, // effects::Who, via super::*
                zone: ZoneId::Hand,
                filter: ir_type(CardType::Creature),
            }),
            body,
            timing: ActivationTiming::Default, // instant speed
            activation_condition: None,
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("{R}: You may put a creature card from your hand onto the battlefield. That creature gains haste. Sacrifice the creature at the beginning of the next end step."),
    }];
    card
}

// ── Creatures ─────────────────────────────────────────────────────────────────

/// ETB: look at top X cards of your library, where X is the number of cards in it;
/// if you control more blue/black permanents than opponent, you win. Modeled as win-on-ETB
/// via strategy, not via trigger here (no ETB trigger — strategy checks for Oracle).
/// CR 702.15 (devotion), CR 104.3b.
fn thassas_oracle() -> CardDef {
    let data = CreatureData::new("UU", 1, 3);
    simple("Thassa's Oracle", CardKind::Creature(data), parse_colors("UU", false, false), Some(1))
}

/// Doomsday's backup wincon. A minimal blue Legendary Planeswalker stub (enters
/// with its starting loyalty): the "if you'd draw from an empty library, you win
/// instead" wincon and the loyalty abilities aren't needed by the goldfish sim
/// (which wins on Thassa's Oracle) — only its presence in the catalog and its
/// blue color (Force of Will pitch, blue-count) matter.
fn jace_wielder_of_mysteries() -> CardDef {
    let mut def = CardDef::new(
        "Jace, Wielder of Mysteries",
        CardKind::Planeswalker(PlaneswalkerData {
            mana_cost: "3UU".to_string(),
            loyalty: 5,
            abilities: vec![],
        }),
        parse_colors("3UU", true, false), // blue
        Some(1),
        vec![Supertype::Legendary], CardLayout::Normal, None,
        vec![], vec![], vec![], vec![],
    );
    def.abilities = vec![ir_planeswalker_etb_loyalty(5)];
    def
}

/// Cycling (hand ability): discard this + pay 2 life → draw 1. CR 702.28.
fn street_wraith() -> CardDef {
    use crate::ir::action::{Action, Who as IrWho};
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel};
    let mut data = CreatureData::new("3BB", 3, 4);
    data.abilities = vec![AbilityDef {
        source_zone: SourceZone::Hand,
        // `DiscardSelf` is "send the source itself to the graveyard" — no
        // candidate enumeration. Use `Action::Move` (deterministic) rather
        // than `MoveByChoice` (which would try to find the source in `Hand`
        // — but the activation pipeline pre-moves hand-source abilities to
        // Stack before paying costs, so the source isn't in Hand anymore).
        costs: ir_seq(vec![
            Action::Move {
                what: Expr::Ctx(Ctx::Source),
                to: ZoneKindSel::Graveyard,
                to_owner: None,
                bind_as: None,
            },
            act_pay_life(2),
        ]),
        ir_body: Some(Action::Draw { who: IrWho::You, n: Expr::Num(1) }),
        ..Default::default()
    }];
    simple("Street Wraith", CardKind::Creature(data), parse_colors("3BB", false, false), Some(1))
}

/// 0/1 for {2B}. No special abilities — just a beater.
fn barrowgoyf() -> CardDef {
    let mut data = CreatureData::new("2B", 0, 1);
    data.legendary = false;
    simple("Barrowgoyf", CardKind::Creature(data), parse_colors("2B", false, true), None)
}

/// Ninjutsu {1U}: swap in with an unblocked attacker. CR 702.49.
fn ingenious_infiltrator() -> CardDef {
    let mut data = CreatureData::new("1UB", 2, 1);
    data.abilities = vec![ninjutsu_ability("1U")];
    data.creature_subtypes = vec!["Ninja".into()];
    simple(
        "Ingenious Infiltrator",
        CardKind::Creature(data),
        parse_colors("1UB", true, true),
        None,
    )
}

/// Legendary Planeswalker — Kaito. Loyalty 4. Ninjutsu {1UB}.
/// +1: emblem "Ninjas you control get +1/+1."
/// 0: Surveil 2, draw per opponent who lost life this turn.
/// −2: Tap target creature, put 2 stun counters on it.
/// Static: during your turn, as long as loyalty > 0, he's a 3/4 Ninja creature with
/// hexproof (and per the CR ruling stops being a planeswalker, but keeps his loyalty
/// abilities). Modeled as a conditional self-scoped L4 `BecomeCreature` CE.
fn kaito_bane_of_nightmares() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind};
    use crate::ir::ce::CEMod;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, Filter};

    let mut card = CardDef::new(
        "Kaito, Bane of Nightmares",
        CardKind::Planeswalker(PlaneswalkerData {
            mana_cost: "2UB".into(),
            loyalty: 4,
            abilities: vec![
                // Ninjutsu from hand (not a loyalty ability).
                ninjutsu_ability("1UB"),
                // +1: "You get an emblem with 'Ninjas you control get +1/+1.'"
                AbilityDef {
                    costs: ir_loyalty(1),
                    ir_body: Some(crate::ir::action::Action::CreateEmblem {
                        abilities: vec![Ability {
                            kind: AbilityKind::Static {
                                mods: vec![CEMod::PumpPT(Expr::Num(1), Expr::Num(1))],
                                // "Ninjas you control": Ninja ∧ controlled by the
                                // emblem's controller (Ctx::Controller).
                                scope: Some(ir_and(
                                    ir_subtype("Ninja"),
                                    Filter(Expr::Eq(
                                        Box::new(Expr::Controller(Box::new(Expr::Ctx(Ctx::It)))),
                                        Box::new(Expr::Ctx(Ctx::Controller)),
                                    )),
                                )),
                                condition: None,
                            },
                            text: Some("Ninjas you control get +1/+1."),
                        }],
                    }),
                    timing: ActivationTiming::Sorcery,
                    ..Default::default()
                },
                // 0: Surveil 2, then draw a card for each opponent who lost life
                // this turn — counted straight off the event log.
                AbilityDef {
                    costs: ir_loyalty(0),
                    ir_body: Some(crate::ir::action::Action::Sequence(vec![
                        crate::ir::action::Action::Surveil {
                            who: crate::ir::action::Who::You,
                            n: Expr::Num(2),
                        },
                        crate::ir::action::Action::Draw {
                            who: crate::ir::action::Who::You,
                            n: Expr::CountWhere {
                                set: Box::new(Expr::Opponents(Box::new(Expr::Ctx(Ctx::Controller)))),
                                bind: "o",
                                body: Box::new(Expr::Gt(
                                    Box::new(Expr::EventCount {
                                        window: crate::ir::event_log::Window::ThisTurn,
                                        filter: Box::new(crate::ir::expr::EventFilter::LifeLost {
                                            who: Some(Box::new(Expr::Ctx(Ctx::Var("o")))),
                                        }),
                                    }),
                                    Box::new(Expr::Num(0)),
                                )),
                            },
                        },
                    ])),
                    timing: ActivationTiming::Sorcery,
                    ..Default::default()
                },
                // −2: Tap target creature + 2 stun counters.
                AbilityDef {
                    costs: ir_loyalty(-2),
                    ir_body: Some(crate::ir::action::Action::Sequence(vec![
                        crate::ir::action::Action::Tap { target: Expr::Ctx(Ctx::Var("target")) },
                        crate::ir::action::Action::PutCounters {
                            on: Expr::Ctx(Ctx::Var("target")),
                            kind: crate::CounterType::Stun,
                            n: Expr::Num(2),
                        },
                    ])),
                    target_spec: TargetSpec::ObjectInZone {
                        controller: Who::Opp,
                        zone: ZoneId::Battlefield,
                        filter: ir_type(CardType::Creature),
                    },
                    timing: ActivationTiming::Sorcery,
                    ..Default::default()
                },
            ],
        }),
        parse_colors("2UB", true, true),
        None,
        vec![Supertype::Legendary], CardLayout::Normal, None,
        vec![],
        vec![], // ETB loyalty is now an IR Replacement (card.abilities below)
        vec![],
        vec![], // animation is now an IR Static ability (below)
    );

    // "During your turn, as long as Kaito has one or more loyalty counters on him,
    // he's a 3/4 Ninja creature and has hexproof." Self-scoped (`It == Source`) L4
    // BecomeCreature, gated on the active turn being his controller's and loyalty > 0.
    card.abilities = vec![
        ir_planeswalker_etb_loyalty(4),
        Ability {
            kind: AbilityKind::Static {
                mods: vec![CEMod::BecomeCreature {
                    power: Expr::Num(3),
                    toughness: Expr::Num(4),
                    subtypes: vec!["Ninja".to_string()],
                    keywords: vec![Keyword::Hexproof],
                }],
                scope: Some(Filter(Expr::Eq(
                    Box::new(Expr::Ctx(Ctx::It)),
                    Box::new(Expr::Ctx(Ctx::Source)),
                ))),
                condition: Some(Expr::And(
                    Box::new(Expr::Eq(
                        Box::new(Expr::ActivePlayer),
                        Box::new(Expr::Ctx(Ctx::Controller)),
                    )),
                    Box::new(Expr::Gt(
                        Box::new(Expr::LoyaltyOf(Box::new(Expr::Ctx(Ctx::Source)))),
                        Box::new(Expr::Num(0)),
                    )),
                )),
            },
            text: Some("During your turn, as long as Kaito has one or more loyalty counters on him, he's a 3/4 Ninja creature and has hexproof."),
        },
    ];
    card
}

/// ETB: search your library for a creature with toughness ≤ 2, put it into your hand.
/// CR 700.3, CR 701.19.
fn recruiter_of_the_guard() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, EventPattern, TriggerSpec};
    use crate::ir::action::{Action, Who as IrWho};
    use crate::ir::expr::{Expr, ZoneKindSel};
    let mut card = CardDef::new(
        "Recruiter of the Guard",
        CardKind::Creature(CreatureData::new("2W", 1, 1)),
        parse_colors("2W", false, false),
        None,
        vec![], CardLayout::Normal, None,
        vec![], vec![], vec![], vec![],
    );
    // "When this enters, search your library for a creature card with toughness 2
    //  or less, reveal it, put it into your hand, then shuffle."
    card.abilities = vec![Ability {
        kind: AbilityKind::Triggered {
            spec: TriggerSpec::When {
                pattern: EventPattern::EntersZone {
                    obj_filter: ir_self(),
                    zone_kind: ZoneKindSel::Battlefield,
                },
                condition: None,
            },
            target_spec: TargetSpec::None,
            body: Action::Search {
                who: IrWho::You,
                zone: ZoneKindSel::Library,
                filter: ir_and(ir_type(CardType::Creature), ir_toughness_le(2)),
                count: Expr::Num(1),
                dest: ZoneKindSel::Hand,
                to_top: false,
                shuffle: true,
                bind_as: None,
            },
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("When Recruiter of the Guard enters, search your library for a creature card with toughness 2 or less, reveal it, put it into your hand, then shuffle."),
    }];
    card
}

/// Stoneforge Mystic — {1}{W} Creature — Kor Artificer 1/2.
/// "When this creature enters, you may search your library for an Equipment card,
///  reveal it, put it into your hand, then shuffle."
/// "{1}{W}, {T}: You may put an Equipment card from your hand onto the battlefield."
fn stoneforge_mystic() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, EventPattern, TriggerSpec};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel};
    let mut data = CreatureData::new("1W", 1, 2);
    data.creature_subtypes = vec!["Kor".into(), "Artificer".into()];
    let mut card = CardDef::new(
        "Stoneforge Mystic",
        CardKind::Creature(data),
        parse_colors("1W", false, false),
        None,
        vec![], CardLayout::Normal, None,
        vec![], // ETB trigger is now an IR Triggered ability (below)
        vec![], vec![], vec![],
    );
    // "When this enters, search your library for an Equipment card, reveal it, put
    //  it into your hand, then shuffle." (The {1}{W},{T} put-into-play ability is
    //  still a legacy AbilityDef on `data` — that's the separate activated batch.)
    card.abilities = vec![Ability {
        kind: AbilityKind::Triggered {
            spec: TriggerSpec::When {
                pattern: EventPattern::EntersZone {
                    obj_filter: ir_self(),
                    zone_kind: ZoneKindSel::Battlefield,
                },
                condition: None,
            },
            target_spec: TargetSpec::None,
            body: Action::Search {
                who: crate::ir::action::Who::You,
                zone: ZoneKindSel::Library,
                filter: ir_subtype("Equipment"),
                count: Expr::Num(1),
                dest: ZoneKindSel::Hand,
                to_top: false,
                shuffle: true,
                bind_as: None,
            },
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("When Stoneforge Mystic enters, search your library for an Equipment card, reveal it, put it into your hand, then shuffle."),
    },
    // "{1}{W}, {T}: You may put an Equipment card from your hand onto the
    //  battlefield." The chosen Equipment (CR 701.10) is bound as Ctx::Var("target").
    Ability {
        kind: AbilityKind::Activated {
            cost: ir_seq(vec![act_pay_mana_str("1W"), act_tap_source()]),
            target_spec: TargetSpec::None,
            choice_spec: Some(ChoiceSpec {
                controller: Who::Actor,
                zone: ZoneId::Hand,
                filter: ir_subtype("Equipment"),
            }),
            body: Action::Move {
                what: Expr::Ctx(Ctx::Var("target")),
                to: ZoneKindSel::Battlefield,
                to_owner: None,
                bind_as: None,
            },
            timing: ActivationTiming::Sorcery,
            activation_condition: None,
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("{1}{W}, {T}: You may put an Equipment card from your hand onto the battlefield."),
    }];
    card
}

/// ETB trigger + draw-trigger: deal 1 damage to any target and amass Orc 1 whenever
/// opponent draws a non-natural card. Also fires on its own ETB. CR 603.
fn orcish_bowmasters() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, EventPattern, TriggerSpec};
    use crate::ir::action::{Action, TokenSpec, Who as IrWho};
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, Filter, ZoneKindSel, ZoneSel};

    let mut data = CreatureData::new("1B", 1, 1);
    data.legendary = false;
    let mut card = CardDef::new(
        "Orcish Bowmasters",
        CardKind::Creature(data),
        parse_colors("1B", false, true),
        None,
        vec![], CardLayout::Normal, None,
        vec![],
        vec![],
        vec![],
        vec![],
    );

    // Shared body: "deal 1 damage to any target; amass Orc 1".
    let any_target = TargetSpec::Union(vec![
        TargetSpec::ObjectInZone { controller: Who::Opp, zone: ZoneId::Battlefield, filter: ir_type(CardType::Creature) },
        TargetSpec::ObjectInZone { controller: Who::Opp, zone: ZoneId::Battlefield, filter: ir_type(CardType::Planeswalker) },
        TargetSpec::Player(Who::Opp),
    ]);
    // Amass Orcs 1 decomposed:
    //   if you control no Orc Army, first create a 0/0 Orc Army token.
    //   for each Orc Army you control, put a +1/+1 counter on it.
    // The re-query after CreateToken means the freshly-minted army is found
    // without an explicit bind. SBAs don't run mid-Sequence, so the 0/0
    // survives long enough to grow.
    let orc_army_set = || Expr::AllObjects {
        zone: ZoneSel::Global(ZoneKindSel::Battlefield),
        bind: "a",
        filter: Box::new(Expr::And(
            Box::new(Expr::Eq(
                Box::new(Expr::Controller(Box::new(Expr::Ctx(Ctx::Var("a"))))),
                Box::new(Expr::Ctx(Ctx::Controller)),
            )),
            Box::new(Expr::Eq(
                Box::new(Expr::Name(Box::new(Expr::Ctx(Ctx::Var("a"))))),
                Box::new(Expr::NameLit("Orc Army".to_string())),
            )),
        )),
    };
    let amass_orcs_1 = Action::Sequence(vec![
        Action::IfThen {
            cond: Expr::Eq(
                Box::new(Expr::Count(Box::new(orc_army_set()))),
                Box::new(Expr::Num(0)),
            ),
            then: Box::new(Action::CreateToken {
                who: IrWho::You,
                spec: TokenSpec {
                    name: "Orc Army",
                    types: vec![CardType::Creature],
                    subtypes: vec![],
                    colors: vec![],
                    power: Some(0),
                    toughness: Some(0),
                    keywords: vec![],
                },
                n: Expr::Num(1),
                bind_as: None,
            }),
            else_: None,
        },
        Action::ForEach {
            over: orc_army_set(),
            bind: "a",
            body: Box::new(Action::PutCounters {
                on: Expr::Ctx(Ctx::Var("a")),
                kind: CounterType::PlusOnePlusOne,
                n: Expr::Num(1),
            }),
        },
    ]);
    let body = Action::Sequence(vec![
        Action::DealDamage {
            source: Expr::Ctx(Ctx::Source),
            target: Expr::Ctx(Ctx::Var("target")),
            amount: Expr::Num(1),
        },
        amass_orcs_1,
    ]);

    // Filter: entering object == this Bowmasters (self-ETB).
    let self_etb = Filter(Expr::Eq(
        Box::new(Expr::Ctx(Ctx::It)),
        Box::new(Expr::Ctx(Ctx::Source)),
    ));
    // Filter: the drawing player is an opponent.
    let opp_draws = Filter(Expr::Not(Box::new(Expr::Eq(
        Box::new(Expr::Ctx(Ctx::It)),
        Box::new(Expr::Ctx(Ctx::Controller)),
    ))));
    // Condition: draw is not a natural draw-step draw.
    let not_natural = Expr::Not(Box::new(Expr::Ctx(Ctx::Var("triggered_is_natural"))));

    card.abilities = vec![
        Ability {
            kind: AbilityKind::Triggered {
                spec: TriggerSpec::When {
                    pattern: EventPattern::EntersZone {
                        obj_filter: self_etb,
                        zone_kind: ZoneKindSel::Battlefield,
                    },
                    condition: None,
                },
                target_spec: any_target.clone(),
                body: body.clone(),
                active_zone: ZoneKindSel::Battlefield,
            },
            text: Some("When Orcish Bowmasters enters, it deals 1 damage to any target. Amass Orcs 1."),
        },
        Ability {
            kind: AbilityKind::Triggered {
                spec: TriggerSpec::When {
                    pattern: EventPattern::Draw { who: opp_draws },
                    condition: Some(not_natural),
                },
                target_spec: any_target,
                body,
                active_zone: ZoneKindSel::Battlefield,
            },
            text: Some("Whenever an opponent draws a card except the first one they draw in each of their draw steps, Orcish Bowmasters deals 1 damage to any target. Amass Orcs 1."),
        },
    ];
    card
}

/// ETB replacement: enters with counters = # of instants/sorceries in controller's exile.
/// Trigger: gains +1/+1 counter when a spell is exiled from your graveyard. CR 614.1, CR 603.
fn murktide_regent() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, EventPattern, ReplacementBody, TriggerSpec};
    use crate::ir::action::Action;
    use crate::ir::context::{Ctx, EventField};
    use crate::ir::expr::{Expr, Filter, ZoneKindSel};

    // Filter: triggering object is instant or sorcery.
    let is_inst_or_sorc = Filter(Expr::Or(
        Box::new(Expr::Contains(
            Box::new(Expr::TypeLit(CardType::Instant)),
            Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::It)))),
        )),
        Box::new(Expr::Contains(
            Box::new(Expr::TypeLit(CardType::Sorcery)),
            Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::It)))),
        )),
    ));
    // Actor filter: actor == controller (only when you exile).
    let actor_is_you = Filter(Expr::Eq(
        Box::new(Expr::Ctx(Ctx::It)),
        Box::new(Expr::Ctx(Ctx::Controller)),
    ));

    let mut data = CreatureData::new("5UU", 3, 3);
    data.delve = true;
    let mut card = CardDef::new(
        "Murktide Regent",
        CardKind::Creature(data),
        parse_colors("5UU", true, false),
        Some(25),
        vec![], CardLayout::Normal, None,
        vec![],
        vec![], // ETB sunburst (delve count) is now an IR Replacement (below)
        vec![],
        vec![],
    );
    // ETB (CR 614.1c): Murktide enters with a +1/+1 counter for each instant or
    // sorcery card exiled to delve (CR 702.66b). The self-entry Replacement
    // re-does the entry, then counts the instant/sorcery cards among this cast's
    // delved ids — read off Murktide's own logged cast via `ThisCast(DelvedExiled)`.
    let delved_is_inst_or_sorc = Expr::Or(
        Box::new(Expr::Contains(
            Box::new(Expr::TypeLit(CardType::Instant)),
            Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::It)))),
        )),
        Box::new(Expr::Contains(
            Box::new(Expr::TypeLit(CardType::Sorcery)),
            Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::It)))),
        )),
    );
    card.abilities = vec![Ability {
        kind: AbilityKind::Replacement {
            matches: EventPattern::EntersZone {
                obj_filter: ir_self(),
                zone_kind: ZoneKindSel::Battlefield,
            },
            condition: None,
            body: ReplacementBody::Replace(Action::Sequence(vec![
                Action::Move {
                    what: Expr::Ctx(Ctx::Var("triggered_obj")),
                    to: ZoneKindSel::Battlefield,
                    to_owner: None,
                    bind_as: None,
                },
                Action::PutCounters {
                    on: Expr::Ctx(Ctx::Var("triggered_obj")),
                    kind: CounterType::PlusOnePlusOne,
                    n: Expr::CountWhere {
                        set: Box::new(Expr::Ctx(Ctx::ThisCast(EventField::DelvedExiled))),
                        bind: "d",
                        body: Box::new(delved_is_inst_or_sorc),
                    },
                },
            ])),
            active_zone: None, // self-entry replacement
        },
        text: Some("Murktide Regent enters with a +1/+1 counter on it for each instant and sorcery card exiled with it."),
    }];
    // IR: "Whenever an instant or sorcery card is put into exile from your
    // graveyard, put a +1/+1 counter on Murktide Regent."
    card.abilities.push(Ability {
        kind: AbilityKind::Triggered {
            spec: TriggerSpec::When {
                pattern: EventPattern::ZoneChange {
                    obj_filter: is_inst_or_sorc,
                    from: ZoneKindSel::Graveyard,
                    to: ZoneKindSel::Exile,
                    actor_filter: Some(actor_is_you),
                },
                condition: None,
            },
            target_spec: TargetSpec::None,
            body: Action::PutCounters {
                on: Expr::Ctx(Ctx::Source),
                kind: CounterType::PlusOnePlusOne,
                n: Expr::Num(1),
            },
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("Whenever an instant or sorcery card is put into exile from your graveyard, put a +1/+1 counter on Murktide Regent."),
    });
    card
}

/// Shadow (evasion — see strategy.rs), replacement effect (opponent's GY-bound cards
/// exile with a void counter), and {T}, SacSelf activated ability (choose an exiled
/// opponent card with a void counter; grant a free-cast permission for it this turn).
/// CR 702.28 (shadow), CR 614.1a (replacement).
fn dauthi_voidwalker() -> CardDef {
    use crate::ir::ability::{
        Ability, AbilityKind, CostBody, EventPattern, ReplacementBody,
    };
    use crate::ir::action::{Action, Expiry as IrExpiry};
    use crate::ir::ce::{CEMod, CostSpec};
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, Filter, ZoneKindSel};

    let mut data = CreatureData::new("BB", 3, 2);
    data.keywords = Keywords::from_slice(&[Keyword::Shadow]);

    // "A card an opponent owns" — in practice the moving card's controller
    // differs from DV's controller (for the zones this fires in, controller
    // tracks owner).
    let opp_card = Filter(Expr::Not(Box::new(Expr::Eq(
        Box::new(Expr::Controller(Box::new(Expr::Ctx(Ctx::It)))),
        Box::new(Expr::Ctx(Ctx::Controller)),
    ))));

    let exile_with_void = Ability {
        kind: AbilityKind::Replacement {
            matches: EventPattern::EntersZone {
                obj_filter: opp_card,
                zone_kind: ZoneKindSel::Graveyard,
            },
            condition: None,
            body: ReplacementBody::Replace(Action::Sequence(vec![
                Action::Move {
                    what: Expr::Ctx(Ctx::Var("triggered_obj")),
                    to: ZoneKindSel::Exile,
                    to_owner: None,
                    bind_as: None,
                },
                Action::PutCounters {
                    on: Expr::Ctx(Ctx::Var("triggered_obj")),
                    kind: CounterType::Void,
                    n: Expr::Num(1),
                },
            ])),
            active_zone: Some(ZoneKindSel::Battlefield), // functions while Dauthi is in play
        },
        text: Some(
            "If a card an opponent owns would be put into a graveyard from anywhere, \
             instead exile it with a void counter on it.",
        ),
    };

    let may_play = Ability {
        kind: AbilityKind::Activated {
            // Phase 4 step 3: TapSelf+SacSelf conjunction migrated to IR.
            cost: CostBody::Ir(Action::Sequence(vec![
                Action::Tap { target: Expr::Ctx(Ctx::Source) },
                Action::Sacrifice {
                    who: crate::ir::action::Who::You,
                    filter: Filter(Expr::Eq(
                        Box::new(Expr::Ctx(Ctx::It)),
                        Box::new(Expr::Ctx(Ctx::Source)),
                    )),
                    count: Expr::Num(1),
                    bind_as: None,
                },
            ])),
            target_spec: TargetSpec::None,
            choice_spec: Some(ChoiceSpec {
                controller: Who::Opp,
                zone: ZoneId::Exile,
                filter: ir_has_counter(CounterType::Void),
            }),
            body: Action::ApplyCE {
                target: Expr::Ctx(Ctx::Var("target")),
                mods: vec![
                    CEMod::CastableFrom(ZoneKindSel::Exile),
                    CEMod::AltCost(CostSpec::Free),
                ],
                expiry: IrExpiry::EndOfTurn,
            },
            timing: ActivationTiming::Default,
            activation_condition: None,
            active_zone: crate::ir::expr::ZoneKindSel::Battlefield,
        },
        text: Some(
            "{T}, Sacrifice ~: Choose an exiled card an opponent owns with a void counter \
             on it. You may play it this turn without paying its mana cost.",
        ),
    };

    let mut card = CardDef::new(
        "Dauthi Voidwalker",
        CardKind::Creature(data),
        parse_colors("BB", false, false),
        None,
        vec![],
        CardLayout::Normal,
        None,
        vec![],
        vec![],
        vec![],
        vec![],
    );
    card.abilities = vec![exile_with_void, may_play];
    card
}

/// Prohibition: each opponent can't cast noncreature spells with MV > their land count.
/// Trigger: whenever an opponent casts a spell with no mana spent, counter it.
/// CR 614.17 (prohibition), CR 603 (trigger).
fn lavinia_azorius_renegade() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, ActionKind, EventPattern, TriggerSpec};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, Filter, ZoneKindSel, ZoneSel};

    let mut data = CreatureData::new("WU", 2, 2);
    data.legendary = true;
    let mut card = CardDef::new(
        "Lavinia, Azorius Renegade",
        CardKind::Creature(data),
        parse_colors("WU", true, false),
        None,
        vec![], CardLayout::Normal, None,
        vec![],
        vec![],
        vec![],
        vec![],  // "can't cast" is now an AbilityKind::Restriction (card.abilities below)
    );

    // "Each opponent can't cast noncreature spells with mana value greater than the
    // number of lands that player controls." An action-Restriction consulted at
    // legal-cast enumeration (AND-NOT over castable → "can't beats can", CR 101.2).
    // subject = the proposed spell (Ctx::It): opponent-controlled ∧ noncreature ∧
    // MV > (lands its controller controls).
    let lands_controlled = Expr::Count(Box::new(Expr::AllObjects {
        zone: ZoneSel::Scoped {
            zone_kind: ZoneKindSel::Battlefield,
            owner: Box::new(Expr::Controller(Box::new(Expr::Ctx(Ctx::It)))),
        },
        bind: "l",
        filter: Box::new(Expr::Contains(
            Box::new(Expr::TypeLit(CardType::Land)),
            Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::Var("l"))))),
        )),
    }));
    let cant_cast = Ability {
        kind: AbilityKind::Restriction {
            action: ActionKind::Cast,
            subject: Filter(Expr::And(
                Box::new(Expr::Not(Box::new(Expr::Eq(
                    Box::new(Expr::Controller(Box::new(Expr::Ctx(Ctx::It)))),
                    Box::new(Expr::Ctx(Ctx::Controller)),
                )))),
                Box::new(Expr::And(
                    Box::new(Expr::Not(Box::new(Expr::Contains(
                        Box::new(Expr::TypeLit(CardType::Creature)),
                        Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::It)))),
                    )))),
                    Box::new(Expr::Gt(
                        Box::new(Expr::Mv(Box::new(Expr::Ctx(Ctx::It)))),
                        Box::new(lands_controlled),
                    )),
                )),
            )),
        },
        text: Some("Each opponent can't cast noncreature spells with mana value greater than the number of lands that player controls."),
    };

    // Trigger: "Whenever an opponent casts a spell, if no mana was spent to
    // cast it, counter that spell."
    let opp_cast_free = Expr::And(
        Box::new(Expr::Not(Box::new(Expr::Eq(
            Box::new(Expr::Ctx(Ctx::Var("triggered_actor"))),
            Box::new(Expr::Ctx(Ctx::Controller)),
        )))),
        Box::new(Expr::Not(Box::new(Expr::Ctx(Ctx::Var("triggered_mana_spent"))))),
    );
    card.abilities = vec![
        cant_cast,
        Ability {
            kind: AbilityKind::Triggered {
                spec: TriggerSpec::When {
                    pattern: EventPattern::SpellCast { spell_filter: Filter(Expr::Bool(true)) },
                    condition: Some(opp_cast_free),
                },
                target_spec: TargetSpec::None,
                body: Action::Counter {
                    target: Expr::Ctx(Ctx::Var("triggered_obj")),
                },
                active_zone: ZoneKindSel::Battlefield,
            },
            text: Some("Whenever an opponent casts a spell, if no mana was spent to cast it, counter that spell."),
        },
    ];
    card
}

/// Phelia, Exuberant Shepherd — {1}{W} Legendary Creature — Dog (2/2)
/// Flash.
/// Whenever Phelia attacks, exile up to one other target nonland permanent. At the
/// beginning of the next end step, return that card to the battlefield under its
/// owner's control. If it entered under your control, put a +1/+1 counter on Phelia.
///
/// "Entered under your control" ≡ the exiled card's owner is Phelia's controller
/// (since returns go to owner). Blinking your own permanent grows Phelia; blinking
/// an opponent's does not.
///
/// Attack trigger fires on `EnteredStep { DeclareAttackers }` gated by
/// `permanent_bf(src).attacking` (same pattern as Tamiyo). "Up to one" is modeled
/// via `TargetSpec::Union` of Actor+Opp nonland permanents; pick_targets returns
/// at most one; effect no-ops if `targets` is empty. Delayed return is a floating
/// `TriggerInstance` with `Expiry::OneShot` firing on `EnteredStep { End }` (same
/// pattern as Sneak Attack). Controller is reset to owner on return (CR 614 return
/// to battlefield under owner's control).
fn phelia_exuberant_shepherd() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, EventPattern, StepScope, TriggerSpec};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, Filter, ZoneKindSel};

    let mut data = CreatureData::new("1W", 2, 2);
    data.legendary = true;
    data.creature_subtypes = vec!["Dog".into()];
    data.keywords.insert(Keyword::Flash);

    // Nonland permanent, not this Phelia itself ("up to one other").
    let nonland_other = ir_not(ir_type(CardType::Land));
    let nonland_other_for_filter = nonland_other.clone();
    let target_spec = TargetSpec::Union(vec![
        TargetSpec::ObjectInZone {
            controller: Who::Actor, zone: ZoneId::Battlefield,
            filter: {
                // Exclude self at pick time via a wrapping filter that calls the inner.
                // Since TargetSpec::ObjectInZone doesn't see source_id, we rely on the
                // strategy's target filter to skip self; legacy filter did the same by
                // capturing `src`. Here we approximate: exclude via a filter closure
                // bound at target-legality time via legal_targets (which has source_id).
                nonland_other.clone()
            },
        },
        TargetSpec::ObjectInZone {
            controller: Who::Opp, zone: ZoneId::Battlefield,
            filter: nonland_other_for_filter,
        },
    ]);

    let mut card = CardDef::new(
        "Phelia, Exuberant Shepherd",
        CardKind::Creature(data),
        parse_colors("1W", false, false),
        None,
        vec![], CardLayout::Normal, None,
        vec![],
        vec![], vec![], vec![],
    );

    // Filter: the attacker is this Phelia.
    let self_attacks = Filter(Expr::Eq(
        Box::new(Expr::Ctx(Ctx::It)),
        Box::new(Expr::Ctx(Ctx::Source)),
    ));

    // Delayed-trigger body (runs at next end step):
    //   Move exiled card back to battlefield under its owner's control;
    //   if it returns under Phelia's controller (owner == you), +1/+1 counter.
    let delayed_body = Action::Sequence(vec![
        Action::Move {
            what: Expr::Ctx(Ctx::Var("blinked")),
            to: ZoneKindSel::Battlefield,
            to_owner: Some(Expr::Owner(Box::new(Expr::Ctx(Ctx::Var("blinked"))))),
            bind_as: None,
        },
        Action::IfThen {
            cond: Expr::And(
                Box::new(Expr::Bound("blinked")),
                Box::new(Expr::Eq(
                    Box::new(Expr::Owner(Box::new(Expr::Ctx(Ctx::Var("blinked"))))),
                    Box::new(Expr::Ctx(Ctx::Controller)),
                )),
            ),
            then: Box::new(Action::PutCounters {
                on: Expr::Ctx(Ctx::Source),
                kind: CounterType::PlusOnePlusOne,
                n: Expr::Num(1),
            }),
            else_: None,
        },
    ]);

    // Attack-trigger body: exile target (if any), schedule delayed return.
    let body = Action::Sequence(vec![
        Action::Exile {
            target: Expr::Ctx(Ctx::Var("target")),
            bind_as: Some("blinked"),
        },
        Action::ScheduleDelayedTrigger {
            fires: TriggerSpec::AtStep {
                step: StepKind::End,
                who: StepScope::EachPlayer,
                condition: None,
            },
            action: Box::new(delayed_body),
        },
    ]);

    card.abilities = vec![Ability {
        kind: AbilityKind::Triggered {
            spec: TriggerSpec::When {
                pattern: EventPattern::Attacks { attacker_filter: self_attacks },
                condition: None,
            },
            target_spec,
            body,
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("Whenever Phelia, Exuberant Shepherd attacks, exile up to one other target nonland permanent. At the beginning of the next end step, return that card to the battlefield under its owner's control. If it entered under your control, put a +1/+1 counter on Phelia."),
    }];
    card
}

/// {1}{R}, 2/2 Goblin Sorcerer. All four clauses on IR:
/// - "This spell can't be countered." — `cant_be_countered_self` (Prohibition, on stack).
/// - "Spells you control can't be countered." — Prohibition (on battlefield).
/// - "Ward—Pay 2 life." — `ir_ward` (Triggered).
/// - "Other creatures you control have Ward—Pay 2 life." — Static `GrantAbility(ir_ward)`.
fn hexing_squelcher() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, EventPattern};
    use crate::ir::action::{Action, Who as IrWho};
    use crate::ir::ce::CEMod;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, Filter, ZoneKindSel};

    // Ward—Pay 2 life (CR 702.21): the targeting opponent pays 2 life.
    let ward_cost = || Action::PayLife { who: IrWho::You, amount: Expr::Num(2) };

    let mut card = CardDef::new(
        "Hexing Squelcher",
        CardKind::Creature(CreatureData::new("1R", 2, 2)),
        parse_colors("R", false, false),
        None,
        vec![], CardLayout::Normal, None,
        vec![], // ward is now an IR Triggered ability (below)
        vec![],
        vec![], // "can't be countered" clauses are now IR Prohibitions (below)
        vec![], // "other creatures have ward" is now an IR Static GrantAbility (below)
    );

    // Scope for the grant: other creatures Hexing Squelcher's controller controls.
    let other_creatures_you_control = Filter(Expr::And(
        Box::new(Expr::Eq(
            Box::new(Expr::Controller(Box::new(Expr::Ctx(Ctx::It)))),
            Box::new(Expr::Ctx(Ctx::Controller)),
        )),
        Box::new(Expr::And(
            Box::new(Expr::Not(Box::new(Expr::Eq(
                Box::new(Expr::Ctx(Ctx::It)),
                Box::new(Expr::Ctx(Ctx::Source)),
            )))),
            Box::new(Expr::Contains(
                Box::new(Expr::TypeLit(CardType::Creature)),
                Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::It)))),
            )),
        )),
    ));

    card.abilities = vec![
        // "Ward—Pay 2 life." (on itself)
        ir_ward(ward_cost()),
        // "This spell can't be countered." (while a spell on the stack)
        cant_be_countered_self(),
        // "Spells you control can't be countered." (while on the battlefield). A
        // Prohibition suppressing a counter of any spell whose controller is this
        // permanent's controller (`Controller(It) == Ctx::Controller`).
        Ability {
            kind: AbilityKind::Prohibition {
                matches: EventPattern::SpellBeingCountered {
                    spell_filter: Filter(Expr::Eq(
                        Box::new(Expr::Controller(Box::new(Expr::Ctx(Ctx::It)))),
                        Box::new(Expr::Ctx(Ctx::Controller)),
                    )),
                },
                active_zone: Some(ZoneKindSel::Battlefield),
            },
            text: Some("Spells you control can't be countered."),
        },
        // "Other creatures you control have Ward—Pay 2 life."
        Ability {
            kind: AbilityKind::Static {
                mods: vec![CEMod::GrantAbility(Box::new(ir_ward(ward_cost())))],
                scope: Some(other_creatures_you_control),
                condition: None,
            },
            text: Some("Other creatures you control have Ward—Pay 2 life."),
        },
    ];
    card
}

// ── DFCs / split cards ────────────────────────────────────────────────────────

/// Front: 0/3 creature for {U}, generates Clue tokens when it attacks.
/// Back: Tamiyo, Seasoned Scholar — planeswalker with +2 loyalty ability.
/// Transforms after controller draws their 3rd card in a turn. CR 701.28.
fn tamiyo_inquisitive_student() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, EventPattern, StepScope, TriggerSpec};
    use crate::ir::action::{Action, TokenSpec, Who as IrWho};
    use crate::ir::context::Ctx;
    use crate::ir::event_log::Window;
    use crate::ir::expr::{EventFilter, Expr, Filter, ZoneKindSel};
    let mut back = CardDef::new(
        "Tamiyo, Seasoned Scholar",
        CardKind::Planeswalker(PlaneswalkerData {
            mana_cost: String::new(),
            loyalty: 2,
            abilities: vec![
                // +2: until your next turn, creatures your opponents control get
                // −1/−0 while attacking (the engine's model of the damage-reducing
                // ultimate). A floating dynamic-filter CE that catches attackers
                // declared on the opponent's turn, expiring at your next untap.
                AbilityDef {
                    costs: ir_loyalty(2),
                    ir_body: Some(Action::RegisterContinuous {
                        scope: Filter(Expr::And(
                            Box::new(Expr::And(
                                // a creature …
                                Box::new(Expr::Contains(
                                    Box::new(Expr::TypeLit(CardType::Creature)),
                                    Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::It)))),
                                )),
                                // … an opponent controls …
                                Box::new(Expr::Not(Box::new(Expr::Eq(
                                    Box::new(Expr::Controller(Box::new(Expr::Ctx(Ctx::It)))),
                                    Box::new(Expr::Ctx(Ctx::Controller)),
                                )))),
                            )),
                            // … and is attacking.
                            Box::new(Expr::Attacking(Box::new(Expr::Ctx(Ctx::It)))),
                        )),
                        mods: vec![crate::ir::ce::CEMod::PumpPT(Expr::Num(-1), Expr::Num(0))],
                        expiry: crate::ir::action::Expiry::UntilYourNextTurn,
                    }),
                    timing: ActivationTiming::Sorcery,
                    ..Default::default()
                },
                // −3: return target instant/sorcery from your graveyard to hand;
                // if it's green, add one mana of any color (CR 106.1b choice).
                AbilityDef {
                    costs: ir_loyalty(-3),
                    ir_body: Some(Action::Sequence(vec![
                        Action::Move {
                            what: Expr::Ctx(Ctx::Var("target")),
                            to: ZoneKindSel::Hand,
                            to_owner: None,
                            bind_as: None,
                        },
                        Action::IfThen {
                            cond: Expr::Contains(
                                Box::new(Expr::ColorLit(crate::Color::Green)),
                                Box::new(Expr::Colors(Box::new(Expr::Ctx(Ctx::Var("target"))))),
                            ),
                            then: Box::new(Action::AddMana {
                                who: IrWho::You,
                                count: Expr::Num(1),
                                spec: crate::ir::action::ManaSpec::AnyOneColor,
                            }),
                            else_: None,
                        },
                    ])),
                    target_spec: TargetSpec::ObjectInZone {
                        controller: Who::Actor,
                        zone: ZoneId::Graveyard,
                        filter: ir_or(ir_type(CardType::Instant), ir_type(CardType::Sorcery)),
                    },
                    timing: ActivationTiming::Sorcery,
                    ..Default::default()
                },
                // −7: draw cards equal to half your library, rounded up; you get
                // an emblem with "You have no maximum hand size."
                AbilityDef {
                    costs: ir_loyalty(-7),
                    ir_body: Some(Action::Sequence(vec![
                        Action::Draw {
                            who: IrWho::You,
                            // ceil(library / 2) = (library + 1) / 2 (floor div).
                            n: Expr::Div(
                                Box::new(Expr::Add(
                                    Box::new(Expr::LibrarySize(Box::new(Expr::Ctx(Ctx::Controller)))),
                                    Box::new(Expr::Num(1)),
                                )),
                                Box::new(Expr::Num(2)),
                            ),
                        },
                        Action::CreateEmblem {
                            abilities: vec![Ability {
                                kind: AbilityKind::Static {
                                    mods: vec![crate::ir::ce::CEMod::NoMaxHandSize],
                                    scope: None,
                                    condition: None,
                                },
                                text: Some("You have no maximum hand size."),
                            }],
                        },
                    ])),
                    timing: ActivationTiming::Sorcery,
                    ..Default::default()
                },
            ],
        }),
        parse_colors("U", false, false),
        None,
        vec![], CardLayout::Normal, None,
        vec![],
        vec![], // ETB loyalty is now an IR Replacement (below)
        vec![],
        vec![],
    );
    // Tamiyo, Seasoned Scholar (back face) is the planeswalker; she enters with
    // 2 loyalty counters (CR 306.5b) when the front face flips and returns her.
    back.abilities = vec![ir_planeswalker_etb_loyalty(2)];

    let mut front_data = CreatureData::new("U", 0, 3);
    front_data.legendary = true;

    let mut front = CardDef::new(
        "Tamiyo, Inquisitive Student",
        CardKind::Creature(front_data),
        parse_colors("U", false, false),
        None,
        vec![Supertype::Legendary], CardLayout::DoubleFaced, Some(Box::new(back)),
        vec![], vec![], vec![], vec![],
    );

    // "Whenever Tamiyo attacks, create a Clue token." Step-gated on "Tamiyo is
    // attacking" (the attacking flag is set after attackers are declared).
    let clue_on_attack = Ability {
        kind: AbilityKind::Triggered {
            spec: TriggerSpec::AtStep {
                step: StepKind::DeclareAttackers,
                who: StepScope::You,
                condition: Some(Expr::Attacking(Box::new(Expr::Ctx(Ctx::Source)))),
            },
            target_spec: TargetSpec::None,
            body: Action::CreateToken {
                who: IrWho::You,
                spec: TokenSpec {
                    name: "Clue Token",
                    types: vec![CardType::Artifact],
                    subtypes: vec!["Clue"],
                    colors: vec![],
                    power: None,
                    toughness: None,
                    keywords: vec![],
                },
                n: Expr::Num(1),
                bind_as: None,
            },
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("Whenever Tamiyo, Inquisitive Student attacks, create a Clue token."),
    };

    // "Whenever you draw your third card each turn, exile Tamiyo, then return her to
    // the battlefield transformed." "Third card this turn" is a log count, and
    // exile→return is the fresh-object transform (CR 603.6e / 712), so a new untapped
    // Tamiyo, Seasoned Scholar (back face) comes back.
    let flip_on_third_draw = Ability {
        kind: AbilityKind::Triggered {
            spec: TriggerSpec::When {
                pattern: EventPattern::Draw {
                    who: Filter(Expr::Eq(
                        Box::new(Expr::Ctx(Ctx::It)),
                        Box::new(Expr::Ctx(Ctx::Controller)),
                    )),
                },
                condition: Some(Expr::And(
                    // still on the front face (don't re-flip the back)
                    Box::new(Expr::IsFrontFace(Box::new(Expr::Ctx(Ctx::Source)))),
                    Box::new(Expr::Eq(
                        Box::new(Expr::EventCount {
                            window: Window::ThisTurn,
                            filter: Box::new(EventFilter::Draw {
                                who: Some(Box::new(Expr::Ctx(Ctx::Controller))),
                            }),
                        }),
                        Box::new(Expr::Num(3)),
                    )),
                )),
            },
            target_spec: TargetSpec::None,
            body: Action::Sequence(vec![
                Action::Exile { target: Expr::Ctx(Ctx::Source), bind_as: None },
                Action::Move {
                    what: Expr::Ctx(Ctx::Source),
                    to: ZoneKindSel::Battlefield,
                    to_owner: None,
                    bind_as: None,
                },
                Action::Transform { target: Expr::Ctx(Ctx::Source) },
            ]),
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("Whenever you draw your third card each turn, exile Tamiyo, Inquisitive Student, then return her to the battlefield transformed."),
    };
    front.abilities = vec![clue_on_attack, flip_on_third_draw];
    front
}

/// Artifact Creature {2}, 1/3. ETB: choose a color; all objects everywhere gain that color.
/// Layer 5 continuous effect, expires when Painter leaves the battlefield.
/// CR 613.4 (color-changing effects apply at layer 5).
fn painters_servant() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind};
    use crate::ir::ce::CEMod;
    use crate::ir::context::Ctx;
    use crate::ir::expr::Expr;

    let mut def = CardDef::new(
        "Painter's Servant",
        CardKind::Creature(CreatureData::new("2", 1, 3)),
        vec![],
        Some(40),
        vec![], CardLayout::Normal, None,
        vec![],
        vec![], // ETB color choice is now an IR Replacement (below)
        vec![],
        vec![],
    );
    // ETB (CR 614.12 "as ~ enters, choose a color"); then "all cards, spells, and
    // permanents are the chosen color in addition to their other colors" — an L5
    // IR Static (scope None = every object) that adds the color read from this
    // Painter's own ETB choice. Expires with the source by default.
    def.abilities = vec![
        etb_choice_replacement(
            crate::ir::action::EtbChoiceKind::Color,
            "As Painter's Servant enters the battlefield, choose a color.",
        ),
        Ability {
            kind: AbilityKind::Static {
                mods: vec![CEMod::AddColor(Expr::ChosenColor(Box::new(Expr::Ctx(Ctx::Source))))],
                scope: None,
                condition: None,
            },
            text: Some("All cards, spells, and permanents are the chosen color in addition to their other colors."),
        },
    ];
    // Painter's Servant is an Artifact Creature; the constructor derives only one type from
    // CardKind, so we push the second type explicitly.
    def.types.push(CardType::Artifact);
    def
}

/// Enchantment for {2BB}. Replacement: any card going to any graveyard goes to exile instead.
fn leyline_of_the_void() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, EventPattern, ReplacementBody};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel};
    let mut card = CardDef::new(
        "Leyline of the Void",
        CardKind::Enchantment(EnchantmentData::default()),
        parse_colors("2BB", false, true),
        None,
        vec![], CardLayout::Normal, None,
        vec![], vec![], vec![], vec![],
    );
    // "If a card would be put into any graveyard from anywhere, exile it instead."
    // EntersZone(Graveyard) catches the move-to-graveyard from any source zone; the
    // replacement redirects it to Exile (CR 614.5 self-loop guard means the Exile
    // move doesn't re-trigger). Functions while Leyline is on the battlefield.
    card.abilities = vec![Ability {
        kind: AbilityKind::Replacement {
            matches: EventPattern::EntersZone {
                obj_filter: ir_any(),
                zone_kind: ZoneKindSel::Graveyard,
            },
            condition: None,
            body: ReplacementBody::Replace(Action::Move {
                what: Expr::Ctx(Ctx::Var("triggered_obj")),
                to: ZoneKindSel::Exile,
                to_owner: None,
                bind_as: None,
            }),
            active_zone: Some(ZoneKindSel::Battlefield),
        },
        text: Some("If a card would be put into any graveyard from anywhere, exile it instead."),
    }];
    card
}

/// Flash, colorless artifact for {2}.
/// As this enters, choose a card name. Spells with that name cost {3} more to cast.
/// Activated abilities of sources with that name can't be activated unless they're mana abilities.
fn disruptor_flute() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, ActionKind};
    use crate::ir::ce::CEMod;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, Filter};

    let mut card = CardDef::new(
        "Disruptor Flute",
        CardKind::Artifact(ArtifactData {
            mana_cost: "2".to_string(),
            ..Default::default()  // no activated abilities
        }),
        vec![],  // colorless
        Some(40),
        vec![], CardLayout::Normal, None,
        vec![],  // no trigger_defs
        vec![],  // ETB card-name choice is now an IR Replacement (below)
        vec![],  // no prohibition_defs
        vec![],  // ongoing effects are IR abilities (below)
    );

    // Both ongoing clauses scope to "a card whose name == the name this Flute
    // chose" — `Eq(Name(It), ChosenName(Source))`, evaluated per candidate with
    // the Flute bound as Source.
    let name_matches = || Expr::Eq(
        Box::new(Expr::Name(Box::new(Expr::Ctx(Ctx::It)))),
        Box::new(Expr::ChosenName(Box::new(Expr::Ctx(Ctx::Source)))),
    );

    card.abilities = vec![
        // ETB (CR 614.12 "as ~ enters, choose a card name") — recorded in
        // etb_choice; the two ongoing clauses below read it via ChosenName.
        etb_choice_replacement(
            crate::ir::action::EtbChoiceKind::CardName,
            "As Disruptor Flute enters the battlefield, choose a card name.",
        ),
        // "Spells with the chosen name cost {3} more to cast." A casting-cost
        // surcharge CE (recompute writes casting_cost_modifier += 3 on matches).
        Ability {
            kind: AbilityKind::Static {
                mods: vec![CEMod::CastingCostPlus(Expr::Num(3))],
                scope: Some(Filter(name_matches())),
                condition: None,
            },
            text: Some("Spells with the chosen name cost {3} more to cast."),
        },
        // "Activated abilities of sources with the chosen name can't be activated
        // unless they're mana abilities." An action-Restriction over the named card;
        // the "unless they're mana abilities" rider (CR 605.1a) is a subject clause
        // `Not(activating_mana_ability)` — the engine binds that bool while the mana
        // sub-loop is consulting — not a flag on the variant.
        Ability {
            kind: AbilityKind::Restriction {
                action: ActionKind::Activate,
                subject: Filter(Expr::And(
                    Box::new(name_matches()),
                    Box::new(Expr::Not(Box::new(Expr::Ctx(Ctx::Var("activating_mana_ability"))))),
                )),
            },
            text: Some("Activated abilities of sources with the chosen name can't be activated unless they're mana abilities."),
        },
    ];
    card
}

/// Legendary Planeswalker — Karn {4}. Loyalty 5.
/// Static: "Activated abilities of artifacts your opponents control can't be activated."
/// CE sets activatable=false on ALL abilities (AbilityDef + ManaAbility) of opponent-controlled artifacts.
/// +1 animation and -2 wish are modeled for Forge simulation.
fn karn_the_great_creator() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, ActionKind};
    use crate::ir::action::{Action, Expiry as IrExpiry};
    use crate::ir::ce::CEMod;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, Filter, ZoneKindSel};

    // -2 uses the simulator's Forge shortcut: sideboard cards are materialized
    // as face-up exile during game setup. This means Karn has one unified search
    // pool containing both wishboard artifacts and artifacts exiled during play.
    let wish_factory: AbilityFactory = Arc::new(|controller, source_id| {
        Effect(Arc::new(move |state, t, _targets| {
            let choices: Vec<WishOption> = state.exile_of(controller)
                .filter(|o| {
                    // Exile objects do not necessarily have a materialized definition yet
                    // (notably wishboard cards preloaded into exile during setup, and
                    // direct test/scenario construction). Prefer current characteristics
                    // when available, but fall back to the printed catalog definition.
                    state.def_of(o.id)
                        .or_else(|| state.catalog.get(&o.catalog_key))
                        .map_or(false, |d| d.types.contains(&CardType::Artifact))
                })
                .map(|o| WishOption::Exile { id: o.id, name: o.catalog_key.clone() })
                .collect();

            let picked = state.with_strategy(controller, |strategy, st| {
                strategy.choose_wish(source_id, &choices, st)
            });
            if let Some(WishOption::Exile { id, .. }) = picked {
                if state.objects.get(&id).map_or(false, |o| {
                    o.owner == controller && o.in_zone(Zone::Exile { on_adventure: false })
                }) {
                    change_zone(id, ZoneId::Hand, state, t, controller);
                }
            }
        }))
    });

    let wish_ability = AbilityDef {
        costs: ir_loyalty(-2),
        ability_factory: Some(wish_factory),
        timing: ActivationTiming::Sorcery,
        ..Default::default()
    };

    let mut card = CardDef::new(
        "Karn, the Great Creator",
        CardKind::Planeswalker(PlaneswalkerData {
            mana_cost: "4".to_string(),
            loyalty: 5,
            abilities: vec![wish_ability],
        }),
        vec![],
        None,
        vec![Supertype::Legendary], CardLayout::Normal, None,
        vec![], vec![], vec![], vec![],
    );
    let noncreature_artifact = ir_and(
        ir_type(CardType::Artifact),
        ir_not(ir_type(CardType::Creature)),
    );
    card.abilities = vec![
        ir_planeswalker_etb_loyalty(5),
        Ability {
            kind: AbilityKind::Activated {
                cost: ir_loyalty(1),
                target_spec: TargetSpec::ObjectInZone {
                    controller: Who::Any,
                    zone: ZoneId::Battlefield,
                    filter: noncreature_artifact,
                },
                choice_spec: None,
                body: Action::ApplyCE {
                    target: Expr::Ctx(Ctx::Var("target")),
                    mods: vec![CEMod::BecomeCreature {
                        power: Expr::Mv(Box::new(Expr::Ctx(Ctx::Var("target")))),
                        toughness: Expr::Mv(Box::new(Expr::Ctx(Ctx::Var("target")))),
                        subtypes: vec![],
                        keywords: vec![],
                    }],
                    expiry: IrExpiry::UntilYourNextTurn,
                },
                timing: ActivationTiming::Sorcery,
                activation_condition: None,
                active_zone: ZoneKindSel::Battlefield,
            },
            text: Some("+1: Until your next turn, up to one target noncreature artifact becomes an artifact creature with power and toughness each equal to its mana value."),
        },
        Ability {
            kind: AbilityKind::Restriction {
                action: ActionKind::Activate,
                subject: Filter(Expr::And(
                    Box::new(Expr::Contains(
                        Box::new(Expr::TypeLit(CardType::Artifact)),
                        Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::It)))),
                    )),
                    Box::new(Expr::Not(Box::new(Expr::Eq(
                        Box::new(Expr::Controller(Box::new(Expr::Ctx(Ctx::It)))),
                        Box::new(Expr::Ctx(Ctx::Controller)),
                    )))),
                )),
            },
            text: Some("Activated abilities of artifacts your opponents control can't be activated."),
        },
    ];
    card
}


/// Transmute Artifact — sacrifice an artifact on resolution, tutor an artifact,
/// and pay the positive mana-value difference or put the found card in graveyard.
fn transmute_artifact() -> CardDef {
    let factory: SpellFactory = Arc::new(|who, source_id, _x| {
        Effect(Arc::new(move |state, t, _targets| {
            let sacrifices: Vec<ObjId> = state.permanents_of(who)
                .filter(|o| state.def_of(o.id).map_or(false, |d| d.types.contains(&CardType::Artifact)))
                .map(|o| o.id)
                .collect();
            let Some(sacrificed) = state.with_strategy(who, |s, st| s.sacrifice_choice(who, &sacrifices, st)) else {
                // Oracle: if no artifact is sacrificed, the search instruction is
                // skipped as well, so the library is not shuffled.
                return;
            };
            let sacrificed_mv = state.def_of(sacrificed).map(|d| mana_value(d.mana_cost())).unwrap_or(0);
            change_zone(sacrificed, ZoneId::Graveyard, state, t, who);

            let candidates: Vec<ObjId> = state.library_of(who)
                .filter(|o| state.def_of(o.id).map_or(false, |d| d.types.contains(&CardType::Artifact)))
                .map(|o| o.id)
                .collect();
            let chosen = state.with_strategy(who, |s, st| s.choose_for_effect(source_id, &candidates, st));
            let Some(found) = chosen.filter(|id| candidates.contains(id)) else {
                state.shuffle_library(who);
                return;
            };
            let found_mv = state.def_of(found).map(|d| mana_value(d.mana_cost())).unwrap_or(0);
            let difference = (found_mv - sacrificed_mv).max(0);
            let mut paid = difference == 0;
            if difference > 0 {
                let cost = ManaCost { generic: difference, ..Default::default() };
                run_mana_loop(state, t, who, &cost);
                if state.player(who).pool.can_pay(&cost) {
                    state.player_mut(who).pool.spend(&cost);
                    paid = true;
                }
            }
            change_zone(found, if paid { ZoneId::Battlefield } else { ZoneId::Graveyard }, state, t, who);
            state.shuffle_library(who);
        }))
    });
    simple(
        "Transmute Artifact",
        CardKind::Sorcery(SpellData {
            mana_cost: "UU".into(),
            modes: Some(SpellModes::Single(SpellMode { target_spec: TargetSpec::None, factory })),
            ..Default::default()
        }),
        parse_colors("UU", true, false),
        None,
    )
}

/// Kozilek's Command — XCC, choose two. All six mode pairs are represented.
/// Graveyard-exile modes use the engine's choice primitive repeatedly through
/// a dedicated legacy effect, while the other modes stay declarative IR.
fn kozileks_command() -> CardDef {
    use crate::ir::action::{Action, TokenSpec, Who as IrWho};
    use crate::ir::executor::{execute, BindEnv};
    use crate::ir::expr::Expr;

    #[derive(Clone, Copy)]
    struct Pair {
        spawn: bool,
        scry_draw: bool,
        creature: bool,
        graveyard: bool,
    }

    fn mode_factory(pair: Pair) -> SpellFactory {
        Arc::new(move |who, source_id, chosen_x| {
            Effect(Arc::new(move |state, t, targets| {
                let x = chosen_x as i64;

                if pair.spawn && x > 0 {
                    let action = Action::CreateToken {
                        who: IrWho::You,
                        spec: TokenSpec {
                            name: "Eldrazi Spawn Token",
                            types: vec![CardType::Creature],
                            subtypes: vec!["Eldrazi", "Spawn"],
                            colors: vec![],
                            power: Some(0),
                            toughness: Some(1),
                            keywords: vec![],
                        },
                        n: Expr::Num(x),
                        bind_as: None,
                    };
                    let env = BindEnv::new().with_source(source_id).with_controller(who);
                    let _ = execute(&action, state, &env);
                }

                if pair.scry_draw {
                    eff_scry(who, chosen_x as usize).call(state, t, &[]);
                    sim_draw(state, who, t, false);
                }

                if pair.creature {
                    if let Some(&id) = targets.first() {
                        let mv = state.def_of(id)
                            .map(|d| mana_value(d.mana_cost()))
                            .unwrap_or(i32::MAX);
                        if mv <= chosen_x as i32 {
                            change_zone(id, ZoneId::Exile, state, t, who);
                        }
                    }
                }

                if pair.graveyard && chosen_x > 0 {
                    // Oracle uses targets; for the simulator we route the same
                    // card selection through the strategy at resolution. This
                    // preserves the resulting game state while avoiding a new
                    // variable-length multi-target announcement primitive.
                    for _ in 0..chosen_x {
                        let candidates: Vec<ObjId> = state.graveyard_of(PlayerId::Us)
                            .chain(state.graveyard_of(PlayerId::Opp))
                            .map(|o| o.id)
                            .collect();
                        if candidates.is_empty() { break; }
                        let picked = state.with_strategy(who, |strategy, st| {
                            strategy.choose_for_effect(source_id, &candidates, st)
                        });
                        let Some(id) = picked.filter(|id| candidates.contains(id)) else { break; };
                        change_zone(id, ZoneId::Exile, state, t, who);
                    }
                }
            }))
        })
    }

    let creature_target = TargetSpec::ObjectInZone {
        controller: Who::Any,
        zone: ZoneId::Battlefield,
        filter: ir_type(CardType::Creature),
    };
    let pairs = [
        (TargetSpec::None, Pair { spawn: true,  scry_draw: true,  creature: false, graveyard: false }),
        (creature_target.clone(), Pair { spawn: true,  scry_draw: false, creature: true,  graveyard: false }),
        (TargetSpec::None, Pair { spawn: true,  scry_draw: false, creature: false, graveyard: true  }),
        (creature_target.clone(), Pair { spawn: false, scry_draw: true,  creature: true,  graveyard: false }),
        (TargetSpec::None, Pair { spawn: false, scry_draw: true,  creature: false, graveyard: true  }),
        (creature_target, Pair { spawn: false, scry_draw: false, creature: true,  graveyard: true  }),
    ];
    let modes = pairs.into_iter()
        .map(|(target_spec, pair)| SpellMode { target_spec, factory: mode_factory(pair) })
        .collect();

    let mut card = simple(
        "Kozilek's Command",
        CardKind::Instant(SpellData {
            mana_cost: "CC".into(),
            modes: Some(SpellModes::Modal(modes)),
            ..Default::default()
        }),
        vec![], None,
    );
    card.additional_costs = ir_xmana_cost();
    card
}

/// Trinisphere — while untapped, spells whose mana component would be below three
/// are raised to a three-mana floor after other cost modifications.
fn trinisphere() -> CardDef {
    let floor: StaticAbilityDef = Arc::new(|source_id, controller| ContinuousInstance {
        source_id,
        controller,
        layer: ContinuousLayer::L3TextEffects,
        reads: vec![],
        writes: vec![],
        timestamp: 0,
        filter: Arc::new(move |_target_id, _target_controller, state| {
            state.permanent_bf(source_id).map_or(false, |bf| !bf.tapped)
        }),
        modifier: Arc::new(|def, _state| {
            if !def.is_land() {
                def.minimum_cast_mana = def.minimum_cast_mana.max(3);
            }
        }),
        expiry: Expiry::WhileSourceOnBattlefield,
    });
    CardDef::new(
        "Trinisphere",
        CardKind::Artifact(ArtifactData { mana_cost: "3".into(), ..Default::default() }),
        vec![], None, vec![], CardLayout::Normal, None,
        vec![], vec![], vec![], vec![floor],
    )
}

/// Tezzeret, Cruel Captain — artifact-ETB loyalty, 0 untap/counter, -3 tutor,
/// and the -7 beginning-of-combat emblem.
fn tezzeret_cruel_captain() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, EventPattern, TriggerSpec};
    use crate::ir::action::{Action, Who as IrWho};
    use crate::ir::ce::CEMod;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, Filter, ZoneKindSel};

    let your_artifact = Filter(Expr::And(
        Box::new(Expr::Contains(
            Box::new(Expr::TypeLit(CardType::Artifact)),
            Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::It)))),
        )),
        Box::new(Expr::Eq(
            Box::new(Expr::Controller(Box::new(Expr::Ctx(Ctx::It)))),
            Box::new(Expr::Ctx(Ctx::Controller)),
        )),
    ));
    let target_artifact_or_creature = ir_or(ir_type(CardType::Artifact), ir_type(CardType::Creature));
    let artifact_creature = Expr::And(
        Box::new(Expr::Contains(
            Box::new(Expr::TypeLit(CardType::Artifact)),
            Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::Var("target"))))),
        )),
        Box::new(Expr::Contains(
            Box::new(Expr::TypeLit(CardType::Creature)),
            Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::Var("target"))))),
        )),
    );

    let mut card = CardDef::new(
        "Tezzeret, Cruel Captain",
        CardKind::Planeswalker(PlaneswalkerData { mana_cost: "3".into(), loyalty: 4, abilities: vec![] }),
        vec![], None, vec![Supertype::Legendary], CardLayout::Normal, None,
        vec![], vec![], vec![], vec![],
    );
    card.abilities = vec![
        ir_planeswalker_etb_loyalty(4),
        Ability {
            kind: AbilityKind::Triggered {
                spec: TriggerSpec::When {
                    pattern: EventPattern::EntersZone { obj_filter: your_artifact, zone_kind: ZoneKindSel::Battlefield },
                    condition: None,
                },
                target_spec: TargetSpec::None,
                body: Action::PutCounters { on: Expr::Ctx(Ctx::Source), kind: CounterType::Loyalty, n: Expr::Num(1) },
                active_zone: ZoneKindSel::Battlefield,
            },
            text: Some("Whenever an artifact you control enters, put a loyalty counter on Tezzeret."),
        },
        Ability {
            kind: AbilityKind::Activated {
                cost: ir_loyalty(0),
                target_spec: TargetSpec::ObjectInZone { controller: Who::Any, zone: ZoneId::Battlefield, filter: target_artifact_or_creature },
                choice_spec: None,
                body: Action::Sequence(vec![
                    Action::Untap { target: Expr::Ctx(Ctx::Var("target")) },
                    Action::IfThen {
                        cond: artifact_creature,
                        then: Box::new(Action::PutCounters { on: Expr::Ctx(Ctx::Var("target")), kind: CounterType::PlusOnePlusOne, n: Expr::Num(1) }),
                        else_: None,
                    },
                ]),
                timing: ActivationTiming::Sorcery,
                activation_condition: None,
                active_zone: ZoneKindSel::Battlefield,
            },
            text: Some("0: Untap target artifact or creature. If it's an artifact creature, put a +1/+1 counter on it."),
        },
        Ability {
            kind: AbilityKind::Activated {
                cost: ir_loyalty(-3),
                target_spec: TargetSpec::None,
                choice_spec: None,
                body: Action::Search {
                    who: IrWho::You,
                    zone: ZoneKindSel::Library,
                    filter: ir_and(ir_type(CardType::Artifact), ir_mv_le(1)),
                    count: Expr::Num(1),
                    dest: ZoneKindSel::Hand,
                    to_top: false,
                    shuffle: true,
                    bind_as: None,
                },
                timing: ActivationTiming::Sorcery,
                activation_condition: None,
                active_zone: ZoneKindSel::Battlefield,
            },
            text: Some("-3: Search your library for an artifact card with mana value 1 or less, put it into your hand, then shuffle."),
        },
        Ability {
            kind: AbilityKind::Activated {
                cost: ir_loyalty(-7),
                target_spec: TargetSpec::None,
                choice_spec: None,
                body: Action::CreateEmblem {
                    abilities: vec![Ability {
                        kind: AbilityKind::Triggered {
                            spec: TriggerSpec::AtStep {
                                step: StepKind::BeginCombat,
                                who: crate::ir::ability::StepScope::You,
                                condition: None,
                            },
                            target_spec: TargetSpec::ObjectInZone {
                                controller: Who::Actor,
                                zone: ZoneId::Battlefield,
                                filter: ir_type(CardType::Artifact),
                            },
                            body: Action::Sequence(vec![
                                Action::PutCounters {
                                    on: Expr::Ctx(Ctx::Var("target")),
                                    kind: CounterType::PlusOnePlusOne,
                                    n: Expr::Num(3),
                                },
                                Action::IfThen {
                                    cond: Expr::Not(Box::new(Expr::Contains(
                                        Box::new(Expr::TypeLit(CardType::Creature)),
                                        Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::Var("target"))))),
                                    ))),
                                    then: Box::new(Action::ApplyCE {
                                        target: Expr::Ctx(Ctx::Var("target")),
                                        mods: vec![CEMod::BecomeCreature {
                                            power: Expr::Num(0),
                                            toughness: Expr::Num(0),
                                            subtypes: vec!["Robot".to_string()],
                                            keywords: vec![],
                                        }],
                                        expiry: crate::ir::action::Expiry::Permanent,
                                    }),
                                    else_: None,
                                },
                            ]),
                            // Emblems are stored outside normal zones; trigger dispatch
                            // explicitly ignores this source-zone marker for emblems.
                            active_zone: ZoneKindSel::Battlefield,
                        },
                        text: Some("At the beginning of combat on your turn, put three +1/+1 counters on target artifact you control. If it isn't a creature, it becomes a 0/0 Robot artifact creature."),
                    }],
                },
                timing: ActivationTiming::Sorcery,
                activation_condition: None,
                active_zone: ZoneKindSel::Battlefield,
            },
            text: Some("-7: You get an emblem with a beginning-of-combat artifact animation trigger."),
        },
    ];
    card
}

/// IR ability: "Nonbasic lands are Mountains." Shared between Blood Moon and
/// Magus of the Moon. CR 305.6 / 305.7 / 613.1d. The scope filter is `None`:
/// the `SetBasicLandType` modifier is the sole gating point (non-lands and
/// basics short-circuit inside the modifier itself).
fn nonbasic_lands_are_mountains_ir() -> crate::ir::ability::Ability {
    use crate::ir::ability::{Ability, AbilityKind};
    use crate::ir::ce::{BasicLandType, CEMod};
    Ability {
        kind: AbilityKind::Static {
            mods: vec![CEMod::SetBasicLandType(BasicLandType::Mountain)],
            scope: None,
            condition: None,
        },
        text: Some("Nonbasic lands are Mountains."),
    }
}

/// Enchantment {2R}. Static: "Nonbasic lands are Mountains." CR 305.7, 613.1d.
fn blood_moon() -> CardDef {
    let mut card = CardDef::new(
        "Blood Moon",
        CardKind::Enchantment(EnchantmentData::default()),
        parse_colors("2R", false, false),
        None,
        vec![], CardLayout::Normal, None,
        vec![], vec![], vec![],
        vec![],
    );
    card.abilities = vec![nonbasic_lands_are_mountains_ir()];
    card
}

/// Creature {2R}, 2/2. Static: "Nonbasic lands are Mountains." CR 305.7, 613.1d.
fn magus_of_the_moon() -> CardDef {
    let data = CreatureData::new("2R", 2, 2);
    let mut card = CardDef::new(
        "Magus of the Moon",
        CardKind::Creature(data),
        parse_colors("2R", false, false),
        None,
        vec![], CardLayout::Normal, None,
        vec![], vec![], vec![],
        vec![],
    );
    card.abilities = vec![nonbasic_lands_are_mountains_ir()];
    card
}

/// IR ability: "Each land is a <type> in addition to its other land types."
/// Shared between Urborg, Tomb of Yawgmoth and Yavimaya, Cradle of Growth.
/// CR 305.6 / 613.1d. No scope — the modifier's early-return for non-lands
/// is the sole filter.
fn each_land_is_also_ir(
    kind: crate::ir::ce::BasicLandType,
    text: &'static str,
) -> crate::ir::ability::Ability {
    use crate::ir::ability::{Ability, AbilityKind};
    use crate::ir::ce::CEMod;
    Ability {
        kind: AbilityKind::Static {
            mods: vec![CEMod::AddBasicLandType(kind)],
            scope: None,
            condition: None,
        },
        text: Some(text),
    }
}

/// Legendary Land. "Each land is a Swamp in addition to its other land types."
/// Adds Swamp type and "{T}: Add {B}" to all lands. CR 305.7, 613.1d.
fn urborg_tomb_of_yawgmoth() -> CardDef {
    use crate::ir::ce::BasicLandType;
    let mut card = CardDef::new(
        "Urborg, Tomb of Yawgmoth",
        CardKind::Land(LandData::default()),
        vec![],
        None,
        vec![Supertype::Legendary], CardLayout::Normal, None,
        vec![], vec![], vec![],
        vec![],
    );
    card.abilities = vec![each_land_is_also_ir(
        BasicLandType::Swamp,
        "Each land is a Swamp in addition to its other land types.",
    )];
    card
}

/// Legendary Land. "Each land is a Forest in addition to its other land types."
/// Adds Forest type and "{T}: Add {G}" to all lands. CR 305.7, 613.1d.
fn yavimaya_cradle_of_growth() -> CardDef {
    use crate::ir::ce::BasicLandType;
    let mut card = CardDef::new(
        "Yavimaya, Cradle of Growth",
        CardKind::Land(LandData::default()),
        vec![],
        None,
        vec![Supertype::Legendary], CardLayout::Normal, None,
        vec![], vec![], vec![],
        vec![],
    );
    card.abilities = vec![each_land_is_also_ir(
        BasicLandType::Forest,
        "Each land is a Forest in addition to its other land types.",
    )];
    card
}

/// Land. "This land enters tapped unless you control a Mountain or a Forest."
/// {T}: Add {U}.
/// {U}, {T}: The next spell you cast this turn can't be countered. (CR 611.2f)
fn mistrise_village() -> CardDef {
    use crate::ir::ability::{
        Ability, AbilityKind, EventPattern, ReplacementBody,
    };
    use crate::ir::action::{Action, Expiry as IrExpiry, Who};
    use crate::ir::ce::CEMod;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, Filter, ZoneKindSel, ZoneSel};

    // ETB replacement: enters tapped unless you control a Mountain or Forest.
    // Condition (the replacement fires iff true): you control zero lands whose
    // subtypes include "mountain" or "forest" — evaluated on the materialized
    // view, so Yavimaya/Urborg-style CE effects are honored.
    let self_etb = Filter(Expr::Eq(
        Box::new(Expr::Ctx(Ctx::It)),
        Box::new(Expr::Ctx(Ctx::Source)),
    ));
    let controller_has_mountain_or_forest = Expr::AllObjects {
        zone: ZoneSel::Global(ZoneKindSel::Battlefield),
        bind: "p",
        filter: Box::new(Expr::And(
            Box::new(Expr::Eq(
                Box::new(Expr::Controller(Box::new(Expr::Ctx(Ctx::Var("p"))))),
                Box::new(Expr::Ctx(Ctx::Controller)),
            )),
            Box::new(Expr::Or(
                Box::new(Expr::Contains(
                    Box::new(Expr::SubtypeLit("mountain".to_string())),
                    Box::new(Expr::Subtypes(Box::new(Expr::Ctx(Ctx::Var("p"))))),
                )),
                Box::new(Expr::Contains(
                    Box::new(Expr::SubtypeLit("forest".to_string())),
                    Box::new(Expr::Subtypes(Box::new(Expr::Ctx(Ctx::Var("p"))))),
                )),
            )),
        )),
    };
    let enters_tapped = Ability {
        kind: AbilityKind::Replacement {
            matches: EventPattern::EntersZone {
                obj_filter: self_etb,
                zone_kind: ZoneKindSel::Battlefield,
            },
            condition: Some(Expr::Eq(
                Box::new(Expr::Count(Box::new(controller_has_mountain_or_forest))),
                Box::new(Expr::Num(0)),
            )),
            body: ReplacementBody::Replace(Action::Sequence(vec![
                Action::Move {
                    what: Expr::Ctx(Ctx::Var("triggered_obj")),
                    to: ZoneKindSel::Battlefield,
                    to_owner: None,
                    bind_as: None,
                },
                Action::Tap { target: Expr::Ctx(Ctx::Var("triggered_obj")) },
            ])),
            active_zone: None, // self-entry replacement
        },
        text: Some("~ enters tapped unless you control a Mountain or a Forest."),
    };

    // {U},{T}: The next spell you cast this turn can't be countered. (CR 611.2f)
    let next_spell_uncounterable = Ability {
        kind: AbilityKind::Activated {
            cost: ir_seq(vec![act_pay_mana_str("U"), act_tap_source()]),
            target_spec: TargetSpec::None,
            choice_spec: None,
            body: Action::GrantCEToNextSpellCast {
                who: Who::You,
                predicate: None,
                mods: vec![CEMod::Uncounterable],
                expiry: IrExpiry::EndOfTurn,
            },
            timing: ActivationTiming::Default,
            activation_condition: None,
            active_zone: crate::ir::expr::ZoneKindSel::Battlefield,
        },
        text: Some("{U}, {T}: The next spell you cast this turn can't be countered."),
    };

    let mut card = CardDef::new(
        "Mistrise Village",
        CardKind::Land(LandData {
            mana_abilities: vec![tap_produces("U")],
            ..Default::default()
        }),
        vec![], None, vec![], CardLayout::Normal, None,
        vec![],
        vec![],
        vec![],
        vec![],
    );
    card.abilities = vec![enters_tapped, next_spell_uncounterable];
    card
}

/// Great Furnace — Artifact Land. {T}: Add {R}.
/// Primary kind is Land; additionally typed as Artifact (for Brotherhood's End, etc.).
fn great_furnace() -> CardDef {
    let mut def = simple("Great Furnace", CardKind::Land(LandData {
        mana_abilities: vec![],
        ..Default::default()
    }), vec![], None);
    def.types.push(CardType::Artifact);
    def.abilities.push(ir_tap_mana("R"));
    def
}

/// Front: Brazen Borrower — 3/1 flying creature for {1UU}.
/// Back (adventure): Petty Theft — instant for {1U}, bounce a nonland permanent. CR 715.
// ── Tokens ────────────────────────────────────────────────────────────────────

/// 0/0 Orc Army creature token. Created and grown by Amass Orcs. CR 701.45.
fn orc_army_token() -> CardDef {
    simple("Orc Army", CardKind::Creature(CreatureData::new("", 0, 0)), vec![], None)
}

/// Colorless Clue artifact token. Activated ability: {2}, tap self, sacrifice self → draw one.
/// CR 701.28 (Investigate).
fn clue_token() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind};
    use crate::ir::action::{Action, Who as IrWho};
    use crate::ir::expr::Expr;

    let mut card = simple(
        "Clue Token",
        CardKind::Artifact(ArtifactData {
            mana_cost: String::new(),
            ..Default::default()
        }),
        vec![],
        None,
    );
    card.abilities = vec![Ability {
        kind: AbilityKind::Activated {
            cost: ir_seq(vec![
                act_pay_mana_str("2"),
                act_tap_source(),
                act_sac_self("$clue_self"),
            ]),
            target_spec: TargetSpec::None,
            choice_spec: None,
            body: Action::Draw {
                who: IrWho::You,
                n: Expr::Num(1),
            },
            timing: ActivationTiming::Default,
            activation_condition: None,
            active_zone: crate::ir::expr::ZoneKindSel::Battlefield,
        },
        text: Some("{2}, {T}, Sacrifice this artifact: Draw a card."),
    }];
    card
}

/// 1/1 white Monk creature token with prowess.
/// Prowess: "Whenever you cast a noncreature spell, this creature gets +1/+1 until end of turn."
fn monk_token() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, EventPattern, TriggerSpec};
    use crate::ir::action::{Action, Expiry as IrExpiry};
    use crate::ir::ce::CEMod;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel};
    let mut card = CardDef::new(
        "Monk Token",
        CardKind::Creature(CreatureData::new("", 1, 1)),
        vec![Color::White],
        None,
        vec![], CardLayout::Normal, None,
        vec![], vec![], vec![], vec![],
    );
    // Prowess: whenever you cast a noncreature spell, +1/+1 until EOT. The spell's
    // noncreature-ness is in the pattern's `spell_filter` (`It` = the spell); the
    // "you cast" scope is the condition (`triggered_actor == Controller`).
    card.abilities = vec![Ability {
        kind: AbilityKind::Triggered {
            spec: TriggerSpec::When {
                pattern: EventPattern::SpellCast {
                    spell_filter: ir_not(ir_type(CardType::Creature)),
                },
                condition: Some(Expr::Eq(
                    Box::new(Expr::Ctx(Ctx::Var("triggered_actor"))),
                    Box::new(Expr::Ctx(Ctx::Controller)),
                )),
            },
            target_spec: TargetSpec::None,
            body: Action::ApplyCE {
                target: Expr::Ctx(Ctx::Source),
                mods: vec![CEMod::PumpPT(Expr::Num(1), Expr::Num(1))],
                expiry: IrExpiry::EndOfTurn,
            },
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("Whenever you cast a noncreature spell, this creature gets +1/+1 until end of turn."),
    }];
    card
}

/// 0/0 black Phyrexian Germ creature token. Created by Living Weapon equipment (CR 702.92).
fn phyrexian_germ_token() -> CardDef {
    let mut data = CreatureData::new("", 0, 0);
    data.creature_subtypes = vec!["Phyrexian".into(), "Germ".into()];
    simple("Phyrexian Germ", CardKind::Creature(data), vec![Color::Black], None)
}

/// 4/4 colorless Construct artifact creature token with flying and haste — made
/// four-at-a-time when The Fantasticar pops. (Artifact-creature typing is cosmetic
/// for goldfishing; modeled as a plain creature token like the others.)
fn fantasticar_construct_token() -> CardDef {
    let mut data = CreatureData::new("", 4, 4);
    data.creature_subtypes = vec!["Construct".into()];
    data.keywords = Keywords::from_slice(&[Keyword::Flying, Keyword::Haste]);
    simple("Fantasticar Construct", CardKind::Creature(data), vec![], None)
}

/// 2/2 colorless face-down creature token produced by the "cloak" keyword (CR 702.169).
/// ABNORMAL: real cloak puts the actual top card of library onto the battlefield face-down
/// (still that specific card, just hidden/characteristic-stripped), and grants ward {2} and
/// the ability to turn face up for its mana cost if it's a creature card. We model this as a
/// plain 2/2 token — ward/turn-face-up/identity-as-top-card are all omitted. Use anywhere a
/// cloaked permanent is needed (e.g. Cryptic Coat).
fn mysterious_creature_token() -> CardDef {
    let data = CreatureData::new("", 2, 2);
    simple("Mysterious Creature", CardKind::Creature(data), vec![], None)
}


/// Urza's Saga Construct — 0/0 colorless Construct artifact creature with the
/// live characteristic "gets +1/+1 for each artifact you control."
fn construct_token() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind};
    use crate::ir::ce::CEMod;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, Filter};
    let mut data = CreatureData::new("", 0, 0);
    data.creature_subtypes = vec!["Construct".into()];
    let mut card = simple("Construct Token", CardKind::Creature(data), vec![], None);
    card.types.push(CardType::Artifact);
    let artifacts = controlled_artifact_count_expr("construct_artifact");
    card.abilities = vec![Ability {
        kind: AbilityKind::Static {
            mods: vec![CEMod::SetPT(artifacts.clone(), artifacts)],
            scope: Some(Filter(Expr::Eq(
                Box::new(Expr::Ctx(Ctx::It)),
                Box::new(Expr::Ctx(Ctx::Source)),
            ))),
            condition: None,
        },
        text: Some("This creature's power and toughness are each equal to the number of artifacts you control."),
    }];
    card
}

/// 0/1 colorless Eldrazi Spawn token. Sacrifice: add {C}.
fn eldrazi_spawn_token() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind};
    use crate::ir::action::{Action, ManaSpec, Who};
    use crate::ir::expr::{Expr, ZoneKindSel};
    let mut data = CreatureData::new("", 0, 1);
    data.creature_subtypes = vec!["Eldrazi".into(), "Spawn".into()];
    let mut card = simple("Eldrazi Spawn Token", CardKind::Creature(data), vec![], None);
    card.abilities = vec![Ability {
        kind: AbilityKind::Activated {
            cost: ir_sac_self("$spawn_self"),
            target_spec: TargetSpec::None,
            choice_spec: None,
            body: Action::AddMana { who: Who::You, count: Expr::Num(1), spec: ManaSpec::Fixed(vec![]) },
            timing: ActivationTiming::Default,
            activation_condition: None,
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("Sacrifice this creature: Add {C}."),
    }];
    card
}

/// The Fantasticar — {3} Legendary Artifact — Vehicle (4/4, flying).
/// "Whenever you cast your fourth noncreature spell each turn, you may sacrifice
///  The Fantasticar. If you do, create four 4/4 colorless Construct artifact
///  creature tokens with flying and haste."
///
/// Pure-IR triggered ability. The 4th-spell count is read from the event log
/// (`EventCount` over noncreature `SpellCast`s this turn) — the triggering cast
/// is logged by `fire_event` before triggers dispatch, so `== 4` means "this is
/// the 4th". `active_zone: Battlefield` is what makes the "exactly the 4th"
/// rules-text exact: if The Fantasticar isn't in play when the 4th spell is cast
/// (e.g. it *is* the 4th spell, still on the stack, or it landed only after the
/// 4th), the trigger isn't armed and there is no pop.
///
/// The printed "you may … if you do" optionality is collapsed to an always-pop
/// (mandatory) effect: declining is never correct for the goldfish, and a `MayDo`
/// gate would collide with the goldfish strategy's `Mode` handling (Ponder's
/// shuffle). Faithful optionality is deferred (see plan).
fn the_fantasticar() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, EventPattern, TriggerSpec};
    use crate::ir::action::{Action, TokenSpec, Who};
    use crate::ir::context::Ctx;
    use crate::ir::event_log::Window;
    use crate::ir::expr::{EventFilter, Expr, Filter, ZoneKindSel};

    let mut card = simple(
        "The Fantasticar",
        CardKind::Artifact(ArtifactData {
            mana_cost: "3".to_string(),
            subtypes: vec!["Vehicle".into()],
            ..Default::default()
        }),
        vec![], // colorless
        Some(25),
    );
    card.supertypes = vec![Supertype::Legendary];

    // "noncreature spell" predicate, reused by the trigger pattern and the count.
    let noncreature = || ir_not(ir_type(CardType::Creature));

    // Condition: you cast it, and it is your 4th noncreature spell this turn.
    let condition = Expr::And(
        Box::new(Expr::Eq(
            Box::new(Expr::Ctx(Ctx::Var("triggered_actor"))),
            Box::new(Expr::Ctx(Ctx::Controller)),
        )),
        Box::new(Expr::Eq(
            Box::new(Expr::EventCount {
                window: Window::ThisTurn,
                filter: Box::new(EventFilter::SpellCast {
                    caster: Some(Box::new(Expr::Ctx(Ctx::Controller))),
                    card: None,
                    spell_filter: Some(Box::new(noncreature())),
                    alt_cost: None,
                }),
            }),
            Box::new(Expr::Num(4)),
        )),
    );

    // Sacrifice this (It == Source), then create four Construct tokens.
    let sac_self = Action::Sacrifice {
        who: Who::You,
        filter: Filter(Expr::Eq(
            Box::new(Expr::Ctx(Ctx::It)),
            Box::new(Expr::Ctx(Ctx::Source)),
        )),
        count: Expr::Num(1),
        bind_as: None,
    };
    let make_tokens = Action::CreateToken {
        who: Who::You,
        // do_create_token resolves the token by name from the catalog; the other
        // fields are documentary (mirror the registered token).
        spec: TokenSpec {
            name: "Fantasticar Construct",
            types: vec![CardType::Artifact, CardType::Creature],
            subtypes: vec!["Construct"],
            colors: vec![],
            power: Some(4),
            toughness: Some(4),
            keywords: vec![Keyword::Flying, Keyword::Haste],
        },
        n: Expr::Num(4),
        bind_as: None,
    };

    card.abilities = vec![Ability {
        kind: AbilityKind::Triggered {
            spec: TriggerSpec::When {
                pattern: EventPattern::SpellCast { spell_filter: noncreature() },
                condition: Some(condition),
            },
            target_spec: TargetSpec::None,
            body: Action::Sequence(vec![sac_self, make_tokens]),
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some(
            "Whenever you cast your fourth noncreature spell each turn, sacrifice The \
             Fantasticar. If you do, create four 4/4 colorless Construct artifact creature \
             tokens with flying and haste.",
        ),
    }];
    card
}

/// Cori-Steel Cutter — {1}{R} Artifact — Equipment.
/// "Equipped creature gets +1/+1 and has trample and haste."
/// "Flurry — Whenever you cast your second spell each turn, create a 1/1 white Monk
///  creature token with prowess. You may attach this Equipment to it."
/// "Equip {1}{R}"
/// Build an `AbilityKind::Static` continuous effect scoped to the creature this
/// source (an Equipment) is attached to — `Eq(It, AttachedTo(Source))`. Each
/// `CEMod` becomes one CI at its own CR-613 layer (P/T pumps → L7, keyword grants
/// → L6) via the shared `cemod_to_modifier`. Replaces the per-equipment
/// hand-rolled `ContinuousInstance` closures.
fn equipped_creature_ce(mods: Vec<crate::ir::ce::CEMod>, text: &'static str) -> crate::ir::ability::Ability {
    use crate::ir::ability::{Ability, AbilityKind};
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, Filter};
    Ability {
        kind: AbilityKind::Static {
            mods,
            scope: Some(Filter(Expr::Eq(
                Box::new(Expr::Ctx(Ctx::It)),
                Box::new(Expr::AttachedTo(Box::new(Expr::Ctx(Ctx::Source)))),
            ))),
            condition: None,
        },
        text: Some(text),
    }
}

/// "Equip [cost]" (CR 702.6) as a reusable IR activated ability: pay `cost`,
/// target a creature you control, attach this Equipment to it. Sorcery-speed.
fn ir_equip(cost: &'static str, text: &'static str) -> crate::ir::ability::Ability {
    use crate::ir::ability::{Ability, AbilityKind};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel};
    Ability {
        kind: AbilityKind::Activated {
            cost: ir_pay_mana_str(cost),
            target_spec: TargetSpec::ObjectInZone {
                controller: Who::Actor,
                zone: ZoneId::Battlefield,
                filter: ir_type(CardType::Creature),
            },
            choice_spec: None,
            body: Action::Attach {
                what: Expr::Ctx(Ctx::Source),
                to: Expr::Ctx(Ctx::Var("target")),
            },
            timing: ActivationTiming::Sorcery,
            activation_condition: None,
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some(text),
    }
}

/// "[cost]: Return this Equipment to its owner's hand." A reusable bounce-self
/// activated ability (Batterskull, Cryptic Coat). Instant speed.
fn ir_bounce_self(cost: &'static str, text: &'static str) -> crate::ir::ability::Ability {
    use crate::ir::ability::{Ability, AbilityKind};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel};
    Ability {
        kind: AbilityKind::Activated {
            cost: ir_pay_mana_str(cost),
            target_spec: TargetSpec::None,
            choice_spec: None,
            body: Action::Move {
                what: Expr::Ctx(Ctx::Source),
                to: ZoneKindSel::Hand,
                to_owner: None,
                bind_as: None,
            },
            timing: ActivationTiming::Default,
            activation_condition: None,
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some(text),
    }
}

fn cori_steel_cutter() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, EventPattern, TriggerSpec};
    use crate::ir::action::{Action, TokenSpec, Who as IrWho};
    use crate::ir::ce::CEMod;
    use crate::ir::context::Ctx;
    use crate::ir::event_log::Window;
    use crate::ir::expr::{EventFilter, Expr, ZoneKindSel};
    let mut card = CardDef::new(
        "Cori-Steel Cutter",
        CardKind::Artifact(ArtifactData {
            mana_cost: "1R".to_string(),
            subtypes: vec!["Equipment".into()],
            abilities: vec![], // equip is now an IR Activated ability (below)
            mana_abilities: vec![],
        }),
        parse_colors("R", false, false),
        None,
        vec![], CardLayout::Normal, None,
        vec![], // flurry is now an IR Triggered ability (below)
        vec![], vec![],
        vec![], // static CE now IR (card.abilities below)
    );

    // Flurry: "Whenever you cast your second spell each turn, create a 1/1 Monk
    // token; you may attach this to it." "Second spell this turn" is an event-log
    // count, not a counter field: the just-cast spell is already logged (fire_event
    // Stage 3b) before triggers dispatch, so this is the 2nd iff the controller's
    // SpellCast count this turn == 2.
    let is_my_second_spell = Expr::And(
        Box::new(Expr::Eq(
            Box::new(Expr::Ctx(Ctx::Var("triggered_actor"))),
            Box::new(Expr::Ctx(Ctx::Controller)),
        )),
        Box::new(Expr::Eq(
            Box::new(Expr::EventCount {
                window: Window::ThisTurn,
                filter: Box::new(EventFilter::SpellCast {
                    caster: Some(Box::new(Expr::Ctx(Ctx::Controller))),
                    card: None,
                    spell_filter: None,
                    alt_cost: None,
                }),
            }),
            Box::new(Expr::Num(2)),
        )),
    );
    let flurry = Ability {
        kind: AbilityKind::Triggered {
            spec: TriggerSpec::When {
                pattern: EventPattern::SpellCast { spell_filter: ir_any() },
                condition: Some(is_my_second_spell),
            },
            target_spec: TargetSpec::None,
            body: Action::Sequence(vec![
                Action::CreateToken {
                    who: IrWho::You,
                    spec: TokenSpec {
                        name: "Monk Token",
                        types: vec![CardType::Creature],
                        subtypes: vec!["Monk"],
                        colors: vec![Color::Red],
                        power: Some(1),
                        toughness: Some(1),
                        keywords: vec![],
                    },
                    n: Expr::Num(1),
                    bind_as: Some("monk"),
                },
                // "You may attach Cori-Steel Cutter to it."
                Action::MayDo {
                    who: IrWho::You,
                    action: Box::new(Action::Attach {
                        what: Expr::Ctx(Ctx::Source),
                        to: Expr::Ctx(Ctx::Var("monk")),
                    }),
                },
            ]),
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("Whenever you cast your second spell each turn, create a 1/1 red Monk creature token, then you may attach Cori-Steel Cutter to it."),
    };
    // Equipped creature gets +1/+1 and has trample and haste.
    card.abilities = vec![
        flurry,
        ir_equip("1R", "Equip {1}{R}"),
        equipped_creature_ce(
            vec![
                CEMod::PumpPT(Expr::Num(1), Expr::Num(1)),
                CEMod::AddKeyword(Keyword::Trample),
                CEMod::AddKeyword(Keyword::Haste),
            ],
            "Equipped creature gets +1/+1 and has trample and haste.",
        ),
    ];
    card
}

/// Batterskull — {5} Artifact — Equipment.
/// "Living weapon (When this Equipment enters, create a 0/0 black Phyrexian Germ
///  creature token, then attach this to it.)"
/// "Equipped creature gets +4/+4 and has vigilance and lifelink."
/// "{3}: Return this Equipment to its owner's hand."
/// "Equip {5}"
fn batterskull() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, EventPattern, TriggerSpec};
    use crate::ir::action::{Action, TokenSpec, Who as IrWho};
    use crate::ir::ce::CEMod;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel};
    let mut card = CardDef::new(
        "Batterskull",
        CardKind::Artifact(ArtifactData {
            mana_cost: "5".to_string(),
            subtypes: vec!["Equipment".into()],
            abilities: vec![], // bounce + equip are now IR Activated abilities (below)
            mana_abilities: vec![],
        }),
        vec![], None,
        vec![], CardLayout::Normal, None,
        vec![], // living-weapon ETB is now an IR Triggered ability (below)
        vec![], vec![],
        vec![], // static CE now IR (card.abilities below)
    );
    // Living weapon: "When this Equipment enters, create a 0/0 black Phyrexian Germ
    // creature token, then attach this to it." CreateToken binds the new token so the
    // following Attach can reference it.
    let living_weapon = Ability {
        kind: AbilityKind::Triggered {
            spec: TriggerSpec::When {
                pattern: EventPattern::EntersZone {
                    obj_filter: ir_self(),
                    zone_kind: ZoneKindSel::Battlefield,
                },
                condition: None,
            },
            target_spec: TargetSpec::None,
            body: Action::Sequence(vec![
                Action::CreateToken {
                    who: IrWho::You,
                    spec: TokenSpec {
                        name: "Phyrexian Germ",
                        types: vec![CardType::Creature],
                        subtypes: vec!["Phyrexian", "Germ"],
                        colors: vec![Color::Black],
                        power: Some(0),
                        toughness: Some(0),
                        keywords: vec![],
                    },
                    n: Expr::Num(1),
                    bind_as: Some("germ"),
                },
                Action::Attach {
                    what: Expr::Ctx(Ctx::Source),
                    to: Expr::Ctx(Ctx::Var("germ")),
                },
            ]),
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("Living weapon (When this Equipment enters, create a 0/0 black Phyrexian Germ creature token, then attach this to it.)"),
    };
    // Equipped creature gets +4/+4 and has vigilance and lifelink.
    card.abilities = vec![
        living_weapon,
        ir_bounce_self("3", "{3}: Return Batterskull to its owner's hand."),
        ir_equip("5", "Equip {5}"),
        equipped_creature_ce(
            vec![
                CEMod::PumpPT(Expr::Num(4), Expr::Num(4)),
                CEMod::AddKeyword(Keyword::Vigilance),
                CEMod::AddKeyword(Keyword::Lifelink),
            ],
            "Equipped creature gets +4/+4 and has vigilance and lifelink.",
        ),
    ];
    card
}

/// Meteor Sword — {7} Artifact — Equipment.
/// "When this Equipment enters, destroy target permanent."
/// "Equipped creature gets +3/+3."
/// "Equip {3}"
fn meteor_sword() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, EventPattern, TriggerSpec};
    use crate::ir::action::Action;
    use crate::ir::ce::CEMod;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel};
    let any_permanent_target = TargetSpec::Union(vec![
        TargetSpec::ObjectInZone {
            controller: Who::Actor,
            zone: ZoneId::Battlefield,
            filter: ir_any(),
        },
        TargetSpec::ObjectInZone {
            controller: Who::Opp,
            zone: ZoneId::Battlefield,
            filter: ir_any(),
        },
    ]);
    let mut card = CardDef::new(
        "Meteor Sword",
        CardKind::Artifact(ArtifactData {
            mana_cost: "7".to_string(),
            subtypes: vec!["Equipment".into()],
            abilities: vec![], // equip is now an IR Activated ability (below)
            mana_abilities: vec![],
        }),
        vec![], None,
        vec![], CardLayout::Normal, None,
        vec![], // ETB destroy is now an IR Triggered ability (below)
        vec![], vec![],
        vec![], // static CE now IR (card.abilities below)
    );
    // ETB: destroy target permanent.
    let etb_destroy = Ability {
        kind: AbilityKind::Triggered {
            spec: TriggerSpec::When {
                pattern: EventPattern::EntersZone { obj_filter: ir_self(), zone_kind: ZoneKindSel::Battlefield },
                condition: None,
            },
            target_spec: any_permanent_target,
            body: Action::Destroy { target: Expr::Ctx(Ctx::Var("target")) },
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("When Meteor Sword enters, destroy target permanent."),
    };
    // Equipped creature gets +3/+3.
    card.abilities = vec![
        etb_destroy,
        ir_equip("3", "Equip {3}"),
        equipped_creature_ce(
            vec![CEMod::PumpPT(Expr::Num(3), Expr::Num(3))],
            "Equipped creature gets +3/+3.",
        ),
    ];
    card
}

/// Pre-War Formalwear — {2}{W} Artifact — Equipment.
/// "When this Equipment enters, return target creature card with mana value 3 or less
///  from your graveyard to the battlefield and attach this Equipment to it."
/// "Equipped creature gets +2/+2 and has vigilance."
/// "Equip {3}"
fn pre_war_formalwear() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, EventPattern, TriggerSpec};
    use crate::ir::action::Action;
    use crate::ir::ce::CEMod;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel};
    let mut card = CardDef::new(
        "Pre-War Formalwear",
        CardKind::Artifact(ArtifactData {
            mana_cost: "2W".to_string(),
            subtypes: vec!["Equipment".into()],
            abilities: vec![], // equip is now an IR Activated ability (below)
            mana_abilities: vec![],
        }),
        parse_colors("2W", false, false), None,
        vec![], CardLayout::Normal, None,
        vec![], // ETB reanimate is now an IR Triggered ability (below)
        vec![], vec![],
        vec![], // static CE now IR (card.abilities below)
    );
    // "When this Equipment enters, return target creature card with mana value 3 or
    //  less from your graveyard to the battlefield, then attach this to it." The
    //  reanimated creature is bound as `Ctx::Var("target")`.
    let reanimate = Ability {
        kind: AbilityKind::Triggered {
            spec: TriggerSpec::When {
                pattern: EventPattern::EntersZone { obj_filter: ir_self(), zone_kind: ZoneKindSel::Battlefield },
                condition: None,
            },
            target_spec: TargetSpec::ObjectInZone {
                controller: Who::Actor,
                zone: ZoneId::Graveyard,
                filter: ir_and(ir_type(CardType::Creature), ir_mv_le(3)),
            },
            body: Action::Sequence(vec![
                Action::Move {
                    what: Expr::Ctx(Ctx::Var("target")),
                    to: ZoneKindSel::Battlefield,
                    to_owner: None,
                    bind_as: None,
                },
                Action::Attach {
                    what: Expr::Ctx(Ctx::Source),
                    to: Expr::Ctx(Ctx::Var("target")),
                },
            ]),
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("When Pre-War Formalwear enters, return target creature card with mana value 3 or less from your graveyard to the battlefield, then attach Pre-War Formalwear to it."),
    };
    // Equipped creature gets +2/+2 and has vigilance.
    card.abilities = vec![
        reanimate,
        ir_equip("3", "Equip {3}"),
        equipped_creature_ce(
            vec![
                CEMod::PumpPT(Expr::Num(2), Expr::Num(2)),
                CEMod::AddKeyword(Keyword::Vigilance),
            ],
            "Equipped creature gets +2/+2 and has vigilance.",
        ),
    ];
    card
}

/// Cryptic Coat — {2}{U} Artifact — Equipment.
/// "When this Equipment enters, cloak the top card of your library, then attach this
///  Equipment to it. (To cloak a card, put it onto the battlefield face down as a 2/2
///  creature with ward {2}. Turn it face up any time for its mana cost if it's a
///  creature card.)"
/// "Equipped creature gets +1/+0 and can't be blocked."
/// "{1}{U}: Return this Equipment to its owner's hand."
///
/// ABNORMAL simplifications:
///   * Cloak is modeled by creating a "Mysterious Creature" 2/2 token (no ward, no
///     turn-face-up, not actually the top card of the library). This matches how we
///     approximate Living Weapon via phyrexian_germ_token.
///   * "Can't be blocked" is not a supported keyword on the engine yet — we grant no
///     evasion, so combat interactions with equipped creatures are incorrect.
fn cryptic_coat() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, EventPattern, TriggerSpec};
    use crate::ir::action::{Action, TokenSpec, Who as IrWho};
    use crate::ir::ce::CEMod;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel};
    let mut card = CardDef::new(
        "Cryptic Coat",
        CardKind::Artifact(ArtifactData {
            mana_cost: "2U".to_string(),
            subtypes: vec!["Equipment".into()],
            abilities: vec![], // bounce is now an IR Activated ability (below)
            mana_abilities: vec![],
        }),
        parse_colors("U", false, false), None,
        vec![], CardLayout::Normal, None,
        vec![], // ETB cloak is now an IR Triggered ability (below)
        vec![], vec![],
        vec![], // static CE now IR (card.abilities below)
    );
    // "When this Equipment enters, cloak the top card of your library, then attach
    //  this to it." ABNORMAL: cloak is approximated by a 2/2 Mysterious Creature
    //  token (no ward / turn-face-up), like Living Weapon's germ.
    let cloak = Ability {
        kind: AbilityKind::Triggered {
            spec: TriggerSpec::When {
                pattern: EventPattern::EntersZone { obj_filter: ir_self(), zone_kind: ZoneKindSel::Battlefield },
                condition: None,
            },
            target_spec: TargetSpec::None,
            body: Action::Sequence(vec![
                Action::CreateToken {
                    who: IrWho::You,
                    spec: TokenSpec {
                        name: "Mysterious Creature",
                        types: vec![CardType::Creature],
                        subtypes: vec![],
                        colors: vec![],
                        power: Some(2),
                        toughness: Some(2),
                        keywords: vec![],
                    },
                    n: Expr::Num(1),
                    bind_as: Some("cloaked"),
                },
                Action::Attach {
                    what: Expr::Ctx(Ctx::Source),
                    to: Expr::Ctx(Ctx::Var("cloaked")),
                },
            ]),
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("When Cryptic Coat enters, cloak the top card of your library, then attach Cryptic Coat to it."),
    };
    // Equipped creature gets +1/+0. ("can't be blocked" omitted — no keyword.)
    card.abilities = vec![
        cloak,
        ir_bounce_self("1U", "{1}{U}: Return Cryptic Coat to its owner's hand."),
        equipped_creature_ce(
            vec![CEMod::PumpPT(Expr::Num(1), Expr::Num(0))],
            "Equipped creature gets +1/+0.",
        ),
    ];
    card
}

/// Dragon's Rage Channeler — {R} 1/1 Human Shaman.
/// "Whenever you cast a noncreature spell, surveil 1."
/// "Delirium — As long as there are four or more card types among cards in your graveyard,
///  this creature gets +2/+2, has flying, and attacks each combat if able."
fn dragons_rage_channeler() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, EventPattern, TriggerSpec};
    use crate::ir::action::Action;
    use crate::ir::ce::CEMod;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, Filter, ZoneKindSel, ZoneSel};
    let data = CreatureData::new("R", 1, 1);

    let mut card = CardDef::new(
        "Dragon's Rage Channeler",
        CardKind::Creature(data),
        parse_colors("R", false, false),
        None,
        vec![], CardLayout::Normal, None,
        vec![], // surveil trigger is now an IR Triggered ability (below)
        vec![],  // no replacements
        vec![],  // no prohibitions
        vec![],  // delirium CE now IR (card.abilities below)
    );

    // "Whenever you cast a noncreature spell, surveil 1."
    let surveil_trigger = Ability {
        kind: AbilityKind::Triggered {
            spec: TriggerSpec::When {
                pattern: EventPattern::SpellCast {
                    spell_filter: ir_not(ir_type(CardType::Creature)),
                },
                condition: Some(Expr::Eq(
                    Box::new(Expr::Ctx(Ctx::Var("triggered_actor"))),
                    Box::new(Expr::Ctx(Ctx::Controller)),
                )),
            },
            target_spec: TargetSpec::None,
            body: Action::Surveil { who: crate::ir::action::Who::You, n: Expr::Num(1) },
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("Whenever you cast a noncreature spell, surveil 1."),
    };
    // Delirium — as long as ≥4 card types among cards in your graveyard, DRC gets
    // +2/+2 and flying. One gated Static block: the delirium condition is a single
    // first-class gate (Decision 4) shared by both mods; scope is self.
    // ("attacks each combat if able" is not modeled.)
    let delirium = Expr::Ge(
        Box::new(Expr::Count(Box::new(Expr::Types(Box::new(Expr::AllObjects {
            zone: ZoneSel::Scoped {
                zone_kind: ZoneKindSel::Graveyard,
                owner: Box::new(Expr::Ctx(Ctx::Controller)),
            },
            bind: "g",
            filter: Box::new(Expr::Bool(true)),
        }))))),
        Box::new(Expr::Num(4)),
    );
    card.abilities = vec![
        surveil_trigger,
        Ability {
            kind: AbilityKind::Static {
                mods: vec![
                    CEMod::PumpPT(Expr::Num(2), Expr::Num(2)),
                    CEMod::AddKeyword(Keyword::Flying),
                ],
                scope: Some(Filter(Expr::Eq(
                    Box::new(Expr::Ctx(Ctx::It)),
                    Box::new(Expr::Ctx(Ctx::Source)),
                ))),
                condition: Some(delirium),
            },
            text: Some("Delirium — as long as there are four or more card types among cards in your graveyard, this creature gets +2/+2 and has flying."),
        },
    ];
    card
}

/// Creature — Ape Spirit, 2/2. {2}{R}.
/// "Exile this card from your hand: Add {R}." — hand-zone mana ability (CR 605.3).
fn simian_spirit_guide() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, CostBody};
    use crate::ir::action::{Action, ManaSpec, Who};
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel};
    let data = CreatureData::new("2R", 2, 2);
    let mut def = simple("Simian Spirit Guide", CardKind::Creature(data), parse_colors("R", false, false), None);
    def.abilities.push(Ability {
        kind: AbilityKind::Activated {
            // `ExileSelf` from hand: same source-self pattern as Street
            // Wraith's cycling cost — `Action::Move` (deterministic) rather
            // than `MoveByChoice`, since the activation pipeline pre-moves
            // hand-source abilities to Stack before paying costs.
            cost: CostBody::Ir(Action::Move {
                what: Expr::Ctx(Ctx::Source),
                to: ZoneKindSel::Exile,
                to_owner: None,
                bind_as: None,
            }),
            target_spec: TargetSpec::None,
            choice_spec: None,
            body: Action::AddMana {
                who: Who::You,
                count: Expr::Num(1),
                spec: ManaSpec::Fixed(vec![Color::Red]),
            },
            timing: ActivationTiming::Default,
            activation_condition: None,
            active_zone: ZoneKindSel::Hand,
        },
        text: Some("Exile Simian Spirit Guide from your hand: Add {R}."),
    });
    def
}

/// Fury — {3}{R}{R} Elemental Incarnation, 3/3. Double strike.
/// ETB: deals 4 damage divided as you choose among any number of target creatures
/// and/or planeswalkers. Evoke — Exile a red card from your hand. CR 702.74, 702.4.
fn fury() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, EventPattern, TriggerSpec};
    use crate::ir::action::{Action, Who as IrWho};
    use crate::ir::context::Ctx;
    use crate::ir::event_log::Window;
    use crate::ir::expr::{EventFilter, Expr, ZoneKindSel};
    let mut data = CreatureData::new("3RR", 3, 3);
    data.keywords = Keywords::from_slice(&[Keyword::DoubleStrike]);
    let mut c = CardDef::new(
        "Fury",
        CardKind::Creature(data),
        parse_colors("3RR", false, false),
        None,
        vec![], CardLayout::Normal, None,
        vec![], vec![], vec![], vec![],
    );
    // ETB: deal 4 damage to target creature or planeswalker.
    let etb_damage = Ability {
        kind: AbilityKind::Triggered {
            spec: TriggerSpec::When {
                pattern: EventPattern::EntersZone { obj_filter: ir_self(), zone_kind: ZoneKindSel::Battlefield },
                condition: None,
            },
            target_spec: TargetSpec::ObjectInZone {
                controller: Who::Opp,
                zone: ZoneId::Battlefield,
                filter: ir_or(ir_type(CardType::Creature), ir_type(CardType::Planeswalker)),
            },
            body: Action::DealDamage {
                source: Expr::Ctx(Ctx::Source),
                target: Expr::Ctx(Ctx::Var("target")),
                amount: Expr::Num(4),
            },
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("When Fury enters, it deals 4 damage to target creature or planeswalker."),
    };
    // Evoke (CR 702.74): "if its evoke cost was paid, sacrifice it." Read from the
    // log — Fury's own SpellCast this turn carried `alt_cost` (the evoke alternative
    // cost was used), which the just-resolved cast left in the event log.
    let evoke_sacrifice = Ability {
        kind: AbilityKind::Triggered {
            spec: TriggerSpec::When {
                pattern: EventPattern::EntersZone { obj_filter: ir_self(), zone_kind: ZoneKindSel::Battlefield },
                condition: Some(Expr::Gt(
                    Box::new(Expr::EventCount {
                        window: Window::ThisTurn,
                        filter: Box::new(EventFilter::SpellCast {
                            caster: None,
                            card: Some(Box::new(Expr::Ctx(Ctx::Source))),
                            spell_filter: None,
                            alt_cost: Some(true),
                        }),
                    }),
                    Box::new(Expr::Num(0)),
                )),
            },
            target_spec: TargetSpec::None,
            body: Action::Sacrifice {
                who: IrWho::You,
                filter: ir_self(),
                count: Expr::Num(1),
                bind_as: None,
            },
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("When Fury enters, if its evoke cost was paid, sacrifice it."),
    };
    c.abilities = vec![etb_damage, evoke_sacrifice];
    c.alternate_costs = vec![
        AlternateCost {
            // Evoke — Exile a red card from your hand.
            costs: CostBody::Ir(crate::ir::action::Action::MoveByChoice {
                who: crate::ir::action::Who::You,
                from: crate::ir::expr::ZoneKindSel::Hand,
                to: crate::ir::expr::ZoneKindSel::Exile,
                verb: crate::ir::action::MoveVerb::Exile,
                filter: pitch_color_filter(Color::Red),
                count: crate::ir::expr::Expr::Num(1),
                bind_as: Some("$fury_evoke"),
            }),
            hand_min: 2,
            ..Default::default()
        },
    ];
    c
}

/// Quantum Riddler — {3}{U}{U} Creature — Sphinx 4/6.
/// Flying.
/// "When this creature enters, draw a card."
/// "As long as you have one or fewer cards in hand, if you would draw one or more cards,
///  you draw that many cards plus one instead." — TODO: not modeled. Rulings (2025-07-25)
///  say the replacement applies at the draw-instruction level; the engine fires `Draw`
///  per card, so hooking it accurately requires a new `DrawInstruction` event.
/// "Warp {1}{U}" (CR 702.185): alternative cost; when cast for warp cost, a delayed
/// trigger at the beginning of the next end step exiles the permanent. TODO: the
/// "its owner may cast this card after the current turn has ended" part is not modeled
/// (requires a castable-from-exile flag tied to the exiled card).
fn quantum_riddler() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, EventPattern, StepScope, TriggerSpec};
    use crate::ir::action::{Action, Who as IrWho};
    use crate::ir::context::Ctx;
    use crate::ir::event_log::Window;
    use crate::ir::expr::{EventFilter, Expr, ZoneKindSel};
    let mut data = CreatureData::new("3UU", 4, 6);
    data.creature_subtypes = vec!["Sphinx".into()];
    data.keywords = Keywords::from_slice(&[Keyword::Flying]);
    let mut c = CardDef::new(
        "Quantum Riddler",
        CardKind::Creature(data),
        parse_colors("3UU", false, false),
        None,
        vec![], CardLayout::Normal, None,
        vec![], vec![], vec![], vec![],
    );
    // ETB: draw a card.
    let etb_draw = Ability {
        kind: AbilityKind::Triggered {
            spec: TriggerSpec::When {
                pattern: EventPattern::EntersZone { obj_filter: ir_self(), zone_kind: ZoneKindSel::Battlefield },
                condition: None,
            },
            target_spec: TargetSpec::None,
            body: Action::Draw { who: IrWho::You, n: Expr::Num(1) },
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("When Quantum Riddler enters, draw a card."),
    };
    // Warp: if cast for its warp (alternative) cost, exile it at the beginning of the
    // next end step. Detected from the log — Source's own alt-cost SpellCast this turn
    // (warp's {1}{U} spends mana, so `alt_cost`, not `!mana_spent`, is the signal).
    let warp_exile = Ability {
        kind: AbilityKind::Triggered {
            spec: TriggerSpec::When {
                pattern: EventPattern::EntersZone { obj_filter: ir_self(), zone_kind: ZoneKindSel::Battlefield },
                condition: Some(Expr::Gt(
                    Box::new(Expr::EventCount {
                        window: Window::ThisTurn,
                        filter: Box::new(EventFilter::SpellCast {
                            caster: None,
                            card: Some(Box::new(Expr::Ctx(Ctx::Source))),
                            spell_filter: None,
                            alt_cost: Some(true),
                        }),
                    }),
                    Box::new(Expr::Num(0)),
                )),
            },
            target_spec: TargetSpec::None,
            body: Action::ScheduleDelayedTrigger {
                fires: TriggerSpec::AtStep { step: StepKind::End, who: StepScope::EachPlayer, condition: None },
                action: Box::new(Action::Exile { target: Expr::Ctx(Ctx::Source), bind_as: None }),
            },
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("Warp — when Quantum Riddler enters, if it was cast for its warp cost, exile it at the beginning of the next end step."),
    };
    c.abilities = vec![etb_draw, warp_exile];
    c.alternate_costs = vec![
        AlternateCost {
            costs: ir_pay_mana_str("1U"),
            ..Default::default()
        },
    ];
    c
}

/// Griselbrand — {4}{B}{B}{B}{B} Legendary 7/7 Demon.
/// Flying, lifelink. Pay 7 life: Draw seven cards.
fn griselbrand() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind};
    use crate::ir::action::{Action, Who as IrWho};
    use crate::ir::expr::{Expr, ZoneKindSel};
    let mut data = CreatureData::new("4BBBB", 7, 7);
    data.legendary = true;
    data.keywords = Keywords::from_slice(&[Keyword::Flying, Keyword::Lifelink]);
    let mut card = simple("Griselbrand", CardKind::Creature(data), parse_colors("4BBBB", false, false), None);
    card.abilities.push(Ability {
        kind: AbilityKind::Activated {
            cost: ir_pay_life(7),
            target_spec: TargetSpec::None,
            choice_spec: None,
            body: Action::Draw { who: IrWho::You, n: Expr::Num(7) },
            timing: ActivationTiming::Default,
            activation_condition: None,
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("Pay 7 life: Draw seven cards."),
    });
    card
}

/// Emrakul, the Aeons Torn — {15} Legendary 15/15 Eldrazi.
/// Flying, annihilator 6, protection from spells that are one or more colors.
/// This spell can't be countered.
/// When you cast this spell, take an extra turn after this one.
/// When put into a graveyard from anywhere, owner shuffles graveyard into library.
/// TODO: cast trigger (extra turn), annihilator 6, graveyard shuffle not modeled.
/// Ward (CR 702.21) as a reusable IR triggered ability: when an opponent's spell
/// targets the holder, counter it unless its controller pays `cost`. The trigger
/// self-scopes via `Contains(Source, ChosenTargets(triggered_obj))`, so it works
/// both directly (self-ward) and when granted to another permanent via
/// `CEMod::GrantAbility` (then `Source` is the grantee). The pay-or-counter
/// decision runs in `Action::Ward` → `ward_pay_or_counter`.
pub(crate) fn ir_ward(cost: crate::ir::action::Action) -> crate::ir::ability::Ability {
    use crate::ir::ability::{Ability, AbilityKind, EventPattern, TriggerSpec};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, Filter, ZoneKindSel};
    Ability {
        kind: AbilityKind::Triggered {
            spec: TriggerSpec::When {
                pattern: EventPattern::SpellCast { spell_filter: Filter(Expr::Bool(true)) },
                condition: Some(Expr::And(
                    // cast by an opponent of the holder
                    Box::new(Expr::Not(Box::new(Expr::Eq(
                        Box::new(Expr::Ctx(Ctx::Var("triggered_actor"))),
                        Box::new(Expr::Ctx(Ctx::Controller)),
                    )))),
                    // targeting the holder: Source ∈ the spell's chosen targets
                    Box::new(Expr::Contains(
                        Box::new(Expr::Ctx(Ctx::Source)),
                        Box::new(Expr::ChosenTargets(Box::new(Expr::Ctx(Ctx::Var("triggered_obj"))))),
                    )),
                )),
            },
            target_spec: TargetSpec::None,
            body: Action::Ward { cost: Box::new(cost) },
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("Ward (Whenever this permanent becomes the target of a spell an opponent controls, counter that spell unless its controller pays the ward cost.)"),
    }
}

/// "This spell can't be countered." An `AbilityKind::Prohibition` that matches a
/// `SpellBeingCountered` event whose spell is this source (`It == Source`) and
/// suppresses it in the event pipeline (CR 701.5 / 101.2 "can't beats can"). The
/// self-scoping pattern is its own armed-when gate: it only matches while the
/// source is the spell on the stack being countered.
fn cant_be_countered_self() -> crate::ir::ability::Ability {
    use crate::ir::ability::{Ability, AbilityKind, EventPattern};
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, Filter};
    Ability {
        kind: AbilityKind::Prohibition {
            matches: EventPattern::SpellBeingCountered {
                spell_filter: Filter(Expr::Eq(
                    Box::new(Expr::Ctx(Ctx::It)),
                    Box::new(Expr::Ctx(Ctx::Source)),
                )),
            },
            // No zone gate: `It == Source` already pins this to the one spell on the
            // stack being countered (`counter_one` only fires on stack items).
            active_zone: None,
        },
        text: Some("This spell can't be countered."),
    }
}

/// "As ~ enters the battlefield, choose a color/creature type/card name"
/// (CR 614.12). A self-entry `Replacement` re-does the entry (`Move`) then
/// records the choice in the permanent's `etb_choice` via `RecordEtbChoice`;
/// the card's ongoing abilities read it back with `ChosenColor`/`ChosenName`.
fn etb_choice_replacement(
    kind: crate::ir::action::EtbChoiceKind,
    text: &'static str,
) -> crate::ir::ability::Ability {
    use crate::ir::ability::{Ability, AbilityKind, EventPattern, ReplacementBody};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel};
    Ability {
        kind: AbilityKind::Replacement {
            matches: EventPattern::EntersZone {
                obj_filter: ir_self(),
                zone_kind: ZoneKindSel::Battlefield,
            },
            condition: None,
            body: ReplacementBody::Replace(Action::Sequence(vec![
                Action::Move {
                    what: Expr::Ctx(Ctx::Var("triggered_obj")),
                    to: ZoneKindSel::Battlefield,
                    to_owner: None,
                    bind_as: None,
                },
                Action::RecordEtbChoice { kind },
            ])),
            active_zone: None, // self-entry replacement
        },
        text: Some(text),
    }
}

/// "This permanent enters the battlefield with a number of loyalty counters on
/// it equal to its printed loyalty number" (CR 306.5b) — the intrinsic
/// planeswalker ETB replacement. A self-entry `Replacement` re-does the entry
/// then places `base` loyalty counters (CR 306.5c: loyalty *is* that count).
fn ir_planeswalker_etb_loyalty(base: i32) -> crate::ir::ability::Ability {
    use crate::ir::ability::{Ability, AbilityKind, EventPattern, ReplacementBody};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel};
    Ability {
        kind: AbilityKind::Replacement {
            matches: EventPattern::EntersZone {
                obj_filter: ir_self(),
                zone_kind: ZoneKindSel::Battlefield,
            },
            condition: None,
            body: ReplacementBody::Replace(Action::Sequence(vec![
                Action::Move {
                    what: Expr::Ctx(Ctx::Var("triggered_obj")),
                    to: ZoneKindSel::Battlefield,
                    to_owner: None,
                    bind_as: None,
                },
                Action::PutCounters {
                    on: Expr::Ctx(Ctx::Var("triggered_obj")),
                    kind: crate::CounterType::Loyalty,
                    n: Expr::Num(base as i64),
                },
            ])),
            active_zone: None, // self-entry replacement
        },
        text: Some("This planeswalker enters with its starting loyalty counters."),
    }
}

fn emrakul_the_aeons_torn() -> CardDef {
    let mut data = CreatureData::new("15", 15, 15);
    data.legendary = true;
    data.keywords = Keywords::from_slice(&[Keyword::Flying, Keyword::Annihilator6]);
    let mut def = CardDef::new(
        "Emrakul, the Aeons Torn",
        CardKind::Creature(data),
        vec![],  // colorless
        None,
        vec![], CardLayout::Normal, None,
        vec![], vec![],
        vec![], // "can't be countered" is now an IR Prohibition (def.abilities below)
        vec![],
    );
    def.counterable = false;
    def.protection_from = vec![ir_colored_spell()];
    def.abilities.push(cant_be_countered_self());
    def
}

/// Atraxa, Grand Unifier — {3}{G}{W}{U}{B} Legendary 7/7 Phyrexian Angel.
/// Flying, vigilance, deathtouch, lifelink.
/// ETB: reveal top 10 of library, for each card type you may put one into hand, rest to bottom.
/// TODO: real ETB needs per-type strategy choices over actual revealed cards; placeholder
/// adds 4 cards to hand silently (no Draw events).
fn atraxa_grand_unifier() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, EventPattern, TriggerSpec};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel, ZoneSel};
    let mut data = CreatureData::new("3GWUB", 7, 7);
    data.legendary = true;
    data.keywords = Keywords::from_slice(&[
        Keyword::Flying, Keyword::Vigilance, Keyword::Deathtouch, Keyword::Lifelink,
    ]);
    let mut card = CardDef::new(
        "Atraxa, Grand Unifier",
        CardKind::Creature(data),
        parse_colors("3GWUB", false, false),
        None,
        vec![], CardLayout::Normal, None,
        vec![], vec![], vec![], vec![],
    );
    // ETB — PLACEHOLDER for "reveal the top ten cards of your library; put one card of
    // each card type into your hand and the rest on the bottom". Approximated as
    // moving the top four cards of your library into your hand (no reveal-by-type).
    card.abilities = vec![Ability {
        kind: AbilityKind::Triggered {
            spec: TriggerSpec::When {
                pattern: EventPattern::EntersZone { obj_filter: ir_self(), zone_kind: ZoneKindSel::Battlefield },
                condition: None,
            },
            target_spec: TargetSpec::None,
            body: Action::Move {
                what: Expr::Top {
                    zone: ZoneSel::Scoped {
                        zone_kind: ZoneKindSel::Library,
                        owner: Box::new(Expr::Ctx(Ctx::Controller)),
                    },
                    n: Box::new(Expr::Num(4)),
                },
                to: ZoneKindSel::Hand,
                to_owner: None,
                bind_as: None,
            },
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("When Atraxa, Grand Unifier enters, reveal the top ten cards of your library, put one card of each card type into your hand, then put the rest on the bottom of your library in a random order. (Placeholder: puts the top four cards into your hand.)"),
    }];
    card
}

fn brazen_borrower() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel};
    let mut back = simple(
        "Petty Theft",
        CardKind::Instant(SpellData {
            mana_cost: "1U".to_string(),
            subtypes: vec!["adventure".to_string()],
            modes: None,
            ..Default::default()
        }),
        parse_colors("1UU", true, false),
        None,
    );
    back.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::ObjectInZone {
                    controller: Who::Opp,
                    zone: ZoneId::Battlefield,
                    filter: ir_not(ir_type(CardType::Land)),
                },
                body: Action::Return {
                    what: Expr::Ctx(Ctx::Var("target")),
                    to: ZoneKindSel::Hand,
                    bind_as: None,
                },
            }],
        },
        text: Some("Return target nonland permanent to its owner's hand."),
    }];

    let mut data = CreatureData::new("1UU", 3, 1);
    data.keywords.insert(Keyword::Flash);
    data.keywords.insert(Keyword::Flying);

    CardDef::new(
        "Brazen Borrower",
        CardKind::Creature(data),
        parse_colors("1UU", true, false),
        None,
        vec![], CardLayout::Split, Some(Box::new(back)),
        vec![], vec![], vec![], vec![],
    )
}

/// Mishra's Bauble — {0} Artifact.
/// {T}, Sacrifice: Look at the top card of target player's library.
/// Draw a card at the beginning of the next turn's upkeep.
fn mishras_bauble() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, StepScope, TriggerSpec};
    use crate::ir::action::{Action, Who as IrWho};
    use crate::ir::expr::{Expr, ZoneKindSel};
    let mut card = simple("Mishra's Bauble", CardKind::Artifact(ArtifactData {
        mana_cost: "0".to_string(),
        ..Default::default()
    }), vec![], Some(25));
    // "{T}, Sacrifice Mishra's Bauble: Look at the top card of any library. Draw a
    //  card at the beginning of the next turn's upkeep." The look is informational
    //  in the sim; the draw is a delayed trigger fired at the next upkeep.
    card.abilities.push(Ability {
        kind: AbilityKind::Activated {
            cost: ir_seq(vec![act_tap_source(), act_sac_self("$bauble_self")]),
            target_spec: TargetSpec::None,
            choice_spec: None,
            body: Action::ScheduleDelayedTrigger {
                fires: TriggerSpec::AtStep { step: StepKind::Upkeep, who: StepScope::EachPlayer, condition: None },
                action: Box::new(Action::Draw { who: IrWho::You, n: Expr::Num(1) }),
            },
            timing: ActivationTiming::Default,
            activation_condition: None,
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("{T}, Sacrifice Mishra's Bauble: Look at the top card of any library. Draw a card at the beginning of the next turn's upkeep."),
    });
    card
}

/// Containment Priest — {1}{W} Creature — Human Cleric 2/2. Flash.
/// If a nontoken creature would enter the battlefield and it wasn't cast,
/// exile it instead.
fn containment_priest() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, EventPattern, ReplacementBody};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel};
    let mut data = CreatureData::new("1W", 2, 2);
    data.keywords.insert(Keyword::Flash);
    let mut card = CardDef::new(
        "Containment Priest",
        CardKind::Creature(data),
        parse_colors("1W", true, false),
        None,
        vec![], CardLayout::Normal, None,
        vec![], vec![], vec![], vec![],
    );
    // "If a nontoken creature would enter the battlefield and it wasn't cast, exile
    //  it instead." obj_filter: a nontoken creature other than Priest itself;
    //  condition: it entered from a zone other than the stack (= "wasn't cast").
    card.abilities = vec![Ability {
        kind: AbilityKind::Replacement {
            matches: EventPattern::EntersZone {
                obj_filter: ir_and(
                    ir_not(ir_token()),
                    ir_and(ir_type(CardType::Creature), ir_not(ir_self())),
                ),
                zone_kind: ZoneKindSel::Battlefield,
            },
            condition: Some(Expr::Not(Box::new(Expr::Eq(
                Box::new(Expr::Ctx(Ctx::Var("triggered_from"))),
                Box::new(Expr::ZoneLit(ZoneId::Stack)),
            )))),
            body: ReplacementBody::Replace(Action::Move {
                what: Expr::Ctx(Ctx::Var("triggered_obj")),
                to: ZoneKindSel::Exile,
                to_owner: None,
                bind_as: None,
            }),
            active_zone: Some(ZoneKindSel::Battlefield),
        },
        text: Some("If a nontoken creature would enter the battlefield and it wasn't cast, exile it instead."),
    }];
    card
}

// ── Delver of Secrets ────────────────────────────────────────────────────────

/// Delver of Secrets — {U} Creature — Human Wizard 1/1. DFC.
/// "At the beginning of your upkeep, look at the top card of your library.
///  You may reveal that card. If an instant or sorcery card is revealed this way,
///  transform this creature."
/// Back face: Insectile Aberration — 3/2 Flying.
fn delver_of_secrets() -> CardDef {
    let back = CardDef::new(
        "Insectile Aberration",
        CardKind::Creature({
            let mut c = CreatureData::new("", 3, 2);
            c.keywords = Keywords::from_slice(&[Keyword::Flying]);
            c
        }),
        parse_colors("U", false, false),
        None,
        vec![], CardLayout::Normal, None,
        vec![], vec![], vec![], vec![],
    );

    use crate::ir::ability::{Ability, AbilityKind, StepScope, TriggerSpec};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel, ZoneSel};

    let mut card = CardDef::new(
        "Delver of Secrets",
        CardKind::Creature(CreatureData::new("U", 1, 1)),
        parse_colors("U", false, false),
        Some(50),
        vec![], CardLayout::DoubleFaced, Some(Box::new(back)),
        vec![], vec![], vec![], vec![],
    );

    // "At the beginning of your upkeep, look at the top card of your library …
    //  if an instant or sorcery card is revealed this way, transform Delver."
    // Fires each upkeep (AtStep has no condition slot); the body is an intervening-
    // if that flips only when still on the front face and the top card is I/S.
    let top_is_inst_or_sorc = |t: CardType| Expr::Contains(
        Box::new(Expr::TypeLit(t)),
        Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::Var("c"))))),
    );
    let condition = Expr::And(
        Box::new(Expr::IsFrontFace(Box::new(Expr::Ctx(Ctx::Source)))),
        Box::new(Expr::Any {
            set: Box::new(Expr::Top {
                zone: ZoneSel::Scoped {
                    zone_kind: ZoneKindSel::Library,
                    owner: Box::new(Expr::Ctx(Ctx::Controller)),
                },
                n: Box::new(Expr::Num(1)),
            }),
            bind: "c",
            body: Box::new(Expr::Or(
                Box::new(top_is_inst_or_sorc(CardType::Instant)),
                Box::new(top_is_inst_or_sorc(CardType::Sorcery)),
            )),
        }),
    );
    card.abilities = vec![Ability {
        kind: AbilityKind::Triggered {
            spec: TriggerSpec::AtStep {
                step: StepKind::Upkeep,
                who: StepScope::You,
                condition: Some(condition),
            },
            target_spec: TargetSpec::None,
            body: Action::Transform { target: Expr::Ctx(Ctx::Source) },
            active_zone: ZoneKindSel::Battlefield,
        },
        text: Some("At the beginning of your upkeep, look at the top card of your library. You may reveal that card. If an instant or sorcery card is revealed this way, transform Delver of Secrets."),
    }];
    card
}

// ── Unholy Heat ──────────────────────────────────────────────────────────────

/// Unholy Heat — {R} Instant. Deals 2 damage to target creature or planeswalker.
/// Delirium — deals 6 damage instead if ≥4 card types in graveyard.
fn unholy_heat() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, ZoneKindSel, ZoneSel};

    // Delirium: four or more card types among cards in your graveyard
    // (CR 700.5) — `Count` of the deduped type-union over your graveyard.
    let delirium = Expr::Ge(
        Box::new(Expr::Count(Box::new(Expr::Types(Box::new(Expr::AllObjects {
            zone: ZoneSel::Scoped {
                zone_kind: ZoneKindSel::Graveyard,
                owner: Box::new(Expr::Ctx(Ctx::Controller)),
            },
            bind: "g",
            filter: Box::new(Expr::Bool(true)),
        }))))),
        Box::new(Expr::Num(4)),
    );
    let damage = |n: i64| Action::DealDamage {
        source: Expr::Ctx(Ctx::Source),
        target: Expr::Ctx(Ctx::Var("target")),
        amount: Expr::Num(n),
    };

    let mut card = simple("Unholy Heat", CardKind::Instant(SpellData {
        mana_cost: "R".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("R", false, false), None);
    card.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::Union(vec![
                    TargetSpec::ObjectInZone { controller: Who::Opp, zone: ZoneId::Battlefield, filter: ir_type(CardType::Creature) },
                    TargetSpec::ObjectInZone { controller: Who::Opp, zone: ZoneId::Battlefield, filter: ir_type(CardType::Planeswalker) },
                ]),
                body: Action::IfThen {
                    cond: delirium,
                    then: Box::new(damage(6)),
                    else_: Some(Box::new(damage(2))),
                },
            }],
        },
        text: Some("Unholy Heat deals 2 damage to target creature or planeswalker. Delirium — 6 damage instead if there are four or more card types among cards in your graveyard."),
    }];
    card
}

// ── Price of Progress ────────────────────────────────────────────────────────

/// Price of Progress — {1}{R} Instant. Deals damage to each player equal to
/// twice the number of nonbasic lands that player controls.
fn price_of_progress() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, Filter, ZoneKindSel, ZoneSel};

    // Nonbasic lands the bound player `p` controls (`p` is a player object).
    let nonbasic_lands_controlled = || Expr::AllObjects {
        zone: ZoneSel::Global(ZoneKindSel::Battlefield),
        bind: "it",
        filter: Box::new(ir_and(
            ir_and(ir_type(CardType::Land), ir_not(ir_supertype(Supertype::Basic))),
            Filter(Expr::Eq(
                Box::new(Expr::Controller(Box::new(Expr::Ctx(Ctx::It)))),
                Box::new(Expr::Ctx(Ctx::Var("p"))),
            )),
        ).0),
    };

    let mut card = simple("Price of Progress", CardKind::Instant(SpellData {
        mana_cost: "1R".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("R", false, false), None);
    card.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::None,
                // The direct oracle form: for each player, deal 2 × the nonbasic
                // lands they control. Players are objects, so this is a ForEach
                // over `Expr::Players`; damage to a player object is life loss.
                body: Action::ForEach {
                    over: Expr::Players,
                    bind: "p",
                    body: Box::new(Action::DealDamage {
                        source: Expr::Ctx(Ctx::Source),
                        target: Expr::Ctx(Ctx::Var("p")),
                        amount: Expr::Mul(
                            Box::new(Expr::Num(2)),
                            Box::new(Expr::Count(Box::new(nonbasic_lands_controlled()))),
                        ),
                    }),
                },
            }],
        },
        text: Some("Price of Progress deals damage to each player equal to twice the number of nonbasic lands they control."),
    }];
    card
}

// ── Meltdown ─────────────────────────────────────────────────────────────────

/// Meltdown — {X}{R} Sorcery. "Destroy each artifact with mana value X or less."
fn meltdown() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::Expr;

    let mut def = simple("Meltdown", CardKind::Sorcery(SpellData {
        mana_cost: "R".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("R", false, false), None);
    def.additional_costs = ir_xmana_cost();
    def.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::None,
                // Destroy each artifact whose mana value is ≤ the announced X
                // (bound as `Ctx::Var("x")` by `build_spell_effect`).
                body: ir_for_each_on_battlefield(
                    ir_and(ir_type(CardType::Artifact), ir_mv_le_expr(Expr::Ctx(Ctx::Var("x")))),
                    Action::Destroy { target: Expr::Ctx(Ctx::Var("v")) },
                ),
            }],
        },
        text: Some("Destroy each artifact with mana value X or less."),
    }];
    def
}

// ── Rough // Tumble ──────────────────────────────────────────────────────────

/// Rough // Tumble — split card (first true split, not adventure).
/// Rough: {1}{R} Sorcery — "Rough deals 2 damage to each creature without flying."
/// Tumble: {5}{R} Sorcery — "Tumble deals 6 damage to each creature with flying."
fn rough_tumble() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, Filter};

    // "deals `n` damage to each creature matching `flying_filter`". Protection is
    // enforced by the `DealDamage` primitive (CR 702.16e), so no per-card check.
    let damage_each = |n: i64, flying_filter: Filter| ir_for_each_on_battlefield(
        ir_and(ir_type(CardType::Creature), flying_filter),
        Action::DealDamage {
            source: Expr::Ctx(Ctx::Source),
            target: Expr::Ctx(Ctx::Var("v")),
            amount: Expr::Num(n),
        },
    );

    let mut tumble = simple("Tumble", CardKind::Sorcery(SpellData {
        mana_cost: "5R".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("R", false, false), None);
    tumble.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::None,
                body: damage_each(6, ir_keyword(Keyword::Flying)),
            }],
        },
        text: Some("Tumble deals 6 damage to each creature with flying."),
    }];

    let mut card = CardDef::new(
        "Rough // Tumble",
        CardKind::Sorcery(SpellData {
            mana_cost: "1R".to_string(),
            modes: None,
            ..Default::default()
        }),
        parse_colors("R", false, false),
        None,
        vec![], CardLayout::Split, Some(Box::new(tumble)),
        vec![], vec![], vec![], vec![],
    );
    card.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::None,
                body: damage_each(2, ir_not(ir_keyword(Keyword::Flying))),
            }],
        },
        text: Some("Rough deals 2 damage to each creature without flying."),
    }];
    card
}

// ── Prismatic Ending ─────────────────────────────────────────────────────────

/// Prismatic Ending — {X}{W} Sorcery.
/// Converge — Exile target nonland permanent if its mana value is less than or
/// equal to the number of colors of mana spent to cast this spell.
///
/// Modeled as base cost {W} plus `XMana` additional cost (same sunburst pattern
/// as Engineered Explosives / Meltdown — strategy declares `chosen_x` distinct
/// colored mana toward the {X} generic). Converge count = chosen_x + 1; the +1
/// is the mandatory {W} pip. At resolution, the target is exiled iff its mana
/// value ≤ converge count; otherwise the effect does nothing (CR 702.103a).
fn prismatic_ending() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, IrSpellMode};
    use crate::ir::action::Action;
    use crate::ir::context::Ctx;
    use crate::ir::expr::Expr;
    let mut def = simple("Prismatic Ending", CardKind::Sorcery(SpellData {
        mana_cost: "W".to_string(),
        modes: None,
        ..Default::default()
    }), parse_colors("W", false, false), None);
    // Converge = number of colors of mana spent, modeled as the announced X + 1
    // (`Ctx::Var("x")` bound by `build_spell_effect`). Exile the target iff its
    // mana value ≤ converge; otherwise the spell does nothing.
    def.abilities = vec![Ability {
        kind: AbilityKind::OnResolve {
            modes: vec![IrSpellMode {
                target_spec: TargetSpec::ObjectInZone {
                    controller: Who::Opp,
                    zone: ZoneId::Battlefield,
                    filter: ir_not(ir_type(CardType::Land)),
                },
                body: Action::IfThen {
                    cond: Expr::Le(
                        Box::new(Expr::Mv(Box::new(Expr::Ctx(Ctx::Var("target"))))),
                        Box::new(Expr::Add(
                            Box::new(Expr::Ctx(Ctx::Var("x"))),
                            Box::new(Expr::Num(1)),
                        )),
                    ),
                    then: Box::new(Action::Exile {
                        target: Expr::Ctx(Ctx::Var("target")),
                        bind_as: None,
                    }),
                    else_: None,
                },
            }],
        },
        text: Some("Exile target nonland permanent if its mana value is less than or equal to the amount of mana spent to cast this spell."),
    }];
    def.additional_costs = ir_xmana_cost();
    def
}

// ── Null Rod ─────────────────────────────────────────────────────────────────

/// Null Rod — {2} Artifact. "Activated abilities of artifacts can't be activated."
/// An action-Restriction (Activate) over artifacts — symmetric, and its subject has
/// no mana-ability exemption, so it covers mana abilities too (shuts off Moxen).
/// CR 101.2 "can't beats can".
fn null_rod() -> CardDef {
    use crate::ir::ability::{Ability, AbilityKind, ActionKind};
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, Filter};
    let mut card = CardDef::new(
        "Null Rod",
        CardKind::Artifact(ArtifactData {
            mana_cost: "2".to_string(),
            ..Default::default()
        }),
        vec![],
        None,
        vec![], CardLayout::Normal, None,
        vec![], vec![], vec![],
        vec![], // "can't activate" is now an AbilityKind::Restriction (card.abilities below)
    );
    // "Activated abilities of artifacts can't be activated." Symmetric (both players').
    // An action-Restriction consulted at activation-legality (incl. mana abilities) —
    // keyed on the *source* permanent: any artifact's activations are forbidden.
    card.abilities = vec![Ability {
        kind: AbilityKind::Restriction {
            action: ActionKind::Activate,
            subject: Filter(Expr::Contains(
                Box::new(Expr::TypeLit(CardType::Artifact)),
                Box::new(Expr::Types(Box::new(Expr::Ctx(Ctx::It)))),
            )),
        },
        text: Some("Activated abilities of artifacts can't be activated."),
    }];
    card
}
