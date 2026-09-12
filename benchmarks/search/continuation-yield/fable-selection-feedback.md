**Recommendation:** run the route-cost ablation as the next mechanism test, as one factor under one new identifier, but gate it on a zero-budget diagnostic first: stratify the outcomes of the existing uniform draws by the parent's within-cell cost band. Neither alternative is a better use of eight hours.

## What the code says the bonuses are

- **Between cells.** In `draw_group_index` the cell weight is barren energy shifted right by the smaller of 16 and novelty plus costlier. Costlier is the count of strictly cheaper cells integer-divided by CHEAPEST_RANK_SCALE. So the term is identically zero until a cell has at least that many cheaper cells, and it shares a single cap with novelty. A cell's cost is the minimum time_in_group over its newest window members, per `cheapest_offered`.
- **Within a cell.** In `draw_from_cell` the newest 128 sampleable members are sorted by the pair of time_in_group and id, and each member's weight halves every CHEAPEST_RANK_SCALE ranks down to a floor of one. This is geometric, so the cheapest band holds roughly half the window mass whatever the scale constant is. Ties are broken by id, so among equal costs the oldest member is favored.
- **Precedent.** The non-cheapest policies already take the uniform-window branch within a cell, so that half of the ablation is existing code. The between-cell removal is new code and needs a new identifier because replay rejects unknown identifiers.

The ablation leaves the class walk, semantic frontier rank, barren counters, uniform quarter, concentration window and retention untouched. It is coherent.

Keep the two effects distinct. The per-draw effect is exact: removing costlier changes a cell's weight by a factor between one and two to the sixteenth, and only for cells with nonzero costlier. Per-draw total variation between the used and cost-free weight vectors is computable from the selector's own inputs. The cumulative exposure difference is not, because barren counters and window membership evolve under whichever draws actually happen. Only the paired runs measure the second quantity.

## Confounds

- **Novelty cap.** Because novelty and costlier share one cap, cells previously saturated at 16 halvings become re-differentiated by novelty alone. The ablation therefore also steepens the effective novelty gradient. Report how often the chosen cell's combined rank hit the cap.
- **Age tiebreak.** Dropping the within-cell rank also drops the oldest-first ordering among cost ties. If time_in_group is coarse or often equal, that tiebreak, not cost, may be the active ingredient.
- **Semantics of time_in_group.** The excerpt does not define it. If it is dwell time inside the cell rather than route length, the bonus is an arrival-freshness prior, which is a much more plausible yield signal than global cost. Hypothesis, not fact.
- **Retention is unchanged.** Workload same-slot preference may already prefer cheap states. A null says nothing about retention-side cost preference.
- **Scope mismatch of the pilot.** The between-cell term only bites once many cells exist. At the first-missile horizon the archive is small, so the pilot mostly tests the within-cell term while the long-horizon claim rests more on the between-cell term. Read the pilot accordingly.
- **Binding cap.** The README notes a wall stop shifts with execution speed. Record whether the frame or the wall bound censored each cell.

## Alternatives and verdicts

**(a) Another yield estimate.** The barren counters already halve weight with a floor and reset on a new cell descendant. A rolling average is the same mechanism with an extra tunable you cannot tune on four pairs. Reject.

**(b) Enable count weighting.** It exists, prior panels showed no general gain, and it multiplies the cost-ranked base weight rather than replacing it. Running it isolates nothing about route cost. Keep it as a later crossed factor.

## Toy example

The clean discriminator is within one cell, using the finite transition fixtures. One cell holds 128 window members. The cheap members are dead ends. One expensive member holds the only exit to a new cell. Novelty and barren energy are constant here because they are per cell, so they cannot confound. Under cost ranking the exit-holding member sits at the floor weight while each cheap band member holds 256, so expected draws to first exit scale with the window's total weight. Under uniform draws it is 128. The mirror fixture, exit held by the cheapest member, shows the bonus winning, which is why this is a prior and not a theorem. For the between-cell term the toy needs more cheap dead-end cells than CHEAPEST_RANK_SCALE, otherwise costlier is zero and the fixture proves nothing.

## Cheap diagnostic and falsifier

The uniform quarter is an unweighted probe of the active archive. Add sidecar-only counters, which the README says never enter selection or the stream. For each uniform-path draw, record the parent's cost band within its cell window at draw time, and on the outcome record whether it produced a new-cell descendant. Also record, for group-walk draws, the per-draw total variation between the cost-ranked and cost-free vectors at both levels, and the cap-hit rate.

Run this inside the four calibration seeds if they have not started. Otherwise one control seed at 5M frames suffices.

**Falsifier.** If new-cell yield per uniform draw falls monotonically with cost band, the bonus is allocating draws where yield is, and the ablation is predicted to hurt. Skip it and spend the pairs elsewhere. If yield is flat or rises with cost, the prior is misallocating and the ablation is predicted to help or be neutral. Run the four pairs and score first-missile frames as censored paired differences, direction stated in advance.

The exposure-adjusted estimate has a caveat: which states are active is shaped by retention, so the strata are not a random sample of physical states. It is a direction check, not an effect size.

## Strongest reason it fails

The selection-side cost bonus may be a weak lever. A quarter of draws ignore it, barren decay erodes concentration on cheap cells over time, and retention may already filter by cost upstream. Four pairs at a censored pilot endpoint will then show nothing, and a null will be uninformative about long-horizon discovery. The diagnostic above is the only cheap defense: if per-draw total variation is small at both levels, the ablation cannot matter and should not be run.