# v4.2 compile fixes

This pass addresses the compiler errors reported after v4.1:

- Restored the local `IrSpellMode` import in `flusterstorm()`.
- Replaced two `Option::map(...).unwrap_or(base)` cost branches with explicit `match` expressions so `ManaCost` is moved exactly once.
- Removed the remaining unused `CostBody` import from `grim_monolith()`.

The warnings about irrefutable `let ... else` patterns and the unrelated unused test variable are pre-existing warnings and do not block compilation.
