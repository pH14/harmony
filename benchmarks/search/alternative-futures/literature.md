# Initial literature pass

Read methods and relevant ablations on 2026-09-08, within the initial 45-minute
limit. These published findings are starting hypotheses, not Harmony results.

- [First return, then explore](https://arxiv.org/html/2004.12919v6): separate
  archive selection, reliable return, and exploration. Same-cell replacement
  prefers better/shorter trajectories. Policy-based exploration discovers
  substantially more cells than random actions in its reported Atari runs,
  after learning develops. Simulator return avoids derailment but does not
  produce a policy robust to stochastic execution. Harmony already supplies
  exact return; smallest transfer is a paired local suffix experiment from
  discovered states, with return and extension costs separated.
- [Time-Myopic Go-Explore](https://arxiv.org/pdf/2301.05635): learn directed
  short temporal distances, admit candidates far from every archive entry,
  retain entries and combine score/visit weights. It adds proxy trajectories
  and local training datasets to prevent forgetting. It changes action
  repetition from mean 10 to 4; the comparison therefore does not isolate
  representation alone. Native Go-Explore is stronger on some reported
  equal-frame Atari outcomes; learned temporal extrapolation is limited.
  Smallest transfer: identical-suffix tests for colliding states, then separate
  retention and exposure ablations. No learned embedding is yet warranted.
- [Cell-Free Latent Go-Explore](https://arxiv.org/html/2208.14928v3): learned
  representations, density-biased goal selection, and reduced subgoal paths.
  Maze ablations remove post-return exploration, density skew, or subgoals;
  all hurt, particularly removing post-return exploration. Atari comparisons
  remain far below fully configured Go-Explore with domain knowledge and
  arbitrary-state reset. Its fixed initial state and latent goal threshold
  are limitations. Smallest transfer: measure extensions after selection;
  compare local exploration only if surviving states already get exposure.
- [Multi-Objective Quality Diversity Optimization / MOME](https://arxiv.org/html/2202.03057v2):
  store bounded Pareto fronts within descriptor cells; sample cells then
  representatives. The paper caps total stored solutions for comparisons.
  Global-front quality is not uniformly best, and hypervolume depends on
  reference/scaling choices. Smallest transfer: two bounded resource tradeoffs
  within a slot at unchanged total bytes, with selection held fixed. Resource
  nondominance is a useful preservation heuristic, not proof of future reach.
- [Multi-Stage Episodic Control / XTX](https://arxiv.org/pdf/2201.01251): combine
  imitation-based exploitation with inverse-dynamics exploration in phases.
  Pure imitation, pure exploration, and constant mixing each have weaknesses;
  strict phase separation also stalls on some games. Text-game action-space
  assumptions differ greatly from controller sampling. Smallest transfer:
  independently test campaign-learned continuations versus ordinary suffixes
  from development-discovered states, then test fresh search if qualified.

Harmony observation: existing Metroid continuation replay changes breadth and
discovery timing without defeating a boss. Proposed extrapolation: retention
must be evaluated by downstream continuation outcomes and exposure, not by
archive size or endpoint inequality. No publication establishes that Metroid
health/missile dominance implies behavioral dominance.
