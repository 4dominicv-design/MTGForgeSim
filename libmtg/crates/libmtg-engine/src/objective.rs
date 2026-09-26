//! Simulation objectives — when does a run end, and what does it capture.
//!
//! An [`Objective`] observes the game's event stream (consulted by
//! [`crate::fire_event`] for every event that actually happens, post-prohibition
//! and post-replacement) and decides when the simulation terminates. This
//! replaces the former `Action::EndSimulation` / `SimState.success` sentinel —
//! termination is an *application* concern, not an engine effect.
//!
//! Concrete objectives (e.g. the Doomsday apps terminating on Doomsday
//! *resolving*) are content and live in their own crates; the engine only owns
//! this trait. A future "proper" full game would terminate on a `GameOver` event
//! through the very same primitive, with no special-casing.

use crate::{GameEvent, SimState};

/// Decides simulation termination off the event stream. `observe` is called for
/// every event that fires; returning `true` ends the simulation. Implementors
/// may mutate state to record app-specific capture (e.g. pre-resolution life).
pub trait Objective {
    fn observe(&mut self, event: &GameEvent, state: &mut SimState) -> bool;
}
