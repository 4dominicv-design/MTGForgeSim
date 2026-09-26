#![allow(dead_code)]
//! MTG Intermediate Representation (IR).
//!
//! A data-based DSL for expressing MTG card behavior as pure data, interpreted
//! by a generic executor. See `../DESIGN.org` for the engine design rationale.
//!
//! Sub-languages:
//! - `expr` — pure queries over game state (no side effects)
//! - `action` — state mutations, dispatched by the executor
//! - `ability` — wrappers (triggered / replacement / prohibition / static / activated)
//! - `ce` — continuous-effect modifications, composition primitive
//! - `context` — pointers into current event / cast / triggering frame
//! - `event_log` — append-only record of game events (Layer B)
//! - `executor` — interprets actions, evaluates expressions, matches filters

pub mod expr;
pub mod action;
pub mod ability;
pub(crate) mod ce;
pub(crate) mod context;
pub(crate) mod event_log;
pub mod executor;
pub mod cost;
pub mod cost_exec;

#[cfg(test)]
mod tests;
