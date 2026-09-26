//! Append-only log of every `GameEvent`. Subsumes the scattered
//! `this_turn`-shaped counters and powers `Expr::EventLog`-style folds.
//!
//! Retention:
//! - per-turn events: pruned at cleanup step
//! - per-game events: retained until the sim resets
//!
//! Memory: trivial. Worst case ≤ ~500 events/turn.

use crate::GameEvent;

/// A logged game event with a monotonic sequence number and the turn on which
/// it occurred. Kept minimal; per-event structural fields live on `GameEvent`
/// itself and are projected out via `EventField`.
#[derive(Clone)]
pub struct LoggedEvent {
    pub seq: u64,
    pub turn: u32,
    pub event: GameEvent,
}

/// Time window for event-log queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Window {
    /// Since the current turn began.
    ThisTurn,
    /// Since the current game began.
    ThisGame,
    /// Since a named checkpoint (e.g. "before this spell resolved").
    /// Checkpoints are pushed by the engine at ability-start / resolve-start.
    SinceCheckpoint(&'static str),
}

/// The log itself. Owns its entries; queried by `filter` / `count` / `any`.
#[derive(Default, Clone)]
pub struct EventLog {
    pub(crate) entries: Vec<LoggedEvent>,
    pub(crate) next_seq: u64,
    pub(crate) turn_start_seq: u64,
    pub(crate) checkpoints: Vec<(&'static str, u64)>,
}

impl EventLog {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn push(&mut self, turn: u32, event: GameEvent) {
        let seq = self.next_seq;
        self.next_seq += 1;
        self.entries.push(LoggedEvent { seq, turn, event });
    }

    pub(crate) fn mark_turn_start(&mut self) {
        self.turn_start_seq = self.next_seq;
    }

    pub(crate) fn push_checkpoint(&mut self, name: &'static str) {
        self.checkpoints.push((name, self.next_seq));
    }

    pub(crate) fn pop_checkpoint(&mut self, name: &'static str) {
        if let Some(pos) = self.checkpoints.iter().rposition(|(n, _)| *n == name) {
            self.checkpoints.remove(pos);
        }
    }

    /// Prune per-turn events at cleanup. Kept events are whole-game; for now,
    /// retain everything — pruning heuristics are a future tuning knob.
    pub(crate) fn prune_for_new_turn(&mut self) {
        self.turn_start_seq = self.next_seq;
    }

    fn window_start(&self, w: Window) -> u64 {
        match w {
            Window::ThisTurn => self.turn_start_seq,
            Window::ThisGame => 0,
            Window::SinceCheckpoint(name) => self
                .checkpoints
                .iter()
                .rev()
                .find(|(n, _)| *n == name)
                .map(|(_, seq)| *seq)
                .unwrap_or(0),
        }
    }

    pub(crate) fn filter<'a, F>(&'a self, w: Window, mut pred: F) -> Vec<&'a LoggedEvent>
    where
        F: FnMut(&LoggedEvent) -> bool,
    {
        let start = self.window_start(w);
        self.entries
            .iter()
            .filter(|e| e.seq >= start && pred(e))
            .collect()
    }

    pub(crate) fn count<F>(&self, w: Window, mut pred: F) -> usize
    where
        F: FnMut(&LoggedEvent) -> bool,
    {
        let start = self.window_start(w);
        self.entries
            .iter()
            .filter(|e| e.seq >= start && pred(e))
            .count()
    }

    pub(crate) fn any<F>(&self, w: Window, mut pred: F) -> bool
    where
        F: FnMut(&LoggedEvent) -> bool,
    {
        let start = self.window_start(w);
        self.entries.iter().any(|e| e.seq >= start && pred(e))
    }
}
