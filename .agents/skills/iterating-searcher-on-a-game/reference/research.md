# Research behind the process

Each entry gives the published result and the step of the process it supports.

## Starting from saved states along a route

**The backward algorithm.** Salimans and Chen, "Learning Montezuma's Revenge
from a Single Demonstration", arXiv 1812.03381 (2018). Training starts near the
end of one demonstration and moves the start point earlier each time enough
workers match the demonstration's return from there. On a cliff walk, learning
a required sequence of N actions took time that grew with N squared from
demonstration states, against 2 to the N from the start (fits of 47·N² and
6.0·2^N). This is the reason rooted segments are cheap: a hard part is learned
from a nearby good state, and its cost stops multiplying with everything before
it.

**Pokémon Red, February 2025.** The drubinstein continuation of the Pokémon
Red reinforcement learning project finished the game with a policy under 10M
parameters. It trained in curriculum stages, each seeded from save states at
known checkpoints (drubinstein.github.io/pokerl). Staging from saved states is
the same idea as rooted segments and checkpoint slices; a whole-game result
still needs the stages to connect, which is what the power-on run tests.

## Archives of states

**Go-Explore.** Ecoffet et al., "Go-Explore: a New Approach for
Hard-Exploration Problems", arXiv 1901.10995 (2019), and "First return, then
explore", Nature 590 (2021). It keeps an archive of cells, returns to a chosen
cell by restoring its state, and explores from there. Dissonance's searcher is
in this family. The Nature paper separates finding a route from making it
reliable, which matches the split between rooted segments (find what a hard
part needs) and the power-on run (carry it there in one search).

**Absplore.** Liu et al., "Learning Abstract Models for Strategic Exploration
and Fast Reward Transfer", ICML 2020, arXiv 2007.05896. It explores an
abstract graph of places and then learns which transitions between them are
reliable, rejecting arrivals that only look valid. Metroid showed the same
split: a recorded route from one state fails from another state at the same
place, so a transition has to be re-established from the state that will use
it.

## Small test worlds and their limits

**MiniGrid and MiniHack.** Chevalier-Boisvert et al., "Minigrid & Miniworld",
arXiv 2306.13831 (2023); Samvelyan et al., "MiniHack the Planet", NeurIPS 2021
Datasets and Benchmarks, arXiv 2109.13202. Both are small grid worlds used to
test exploration methods in minutes, as the tiny-worlds panel is.

**The transfer caveat.** TopoExplore, arXiv 2607.09971 (2026), added a bonus
at entrances to unexplored enclosed regions of the visited grid. It was 1.52
times faster on MiniGrid and worse on Montezuma's Revenge at 30M frames (8 to
18 rooms against 19 without the bonus), because screen features that could
never be entered became permanent targets. A small-world gain predicts a game
gain only where the world reproduces the game's mechanism, which is why the
panel filters changes and the scorecard decides when it forecasts.
