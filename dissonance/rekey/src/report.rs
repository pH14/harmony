// SPDX-License-Identifier: AGPL-3.0-or-later
//! `REKEY-REPORT.md` — the deliverable.
//!
//! Rendering is a pure function of the [`Analysis`](crate::Analysis): no
//! wall-clock, no generated-date line, every number formatted by integer
//! division. Two runs produce byte-identical bytes, which is the determinism
//! gate.

use crate::score::{TARGET_CELLS, TARGET_SENSITIVITY, cell};
use crate::{Analysis, PRIMARY_SLICE};

/// The playbook's step-5 limit, quoted verbatim from `docs/SCORING.md`.
const STEP_5_LIMIT: &str = "\
> **State the limit** (EXPLORATION's \"diagnostic, not predictive,\" operationalized): offline\n\
> re-keying proves a candidate *would have* distinguished the *recorded* states; it cannot prove\n\
> the candidate will surface *unrecorded* ones — admission order also shifts which runs would\n\
> have existed (the counterfactual cascade, which step 3c inherits). The playbook is a cheap\n\
> filter that kills bad cell functions, not an oracle that crowns the best one.";

/// The ratification menu's editorial paragraph for each candidate that can
/// reach the top three. Keyed by candidate id; a candidate with no entry gets a
/// generic line (and, in practice, never ranks).
fn menu_prose(id: &str) -> &'static str {
    match id {
        "draw-top-256" => "\
**What changes.** The cell key gains one chosen sparse state channel — the entropy draw the \
workload already prints on its console, keyed on its top byte, unfolded. The template channels \
stay exactly as shipped. Cells go from 3–4 per campaign to hundreds, the frontier stops \
saturating after branch 0, and — for the first time — the archive grows *while the search is \
still searching* rather than only when it crashes.

**What it risks.** This is the trigger byte. Bug 3 fires exactly when `draw >> 56 == 0xA5`, so \
this descriptor was chosen with the answer in hand, and its twin control (`draw-low-256`, the \
same draw's trigger-blind low byte) matches it on every quantity a search could use. Nothing in \
this report distinguishes them in the trigger's favour — where they differ, the *blind* one looks \
better. That is law 6 (Böhme–Szekeres–Metzman, ICSE 2022) reproduced on harmony's own corpus. Ratifying it is a bet that *some* projection of a \
guest's chosen state correlates with *some* class of trigger — which is IJON's claim, and a \
reasonable one — not evidence that this one does. Its cost is also real: 257× the cell space \
divides per-cell search energy 257 ways (RAID'19: the two most sensitive metrics tested finish \
below baseline because promotion explodes). Confirm live before believing it.",

        "draw-top-64" => "\
**What changes.** The cell key gains one chosen sparse state channel — the entropy draw the \
workload already prints on its console (`UUID_DRAW: draw=0x… prefix_bits=8`), keyed on its top \
byte and folded `mod 64` by the shipped `DEFAULT_FOLD_K`. The template channels stay exactly as \
shipped. It ranks first because 65.7 cells per campaign sits almost exactly on the stated target \
`T = 64` — roughly a quarter of `draw-top-256`\'s cells, so a quarter of the promotion pressure, \
at the cost of aliasing the trigger byte `0xA5` with `0x25`, `0x65`, and `0xE5`.

**What it risks.** The same trigger-alignment critique as `draw-top-256`, plus the fold: three \
unrelated draws now share the bug's cell, so a selector exploiting that cell is three-quarters \
of the time exploiting a state that has nothing to do with the trigger. It is the conservative \
version of the same bet — smaller archive, blunter signal.",

        "draw-low-256" => "\
**What changes.** Nothing a campaign author would ever choose deliberately: it keys the same \
draw's low byte, which no trigger in the benchmark reads. It is in the menu **as a control**, \
not as a proposal.

**What it risks.** Ratifying it would be ratifying noise. That it survives the axes at all — and \
outscores its trigger-aligned twin on raw pooled breadth — is the report's central negative \
result: breadth and granularity cannot tell a bug-aligned descriptor from a bug-blind one, and on \
this corpus neither can chain preservation.",

        "v1-shipped" => "\
**What changes.** Nothing. This row is the control, and its reproduction of the campaign's \
recorded discovery events (all 60 campaigns, every branch, exactly) is the harness's own \
correctness gate.

**What it risks.** Keeping it is keeping the NO-GO: 3 cells at branch 0, then a frozen archive \
until the crash mints a fourth. It cannot steer a search because it discovers nothing while the \
search is running.",

        _ => "\
**What changes / what it risks.** A knob-set variant of the shipped v1 composition. See the \
axis table above; the knob space cannot change what the template channels can see.",
    }
}

/// Render the report.
pub fn render(analysis: &Analysis) -> String {
    let mut out = String::new();
    header(&mut out, analysis);
    corpus(&mut out, analysis);
    candidate_space(&mut out, analysis);
    axes(&mut out, analysis);
    mechanism(&mut out, analysis);
    ranking(&mut out, analysis);
    menu(&mut out, analysis);
    limit(&mut out);
    out
}

fn header(out: &mut String, a: &Analysis) {
    let v1 = a.primary("v1-shipped");
    let cells_before = v1.map_or(0, |s| s.cells_before_find);
    out.push_str(&format!(
        "# REKEY-REPORT — offline `CellFn` iteration over the GO/NO-GO #2 trace corpus\n\
         \n\
         > **The E-fails playbook, steps 2–4** (`docs/SCORING.md`). GO/NO-GO #2 closed **NO-GO**\n\
         > (`CORRELATION-REPORT.md`): the log-template signal is behaviour-neutral but nearly\n\
         > blind, and the ¾-exploit budget was the entire find-rate deficit. The playbook's\n\
         > response is a procedure — freeze the campaign, re-key candidates offline against its\n\
         > retained traces, score three axes, hand a human a ranked menu. This is that menu.\n\
         > **It does not re-open the NO-GO, and it promotes nothing:** R2 rules that fixed cell\n\
         > parameters beat the adaptive tuner on Go-Explore's own headline domain, twice —\n\
         > *auto-tuning proposes, a human ratifies.*\n\
         \n\
         > Corpus manifest `campaign-data/rekey-corpus.json`, sha256 `{}`.\n\
         > {} trace files · {} recorded branches · {} excluded · {} reference logs.\n\
         > All arithmetic is integer/fixed-point; the report has no generated-date line, so two\n\
         > runs on any two hosts produce byte-identical bytes.\n\
         \n\
         ## The finding, in one paragraph\n\
         \n\
         The shipped `CellFn` v1 discovers **{} while the bug-3 search is still searching**,\n\
         summed over all {} campaigns of the primary slice. Three species arrive on branch 0 (a\n\
         blank line, the supervisor's checkpoint message, and the `UUID_DRAW` line), and the\n\
         archive is then **frozen** — until the finding branch, where the kernel's general\n\
         protection fault message mints a fourth species. v1's entire novelty signal on this bug\n\
         is a *post-hoc crash artifact*: it arrives only after the bug has already been found.\n\
         That is the mechanical explanation of both the ρ = −0.671 the correlation report\n\
         computed and the frontier that saturates at two entries. **No setting of v1's knobs\n\
         changes this** — `fold_k`, quantization, and channel ablation can only coarsen a\n\
         three-species vocabulary, never enrich it. Only a *new channel* moves the needle, and\n\
         the report's second finding is that no offline axis can tell a good new channel from a\n\
         useless one.\n\
         \n",
        a.manifest_sha256,
        a.totals.trace_files,
        a.totals.branches,
        a.totals.excluded_traces,
        a.totals.reference_logs,
        match cells_before {
            0 => "not one cell".to_string(),
            1 => "exactly 1 cell".to_string(),
            n => format!("{n} cells"),
        },
        v1.map_or(0, |s| s.campaigns),
    ));
}

fn corpus(out: &mut String, a: &Analysis) {
    out.push_str("## The corpus\n\nLoaded **only** through the manifest, every artifact pinned by content hash (the `hm-xdp` lesson: reference artifacts by content, never by mutable path). A hash mismatch aborts the run; it is never a warning.\n\n");
    out.push_str(
        "| slice | bug | campaigns | `explore_period` | what it is |\n|---|---|---|---|---|\n",
    );
    for s in &a.slices {
        out.push_str(&format!(
            "| `{}` | {} | {} | {} | {} |\n",
            s.id, s.bug, s.campaigns, s.explore_period, s.description
        ));
    }
    out.push_str(&format!(
        "| `bug1-reference` | 1 | {} | 4 | **recorded logs only — not re-keyable** |\n\n",
        a.reference.len()
    ));

    out.push_str("### Exclusions\n\n");
    out.push_str("| slice | member | reason |\n|---|---|---|\n");
    for e in &a.exclusions {
        out.push_str(&format!(
            "| `{}` | `{}` | {} |\n",
            e.slice, e.member, e.reason
        ));
    }
    out.push_str(&format!("\n**{} excluded**, all `-solo` determinism re-runs. Each is pinned by sha256 in the manifest too: an exclusion names a *known* artifact, not merely an absent one.\n\n", a.totals.excluded_traces));

    out.push_str("### Bug 1 — a reference row, not an evaluation slice\n\n");
    out.push_str(&format!("{}\n\n", a.reference_reason));
    let signal: Vec<&crate::ReferenceRow> = a
        .reference
        .iter()
        .filter(|r| r.config == "Signal")
        .collect();
    let baseline: Vec<&crate::ReferenceRow> = a
        .reference
        .iter()
        .filter(|r| r.config == "Baseline")
        .collect();
    let cells = |rows: &[&crate::ReferenceRow]| {
        let mut v: Vec<u64> = rows.iter().map(|r| r.distinct_cells).collect();
        v.sort_unstable();
        v.dedup();
        v.iter().map(u64::to_string).collect::<Vec<_>>().join(", ")
    };
    out.push_str(&format!(
        "Its recorded per-campaign distinct-cell counts, for reference: **{}** over {} signal \
         campaigns and **{}** over {} baseline campaigns — a *two*-cell vocabulary, thinner \
         even than bug 3's. Every campaign found the bug (it fires on any canary bit-flip), so it \
         was never a discriminator. The trigger-orthogonal twin candidate (`draw-low-256`) \
         replaces it as this report's noise-fitting control, per the tasks/97 amendment.\n\n",
        cells(&signal),
        signal.len(),
        cells(&baseline),
        baseline.len(),
    ));
}

fn candidate_space(out: &mut String, a: &Analysis) {
    out.push_str(&format!(
        "## The candidate space (R2's knob-sets — configs, not code)\n\n\
         Each candidate is a `logtmpl::CellConfig` recorded verbatim below, optionally composed \
         with one **chosen sparse state channel** (IJON's discipline: sparse chosen state \
         annotations beat indiscriminate state feedback; the empty `cell_channels` default is a \
         ruling, not an accident). The corpus offers exactly one such observable — the \
         `UUID_DRAW: draw=0x… prefix_bits=8` line the workload prints once per branch.\n\n\
         Corpus constants used by the key-space normalizer, derived from the observations rather \
         than assumed: **max_species = {}**, **|top-byte alphabet| = {}**, **|low-byte alphabet| \
         = {}**.\n\n",
        a.constants.max_species, a.constants.top_alphabet, a.constants.low_alphabet
    ));
    out.push_str(
        "| candidate | state channel | `CellConfig` (verbatim) | `\\|K\\|` |\n|---|---|---|---|\n",
    );
    for c in &a.candidates {
        let score = a.primary(c.id);
        out.push_str(&format!(
            "| `{}` | {} | `{}` | {} |\n",
            c.id,
            c.state.map_or("—", |p| p.label()),
            c.config_json(),
            score.map_or(0, |s| s.key_space),
        ));
    }
    out.push('\n');
    for c in &a.candidates {
        out.push_str(&format!("- `{}` — {}\n", c.id, c.summary));
    }
    out.push('\n');
}

fn axes(out: &mut String, a: &Analysis) {
    out.push_str(&format!(
        "## The three axes\n\n\
         - **(a) breadth** — cells discovered over the fixed trace set. `pooled` is the distinct \
         cells over the whole slice; `mean` is per campaign; `coverage` normalizes `pooled` by the \
         candidate's key-space cardinality `|K|`, because raw QD-style scores scale with \
         resolution and would crown the finest candidate by construction.\n\
         - **(b) granularity** — Go-Explore's re-tune objective `O = H_n(p)/√(|n/T−1|+1)`, per \
         campaign, averaged. `p` is the arrival count per cell (the STADS abundance stream). The \
         **stated target** is `T = {TARGET_CELLS}` — a cell per ~8 branches of the 512-branch \
         budget: fine enough that the frontier has somewhere to go, coarse enough that each cell \
         still earns search energy. `O@{TARGET_SENSITIVITY}` re-scores at a second target so the \
         ranking's dependence on `T` is visible rather than hidden.\n\
         - **(c) chain preservation** — mandatory, law 6. The admission fold is re-run in recorded \
         campaign order under the candidate; every **proper ancestor** of every bug-finding run \
         must still claim a cell when it arrives. A candidate that would have judged any link \
         uninteresting would have lost the bug.\n\
         \n\
         Diagnostics, clearly *not* a fourth axis: `admitted` is the mean frontier size a selector \
         would have had to exploit; `cells>0` counts cells first claimed after branch 0; \
         **`steering`** counts cells first claimed strictly between branch 0 and the find — the \
         cells a search could actually have used; **`crash-only`** counts pooled cells never keyed \
         before the guest crashed, which a search can never have used at all.\n\n"
    ));

    for s in &a.slices {
        out.push_str(&format!(
            "### `{}` — {} campaigns, {} found the bug\n\n",
            s.id,
            s.campaigns,
            s.scores.first().map_or(0, |x| x.finders)
        ));
        out.push_str(&format!(
            "| candidate | (a) pooled | (a) mean | (a) coverage | (b) O@{TARGET_CELLS} | \
             (b) O@{TARGET_SENSITIVITY} | (c) chains | admitted | cells>0 | steering | crash-only |\n\
             |---|---:|---:|---:|---:|---:|---|---:|---:|---:|---:|\n"
        ));
        for score in &s.scores {
            out.push_str(&format!(
                "| `{}` | {} | {} | {} | {} | {} | {} | {} | {} | {} | {} |\n",
                score.candidate,
                score.pooled_cells,
                cell(score.mean_cells_q32),
                cell(score.breadth_q32),
                cell(score.objective_q32),
                cell(score.objective_alt_q32),
                score.chain_cell(),
                cell(score.mean_admitted_q32),
                score.cells_after_branch0,
                score.cells_before_find,
                score.crash_only_cells,
            ));
        }
        out.push('\n');
    }
}

fn mechanism(out: &mut String, a: &Analysis) {
    let Some(primary) = a.slices.iter().find(|s| s.id == PRIMARY_SLICE) else {
        return;
    };
    let d = &primary.debut;
    out.push_str("## Why v1 is blind: the fourth cell *is* the crash\n\n");
    out.push_str(&format!(
        "Across the {} primary-slice campaigns, **{}** have every template species debut either on \
         branch 0 or on the finding branch — nothing in between. Of the {} campaigns that found the \
         bug, **{}** mint their last species *exactly at the find*. Of the {} that did not find it, \
         **{}** mint every species on branch 0 and then discover nothing for all 512 branches.\n\n",
        d.campaigns,
        d.debut_at_zero_or_find,
        d.finders,
        d.terminal_debut_at_find,
        d.campaigns - d.finders,
        d.frozen_non_finders,
    ));
    out.push_str(
        "The species, and the lines that mint them (parameters masked to `<*>`, as Drain's own \
         clustering masks them):\n\n",
    );
    out.push_str("| species | debut line |\n|---|---|\n");
    for (species, lines) in &d.debut_lines {
        for line in lines {
            let shown = if line.is_empty() {
                "*(a blank line)*".to_string()
            } else {
                format!("`{line}`")
            };
            out.push_str(&format!("| {species} | {shown} |\n"));
        }
    }
    out.push_str(&format!(
        "\nSpecies **{}** is the guest kernel's fault message. The campaign filters the bug's \
         `UUID_BUG` attribution marker out of the console before clustering — precisely so the \
         signal cannot key its own marker — but the kernel's `traps: … general protection fault` \
         line rides *behind* the marker and is not filtered. So the one cell v1 ever discovers \
         after branch 0 is minted by the crash itself.\n\n\
         This is not a bug in the marker filter's intent; it is the honest consequence of a \
         bug-agnostic console. It does mean that **v1's `cells@256` statistic, and therefore the \
         ρ = −0.671 the correlation report computed, is a restatement of \"did this campaign find \
         the bug before branch 256?\"** — not a graded novelty↔progress relationship. \
         `CORRELATION-REPORT.md` already suspected as much (\"degenerate: `cells@256` takes only \
         *two* values\"); the re-key proves the mechanism.\n\n\
         **The knob space cannot fix it.** With a three-species pre-crash vocabulary, \
         species-progress ranges over `1..=3` and last-new-species over ids `0..=2`. Every `fold_k` \
         in the sweep exceeds 3, so the fold is the identity; `Quant::Identity` distinguishes \
         counts the `Log2` bucket already distinguishes. Every knob-set candidate in the table \
         above therefore ties the control or falls below it, and this is a *proof from the corpus*, \
         not a sampling accident.\n\n",
        d.debut_lines.keys().next_back().copied().unwrap_or(0),
    ));
}

fn ranking(out: &mut String, a: &Analysis) {
    let Some(primary) = a.slices.iter().find(|s| s.id == PRIMARY_SLICE) else {
        return;
    };
    out.push_str(&format!(
        "## The ranking (on `{PRIMARY_SLICE}` — the sole real discriminator)\n\n\
         Chain preservation **gates**: a candidate that breaks any finding chain is disqualified \
         outright, whatever its curves. Survivors are ordered by the granularity objective at the \
         stated target, tie-broken by raw breadth and then by declaration order — an exact tie \
         means the two candidates *are the same descriptor on this corpus*, and the control is \
         declared first, so a knob variant can never displace the v1 row it is indistinguishable \
         from. On this corpus the gate disqualifies **nothing**; see below.\n\n"
    ));
    out.push_str(&format!(
        "| # | candidate | (b) O@{TARGET_CELLS} | (a) pooled | (c) chains | steering | verdict |\n\
         |---:|---|---:|---:|---|---:|---|\n"
    ));
    for (rank, &i) in a.ranking.iter().enumerate() {
        let s = &primary.scores[i];
        let verdict = if s.chain_preserved() {
            "eligible"
        } else {
            "**disqualified** (broke a finding chain)"
        };
        out.push_str(&format!(
            "| {} | `{}` | {} | {} | {} | {} | {} |\n",
            rank + 1,
            s.candidate,
            cell(s.objective_q32),
            s.pooled_cells,
            s.chain_cell(),
            s.cells_before_find,
            verdict,
        ));
    }

    // The ranking is a function of the stated target. Show it changing.
    let mut alt: Vec<usize> = (0..primary.scores.len()).collect();
    alt.sort_by(|&x, &y| {
        primary.scores[y]
            .objective_alt_q32
            .cmp(&primary.scores[x].objective_alt_q32)
            .then(x.cmp(&y))
    });
    let names = |order: &[usize]| {
        order
            .iter()
            .take(3)
            .map(|&i| format!("`{}`", primary.scores[i].candidate))
            .collect::<Vec<_>>()
            .join(" → ")
    };
    out.push_str(&format!(
        "\n**The ranking is a function of the stated target, not of the corpus.** At the stated \
         `T = {TARGET_CELLS}` the order is {}. At `T = {TARGET_SENSITIVITY}` it becomes {} — the \
         two `draw-top-*` candidates swap, because Go-Explore's penalty term `√(|n/T−1|+1)` is \
         asymmetric (undershooting the target costs at most `√2`, overshooting is unbounded), so \
         `T` alone decides how much resolution is \"too much\". Choosing `T` is a human judgment \
         about how much search energy a cell should get; the harness cannot make it, and it is \
         precisely the kind of decision R2 reserves for ratification.\n\n",
        names(&a.ranking),
        names(&alt),
    ));

    let ancestors: u64 = primary.scores.first().map_or(0, |s| s.ancestors_checked);
    let chains: u64 = primary.scores.first().map_or(0, |s| s.chains_checked);
    let floor = a.primary("no-channels");
    out.push_str(&format!(
        "\n### Axis (c) has no discriminating power on this corpus — say it out loud\n\n\
         The primary slice's {chains} finding chains contain **{ancestors} proper ancestors in \
         total**, and every one of them is branch 0. That follows directly from the NO-GO's own \
         diagnosis: v1 admits branch 0 (three fresh cells) and then, at most, the finding branch \
         (the crash cell), so the frontier never holds more than two entries and every exploit \
         jitters branch 0's seed. Branch 0 claims a fresh cell under *every* candidate, because \
         the archive starts empty.\n\n\
         The consequence is not subtle: **`no-channels` — the candidate that keys all 30 720 \
         branches into a single cell — passes axis (c) with {}.** The playbook's one **bug-based** \
         axis, the one law 6 makes mandatory, cannot distinguish the shipped descriptor from a \
         constant function. It is computed and reported because it is mandatory, and because it \
         *would* fail a candidate on a corpus with real chain depth (the unit tests exercise \
         exactly that). Here it crowns nothing and kills nothing.\n\n\
         So the ranking rests entirely on axes (a) and (b) — the discovery curves law 6 disqualifies \
         as sole evidence. **And on the ablation slice, the one slice free of the exploit's \
         confound, the trigger-aligned `draw-top-256` and its trigger-blind twin `draw-low-256` are \
         indistinguishable on every quantity a search could use:**\n\n\
         {}\n\
         The two candidates read the same 64-bit draw. One reads the byte the bug compares; the \
         other reads a byte no trigger in the benchmark ever looks at. Mean cells per campaign, the \
         objective at both targets, steering, and chain preservation all agree to within noise. That \
         is Böhme–Szekeres–Metzman (ICSE 2022) reproduced on harmony's own corpus, and it is the \
         reason this report hands over a menu rather than a winner.\n\n\
         The two places they *do* differ both cut **against** the trigger-aligned candidate:\n\n\
         1. **Pooled cells.** `draw-low-256` pools more ({} vs {} on the campaign slice) — and the \
            entire surplus is `crash-only` cells ({} vs {}). The top-byte projection pins every \
            crashing branch to the one cell `0xA5`; the low-byte projection scatters them across as \
            many cells as they have distinct low bytes. **Raw pooled breadth rewards the \
            trigger-blind descriptor, for fragmenting the crash it should be ignoring.**\n\
         2. **Mean cells per campaign** on the *steered* slice ({} vs {}), which is an artifact of \
            the exploit rather than of the trigger. Measured over the {} exploit branches of that \
            slice: a child inherits its parent's draw **low byte {}% of the time** but its **top \
            byte only {}%** (chance is 1/256 ≈ 0.4%). Twiddling a *low* seed bit preserves the low \
            byte in {}/{} of those exploits; twiddling a *high* one preserves it in {}/{} ({}%). So a \
            steered campaign resamples the low byte far less often than the top byte. The ablation \
            slice never exploits, which is exactly why the comparison is clean there.\n\n",
        floor.map_or_else(|| "—".to_string(), |s| s.chain_cell()),
        ablation_twin_table(a),
        a.primary("draw-low-256").map_or(0, |s| s.pooled_cells),
        a.primary("draw-top-256").map_or(0, |s| s.pooled_cells),
        a.primary("draw-low-256").map_or(0, |s| s.crash_only_cells),
        a.primary("draw-top-256").map_or(0, |s| s.crash_only_cells),
        cell(a.primary("draw-low-256").map_or(0, |s| s.mean_cells_q32)),
        cell(a.primary("draw-top-256").map_or(0, |s| s.mean_cells_q32)),
        primary.locality.exploits,
        pct(primary.locality.shares_low, primary.locality.exploits),
        pct(primary.locality.shares_top, primary.locality.exploits),
        primary.locality.low_bit_shares_low,
        primary.locality.low_bit_exploits,
        primary.locality.high_bit_shares_low(),
        primary.locality.high_bit_exploits(),
        pct(
            primary.locality.high_bit_shares_low(),
            primary.locality.high_bit_exploits(),
        ),
    ));
}

/// `n / d` as a percentage to one decimal place, by integer division with
/// round-half-up — the report has no floats.
fn pct(n: u64, d: u64) -> String {
    if d == 0 {
        return "0.0".to_string();
    }
    let tenths = (n * 1000 + d / 2) / d;
    format!("{}.{}", tenths / 10, tenths % 10)
}

/// The twin-control comparison on the unsteered ablation slice — the report's
/// central negative result, isolated into its own table.
fn ablation_twin_table(a: &Analysis) -> String {
    let Some(ablation) = a
        .slices
        .iter()
        .find(|s| s.id == crate::manifest::BUG3_ABLATION)
    else {
        return String::new();
    };
    let row = |id: &str| ablation.scores.iter().find(|s| s.candidate == id);
    let (Some(top), Some(low)) = (row("draw-top-256"), row("draw-low-256")) else {
        return String::new();
    };
    format!(
        "| `bug3-ablation` (unsteered) | mean cells | (a) coverage | (b) O@{} | (b) O@{} | steering | (c) chains |\n\
         |---|---:|---:|---:|---:|---:|---|\n\
         | `draw-top-256` — reads the trigger byte | {} | {} | {} | {} | {} | {} |\n\
         | `draw-low-256` — reads a byte no bug uses | {} | {} | {} | {} | {} | {} |\n",
        TARGET_CELLS,
        TARGET_SENSITIVITY,
        cell(top.mean_cells_q32),
        cell(top.breadth_q32),
        cell(top.objective_q32),
        cell(top.objective_alt_q32),
        top.cells_before_find,
        top.chain_cell(),
        cell(low.mean_cells_q32),
        cell(low.breadth_q32),
        cell(low.objective_q32),
        cell(low.objective_alt_q32),
        low.cells_before_find,
        low.chain_cell(),
    )
}

fn menu(out: &mut String, a: &Analysis) {
    let Some(primary) = a.slices.iter().find(|s| s.id == PRIMARY_SLICE) else {
        return;
    };
    out.push_str(
        "## The ratification menu\n\n\
         **A human (Paul) ratifies. The harness never auto-promotes.** The three highest-ranked \
         *distinct* eligible proposals, each with what it changes and what it risks. Candidates \
         whose every axis is identical to one already listed are the **same descriptor on this \
         corpus** — the knob that separates them addresses a distinction the traces cannot make — \
         so they are folded into that entry and named there rather than padding the menu. That is \
         why the third entry carries a double-digit rank: the eight rows between it and the second \
         are all the same descriptor.\n\n",
    );
    for entry in crate::score::menu(&primary.scores, &a.ranking, 3) {
        let s = &primary.scores[entry.row];
        out.push_str(&format!(
            "### `{}` — ranked {}\n\n",
            s.candidate, entry.rank
        ));
        if !entry.tied_with.is_empty() {
            let tied = entry
                .tied_with
                .iter()
                .map(|t| format!("`{t}`"))
                .collect::<Vec<_>>()
                .join(", ");
            out.push_str(&format!(
                "> Indistinguishable on every axis from {tied} — ratifying any of them ratifies \
                 this one. The `fold_k` and `Quant` knobs have **no effect whatsoever** on this \
                 corpus: with a three-species pre-crash vocabulary, every modulus in the sweep \
                 exceeds the largest species id, so every fold is the identity.\n\n"
            ));
        }
        out.push_str(&format!("{}\n\n", menu_prose(&s.candidate)));
    }
    out.push_str(
        "### The recommendation the harness is entitled to make\n\n\
         Not a candidate — a **sequencing**. The offline filter did its job: it killed the entire \
         v1 knob space (proof, not evidence: a three-species vocabulary has no granularity to \
         tune) and it surfaced one class of candidate that is not blind. It cannot tell you \
         whether that class *works*, because its only bug-based axis is vacuous on this corpus and \
         its two curve axes rate a bug-blind descriptor exactly as highly as a bug-aligned one.\n\n\
         What decides it is a live run, and the spec already names the cheap one: once a `CellFn` \
         is ratified **and** task-95 M2's snapshot speedup lands, run the top candidate against \
         `explore_period ∈ {1, 2, 4}` on bug 3 — an afternoon on the box. Under the 2026-07-09 \
         workloads-first directive, the deciding weight belongs to the game workloads (tasks 86/87) \
         regardless; a bug-3 re-run is a cheap red-flag check, not the gate.\n\n\
         Two structural findings ride out of this report independently of any ratification:\n\n\
         1. **The console the signal reads is not instrumented for search.** Three bug-agnostic \
            lifecycle lines and one crash message is not a species ladder. Every candidate that \
            improved anything did so by reading a *state value* off the console, not a template. \
            This is IJON's thesis, and it points at the guest SDK (`assert`/state-register \
            annotations, task 73's seams) rather than at the cell function.\n\
         2. **The marker filter has a hole.** `UUID_BUG` is filtered before clustering; the kernel \
            fault message it precedes is not. Any future campaign whose guest crashes noisily will \
            key its own crash as novelty. Filtering post-crash console output — or, better, \
            keying only on records at or before the seal — is a one-line discipline that should \
            land before the next correlation campaign.\n\n",
    );
}

fn limit(out: &mut String) {
    out.push_str("## The limit (playbook step 5, verbatim)\n\n");
    out.push_str(STEP_5_LIMIT);
    out.push_str(
        "\n\nIt applies to axis (c) with particular force here. The chains this report checks are \
         the chains the *v1* campaign walked. Under a candidate that admits hundreds of cells, the \
         frontier would have held hundreds of exemplars, the selector would have exploited \
         different parents, and the runs that exist in this corpus would largely never have been \
         minted. Re-keying tells you what a candidate would have *said* about the states we \
         recorded. It cannot tell you which states it would have led the search to.\n",
    );
}
