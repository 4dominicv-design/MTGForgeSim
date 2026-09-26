# Forge v4.1 compile fixes

Fixes made from the first Windows `cargo test -p libmtg-engine` run:

- Escaped `{3}` in the Basalt Monolith assertion format string.
- Replaced legacy target-controller `Who::You` with `Who::Actor` for Manifold Key and the Tezzeret emblem target specs.
- Corrected adventure/back-face casting-cost calculation so `apply_casting_cost_rules` receives `&CardDef`, not `&Option<&CardDef>`.
- Removed the unused local `CostBody` and `IrSpellMode` imports reported by the compiler.

Recommended next commands:

```powershell
cargo test -p libmtg-engine
cargo test -p libmtg-forge
```
