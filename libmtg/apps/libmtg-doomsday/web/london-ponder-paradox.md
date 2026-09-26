# The London Ponder Paradox

*A companion to [Assessing marginal speed-ups for Tempo Doomsday](../tempo-dd-speedup-6-22-26/).*

In the previous analysis, we reached a surprising conclusion: exchanging copies of Ponder and Wasteland between copies 2-4 turned out to have no discernable effect on the observed probability of casting Doomsday by turn 3. This is a huge surprise, as Ponder is the single most impactful cantrip when hunting for a particular card (like Doomsday!) and Wasteland is borderline dead for casting Doomsday (it can cycle to Edge of Autumn or pay for Flow State). So how could a deck with 4 Ponder and 2 Wasteland be equally good at casting T3 Doomsdays as a deck with 2 Ponder and 4 Wasteland?!

The answer is the London mulligan, and our simulator's mulligan strategy's respect for Ponder.

## The raw deck *does* care

I re-ran the same three builds under a trivial mulligan strategy, **Keep7**, which keeps every opening seven no matter what:

| Build | Keep7 P(DD<=T3) |
|---------------|--------------|
| 2 Pon / 4 Wst | 50.2% |
| 3 Pon / 3 Wst | 51.6% |
| 4 Pon / 2 Wst | 53.1% |

About **+1.5% per Ponder**. So selection is genuinely worth something: the extra Ponders dig you to Doomsday faster when you are forced to play whatever you are dealt. The intuition that Ponder should help dig is vindicated, and we confirm that the strategy really is able to take advantage of Ponders to dig for Doomsdays. A Ponder isn't quite as good as a Personal Tutor, but it is very helpful.

## The mulligan erases the speed-up of Ponder

Now turn the mulligan strategy back on (**Realistic** = keep only hands with a viable plan):

| Build | Keep7 | Realistic | mulligan lift |
|---------------|------|------|------|
| 2 Pon / 4 Wst | 50.2% | 61.5% | **+11.3** |
| 3 Pon / 3 Wst | 51.6% | 61.9% | **+10.3** |
| 4 Pon / 2 Wst | 53.1% | 60.9% | **+7.8** |

Two things jump out. The mulligan adds a lot (~+10 points). And the lift it adds *shrinks* as the deck gets better at finding Doomsday — **+11.3 for the slowest deck, +7.8 for the fastest**.

## We mulligan the hands with wastelands!

The mechanism shows up in three numbers.

**Air-heavy hands get thrown back.** Wasteland is considered "air" in our hand evaluator, and contributes to flood-type mulligans (similar to excess colored mana sources or Doomsday win-cons)

| Build | air in kept 7 | air in mulliganed 7 |
|---------------|------|------|
| 2 Pon / 4 Wst | 0.89 | 1.38 |
| 3 Pon / 3 Wst | 0.78 | 1.22 |
| 4 Pon / 2 Wst | 0.72 | 1.04 |

So as we increase the count of Wastelands, our mulligans tend to contain more air on average.

**The air-heavy deck mulligans more.** Because it opens more air, the 2-Ponder build keeps only 65.7% of its sevens while the 4-Ponder build keeps 70.6%. The slower wasteland heavy deck throws back more hands, and each mulligan is another chance to find a plan.

| Build | keeps at 7 | at 6 | at 5 | at 4 |
|---------------|------|------|------|------|
| 2 Pon / 4 Wst | 65.7% | 23.7% | 7.6% | 3.0% |
| 4 Pon / 2 Wst | 70.6% | 21.2% | 6.4% | 1.8% |

**The hands that survive are equally fast.** The P(cast by T3) of a *kept* hand barely moves across the three decks — about 62% whether the deck runs 2 or 4 Ponder:

| Build | P(cast) of a keep |
|---------------|--------------|
| 2 Pon / 4 Wst | 62.5% |
| 3 Pon / 3 Wst | 62.9% |
| 4 Pon / 2 Wst | 61.6% |

The keep bar is a quality floor, and every deck mulligans until it clears it. As a result, the decks differ in how often they have to mulligan, not in how fast the keeps are. We are likely enough to find a functionally equivalent 5 (most hands have a couple cards that don't end up contributing to finding or casting the Doomsday) that this doesn't actually reduce our chance of a Doomsday by T3.

## Future Work
Since there is no discernable effect on speed from Ponders with Realistic mulligans, the place we would expect them to really shine is in hand *quality* when we Doomsday. Sure, we can paper over our Ponders with London mulligans, but we are much less likely to still have that Force of Will to back up the Doomsday if we had to mull to 5 to find it.

If we can expand the simulator to try to also optimize for and measure the valid interaction it layered in with the Doomsday presented, then we are likely to see the beneficial effects of Ponder. The naive treatment bolted on to the sim currently which doesn't really optimize for keeping interaction does not yet show this effect (protection counts are relatively flat across the builds).

## Method footnotes

- Goldfish = solitaire Monte-Carlo, 10k games per cell.
- "Air" = cards that contribute to no realistic plan in the keep logic; Extra mana, LED, Edge of Autumn, Jace, Thassa, Wasteland, etc.
- Simulator and code: bur.io/doomsday/goldfish, github.com/boryas/libmtg
