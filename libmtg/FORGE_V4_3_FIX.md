# Forge v4.3 fix

Fixes Karn, the Great Creator -2 filtering owned cards in exile.

`SimState::def_of()` intentionally returns only a materialized/current `CardDef`. Exile objects can legitimately have `materialized = None` before the first recompute (including wishboard cards preloaded into exile and directly-constructed test states). Karn therefore now checks the current materialized definition when present and falls back to the object's printed catalog definition.

This preserves current-characteristic behavior while correctly allowing Karn to retrieve artifacts from the unified exile pool, including a One Ring exiled during the game.
