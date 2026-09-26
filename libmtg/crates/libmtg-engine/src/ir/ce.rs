//! Continuous-effect modifications. Data replaces the legacy
//! `Arc<dyn ContinuousModFn>` closure used by `recompute`.
//!
//! Also doubles as the composition primitive for cast-modifying effects
//! (flashback, cascade, madness, Snapcaster, Dauthi Voidwalker) — any
//! "cast-from-elsewhere / pay-an-alt-cost / gain-permissions" lives here and
//! is consumed by `Action::OfferCast { permissions }`.

use crate::ir::expr::{Expr, ZoneKindSel};
use crate::{CardType, Color, Keyword};

pub(crate) use crate::catalog::BasicLandType;

/// Alternative cost spec — what you pay instead of the card's mana cost.
#[derive(Clone)]
pub enum CostSpec {
    Free,
    Mana(&'static str),           // "{2}{U}"
    ExileFromGraveyard { n: Expr, filter: crate::ir::expr::Filter },
    Delve,                        // CR 702.66
    Other(&'static str),          // named cost paid by engine
}

/// Continuous-effect modification. Each variant corresponds to a CR 613 layer
/// or (for the permission family) a cost/rule override consumed by
/// `Action::OfferCast`.
#[derive(Clone)]
pub enum CEMod {
    // ── layer 1 (copy) ───────────────────────────────────────────────────
    CopyOf(Expr), // characteristics copy of target

    // ── layer 4 (type) ───────────────────────────────────────────────────
    OverrideTypes(Vec<CardType>),
    AddType(CardType),
    AddSubtype(String),
    RemoveSubtype(String),
    /// "~ is a [P]/[T] [subtypes] creature [with keywords]" (CR 613.1c layer 4 +
    /// the P/T it sets). The animate-as-creature primitive: makes the object a
    /// creature with the given base P/T, creature subtypes, and keywords, carrying
    /// its existing activated abilities over (loyalty abilities stay usable). Used
    /// for a planeswalker that becomes a creature (Kaito) — per the CR ruling it
    /// stops being a planeswalker while animated.
    BecomeCreature {
        power: Expr,
        toughness: Expr,
        subtypes: Vec<String>,
        keywords: Vec<Keyword>,
    },
    /// CR 305.6: set the land's subtype to a basic land type. Replaces all
    /// existing land subtypes, swaps the intrinsic mana ability, and — per
    /// CR 305.7 — strips all abilities generated from rules text. Applies to
    /// nonbasic lands (scope filter handled by the enclosing `Static` block).
    SetBasicLandType(BasicLandType),
    /// CR 305.6: "is a <type> in addition to its other land types." Adds the
    /// basic land subtype and its intrinsic mana ability without displacing
    /// existing types or abilities (Urborg / Yavimaya). Idempotent — re-adding
    /// the same type is a no-op on the modifier side.
    AddBasicLandType(BasicLandType),

    // ── layer 5 (color) ──────────────────────────────────────────────────
    SetColors(Vec<Color>),
    /// Add the color the `Expr` evaluates to (CR 105 / 613.4). An `Expr` rather
    /// than a literal `Color` so a runtime-chosen color works: Painter's Servant
    /// adds `ChosenColor(Source)`; a fixed grant uses `ColorLit(c)`.
    AddColor(Expr),

    // ── layer 6 (abilities) ──────────────────────────────────────────────
    AddKeyword(Keyword),
    RemoveKeyword(Keyword),
    /// Grants a static/triggered ability while the CE is in force.
    GrantAbility(Box<crate::ir::ability::Ability>),
    /// "Can't be countered" (CR 701.5). Prohibits `SpellBeingCountered`
    /// events targeting the scoped spell. Typically paired with
    /// `Action::GrantCEToNextSpellCast` to apply on a not-yet-cast spell.
    Uncounterable,

    // ── layer 7 (P/T) ────────────────────────────────────────────────────
    PumpPT(Expr, Expr),     // +P/+T (or -/-)
    SetPT(Expr, Expr),      // absolute override
    SetPower(Expr),
    SetToughness(Expr),

    // ── protection / prohibitions as CE ──────────────────────────────────
    SetProtection(Expr),    // from <value>
    CantAttack,
    CantBlock,
    CantBeTargeted(crate::ir::expr::Filter),

    // ── rule modifiers (game-level) ──────────────────────────────────────
    AllowLoss(Expr),        // Platinum Angel: "you can't lose" when true
    MaxHandSize(Expr),
    /// "You have no maximum hand size" (e.g. Tamiyo −7 emblem). A player-level
    /// rule override carried as an emblem's static ability and consulted by the
    /// cleanup discard check; produces no object-CE.
    NoMaxHandSize,
    ExtraLandDrops(Expr),
    SkipStep(crate::StepKind),

    // ── permissions / cost overrides (consumed by OfferCast) ─────────────
    CastableFrom(ZoneKindSel),
    AltCost(CostSpec),
    AnyColorMana,
    GrantFlash,
    /// After resolution, exile this spell instead of normal rules.
    OnResolveExile,
    CastingCostPlus(Expr), // +N generic to cast cost

    // ── cost-to-play modifiers ───────────────────────────────────────────
    SpellsCostMore {
        filter: crate::ir::expr::Filter,
        amount: Expr,
    },
    SpellsCostLess {
        filter: crate::ir::expr::Filter,
        amount: Expr,
    },
}
