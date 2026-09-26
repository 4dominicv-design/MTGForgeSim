# Forge evaluator v2

The v2 rollout evaluator fixes the short-horizon bias where passing could score
better than deploying an artifact simply because the current mana remained
untapped and the card stayed in hand.

Key changes:

- current `potential_mana` weight reduced from 0.55 to 0.10;
- generic hand-size weight reduced from 0.45 to 0.18;
- added approximate next-turn mana infrastructure, ignoring ordinary tapped
  lands because they will untap normally;
- Monoliths are treated specially because they do not naturally untap;
- artifact count and metalcraft thresholds are rewarded;
- Forge/Key/Monolith/Ring/Paradox synergies are explicitly represented;
- Giant's Boulder is treated as a mana filter, not extra mana;
- Mystic Forge top-card access receives a larger virtual-card bonus;
- every training record is stamped `forge_eval_v2` and includes
  `next_turn_mana`, `artifact_count`, and `metalcraft`;
- the LightGBM script rejects mixed/old evaluator labels.

Do not append v2 examples to an existing v1 JSONL dataset. Generate a fresh
training file after upgrading.
