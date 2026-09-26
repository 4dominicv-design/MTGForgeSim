//! The Doomsday application objective (concrete content, peeled out of the
//! engine). The `Objective` trait itself stays in libmtg-engine; only this
//! Doomsday-specific implementation lives here.

use libmtg_engine::{GameEvent, Objective, PlayerId, SimState};

/// dd-pilegen objective: the run ends when our Doomsday resolves. Captures
/// pre-resolution life for the "X → Y" display and applies Doomsday's
/// lose-half-life accounting. In this engine Doomsday's pile-building is
/// deferred to the human (via the web UI), so resolution is the stopping point —
/// the card body itself is a no-op and this objective owns the terminal logic.
#[derive(Default)]
pub struct DoomsdayResolvedObjective;

impl Objective for DoomsdayResolvedObjective {
    fn observe(&mut self, event: &GameEvent, state: &mut SimState) -> bool {
        if let GameEvent::SpellResolved { card_id, controller } = event {
            if *controller == PlayerId::Us
                && state.objects.get(card_id).map_or(false, |o| o.catalog_key == "Doomsday")
            {
                let life = state.player(PlayerId::Us).life;
                state.life_before_dd = Some(life);
                state.player_mut(PlayerId::Us).life = life / 2;
                return true;
            }
        }
        false
    }
}
