# libmtg

A generic, reusable rules engine for Magic: The Gathering.

This repository is a Cargo workspace. The headline crate is **`libmtg-engine`** —
a standalone engine with a public state/decision API (no game-specific strategy,
no database, no UI). The other members are a portable decklist library and the
Doomsday apps, and the **`libmtg`** umbrella crate re-exports them under one
namespace.

## Crates

| Crate | What it is |
|-------|------------|
| [`libmtg-engine`](crates/libmtg-engine) | The generic engine: cards, zones, mana, the stack, replacement/trigger/continuous infrastructure, and a public API (`SimState`, `Strategy`, `Objective`, `run_game`) for driving game state and decisions. Card behavior is data in a small IR. Depends on none of the others. |
| [`libmtg-decklist`](crates/libmtg-decklist) | Portable text/URL decklists, independent of the engine. The text parser is wasm-safe; a native-only `fetch` feature resolves Moxfield / MTGGoldfish URLs with an on-disk cache. |
| [`libmtg-doomsday`](apps/libmtg-doomsday) | The Doomsday deck: strategy, planner, and objective content, plus the apps — a Monte-Carlo goldfish simulator (`dd-goldfish` CLI + web) and a pile-scenario builder (web). |
| [`libmtg`](crates/libmtg) | Umbrella crate that re-exports the three above as `libmtg::engine`, `libmtg::decklist`, and `libmtg::doomsday`. |

## How it fits together

**The engine (`libmtg-engine`) is generic and content-free.** It models the game —
objects, zones, mana, the stack, the priority loop — and exposes a state + decision
API; it knows nothing about any particular deck. Card behavior is expressed as data
in a small IR run by one generic executor, so adding a card is mostly writing data,
not code. *Who* plays, *what* decks, *how* cards are valued, and *when* a game ends
are supplied by the caller through the `Strategy` and `Objective` traits. (See
[`DESIGN.org`](crates/libmtg-engine/src/DESIGN.org) for the architecture.)

**`libmtg-doomsday` is the concrete content + apps** for one real Legacy deck
(Doomsday) and two tools for studying it:

- **goldfish** — plays thousands of solitaire games to answer *"how fast and how
  reliably does this list combo off?"* (cast-turn distribution, P(win by turn N),
  protection in hand). Available as the `dd-goldfish` CLI and a web page.
- **pile builder** — plays a single game up to the instant Doomsday resolves, then
  lets you arrange the five-card "pile" and share the exact board as a URL — for
  studying or posting specific lines.

## Using it in your own project

**Just the engine** (leanest — pulls only `rand`, `serde`, `serde_json`):

```toml
[dependencies]
libmtg-engine = { git = "https://github.com/boryas/libmtg", package = "libmtg-engine" }
```
```rust
use libmtg_engine::{build_catalog, run_game};
```

**The whole family through one namespace** (engine + decklist + doomsday):

```toml
[dependencies]
libmtg = { git = "https://github.com/boryas/libmtg", package = "libmtg" }
```
```rust
use libmtg::engine::build_catalog;
use libmtg::decklist::Decklist;
```

Pin to a `tag`, `rev`, or `branch` for reproducibility.

## Build & test

```sh
cargo build
cargo test
```

Run the goldfish CLI:

```sh
cargo run -p libmtg-doomsday --bin dd-goldfish -- --help
```

## Design docs

The engine's design lives next to its source:

- [`crates/libmtg-engine/src/DESIGN.org`](crates/libmtg-engine/src/DESIGN.org) — engine architecture and the IR primitive vocabulary.
- [`crates/libmtg-engine/src/CARD_INDEX.org`](crates/libmtg-engine/src/CARD_INDEX.org) — how MTG mechanics decompose into IR primitives.

## Web frontends

`libmtg-doomsday` compiles to a single wasm module that serves both web pages
(`web/pilegen.html` — pile builder, `web/dd-goldfish.html` — goldfish). Build it
with [`wasm-pack`](https://rustwasm.github.io/wasm-pack/):

```sh
wasm-pack build apps/libmtg-doomsday --release --target web --no-default-features
```

`--no-default-features` drops the native `cli` set (`clap` / `textplots` /
`ureq`) that doesn't build for wasm. The pages load the generated
`pkg/libmtg_doomsday.js`; serve `apps/libmtg-doomsday/web/` over HTTP to run them
locally.

Currently served at bur.io/doomsday/

## License

Licensed under the [MIT license](LICENSE-MIT).
