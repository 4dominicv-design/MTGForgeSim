use super::*;

// ── Keywords ──────────────────────────────────────────────────────────────────

#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum Keyword {
    Flying,
    Haste,
    Shadow,
    Lifelink,
    Vigilance,
    Deathtouch,
    Annihilator6,
    FirstStrike,
    DoubleStrike,
    Trample,
    Flash,
    Hexproof,
    Reach,
    /// This creature can't be blocked. Used for temporary effects such as Manifold Key.
    Unblockable,
}

/// Compact bitset of keyword abilities. Copy, allocation-free, O(1) contains/insert.
#[derive(Clone, Copy, Default, Debug)]
pub struct Keywords(u32);

impl Keywords {
    pub(crate) fn contains(self, kw: Keyword) -> bool {
        self.0 & (1 << kw as u32) != 0
    }
    pub(crate) fn insert(&mut self, kw: Keyword) {
        self.0 |= 1 << kw as u32;
    }
    pub(crate) fn from_slice(kws: &[Keyword]) -> Self {
        let mut s = Self(0);
        for &kw in kws { s.insert(kw); }
        s
    }
}

// ── Mana cost ─────────────────────────────────────────────────────────────────

#[derive(Clone, Default, Debug)]
pub struct ManaCost {
    pub(crate) w: i32,
    pub(crate) u: i32,
    pub(crate) b: i32,
    pub(crate) r: i32,
    pub(crate) g: i32,
    pub(crate) c: i32,       // colorless pips {C}
    pub(crate) generic: i32, // any-color pips {1}, {2}, ...
}

impl ManaCost {
    pub(crate) fn total_specific(&self) -> i32 { self.w + self.u + self.b + self.r + self.g + self.c }
    pub(crate) fn mana_value(&self) -> i32 { self.total_specific() + self.generic }

    /// Reconstruct a compact display string (e.g. `ManaCost{generic:1,u:1}` → `"1U"`).
    pub(crate) fn display(&self) -> String {
        let mut s = String::new();
        if self.generic > 0 { s.push_str(&self.generic.to_string()); }
        for _ in 0..self.w { s.push('W'); }
        for _ in 0..self.u { s.push('U'); }
        for _ in 0..self.b { s.push('B'); }
        for _ in 0..self.r { s.push('R'); }
        for _ in 0..self.g { s.push('G'); }
        for _ in 0..self.c { s.push('C'); }
        s
    }
}

/// Parse a mana cost string into a ManaCost.
/// Leading digits → generic; W/U/B/R/G/C → specific color pips.
/// Empty string = no castable mana cost (alt-cost-only or uncostable cards like Daze/FoW).
/// "0" = genuinely free (Lotus Petal, LED).
pub fn parse_mana_cost(cost: &str) -> ManaCost {
    let mut mc = ManaCost::default();
    let mut chars = cost.trim().chars().peekable();
    let mut num = String::new();
    while chars.peek().map(|c| c.is_ascii_digit()).unwrap_or(false) {
        num.push(chars.next().unwrap());
    }
    if !num.is_empty() {
        mc.generic = num.parse().unwrap_or(0);
    }
    for c in chars {
        match c {
            'W' => mc.w += 1,
            'U' => mc.u += 1,
            'B' => mc.b += 1,
            'R' => mc.r += 1,
            'G' => mc.g += 1,
            'C' => mc.c += 1,
            _ => mc.generic += 1,
        }
    }
    mc
}

/// Convert a mana-production string (e.g. "U", "WUBRG") to a Vec<Color>.
/// Each character maps to the corresponding Color; unknown chars are ignored.
pub(crate) fn produces_colors(s: &str) -> Vec<Color> {
    s.chars().filter_map(|c| match c {
        'W' => Some(Color::White),
        'U' => Some(Color::Blue),
        'B' => Some(Color::Black),
        'R' => Some(Color::Red),
        'G' => Some(Color::Green),
        _ => None,
    }).collect()
}

/// Total mana value (CMC) of a cost string.
pub(crate) fn mana_value(cost: &str) -> i32 {
    parse_mana_cost(cost).mana_value()
}

// ── Effect factories ──────────────────────────────────────────────────────────

// ── Cost types ────────────────────────────────────────────────────────────────

/// Where the source object must be located for an ability to be activated.
#[derive(Clone, Default)]
pub enum SourceZone {
    #[default]
    Battlefield,
    Hand,
}

/// When an ability can be activated (CR 602.5b, 605.3a).
///
/// Each ability type has a natural default speed:
/// - `ManaAbility`: usable in the mana sub-loop of spell casting (CR 601.2g)
/// - `AbilityDef`: instant speed (any time you have priority)
///
/// `ActivationTiming` overrides that default:
/// - `Default` — use the natural speed for this ability type.
/// - `Instant` — only during priority windows (any stack state). For `ManaAbility` this
///    excludes it from the CR 601.2g mana sub-loop (e.g. Lion's Eye Diamond).
/// - `Sorcery` — only during priority when the stack is empty and it's the controller's
///    main phase (e.g. loyalty abilities, "activate only as a sorcery").
#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub enum ActivationTiming {
    #[default]
    Default,
    Instant,
    Sorcery,
}


/// Factory for a spell effect: takes (controller, source_id, chosen_x) and returns the resolved `Effect`.
/// `chosen_x` is the strategy-chosen X value; 0 for spells without an X cost.
pub type SpellFactory = std::sync::Arc<dyn Fn(PlayerId, ObjId, u32) -> Effect + Send + Sync>;

/// Factory for an activated ability effect: takes (controller, source_id), returns `Effect`.
pub type AbilityFactory = std::sync::Arc<dyn Fn(PlayerId, ObjId) -> Effect + Send + Sync>;

/// One way to pay for a spell instead of (or in addition to) its mana cost.
/// Options are tried in order; the first affordable one is taken.
///
/// `hand_min` and `prob` are strategy metadata, not rules costs.
/// Per CR 118.9d, additional costs apply on top when an AlternateCost is chosen;
/// that is enforced at the cast-spell call site.
///
/// `condition` — optional rules restriction on when this cost may be chosen (CR 118.9b),
/// e.g. "If it's not your turn" on Force of Negation. `None` = always available.
#[derive(Clone, Default)]
pub struct AlternateCost {
    pub costs: crate::ir::ability::CostBody,
    pub hand_min: i32,
    pub prob: Option<f64>,
    pub(crate) condition: Option<std::sync::Arc<dyn Fn(PlayerId, &SimState) -> bool + Send + Sync>>,
}


/// An activated ability a permanent or hand card can use.
///
/// Describes a choice made at ability-resolution time (CR: "choose" ≠ "target").
/// The engine enumerates valid objects and delegates the pick to `Strategy::choose_for_effect`.
#[derive(Clone)]
pub struct ChoiceSpec {
    /// Whose objects to enumerate (relative to the ability's controller).
    pub(crate) controller: Who,
    /// Zone the chosen object must be in.
    pub(crate) zone: ZoneId,
    /// Predicate the chosen object must satisfy.
    pub(crate) filter: crate::ir::expr::Filter,
}

/// Enumerate valid choices for a `ChoiceSpec` from the perspective of `controller`.
pub(crate) fn enumerate_choices(spec: &ChoiceSpec, controller: PlayerId, state: &SimState) -> Vec<ObjId> {
    let target_who = spec.controller.resolve(controller);
    state.objects.values()
        .filter(|o| {
            let zone_match = match spec.zone {
                ZoneId::Exile => o.in_zone(Zone::Exile { on_adventure: false }),
                ZoneId::Hand => o.in_zone(Zone::Hand { known: false }),
                ZoneId::Battlefield => o.in_zone(Zone::Battlefield),
                ZoneId::Graveyard  => o.in_zone(Zone::Graveyard),
                ZoneId::Stack      => o.in_zone(Zone::Stack),
                ZoneId::Library    => o.in_zone(Zone::Library),
            };
            zone_match && (o.owner == target_who || o.controller == target_who)
        })
        .filter(|o| {
            let env = crate::ir::executor::BindEnv::new().with_controller(controller);
            crate::ir::executor::matches(&spec.filter, o.id, state, &env)
        })
        .map(|o| o.id)
        .collect()
}

/// Preconditions are derived automatically: ability is available iff
/// every cost component can be paid and a valid target exists (if required).
#[derive(Clone)]
pub struct AbilityDef {
    /// Where the source must be located for this ability to be activatable.
    /// Default: Battlefield. Set to Hand for cycling, channel, ninjutsu, etc.
    pub source_zone: SourceZone,
    /// All costs to activate this ability, paid simultaneously. Single-
    /// variant `CostBody::Ir(Action)` after Phase 6 collapsed the dual world.
    pub costs: crate::ir::ability::CostBody,

    // ── Target (optional) ─────────────────────────────────────────────────────
    /// If not `TargetSpec::None`, a valid target must exist for the ability to be available.
    pub(crate) target_spec: TargetSpec,

    // ── Choice (optional, CR "choose" ≠ "target") ────────────────────────────
    /// If `Some`, the engine enumerates matching objects at resolution time and
    /// delegates the pick to `Strategy::choose_for_effect`. The chosen ObjId is
    /// passed to the effect closure via its `targets` slice.
    pub(crate) choice_spec: Option<ChoiceSpec>,

    // ── Effect ────────────────────────────────────────────────────────────────
    pub(crate) ability_factory: Option<AbilityFactory>,
    /// IR resolution body. When set, preferred over `ability_factory`.
    pub(crate) ir_body: Option<crate::ir::action::Action>,

    /// False when a CE prevents activation (e.g. Disruptor Flute, Karn).
    /// Reset to true each recompute. Checked by ability_available / collect_legal_actions.
    pub activatable: bool,
    /// Timing override. Default = instant speed. Sorcery = empty stack + main phase only.
    pub timing: ActivationTiming,
}

impl Default for AbilityDef {
    fn default() -> Self {
        AbilityDef {
            source_zone: SourceZone::Battlefield,
            costs: crate::ir::ability::CostBody::default(),
            target_spec: TargetSpec::None,
            choice_spec: None,
            ability_factory: None,
            ir_body: None,
            activatable: true,
            timing: ActivationTiming::Default,
        }
    }
}

impl AbilityDef {
    /// True if this is a loyalty ability (costs contain a `LoyaltyAdjust`).
    pub fn is_loyalty_ability(&self) -> bool {
        self.loyalty_delta().is_some()
    }

    /// Returns the loyalty delta if this is a loyalty ability — walks the
    /// IR action tree for `Action::LoyaltyAdjust(n)`.
    pub(crate) fn loyalty_delta(&self) -> Option<i32> {
        let crate::ir::ability::CostBody::Ir(a) = &self.costs;
        action_loyalty_delta(a)
    }

    /// True if this looks like a fetch land activation (SacSelf + Life cost > 0).
    pub fn is_fetch_ability(&self) -> bool {
        if !self.costs.requires_sac_self() {
            return false;
        }
        let crate::ir::ability::CostBody::Ir(a) = &self.costs;
        action_includes_paylife_positive(a)
    }
}

fn action_loyalty_delta(a: &crate::ir::action::Action) -> Option<i32> {
    use crate::ir::action::Action::*;
    match a {
        LoyaltyAdjust(n) => Some(*n),
        Sequence(actions) => actions.iter().find_map(action_loyalty_delta),
        IfThen { then, else_, .. } => action_loyalty_delta(then)
            .or_else(|| else_.as_ref().and_then(|e| action_loyalty_delta(e))),
        MayDo { action, .. } => action_loyalty_delta(action),
        ForEach { body, .. } => action_loyalty_delta(body),
        Choose { options, .. } => options.iter().find_map(|o| action_loyalty_delta(&o.action)),
        _ => None,
    }
}

fn action_includes_paylife_positive(a: &crate::ir::action::Action) -> bool {
    use crate::ir::action::Action::*;
    use crate::ir::expr::Expr;
    match a {
        PayLife { amount: Expr::Num(n), .. } => *n > 0,
        Sequence(actions) => actions.iter().any(action_includes_paylife_positive),
        IfThen { then, else_, .. } => {
            action_includes_paylife_positive(then)
                || else_.as_ref().map_or(false, |e| action_includes_paylife_positive(e))
        }
        MayDo { action, .. } => action_includes_paylife_positive(action),
        ForEach { body, .. } => action_includes_paylife_positive(body),
        Choose { options, .. } => options.iter().any(|o| action_includes_paylife_positive(&o.action)),
        _ => false,
    }
}

// ── Mana ability types ────────────────────────────────────────────────────────

/// Factory that builds the mana-production effect for one activation.
/// `who` — resolved controller; `color` — the specific color needed (`None` = generic slot).
/// Fixed-color sources (e.g. Islands) ignore `color`; any-color sources (e.g. Lotus Petal)
/// use `color` to produce exactly the requested pip.
pub type ManaEffectFactory =
    std::sync::Arc<dyn Fn(PlayerId, Option<Color>) -> Effect + Send + Sync>;

/// How a permanent (or hand card) produces mana.
/// `source_zone`   — where the source must be to activate (default Battlefield).
/// `costs`         — what must be paid to activate (typically TapSelf or SacSelf).
/// `produces`      — colors produced; empty vec → colorless only. Used for affordability prediction.
/// `produces_count`— number of mana produced per activation (default 1). Used for prediction.
/// `make_effect`   — factory that builds the actual production effect (add mana + any side effects).
/// `condition`     — optional gate: `(source_id, &SimState) -> bool`. When `Some`, the ability
///                   can only be activated (and counts toward potential mana) if the predicate
///                   returns true. Used for Metalcraft, etc.
#[derive(Clone)]
pub struct ManaAbility {
    pub source_zone: SourceZone,
    pub costs: crate::ir::ability::CostBody,
    pub produces: Vec<Color>,
    pub produces_count: usize,
    pub(crate) make_effect: ManaEffectFactory,
    pub condition: Option<crate::ir::expr::Filter>,
    /// False when a CE prevents activation (e.g. Karn, Null Rod).
    /// Reset to true each recompute. Checked by collect_legal_actions and mana sub-loop.
    pub activatable: bool,
    /// Timing override. Default = usable in mana sub-loop (CR 601.2g).
    /// Instant = excluded from sub-loop, available during priority (LED).
    /// Sorcery = priority only, empty stack + main phase.
    pub timing: ActivationTiming,
}

impl Default for ManaAbility {
    fn default() -> Self {
        Self {
            source_zone: SourceZone::Battlefield,
            costs: crate::ir::ability::CostBody::default(),
            produces: vec![],
            produces_count: 1,
            make_effect: std::sync::Arc::new(|_, _| Effect(std::sync::Arc::new(|_, _, _| {}))),
            condition: None,
            activatable: true,
            timing: ActivationTiming::Default,
        }
    }
}

/// One of the five basic land subtypes. Closed set (CR 205.3i); has not grown
/// in the game's history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BasicLandType {
    Plains,
    Island,
    Swamp,
    Mountain,
    Forest,
}

impl BasicLandType {
    const ALL: [BasicLandType; 5] = [
        BasicLandType::Plains,
        BasicLandType::Island,
        BasicLandType::Swamp,
        BasicLandType::Mountain,
        BasicLandType::Forest,
    ];

    fn bit(self) -> u8 {
        match self {
            BasicLandType::Plains => 1,
            BasicLandType::Island => 2,
            BasicLandType::Swamp => 4,
            BasicLandType::Mountain => 8,
            BasicLandType::Forest => 16,
        }
    }

    /// Intrinsic mana color (CR 305.6).
    pub(crate) fn mana_color(self) -> &'static str {
        match self {
            BasicLandType::Plains => "W",
            BasicLandType::Island => "U",
            BasicLandType::Swamp => "B",
            BasicLandType::Mountain => "R",
            BasicLandType::Forest => "G",
        }
    }

    /// Lowercase subtype name ("plains", "island", …) — matches predicate strings.
    pub(crate) fn as_lower(self) -> &'static str {
        match self {
            BasicLandType::Plains => "plains",
            BasicLandType::Island => "island",
            BasicLandType::Swamp => "swamp",
            BasicLandType::Mountain => "mountain",
            BasicLandType::Forest => "forest",
        }
    }
}

/// Set of basic land subtypes, stored as a bitset. `Copy` and allocation-free.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LandTypes(u8);

impl LandTypes {
    pub(crate) fn new() -> Self {
        LandTypes(0)
    }

    pub(crate) fn from_types(types: &[BasicLandType]) -> Self {
        let mut out = LandTypes(0);
        for &t in types {
            out.insert(t);
        }
        out
    }

    pub(crate) fn contains(self, t: BasicLandType) -> bool {
        self.0 & t.bit() != 0
    }

    pub(crate) fn insert(&mut self, t: BasicLandType) {
        self.0 |= t.bit();
    }

    pub(crate) fn iter(self) -> impl Iterator<Item = BasicLandType> {
        BasicLandType::ALL.into_iter().filter(move |&t| self.contains(t))
    }
}

// ── Per-variant data structs ──────────────────────────────────────────────────

#[derive(Clone, Default)]
pub struct LandData {
    pub(crate) land_types: LandTypes,
    /// Nonbasic land subtypes (e.g. "Urza's", "Mine", "Power-Plant", "Tower").
    /// Basic land types remain in `land_types` because they carry intrinsic mana rules.
    pub(crate) nonbasic_subtypes: Vec<String>,
    pub(crate) mana_abilities: Vec<ManaAbility>,
    pub abilities: Vec<AbilityDef>,
}

#[derive(Clone)]
pub struct CreatureData {
    pub(crate) mana_cost: String,
    // `power` and `toughness` are private — always read through the materialized `CardDef`
    // (which folds in counters and CE modifiers). Write only via `adjust_pt`.
    power: i32,
    toughness: i32,
    pub(crate) legendary: bool,
    pub(crate) delve: bool,
    pub(crate) abilities: Vec<AbilityDef>,
    pub(crate) mana_abilities: Vec<ManaAbility>,
    pub(crate) keywords: Keywords,
    /// Creature subtypes (e.g. "Ninja", "Wizard", "Human"). Used for emblem/CE filters.
    pub(crate) creature_subtypes: Vec<String>,
}

#[derive(Clone, Default)]
pub struct ArtifactData {
    pub(crate) mana_cost: String,
    pub(crate) abilities: Vec<AbilityDef>,
    pub(crate) mana_abilities: Vec<ManaAbility>,
    /// Artifact subtypes (e.g. "Equipment", "Treasure"). CR 205.3g.
    pub(crate) subtypes: Vec<String>,
}

#[derive(Clone, Default)]
pub struct EnchantmentData {
    pub(crate) abilities: Vec<AbilityDef>,
}

/// Build a ninjutsu activated ability (CR 702.49).
///
/// Costs: mana + return an unblocked attacker. Source zone: Hand.
/// Effect: put this card onto the battlefield tapped and attacking the same target.
///
/// Cost is `Sequence([PayMana(mc), MoveByChoice(BF→Hand, Return,
/// unblocked-attacker-controlled-by-you, count=1, bind=$ninjutsu_attacker)])`.
/// `cost_exec::pay` (`capture_returned_attack_targets`) captures the chosen
/// attacker's pre-move `attack_target` into `CostsPaidCtx`, which the
/// resolution effect reads to inherit the same combat target.
pub(crate) fn ninjutsu_ability(mana_cost: &str) -> AbilityDef {
    use crate::ir::action::{Action, MoveVerb, Who};
    use crate::ir::context::Ctx;
    use crate::ir::expr::{Expr, Filter, ZoneKindSel};
    let mc = parse_mana_cost(mana_cost);
    let unblocked_attacker = Filter(Expr::And(
        Box::new(Expr::Eq(
            Box::new(Expr::Controller(Box::new(Expr::Ctx(Ctx::It)))),
            Box::new(Expr::Ctx(Ctx::Controller)),
        )),
        Box::new(Expr::And(
            Box::new(Expr::Attacking(Box::new(Expr::Ctx(Ctx::It)))),
            Box::new(Expr::Unblocked(Box::new(Expr::Ctx(Ctx::It)))),
        )),
    ));
    AbilityDef {
        source_zone: SourceZone::Hand,
        costs: crate::ir::ability::CostBody::Ir(Action::Sequence(vec![
            Action::PayMana(mc),
            Action::MoveByChoice {
                who: Who::You,
                from: ZoneKindSel::Battlefield,
                to: ZoneKindSel::Hand,
                verb: MoveVerb::Return,
                filter: unblocked_attacker,
                count: Expr::Num(1),
                bind_as: Some("$ninjutsu_attacker"),
            },
        ])),
        ir_body: Some(Action::NinjutsuEnter),
        ..Default::default()
    }
}

impl CreatureData {
    /// Read effective power — call only on a value from `MaterializedState.defs`, never
    /// directly from `catalog_map`, so continuous effects are always reflected.
    pub fn power(&self) -> i32 { self.power }

    /// Read effective toughness — same rule as `power()`.
    pub fn toughness(&self) -> i32 { self.toughness }

    /// Apply a power/toughness delta. Used exclusively by `fold_game_state_into_def`
    /// (counters + temporary mods) and `ContinuousModFn` closures in CE machinery.
    pub(crate) fn adjust_pt(&mut self, delta_power: i32, delta_toughness: i32) {
        self.power     += delta_power;
        self.toughness += delta_toughness;
    }

    /// Construct a `CreatureData` with the mandatory fields.
    /// All optional fields (legendary, delve, abilities, keywords)
    /// default to false/empty and can be set on the returned value.
    pub(crate) fn new(mana_cost: impl Into<String>, power: i32, toughness: i32) -> Self {
        CreatureData {
            mana_cost: mana_cost.into(),
            power,
            toughness,
            legendary: false,
            delve: false,
            abilities: vec![],
            mana_abilities: vec![],
            keywords: Keywords::default(),
            creature_subtypes: vec![],
        }
    }
}


/// One mode of a spell: its targeting requirement and effect factory.
#[derive(Clone)]
pub struct SpellMode {
    pub(crate) target_spec: TargetSpec,
    pub(crate) factory: SpellFactory,
}

/// The mode structure of a spell (CR 700.2).
/// Non-modal spells have a single mode; modal spells ("Choose one —") have two or more.
/// Mode is chosen at cast time (CR 700.2a) and stored in `CostsPaidCtx::chosen_mode`.
#[derive(Clone)]
pub enum SpellModes {
    /// Non-modal: exactly one mode.
    Single(SpellMode),
    /// Modal (CR 700.2): two or more modes, chosen at cast time via `ChoiceRequest::Mode`.
    /// Invariant: len >= 2 (enforced by `SpellModes::modal`).
    Modal(Vec<SpellMode>),
}

impl SpellModes {
    /// Construct a modal spell. Panics if fewer than 2 modes are provided.
    /// Unused: every spell moved to IR `AbilityKind::OnResolve { modes }` (Show
    /// and Tell was the last single-mode holdout). The whole `SpellModes`/
    /// `SpellMode` path and the `SpellData.modes` field are now vestigial and can
    /// be deleted once a sweep removes the legacy resolution branch.
    #[allow(dead_code)]
    pub(crate) fn modal(modes: Vec<SpellMode>) -> Self {
        assert!(modes.len() >= 2, "modal spells require at least 2 modes, got {}", modes.len());
        SpellModes::Modal(modes)
    }

    pub(crate) fn get(&self, mode: usize) -> Option<&SpellMode> {
        match self {
            SpellModes::Single(m) if mode == 0 => Some(m),
            SpellModes::Modal(modes) => modes.get(mode),
            _ => None,
        }
    }

    pub(crate) fn len(&self) -> usize {
        match self {
            SpellModes::Single(_) => 1,
            SpellModes::Modal(modes) => modes.len(),
        }
    }

    #[allow(dead_code)]
    pub(crate) fn is_modal(&self) -> bool {
        matches!(self, SpellModes::Modal(_))
    }
}

/// Spell data shared by Instant and Sorcery variants.
#[derive(Clone)]
pub struct SpellData {
    pub(crate) mana_cost: String,
    pub(crate) delve: bool,
    /// Card subtypes (e.g. `["adventure"]` for the adventure face of a split card).
    pub(crate) subtypes: Vec<String>,
    /// Spell modes (CR 700.2). `None` for default-constructed placeholders;
    /// real spells always have `Some`.
    pub(crate) modes: Option<SpellModes>,
}

impl Default for SpellData {
    fn default() -> Self {
        SpellData {
            mana_cost: String::new(),
            delve: false,
            subtypes: Vec::new(),
            modes: None,
        }
    }
}

#[derive(Clone, Default)]
pub struct PlaneswalkerData {
    pub(crate) mana_cost: String,
    pub(crate) loyalty: i32,
    pub(crate) abilities: Vec<AbilityDef>,
}

#[derive(Clone)]
pub enum CardKind {
    Land(LandData),
    Creature(CreatureData),
    Artifact(ArtifactData),
    Instant(SpellData),
    Sorcery(SpellData),
    Planeswalker(PlaneswalkerData),
    Enchantment(EnchantmentData),
}

/// Layout of a multi-face card. Determines how `back` is interpreted at cast/flip time.
/// `Normal` = single-faced (default). `DoubleFaced` = transform DFC (e.g. Tamiyo).
/// `Split` = two castable halves (split cards, adventures).
#[derive(Clone, Default, Debug, PartialEq)]
pub enum CardLayout {
    #[default]
    Normal,
    DoubleFaced,
    Split,
}

// ── CardDef wrapper ───────────────────────────────────────────────────────────

/// A card the generator knows about. Cards not in the catalog are treated as
/// generic non-land spells: hand-eligible, not permanent candidates.
///
/// Wrapper struct preserving direct `.catalog_key` access and stable HashMap keys while
/// holding a typed `kind` that enforces card-category invariants.
/// A replacement effect definition stored directly on a `CardDef`.
/// `check` returns `Some(targets)` if the replacement applies to an event; `make_effect` builds
/// the closure that runs instead of the original event.
#[derive(Clone)]
pub struct ReplacementDef {
    pub(crate) check: ReplacementCheckFn,
    /// Factory: called at evaluation time with `(source_id, controller)`.
    /// CardDef-specific data is captured inside the factory at card-load time.
    pub(crate) make_effect: std::sync::Arc<dyn Fn(ObjId, PlayerId) -> Effect + Send + Sync>,
    /// Predicate controlling when this replacement is active.
    /// Default: source is on the battlefield (static ability replacements).
    /// ETB-self replacements (CR 614.1c/d): always active regardless of zone —
    /// the check fn guards specificity (id == source_id && to == Battlefield).
    pub(crate) active_when: TriggerPredicate,
}

/// A "can't happen" prohibition stored on a `CardDef` (CR 614.17).
/// Checked before replacements in `fire_event`. Takes `&SimState` so type/zone/controller
/// checks can be performed without string dispatch.
#[derive(Clone)]
pub struct ProhibitionDef {
    pub(crate) check: ProhibitionCheckFn,
    /// Predicate controlling when this prohibition is active.
    /// Default: source is on the battlefield (static ability prohibitions).
    /// "This spell can't be countered": tp_on_stack (active while on the stack).
    pub(crate) active_when: TriggerPredicate,
}

#[derive(Clone)]
pub struct CardDef {
    pub(crate) name: String,
    /// Relative likelihood of appearing as a permanent in play (default 100).
    #[allow(dead_code)]
    pub(crate) play_weight: Option<u32>,
    pub kind: CardKind,
    /// Colors of this card, derived from mana cost and explicit color flags at load time.
    pub(crate) colors: Vec<Color>,
    /// Card types (Land, Creature, Instant, etc.) — mirrors the `kind` discriminant but
    /// allows multi-type and is accessible without pattern-matching on `kind`.
    pub(crate) types: Vec<CardType>,
    /// Supertypes (Legendary, Basic, Snow).
    pub(crate) supertypes: Vec<Supertype>,
    /// Layout of a multi-face card (Normal / DoubleFaced / Split).
    pub(crate) layout: CardLayout,
    /// Back/second face for DFCs and split/adventure cards.
    /// For DoubleFaced cards, this is the transformed face.
    /// For Split cards (including adventures), this is the second castable half.
    pub(crate) back: Option<Box<CardDef>>,
    /// Trigger check functions for this card (set at card definition time).
    pub(crate) trigger_defs: Vec<TriggerDef>,
    /// Replacement effect definitions for this card (set at card definition time).
    pub(crate) replacement_defs: Vec<ReplacementDef>,
    /// Prohibition definitions for this card (CR 614.17 — "can't" effects).
    /// Checked before replacements in `fire_event`; if any match, the event is suppressed.
    pub(crate) prohibition_defs: Vec<ProhibitionDef>,
    /// Static ability factories. Called at ETB to register a `ContinuousInstance` for this
    /// object. The CI has `expiry: WhileSourceOnBattlefield` and is removed on LTB.
    pub(crate) static_ability_defs: Vec<StaticAbilityDef>,
    /// Trigger check functions granted to this object by continuous effects (Layer 6).
    /// Reset to empty at the start of each `recompute` cycle (since recompute clones from the
    /// catalog where this is always empty). CE modifiers push to this during recompute.
    /// Checked by `fire_triggers` for each active battlefield object.
    pub(crate) granted_trigger_defs: Vec<TriggerCheckFn>,
    /// IR abilities granted to this object by continuous effects (`CEMod::GrantAbility`,
    /// Layer 6) — the declarative analog of `granted_trigger_defs`. Same lifecycle:
    /// empty in the catalog, pushed during recompute, fired by `fire_triggers` (the
    /// `Triggered` ones) for each active battlefield object.
    pub(crate) granted_abilities: Vec<crate::ir::ability::Ability>,
    /// Costs that must always be paid in addition to the chosen base/alternative cost.
    /// Per CR 118.9d these apply regardless of which cost path is taken. Single-
    /// variant `CostBody::Ir(Action)`; X-cost amounts use `Expr::Ctx(Ctx::Var("$x"))`
    /// bound to the announced `chosen_x` at pay time.
    pub(crate) additional_costs: crate::ir::ability::CostBody,
    /// False iff this spell can't be countered (CR 608.2b). Checked at resolution by
    /// `eff_counter_target`; the spell is still a legal target for counterspells.
    /// Modifiable by continuous effects.
    pub(crate) counterable: bool,
    /// Generic-mana surcharge applied to this card's casting cost by a CE (e.g. Disruptor Flute).
    /// Added to `ManaCost.generic` during affordability checks. Reset to 0 on each `recompute`
    /// because materialized views start from a fresh catalog clone.
    pub(crate) casting_cost_modifier: i32,
    /// Minimum mana component required to cast this spell after increases/reductions.
    /// Used by Trinisphere (CR 601.2f / 118.9). Zero means no floor.
    pub(crate) minimum_cast_mana: i32,
    /// Whether this permanent untaps normally during its controller's untap step.
    /// Grim/Basalt Monolith set this false; explicit untap effects still work.
    pub(crate) untaps_during_untap_step: bool,
    /// Generic indestructible characteristic used by destroy effects/SBAs.
    /// Stored on CardDef so noncreature permanents such as The One Ring are covered.
    pub(crate) indestructible: bool,
    /// True when this card may be cast from its current zone.
    /// Default true for cards in hand, false for other zones. CEs can override:
    /// Dauthi Voidwalker sets true on exiled cards, Lavinia sets false on opponent hand cards.
    /// Reset to zone-based default each recompute. Checked by collect_legal_actions.
    pub castable: bool,
    /// Alternate costs granted by continuous effects (e.g. Omniscience grants a zero-cost
    /// alternate). Works for ALL card types, unlike `SpellData.alternate_costs` which only
    /// covers Instant/Sorcery. Reset to empty on each `recompute`.
    pub(crate) alternate_costs: Vec<AlternateCost>,
    /// Protection predicates (CR 702.16). Each predicate tests a *source* ObjId:
    /// if any returns true, the source cannot target, damage, or block this object.
    /// Uses `ObjPredicate` — the predicate can inspect the source's CardDef, zone,
    /// controller, etc. via the uniform id model.
    pub(crate) protection_from: Vec<crate::ir::expr::Filter>,
    /// Data-based IR abilities (Stage 3 dual-pathway: coexists with the
    /// closure-based fields above). When non-empty, the engine dispatches
    /// this card's behavior through the IR executor.
    pub(crate) abilities: Vec<crate::ir::ability::Ability>,
    /// Saga chapter abilities (CR 714), in order: `chapters[N-1]` is chapter N's
    /// effect, a triggered ability that fires as the Saga's lore count reaches N.
    /// Non-empty iff this card is a Saga; its length is the final chapter number.
    pub(crate) chapters: Vec<crate::ir::action::Action>,
    /// Optional target specification for each Saga chapter. Missing entries default
    /// to `TargetSpec::None`. Kept parallel to `chapters` to avoid changing the
    /// existing chapter-action representation used by Urza's Saga.
    pub(crate) chapter_target_specs: Vec<crate::TargetSpec>,
}

/// Factory that creates a `ContinuousInstance` for a specific game object.
/// Called when the object enters the battlefield; `source_id` and `controller` are bound then.
pub type StaticAbilityDef =
    std::sync::Arc<dyn Fn(ObjId, PlayerId) -> ContinuousInstance + Send + Sync>;

/// Digested result of [`CardDef::added_mana_on_resolve`] — the mana a ritual
/// spell adds on resolution, read structurally from its IR body.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddedMana {
    /// Total mana produced.
    pub count: u32,
    /// `Some(colors)` = the produced mana is these fixed colors (Dark Ritual →
    /// `[Black, Black, Black]`); a colored list shorter than `count` is padded
    /// with colorless. `None` = all `count` mana are a single color the
    /// controller chooses at resolution (Lotus Petal / LED style).
    pub colors: Option<Vec<Color>>,
}

/// Walk an IR action tree for the first reachable `AddMana` (mirrors the
/// engine's internal `find_first_add_mana`). Pessimistic over branches.
fn first_add_mana(
    action: &crate::ir::action::Action,
) -> Option<(&crate::ir::expr::Expr, &crate::ir::action::ManaSpec)> {
    use crate::ir::action::Action;
    match action {
        Action::AddMana { count, spec, .. } => Some((count, spec)),
        Action::Sequence(actions) => actions.iter().find_map(first_add_mana),
        Action::IfThen { then, else_, .. } => {
            first_add_mana(then).or_else(|| else_.as_ref().and_then(|e| first_add_mana(e)))
        }
        Action::MayDo { action, .. } => first_add_mana(action),
        Action::ForEach { body, .. } => first_add_mana(body),
        Action::Choose { options, .. } => options.iter().find_map(|o| first_add_mana(&o.action)),
        _ => None,
    }
}

/// The library-search `Filter` of the first "search library → put the find on
/// TOP" tutor in an IR action tree (Personal/Vampiric/Mystical Tutor), or `None`.
/// A `dest:Battlefield` searcher (Green Sun's Zenith) is NOT a top tutor: it
/// doesn't place a card where a subsequent draw will see it, so it can't acquire
/// a hand-cast payoff.
fn first_top_library_search(
    action: &crate::ir::action::Action,
) -> Option<&crate::ir::expr::Filter> {
    use crate::ir::action::Action;
    use crate::ir::expr::ZoneKindSel;
    match action {
        Action::Search {
            zone: ZoneKindSel::Library,
            dest: ZoneKindSel::Library,
            to_top: true,
            filter,
            ..
        } => Some(filter),
        Action::Sequence(actions) => actions.iter().find_map(first_top_library_search),
        Action::IfThen { then, else_, .. } => first_top_library_search(then)
            .or_else(|| else_.as_ref().and_then(|e| first_top_library_search(e))),
        Action::MayDo { action, .. } => first_top_library_search(action),
        Action::ForEach { body, .. } => first_top_library_search(body),
        Action::Choose { options, .. } => options.iter().find_map(|o| first_top_library_search(&o.action)),
        _ => None,
    }
}

/// True if an IR action tree contains a card-selection / card-draw primitive —
/// the signature of a cantrip or dig spell (`Draw`, `Dig`, `Scry`, `Surveil`,
/// `OrderTop`). Used by [`CardDef::digs_on_resolve`]. A library-top tutor
/// (`Search`→top) is intentionally excluded (see [`first_top_library_search`]).
fn body_digs(action: &crate::ir::action::Action) -> bool {
    use crate::ir::action::Action;
    match action {
        Action::Draw { .. }
        | Action::Dig { .. }
        | Action::Scry { .. }
        | Action::Surveil { .. }
        | Action::OrderTop { .. } => true,
        Action::Sequence(actions) => actions.iter().any(body_digs),
        Action::IfThen { then, else_, .. } => {
            body_digs(then) || else_.as_ref().map_or(false, |e| body_digs(e))
        }
        Action::MayDo { action, .. } => body_digs(action),
        Action::ForEach { body, .. } => body_digs(body),
        Action::Choose { options, .. } => options.iter().any(|o| body_digs(&o.action)),
        _ => false,
    }
}

/// Total cards a resolve-body lets you SEE for selection — the sum of the counts on
/// `Draw` / `OrderTop` / `Dig` / `Surveil` / `Scry` / `Look`. (Ponder: OrderTop 3 +
/// Draw 1 = 4; Brainstorm: Draw 3 = 3; Consider: Surveil 1 + Draw 1 = 2.) Branches
/// take the max (you see one branch's cards). Non-literal counts contribute 0.
fn body_cards_seen(action: &crate::ir::action::Action) -> u32 {
    use crate::ir::action::Action;
    use crate::ir::expr::Expr;
    let n = |e: &Expr| if let Expr::Num(v) = e { (*v).max(0) as u32 } else { 0 };
    match action {
        Action::Draw { n: c, .. }
        | Action::OrderTop { n: c, .. }
        | Action::Dig { n: c, .. }
        | Action::Surveil { n: c, .. }
        | Action::Scry { n: c, .. }
        | Action::Look { n: c, .. } => n(c),
        Action::Sequence(actions) => actions.iter().map(body_cards_seen).sum(),
        Action::IfThen { then, else_, .. } => {
            body_cards_seen(then).max(else_.as_ref().map_or(0, |e| body_cards_seen(e)))
        }
        Action::MayDo { action, .. } => body_cards_seen(action),
        Action::ForEach { body, .. } => body_cards_seen(body),
        Action::Choose { options, .. } => options.iter().map(|o| body_cards_seen(&o.action)).max().unwrap_or(0),
        _ => 0,
    }
}

/// Cards a resolve-body actually DRAWS — the sum of `Draw` counts only (Brainstorm 3,
/// Ponder 1, Consider 1). Unlike `body_cards_seen` it ignores scry/surveil/look/order,
/// because some effects key off cards *drawn* (e.g. Tamiyo's transform). Branches take
/// the max; non-literal counts contribute 0.
fn body_cards_drawn(action: &crate::ir::action::Action) -> u32 {
    use crate::ir::action::Action;
    use crate::ir::expr::Expr;
    let n = |e: &Expr| if let Expr::Num(v) = e { (*v).max(0) as u32 } else { 0 };
    match action {
        Action::Draw { n: c, .. } => n(c),
        Action::Sequence(actions) => actions.iter().map(body_cards_drawn).sum(),
        Action::IfThen { then, else_, .. } => {
            body_cards_drawn(then).max(else_.as_ref().map_or(0, |e| body_cards_drawn(e)))
        }
        Action::MayDo { action, .. } => body_cards_drawn(action),
        Action::ForEach { body, .. } => body_cards_drawn(body),
        Action::Choose { options, .. } => options.iter().map(|o| body_cards_drawn(&o.action)).max().unwrap_or(0),
        _ => 0,
    }
}

/// True if a resolve-body can shuffle the library (Ponder's `MayDo(Shuffle)`).
fn body_shuffles(action: &crate::ir::action::Action) -> bool {
    use crate::ir::action::Action;
    match action {
        Action::Shuffle { .. } => true,
        Action::Sequence(actions) => actions.iter().any(body_shuffles),
        Action::IfThen { then, else_, .. } => {
            body_shuffles(then) || else_.as_ref().map_or(false, |e| body_shuffles(e))
        }
        Action::MayDo { action, .. } => body_shuffles(action),
        Action::ForEach { body, .. } => body_shuffles(body),
        Action::Choose { options, .. } => options.iter().any(|o| body_shuffles(&o.action)),
        _ => false,
    }
}

/// True if an IR action tree contains a `Tap` (used to recognise the
/// `Replace(Sequence([Move, Tap]))` shape of an enters-tapped replacement).
fn action_taps_self(action: &crate::ir::action::Action) -> bool {
    use crate::ir::action::Action;
    match action {
        Action::Tap { .. } => true,
        Action::Sequence(actions) => actions.iter().any(action_taps_self),
        Action::IfThen { then, else_, .. } => {
            action_taps_self(then) || else_.as_ref().map_or(false, |e| action_taps_self(e))
        }
        Action::MayDo { action, .. } => action_taps_self(action),
        Action::ForEach { body, .. } => action_taps_self(body),
        Action::Choose { options, .. } => options.iter().any(|o| action_taps_self(&o.action)),
        _ => false,
    }
}

impl CardDef {
    /// Name of the back/second face (DFC, split, adventure), if any. Public
    /// accessor so app crates (e.g. the pilegen snapshot registry) can enumerate
    /// face names without touching the private `back` field.
    pub fn back_name(&self) -> Option<&str> {
        self.back.as_ref().map(|b| b.name.as_str())
    }

    /// Convenience constructor for a vanilla creature with the given power,
    /// toughness, and keywords. Public so content crates / their tests can build
    /// simple bodies without the full `CardDef::new` AST.
    pub fn vanilla_creature(name: &str, power: i32, toughness: i32, keywords: &[Keyword]) -> CardDef {
        let mut data = CreatureData::new("", power, toughness);
        data.keywords = Keywords::from_slice(keywords);
        CardDef::new(name, CardKind::Creature(data), vec![], None, vec![],
                     CardLayout::Normal, None, vec![], vec![], vec![], vec![])
    }

    pub fn is_land(&self) -> bool { matches!(self.kind, CardKind::Land(_)) }
    pub fn is_creature(&self) -> bool { matches!(self.kind, CardKind::Creature(_)) }
    /// True when the object's current characteristics include the Artifact card type.
    /// Uses the type vector rather than only `kind` so continuous effects and
    /// multi-type permanents can be recognized by external strategy crates.
    pub fn is_artifact(&self) -> bool { self.types.contains(&CardType::Artifact) }
    pub fn is_instant(&self) -> bool { matches!(self.kind, CardKind::Instant(_)) }
    #[allow(dead_code)]
    pub fn is_sorcery(&self) -> bool { matches!(self.kind, CardKind::Sorcery(_)) }

    pub fn mana_cost(&self) -> &str {
        match &self.kind {
            CardKind::Land(_) | CardKind::Enchantment(_) => "",
            CardKind::Creature(c) => &c.mana_cost,
            CardKind::Artifact(a) => &a.mana_cost,
            CardKind::Instant(s) | CardKind::Sorcery(s) => &s.mana_cost,
            CardKind::Planeswalker(p) => &p.mana_cost,
        }
    }

    pub fn abilities(&self) -> &[AbilityDef] {
        match &self.kind {
            CardKind::Land(l) => &l.abilities,
            CardKind::Creature(c) => &c.abilities,
            CardKind::Artifact(a) => &a.abilities,
            CardKind::Planeswalker(p) => &p.abilities,
            CardKind::Enchantment(e) => &e.abilities,
            CardKind::Instant(_) | CardKind::Sorcery(_) => &[],
        }
    }

    pub(crate) fn alternate_costs(&self) -> &[AlternateCost] {
        &self.alternate_costs
    }

    /// Target spec for mode 0 (non-modal spells) or the given mode.
    pub fn target_spec(&self) -> &TargetSpec {
        self.target_spec_for_mode(0)
    }

    pub(crate) fn target_spec_for_mode(&self, mode: usize) -> &TargetSpec {
        // IR path: prefer data-based modes when present.
        for a in &self.abilities {
            if let crate::ir::ability::AbilityKind::OnResolve { modes } = &a.kind {
                if let Some(m) = modes.get(mode) {
                    return &m.target_spec;
                }
            }
        }
        match &self.kind {
            CardKind::Instant(s) | CardKind::Sorcery(s) => {
                s.modes.as_ref()
                    .and_then(|m| m.get(mode))
                    .map(|m| &m.target_spec)
                    .unwrap_or(&TargetSpec::None)
            }
            _ => &TargetSpec::None,
        }
    }

    /// Number of modes (CR 700.2) available for this spell.
    /// Returns the IR mode count when `abilities` carries an `OnResolve`;
    /// otherwise falls back to legacy `SpellData.modes`. `None` if neither.
    pub(crate) fn mode_count(&self) -> Option<usize> {
        for a in &self.abilities {
            if let crate::ir::ability::AbilityKind::OnResolve { modes } = &a.kind {
                return Some(modes.len());
            }
        }
        self.spell_modes().map(|m| m.len())
    }

    pub(crate) fn spell_modes(&self) -> Option<&SpellModes> {
        match &self.kind {
            CardKind::Instant(s) | CardKind::Sorcery(s) => s.modes.as_ref(),
            _ => None,
        }
    }

    pub(crate) fn delve(&self) -> bool {
        match &self.kind {
            CardKind::Creature(c) => c.delve,
            CardKind::Instant(s) | CardKind::Sorcery(s) => s.delve,
            _ => false,
        }
    }

    pub(crate) fn legendary(&self) -> bool {
        match &self.kind {
            CardKind::Creature(c) => c.legendary,
            CardKind::Planeswalker(_) => true,  // all PWs are legendary since 2013
            _ => false,
        }
    }

    pub fn is_blue(&self) -> bool { self.colors.contains(&Color::Blue) }

    /// True if this card has the Island land subtype — i.e. it is legal to return for
    /// Daze's free alternative cost (Underground Sea is "Land — Island Swamp", so it counts).
    pub fn is_island(&self) -> bool {
        matches!(&self.kind, CardKind::Land(l) if l.land_types.contains(BasicLandType::Island))
    }

    #[allow(dead_code)]
    pub(crate) fn is_black(&self) -> bool { self.colors.contains(&Color::Black) }

    pub fn mana_abilities(&self) -> &[ManaAbility] {
        match &self.kind {
            CardKind::Land(l) => &l.mana_abilities,
            CardKind::Creature(c) => &c.mana_abilities,
            CardKind::Artifact(a) => &a.mana_abilities,
            _ => &[],
        }
    }

    /// Mana this card adds *when it resolves as a spell* (a "ritual" like Dark
    /// Ritual) — introspected from the `OnResolve` IR body, not name-hardcoded.
    /// This is the structured truth the planner's `spell_mana_production`
    /// name-list only approximated.
    ///
    /// Returns `None` (i.e. "no guaranteed ritual mana") when production isn't
    /// statically and unconditionally known, so callers never overcount:
    /// - the card has no `OnResolve` body (not a spell, or a non-mana spell),
    /// - it's modal (`modes.len() != 1`) — production depends on a chosen mode,
    /// - the `AddMana` count isn't a constant `Expr::Num` (e.g. a threshold-
    ///   conditional Cabal Ritual).
    ///
    /// Mana abilities (Lotus Petal's sac-for-mana, Moxen) are NOT covered here —
    /// they surface through [`CardDef::mana_abilities`]; this is only the
    /// stack-using spell-resolution path.
    pub fn added_mana_on_resolve(&self) -> Option<AddedMana> {
        use crate::ir::ability::AbilityKind;
        use crate::ir::action::ManaSpec;
        use crate::ir::expr::Expr;

        let modes = self.abilities.iter().find_map(|a| match &a.kind {
            AbilityKind::OnResolve { modes } => Some(modes),
            _ => None,
        })?;
        if modes.len() != 1 {
            return None;
        }
        let (count, spec) = first_add_mana(&modes[0].body)?;
        let count = match count {
            Expr::Num(n) if *n >= 0 => *n as u32,
            _ => return None,
        };
        let colors = match spec {
            ManaSpec::Fixed(cs) => Some(cs.clone()),
            ManaSpec::AnyOneColor => None,
        };
        Some(AddedMana { count, colors })
    }

    /// True if this card enters the battlefield tapped — read structurally from
    /// a self-ETB `Replacement` whose body taps the entering object (surveil
    /// duals, taplands; engine composes this as `Replace(Sequence([Move, Tap]))`,
    /// CR 614.1).
    ///
    /// Conservative: returns `true` whenever such a replacement is present, even
    /// if it carries an "unless …" condition (e.g. Mistrise Village). A mana
    /// solver must err toward *tapped* so it never credits same-turn mana a land
    /// can't actually produce — over-counting speed is the cardinal sin here.
    pub fn enters_tapped(&self) -> bool {
        use crate::ir::ability::{AbilityKind, EventPattern, ReplacementBody};
        use crate::ir::expr::ZoneKindSel;
        self.abilities.iter().any(|a| {
            let AbilityKind::Replacement { matches, body, .. } = &a.kind else { return false };
            let EventPattern::EntersZone { zone_kind: ZoneKindSel::Battlefield, .. } = matches else {
                return false;
            };
            matches!(body, ReplacementBody::Replace(action) if action_taps_self(action))
        })
    }

    /// The library-search `Filter` of a "search your library, put the found card
    /// on TOP" tutor (Personal/Vampiric/Mystical Tutor), read from the single-mode
    /// `OnResolve` IR — `None` for anything else. Lets a planner ask "can this card
    /// tutor up <payoff>?" by grounding the returned filter against the payoff
    /// object with `obj_matches`, no name-check. `dest:Battlefield` searchers
    /// (Green Sun's Zenith) are correctly excluded: they don't place a card where
    /// next turn's draw will see it, so they can't acquire a hand-cast payoff.
    ///
    /// Returns `None` for modal spells (`modes.len() != 1`) — like
    /// [`CardDef::added_mana_on_resolve`], production that depends on a chosen mode
    /// isn't statically known.
    pub fn library_top_tutor(&self) -> Option<&crate::ir::expr::Filter> {
        use crate::ir::ability::AbilityKind;
        let modes = self.abilities.iter().find_map(|a| match &a.kind {
            AbilityKind::OnResolve { modes } => Some(modes),
            _ => None,
        })?;
        if modes.len() != 1 {
            return None;
        }
        first_top_library_search(&modes[0].body)
    }

    /// True if this card's on-resolve effect looks at / selects / draws cards — a
    /// cantrip or dig spell (a `Draw`, `Dig`, `Scry`, `Surveil`, or `OrderTop`
    /// somewhere in its IR body). Read structurally, no card names; a multi-mode
    /// spell qualifies if ANY mode digs. Permanents/lands (no `OnResolve` body) and
    /// the empty-bodied combo payload are never diggers. A library-top tutor
    /// (`Search`→top) is deliberately NOT counted here — that's [`library_top_tutor`].
    pub fn digs_on_resolve(&self) -> bool {
        use crate::ir::ability::AbilityKind;
        self.abilities.iter().any(|a| match &a.kind {
            AbilityKind::OnResolve { modes } => modes.iter().any(|m| body_digs(&m.body)),
            _ => false,
        })
    }

    /// True if the card replaces itself via a hand-source activated ability that draws
    /// or digs — a cycling-style free cantrip (e.g. Street Wraith's "discard self, pay
    /// 2 life: draw"). Complements [`digs_on_resolve`] (cast-to-dig cantrips like
    /// Ponder); read structurally from the ability IR, no card names.
    pub fn has_cycling_draw(&self) -> bool {
        self.abilities().iter().any(|a| {
            matches!(a.source_zone, SourceZone::Hand)
                && a.ir_body.as_ref().map_or(false, body_digs)
        })
    }

    /// How many cards this card's on-resolve effect lets you SEE for selection — the
    /// "looks" of a cantrip (Ponder 4, Brainstorm 3, Consider 2, …), read structurally
    /// from the IR (no name table). 0 for non-spells / non-diggers. Multi-mode takes
    /// the max over modes.
    pub fn cards_seen_on_resolve(&self) -> u32 {
        use crate::ir::ability::AbilityKind;
        self.abilities.iter().find_map(|a| match &a.kind {
            AbilityKind::OnResolve { modes } => {
                Some(modes.iter().map(|m| body_cards_seen(&m.body)).max().unwrap_or(0))
            }
            _ => None,
        }).unwrap_or(0)
    }

    /// How many cards this card DRAWS on resolve (Brainstorm 3, Ponder/Consider 1) —
    /// distinct from `cards_seen_on_resolve`, which also counts scry/surveil/look. Used
    /// for triggers that key off cards drawn, e.g. Tamiyo, Inquisitive Student's flip.
    pub fn cards_drawn_on_resolve(&self) -> u32 {
        use crate::ir::ability::AbilityKind;
        self.abilities.iter().find_map(|a| match &a.kind {
            AbilityKind::OnResolve { modes } => {
                Some(modes.iter().map(|m| body_cards_drawn(&m.body)).max().unwrap_or(0))
            }
            _ => None,
        }).unwrap_or(0)
    }

    /// True if this card's on-resolve effect can shuffle the library (Ponder). Used
    /// to model that a shuffle "refreshes" the top so successive cantrips see new
    /// cards. Structural, no name check.
    pub fn shuffles_on_resolve(&self) -> bool {
        use crate::ir::ability::AbilityKind;
        self.abilities.iter().any(|a| match &a.kind {
            AbilityKind::OnResolve { modes } => modes.iter().any(|m| body_shuffles(&m.body)),
            _ => false,
        })
    }

    /// Mutable access to the owning `Vec<AbilityDef>` for this card's kind,
    /// for push/append. Returns `None` for spell kinds (which have no
    /// activated-ability list).
    pub(crate) fn abilities_vec_mut(&mut self) -> Option<&mut Vec<AbilityDef>> {
        match &mut self.kind {
            CardKind::Land(l) => Some(&mut l.abilities),
            CardKind::Creature(c) => Some(&mut c.abilities),
            CardKind::Artifact(a) => Some(&mut a.abilities),
            CardKind::Planeswalker(p) => Some(&mut p.abilities),
            CardKind::Enchantment(e) => Some(&mut e.abilities),
            CardKind::Instant(_) | CardKind::Sorcery(_) => None,
        }
    }

    /// Mutable access to the owning `Vec<ManaAbility>` for push/append.
    /// Returns `None` for kinds without mana-ability lists.
    pub(crate) fn mana_abilities_vec_mut(&mut self) -> Option<&mut Vec<ManaAbility>> {
        match &mut self.kind {
            CardKind::Land(l) => Some(&mut l.mana_abilities),
            CardKind::Creature(c) => Some(&mut c.mana_abilities),
            CardKind::Artifact(a) => Some(&mut a.mana_abilities),
            _ => None,
        }
    }

    pub(crate) fn as_land(&self) -> Option<&LandData> {
        match &self.kind { CardKind::Land(l) => Some(l), _ => None }
    }

    pub fn as_creature(&self) -> Option<&CreatureData> {
        match &self.kind { CardKind::Creature(c) => Some(c), _ => None }
    }

    #[allow(dead_code)]
    pub(crate) fn as_spell(&self) -> Option<&SpellData> {
        match &self.kind {
            CardKind::Instant(s) | CardKind::Sorcery(s) => Some(s),
            _ => None,
        }
    }

    /// Returns the adventure/split second face if this is a `Split`-layout card.
    pub(crate) fn adventure(&self) -> Option<&CardDef> {
        if self.layout == CardLayout::Split { self.back.as_deref() } else { None }
    }


    pub(crate) fn keywords(&self) -> Keywords {
        match &self.kind {
            CardKind::Creature(c) => c.keywords,
            _ => Keywords::default(),
        }
    }

    pub(crate) fn has_keyword(&self, kw: Keyword) -> bool {
        self.keywords().contains(kw)
    }

    /// Returns true if this card has the given subtype (e.g. `"adventure"`, `"Ninja"`, `"Equipment"`).
    pub(crate) fn has_subtype(&self, st: &str) -> bool {
        match &self.kind {
            CardKind::Instant(s) | CardKind::Sorcery(s) => s.subtypes.iter().any(|t| t == st),
            CardKind::Creature(c) => c.creature_subtypes.iter().any(|t| t == st),
            CardKind::Artifact(a) => a.subtypes.iter().any(|t| t == st),
            _ => false,
        }
    }

    /// False iff this spell can't be countered (CR 608.2b).
    pub(crate) fn counterable(&self) -> bool { self.counterable }
}

// ── CardDef constructor + card type helpers ───────────────────────────────────

/// Map a `CardKind` to the corresponding `CardType` enum value.
pub(crate) fn card_type_of(kind: &CardKind) -> CardType {
    match kind {
        CardKind::Land(_)         => CardType::Land,
        CardKind::Creature(_)     => CardType::Creature,
        CardKind::Artifact(_)     => CardType::Artifact,
        CardKind::Instant(_)      => CardType::Instant,
        CardKind::Sorcery(_)      => CardType::Sorcery,
        CardKind::Planeswalker(_) => CardType::Planeswalker,
        CardKind::Enchantment(_)  => CardType::Enchantment,
    }
}


impl CardDef {
    /// Construct a `CardDef` from its parts. Used by `cards.rs` to define cards in Rust.
    /// `colors` must be pre-computed (use `parse_colors`).
    pub(crate) fn new(
        name: impl Into<String>,
        kind: CardKind,
        colors: Vec<Color>,
        play_weight: Option<u32>,
        supertypes: Vec<Supertype>,
        layout: CardLayout,
        back: Option<Box<CardDef>>,
        trigger_defs: Vec<TriggerDef>,
        replacement_defs: Vec<ReplacementDef>,
        prohibition_defs: Vec<ProhibitionDef>,
        static_ability_defs: Vec<StaticAbilityDef>,
    ) -> Self {
        let types = vec![card_type_of(&kind)];
        CardDef {
            name: name.into(),
            play_weight,
            kind,
            colors,
            types,
            supertypes,
            layout,
            back,
            trigger_defs,
            replacement_defs,
            prohibition_defs,
            static_ability_defs,
            granted_trigger_defs: vec![],
            granted_abilities: vec![],
            additional_costs: crate::ir::ability::CostBody::empty(),
            counterable: true,
            casting_cost_modifier: 0,
            minimum_cast_mana: 0,
            untaps_during_untap_step: true,
            indestructible: false,
            castable: false,
            alternate_costs: vec![],
            protection_from: vec![],
            abilities: vec![],
            chapters: vec![],
            chapter_target_specs: vec![],
        }
    }
}

// ── ETB replacement / trigger helpers ────────────────────────────────────────

/// Build a `TriggerDef` for simple self-ETB triggers.
///
/// Fires when this permanent enters the battlefield under its controller's control.
/// Pushes a `TriggerContext` with the given `source_name`, `target_spec`, and effect.
/// Always-active: the event match (`id == source_id`) is the guard.
///
/// Cards with combined ETB+other triggers (e.g. Orcish Bowmasters) or effects that read state
/// at trigger-push time keep their trigger inline.
pub(crate) fn etb_self_trigger<F>(
    source_name: &'static str,
    target_spec: TargetSpec,
    make_effect: F,
) -> TriggerDef
where
    F: Fn(ObjId, PlayerId) -> Effect + Send + Sync + 'static,
{
    TriggerDef {
        check: std::sync::Arc::new(move |event, source_id, controller, _state, pending| {
            if let GameEvent::ZoneChange { id, to: ZoneId::Battlefield, controller: ctlr, .. } = event {
                if *id == source_id && *ctlr == controller {
                    pending.push(TriggerContext {
                        source_name: source_name.into(),
                        controller,
                        target_spec: target_spec.clone(),
                        effect: make_effect(source_id, controller),
                    });
                }
            }
        }),
        active_when: tp_on_battlefield(),
    }
}

// ── Card type enum ─────────────────────────────────────────────────────────────

/// Card category used by the engine and predicates.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug, Default)]
pub enum CardType {
    Land,
    Creature,
    Planeswalker,
    Artifact,
    #[default]
    Instant,
    Sorcery,
    Enchantment,
}


/// Derive the color identity of a card from its mana cost string and explicit
/// per-color override flags (used for cards whose cost doesn't reflect their color,
/// e.g. Force of Will alternative-cost pitch cards).
pub(crate) fn parse_colors(mana_cost: &str, blue: bool, black: bool) -> Vec<Color> {
    let mut colors = Vec::new();
    if mana_cost.contains('W') { colors.push(Color::White); }
    if mana_cost.contains('U') || blue { colors.push(Color::Blue); }
    if mana_cost.contains('B') || black { colors.push(Color::Black); }
    if mana_cost.contains('R') { colors.push(Color::Red); }
    if mana_cost.contains('G') { colors.push(Color::Green); }
    colors
}

// ── Trigger check functions (one per trigger-having card) ─────────────────────

/// ETB trigger for Recruiter of the Guard: search library for a creature with toughness ≤ 2,
/// put it into hand. CR 700.3 (search), CR 701.14 (reveal — not modeled; card goes to hand).
/// ETB trigger for Atraxa, Grand Unifier: placeholder — adds 4 cards to hand.
/// TODO: replace with real reveal-top-10-by-card-type once hands are fully tracked.
/// Check all triggers for the given event.
/// Part 1: Card-bound triggers — walks all objects, checks trigger_defs from catalog,
///         filtered by `active_when` predicate. No instances needed.
/// Part 2: Ephemeral triggers — walks trigger_instances (runtime-created, e.g. delayed sac).
/// Part 3: CE-granted triggers — walks materialized defs for Layer 6 ability grants.
/// Returns (pending triggers, indices of OneShot trigger instances that fired).
/// The caller must remove the OneShot instances from `state.trigger_instances`.
pub(crate) fn fire_triggers(event: &GameEvent, state: &SimState) -> (Vec<TriggerContext>, Vec<usize>) {
    let mut pending: Vec<TriggerContext> = Vec::new();

    // Part 1: Card-bound triggers derived from catalog definitions.
    for (id, obj) in &state.objects {
        let card_def = match state.catalog.get(&obj.catalog_key) {
            Some(d) => d,
            None => continue,
        };
        for tdef in &card_def.trigger_defs {
            if (tdef.active_when)(*id, state) {
                (tdef.check)(event, *id, obj.controller, state, &mut pending);
            }
        }
    }

    // Part 2: Ephemeral trigger instances (runtime-created by abilities).
    // Track OneShot instances that fire so the caller can remove them.
    let mut one_shot_fired: Vec<usize> = Vec::new();
    for (i, inst) in state.trigger_instances.iter().enumerate() {
        let before = pending.len();
        (inst.check)(event, inst.source_id, inst.controller, state, &mut pending);
        if pending.len() > before && inst.expiry == Some(Expiry::OneShot) {
            one_shot_fired.push(i);
        }
    }

    // Part 3: CE-granted triggers from materialized CardDefs (Layer 6 ability grants).
    let bf_ids: Vec<(ObjId, PlayerId)> = state.objects.iter()
        .filter(|(_, o)| matches!(o.zone(), Some(Zone::Battlefield)) && o.materialized.is_some())
        .map(|(id, o)| (*id, o.controller))
        .collect();
    for (obj_id, controller) in bf_ids {
        if let Some(mat) = state.objects.get(&obj_id).and_then(|o| o.materialized.as_ref()) {
            for check in &mat.granted_trigger_defs {
                (check)(event, obj_id, controller, state, &mut pending);
            }
        }
    }

    // Part 4: IR-based triggered abilities from CardDef.abilities.
    // Each Triggered ability declares an `active_zone` — the source must be
    // in that zone for the trigger to be armed. Default Battlefield; Stack
    // for self-triggering spell abilities (storm, cascade, "when you cast").
    let all_ids: Vec<(ObjId, PlayerId, String, crate::ir::expr::ZoneKindSel)> = state
        .objects
        .iter()
        .filter_map(|(id, o)| {
            // `zone()?` skips players (zoneless) — they carry no triggered abilities.
            let zk = match o.zone()? {
                Zone::Battlefield => crate::ir::expr::ZoneKindSel::Battlefield,
                Zone::Stack => crate::ir::expr::ZoneKindSel::Stack,
                Zone::Graveyard => crate::ir::expr::ZoneKindSel::Graveyard,
                Zone::Exile { .. } => crate::ir::expr::ZoneKindSel::Exile,
                Zone::Hand { .. } => crate::ir::expr::ZoneKindSel::Hand,
                Zone::Library => crate::ir::expr::ZoneKindSel::Library,
            };
            Some((*id, o.controller, o.catalog_key.clone(), zk))
        })
        .collect();
    for (obj_id, controller, key, obj_zone) in all_ids {
        let Some(card_def) = state.catalog.get(&key) else { continue };
        for ability in &card_def.abilities {
            fire_ir_triggered(
                ability, obj_id, controller, obj_zone, card_def.name.clone(),
                event, state, &mut pending, true,
            );
        }
    }

    // Part 5: IR abilities granted by continuous effects (`CEMod::GrantAbility`),
    // read from materialized defs of active battlefield objects — the IR analog
    // of Part 3. The grantee is the source of the granted trigger.
    let granted_ids: Vec<(ObjId, PlayerId)> = state.objects.iter()
        .filter(|(_, o)| matches!(o.zone(), Some(Zone::Battlefield)) && o.materialized.is_some())
        .map(|(id, o)| (*id, o.controller))
        .collect();
    for (obj_id, controller) in granted_ids {
        let granted: Vec<crate::ir::ability::Ability> = match state.objects.get(&obj_id)
            .and_then(|o| o.materialized.as_ref())
        {
            Some(mat) => mat.granted_abilities.clone(),
            None => continue,
        };
        let name = state.objects.get(&obj_id).map(|o| o.catalog_key.clone()).unwrap_or_default();
        for ability in &granted {
            fire_ir_triggered(
                ability, obj_id, controller, crate::ir::expr::ZoneKindSel::Battlefield,
                name.clone(), event, state, &mut pending, true,
            );
        }
    }

    // Part 6: IR triggered abilities on emblems. Emblems live in the command
    // zone side-list rather than `state.objects`, so their printed active-zone
    // marker is not meaningful. They otherwise use the same trigger matcher,
    // target selection, and resolution plumbing as card-bound IR triggers.
    for emblem in &state.emblems {
        for ability in &emblem.abilities {
            fire_ir_triggered(
                ability, emblem.id, emblem.controller,
                crate::ir::expr::ZoneKindSel::Battlefield,
                "Emblem".to_string(), event, state, &mut pending, false,
            );
        }
    }

    (pending, one_shot_fired)
}

/// Fire one IR `Triggered` ability for `obj_id` (its source) against `event`,
/// pushing a `TriggerContext` if it matches. Shared by Part 4 (abilities printed
/// on the card) and Part 5 (abilities granted by a CE), so the bind-capture and
/// effect-build logic lives in one place. Non-`Triggered` kinds and zone
/// mismatches are skipped.
fn fire_ir_triggered(
    ability: &crate::ir::ability::Ability,
    obj_id: ObjId,
    controller: PlayerId,
    obj_zone: crate::ir::expr::ZoneKindSel,
    source_name: String,
    event: &GameEvent,
    state: &SimState,
    pending: &mut Vec<TriggerContext>,
    enforce_source_zone: bool,
) {
    let crate::ir::ability::AbilityKind::Triggered { spec, target_spec, body, active_zone } =
        &ability.kind
    else {
        return;
    };
    if enforce_source_zone && *active_zone != obj_zone {
        return;
    }
    let Some(match_env) = crate::ir::executor::match_trigger(spec, event, obj_id, controller, state)
    else {
        return;
    };
    // Capture bindings from the matched event (e.g. triggered_obj,
    // triggered_mana_spent) so the body can reference them on resolution.
    let match_bindings: Vec<(&'static str, crate::ir::expr::Value)> =
        match_env.bindings.iter().map(|(k, v)| (*k, v.clone())).collect();
    // Carry the triggering event itself so `Ctx::Triggering` projections resolve
    // at the ability's (deferred) resolution.
    let trig_event = event.clone();
    let body = body.clone();
    let effect = Effect(std::sync::Arc::new(move |state: &mut SimState, _t, targets: &[ObjId]| {
        use crate::ir::executor::{execute, BindEnv};
        use crate::ir::expr::Value;
        let mut env = BindEnv::new()
            .with_source(obj_id)
            .with_controller(controller)
            .with_triggering_event(trig_event.clone());
        for (k, v) in &match_bindings {
            env = env.with_var(k, v.clone());
        }
        if let Some(&tgt) = targets.first() {
            let v = if tgt == state.us_id || tgt == state.opp_id {
                Value::Player(state.who_pid(tgt))
            } else {
                Value::Obj(tgt)
            };
            env = env.with_var("target", v);
        }
        let _ = execute(&body, state, &env);
    }));
    // "Another target X" is expressed by excluding the ability's own source from
    // every object filter in the spec. Harmless for specs that already exclude self.
    let target_spec = crate::predicates::exclude_from_target_spec(target_spec, obj_id);
    pending.push(TriggerContext { source_name, controller, target_spec, effect });
}

/// Push a vec of `TriggerContext`s onto the stack as triggered ability items.
/// Target selection goes through the controller's strategy (CR 601.2c).
pub(crate) fn push_triggers(triggers: Vec<TriggerContext>, state: &mut SimState) {
    for ctx in triggers {
        let all_targets = legal_targets(&ctx.target_spec, ctx.controller, ObjId(0), state);
        // AlwaysPass::choose_targets falls back to pick_targets, matching the
        // old `unwrap_or_else(pick_targets)`.
        let chosen_targets = state.with_strategy(ctx.controller, |s, st|
            s.choose_targets(st, ObjId(0), &all_targets, &ctx.target_spec));
        let ab_id = state.alloc_id();
        state.insert_stack_ability(ab_id, ctx.source_name.clone(), ctx.controller, crate::AbilityState {
            effect: ctx.effect.clone(),
            chosen_targets,
            costs_paid_ctx: CostsPaidCtx::default(),
            is_triggered: true,
            counterable: true,
            choice_spec: None,
        });
    }
}




/// Build an `Effect` closure for an activated ability at push time.
pub(crate) fn build_ability_effect(
    ability: &AbilityDef,
    who: PlayerId,
    source_id: ObjId,
) -> Effect {
    if let Some(body) = &ability.ir_body {
        let body = body.clone();
        return Effect(std::sync::Arc::new(move |state, _t, targets| {
            use crate::ir::executor::{execute, BindEnv};
            use crate::ir::expr::Value;
            let mut env = BindEnv::new()
                .with_source(source_id)
                .with_controller(who);
            if let Some(&tgt) = targets.first() {
                let v = if tgt == state.us_id || tgt == state.opp_id {
                    Value::Player(state.who_pid(tgt))
                } else {
                    Value::Obj(tgt)
                };
                env = env.with_var("target", v);
            }
            let _ = execute(&body, state, &env);
        }));
    }
    if let Some(factory) = &ability.ability_factory {
        return factory(who, source_id);
    }
    // No factory — no-op (e.g. a loyalty ability that only adjusts loyalty counters).
    Effect(std::sync::Arc::new(|_state, _t, _targets| {}))
}

/// Build a `(TargetSpec, Effect)` for a spell at cast time.
///
/// For spells with modes: uses the chosen mode's factory and target spec.
/// For non-spell cards (permanents): returns `eff_enter_permanent`.
pub(crate) fn build_spell_effect(
    def: &CardDef,
    who: PlayerId,
    source_id: ObjId,
    chosen_x: u32,
    chosen_mode: usize,
) -> (TargetSpec, Effect) {
    // IR path: a CardDef may carry a data-based resolution body.
    for a in &def.abilities {
        if let crate::ir::ability::AbilityKind::OnResolve { modes } = &a.kind {
            if let Some(mode) = modes.get(chosen_mode) {
                let body = mode.body.clone();
                let eff = Effect(std::sync::Arc::new(move |state, _t, targets| {
                    use crate::ir::executor::{execute, BindEnv};
                    use crate::ir::expr::Value;
                    let mut env = BindEnv::new()
                        .with_source(source_id)
                        .with_controller(who)
                        // The announced X for this spell (CR 601.2b), so resolution
                        // bodies can read it as `Ctx::Var("x")` (e.g. Meltdown's MV ≤ X).
                        .with_var("x", Value::Num(chosen_x as i64));
                    if let Some(&tgt) = targets.first() {
                        let v = if tgt == state.us_id || tgt == state.opp_id {
                            Value::Player(state.who_pid(tgt))
                        } else {
                            Value::Obj(tgt)
                        };
                        env = env.with_var("target", v);
                    }
                    let _ = execute(&body, state, &env);
                }));
                return (mode.target_spec.clone(), eff);
            }
        }
    }
    // Legacy path
    if let Some(mode) = def.spell_modes().and_then(|m| m.get(chosen_mode)) {
        return (mode.target_spec.clone(), (mode.factory)(who, source_id, chosen_x));
    }
    (TargetSpec::None, eff_enter_permanent(who, def.name.clone()))
}


/// Pre-register instances for a card object at simulation init.
// ── Grafdigger's Cage creature-entry check ───────────────────────────────────

/// Matches any ZoneChange where a card moves from a graveyard or library to the battlefield.
/// Zone-direction predicate (no creature-type check); useful as a building block for cards
/// that restrict or replace all GY/library → BF transitions.
#[allow(dead_code)]
pub(crate) fn cage_creature_entry_check(event: &GameEvent, _source_id: ObjId, _controller: PlayerId) -> Option<Vec<ObjId>> {
    if let GameEvent::ZoneChange { id, from: ZoneId::Graveyard | ZoneId::Library, to: ZoneId::Battlefield, .. } = event {
        Some(vec![*id])
    } else {
        None
    }
}

#[cfg(test)]
mod added_mana_tests {
    use crate::{build_catalog, Color};

    #[test]
    fn dark_ritual_adds_bbb() {
        let cat = build_catalog();
        let dr = cat.get("Dark Ritual").expect("Dark Ritual in catalog");
        let m = dr.added_mana_on_resolve().expect("Dark Ritual is a ritual");
        assert_eq!(m.count, 3);
        assert_eq!(m.colors, Some(vec![Color::Black, Color::Black, Color::Black]));
    }

    #[test]
    fn non_ritual_spells_and_lands_add_no_resolve_mana() {
        let cat = build_catalog();
        // A cantrip resolves to a draw, not mana.
        assert!(cat.get("Ponder").unwrap().added_mana_on_resolve().is_none());
        // A land has no OnResolve body at all.
        assert!(cat.get("Underground Sea").unwrap().added_mana_on_resolve().is_none());
        // Lotus Petal makes mana via an activated sac ABILITY, not on resolve.
        assert!(cat.get("Lotus Petal").unwrap().added_mana_on_resolve().is_none());
    }

    #[test]
    fn surveil_duals_enter_tapped_abu_duals_dont() {
        let cat = build_catalog();
        // MKM surveil dual — always enters tapped.
        assert!(cat.get("Undercity Sewers").unwrap().enters_tapped());
        // ABU dual + basic — enter untapped.
        assert!(!cat.get("Underground Sea").unwrap().enters_tapped());
        assert!(!cat.get("Swamp").unwrap().enters_tapped());
    }
}
