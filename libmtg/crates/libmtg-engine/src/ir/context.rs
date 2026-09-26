//! Context pointers: the "who / what event / what cast" that an ability or
//! action is running in the scope of.
//!
//! Unifies per-instance context frames and event-log lookups as projections
//! off the same structured event record.

/// Runtime-bound pointers. Evaluated against the current `BindEnv`.
#[derive(Debug, Clone)]
pub enum Ctx {
    /// The source object of the ability/spell — the object that "has" this IR.
    Source,
    /// "You" — controller of the source.
    Controller,
    /// "It" / "that creature" — the most recent bind from `bind_as`,
    /// or in filter bodies, the candidate under test.
    It,
    /// User-named binding (from `Action::*.bind_as` or `Expr::Let`).
    Var(&'static str),
    /// Property of the event that triggered this ability.
    /// Valid only inside triggered-ability bodies.
    Triggering(EventField),
    /// Property of this spell's own `SpellCast` event.
    /// Valid only inside spell effects.
    ThisCast(EventField),
}

/// Field projections off a logged game event.
///
/// Flat enum — the engine resolves each field against the event pointed to by
/// the enclosing `Ctx::Triggering(_)` or `Ctx::ThisCast(_)`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventField {
    // spell-cast properties
    ManaSpent,
    AltCost,
    DelvedExiled,
    SacrificedToCast,
    TappedToCast,
    DiscardedToCast,
    X,
    ModesChosen,
    TargetsDeclared,

    // damage event
    DamageAmount,
    DamageSource,
    DamagedObject,
    DamageIsCombat,

    // draw / turn event
    DrawIndexInDrawStep,

    // zone-change event
    ZoneFrom,
    ZoneTo,
    ObjMoved,

    // death trigger
    DyingCreature,

    // ETB
    EtbChoice,
}

/// Layer C — game-level designations and flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GameCtx {
    Monarch,
    Initiative,
    DayNight,
    CityBlessing,
    RingTempted,
    /// The spell currently being cast (`state.casting_spell`), as an object —
    /// `ObjId::UNSET` when nothing is being cast. Used by cast-gated mana
    /// abilities (Cavern of Souls: colored mana only for a creature spell).
    CastingSpell,
}
