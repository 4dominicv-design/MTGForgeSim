//! Umbrella crate for the **libmtg** family — a reusable Magic: The Gathering
//! rules engine and the apps built on it.
//!
//! Each module re-exports an independent member crate, so you can depend on this
//! one crate and reach everything through a single namespace:
//!
//! ```ignore
//! use libmtg::engine::build_catalog;   // -> libmtg-engine
//! use libmtg::decklist::Decklist;      // -> libmtg-decklist
//! use libmtg::doomsday::run_goldfish;  // -> libmtg-doomsday
//! ```
//!
//! If you only need the engine, depend on `libmtg-engine` directly instead — that
//! avoids pulling in the Doomsday content and its dependencies.

pub use libmtg_decklist as decklist;
pub use libmtg_doomsday as doomsday;
pub use libmtg_engine as engine;
