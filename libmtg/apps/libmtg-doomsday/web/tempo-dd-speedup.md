# Assessing Marginal speed-ups for Tempo Doomsday

A variant of Doomsday that has been consistently putting up results since the printing of Flow State is the tempo list (link https://www.mtggoldfish.com/deck/7813548#paper) built by Eureka22422. With slots getting tighter and tigher, that list notably contains only 3 Ponder to make room for the third Wasteland. This "heresy" got me thinking about the effect of the 4th Ponder and was a large part of my motivation for building the doomsday goldfish simulator (link). In this article, I will attempt to analyze the effect of the 4th ponder, as well as the potential speedup benefit from cutting the Wastelands for more "turbo" style cards like Personal Tutor.

The doomsday goldfish simulator allows us to understand how likely various configurations of the deck are to assemble a T3 doomsday when the pilot decides to dig as hard as possible for the doomsday. This is meant to model a matchup like Boros Energy where we need to cast the Doomsday before the cats tear us to shreds. We will use the goldfish sim to build up our intution about the generic effects of tweaking the deck and to try to directly compare the concrete proposed configurations.

---

## How to read these numbers

- **Metric:** `P(cast Doomsday by turn 3)` — the chance a solitaire game assembles and
  casts Doomsday by T3.
- **Mulligan mode:** all numbers below use `Realistic` — a player-faithful
  plan-coverage rule (keep a hand iff it has a viable gameplan, like a fast flipped tamiyo or cantrips and interaction, not just "fast" hands). How the
  mulligan strategy *itself* changes the numbers is a separate question for another
  article.
- **All runs:** 5000–10000 goldfish games on variants of post Flow State tempo Doomsday (tight-margin questions re-run at 10000).

---

## Question 1 — Ponder vs. Wasteland

Trade Ponder (premier cantrip) for Wasteland (utility land), total held at 6.

| Build         | P(DD<=T3) |
|---------------|--------------|
| 2 Pon / 4 Wst | 61.6% |
| 3 Pon / 3 Wst | 61.8% |
| 4 Pon / 2 Wst | 61.5% |

- Per-card slope: **≈ flat** — all three land at ~61.6% (10000 games); the Ponder count barely moves the T3 rate.

**Takeaway:** The marginal selection of the 4th ponder does not substantially improve the probability of a T3 doomsday. It can, of course, still be optimal to play 4 Ponder for other harder to quantify benefits like cantripping into lands, enabling Flow State, etc.

If this result is surprising, check out [The London Ponder Paradox](../london-ponder-paradox-6-22-26/).

---

## Question 2 — Effect of more Lotus Petals

Vary the total number of Lotus Petals in the deck, swapping for cards that don't affect fast doomsdays, like interaction pieces.

| Lotus Petals       | P(DD<=T3) |
|---------|--------------|
| 0 | 56.8%  |
| 1 | 59.8%  |
| 2 | 62.1% |
| 3 | 63.3% |
| 4 | 65.4% |

- Per-card slope: **≈ +2% / Petal**.

**Takeaway:** Marginal fast mana helps us much more than Ponders to get to Doomsday+BBB before T3, and does meaningfully increase the speed of the deck.

---

## Question 3 — What about Personal Tutor?

Now suppose we cut wasteland and go back to a more traditional 4 Ponder. We could use those extra slots for more interaction, or we can use them to try to speed up the deck. Supposing we want to speed up the deck to help tempo doomsday take on the "turbo" role a little more readily, what is the best use of those two slots? Let's compare two plausible options: lotus petal for more black mana and Personal Tutor for more shots at Doomsday.

The deck's 2 stock Petals are always kept; the table varies the **total** Petal and PT counts as the two ex-Wasteland slots are filled.

| Flex fill            | Total Petal / PT | P(DD<=T3) |
|----------------------|------------------|----------|
| 2 Wasteland (stock)  | 2 Petal / 0 PT   | 62.2%  |
| +2 Petal             | 4 Petal / 0 PT   | 65.8%  |
| +1 Petal +1 PT       | 3 Petal / 1 PT   | 68.0%  |
| +2 PT                | 2 Petal / 2 PT   | 69.2%  |

- Slopes here: **≈ +3.5% / PT** vs **≈ +1.8% / Petal** (still ~additive: 1/1 predicts 67.5, measures 68.0)

**Takeaway:** On pure speed, Personal Tutor yields almost double the effect of Lotus Petal. This is not too surprising, as the deck already has more mana (lands, rituals, and petals) than Doomsdays, so Doomsdays ought to be the main limiting factor.

---

## Conclusion

If optimizing for straightline speed, Personal Tutor has the highest gain of the available options. The 4th Ponder has a marginal impact on pure speed. My personal conclusion is that Tempo Doomsday should forgo the ~7% boost to the probability of a <=T3 DD to play Wasteland, which synergizes heavily with the deck's tempo gameplans and pivots. Wasteland also has non-trivial applications tapping for mana to crack clues, cast Flow States and Barrowgoyfs, etc.

## Method footnotes _(optional, for the curious)_

- More information on the simulator, goldfish strategy, etc. can be found at bur.io/doomsday/goldfish and in the code repo at https://github.com/boryas/libmtg
