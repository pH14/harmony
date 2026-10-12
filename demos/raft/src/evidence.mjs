// SPDX-License-Identifier: AGPL-3.0-or-later
export function readEvidence(report) {
  const replay = report.replays?.[0];
  if (!replay?.timeline) throw new Error("Expected a Harmony replay report");
  const seen = new Set(),
    events = [];
  for (const entry of replay.timeline) {
    for (const line of entry.console.split(/\r?\n/)) {
      const start = line.indexOf("RAFT {");
      if (start < 0) continue;
      const raw = line.slice(start + 5).trim();
      if (seen.has(raw)) continue;
      try {
        const event = JSON.parse(raw);
        if (!Number.isFinite(event.time) || !Number.isInteger(event.line))
          continue;
        seen.add(raw);
        events.push({ ...event, step: entry.step });
      } catch {}
    }
  }
  events.sort((a, b) => a.time - b.time);
  const steps = [];
  for (const entry of replay.timeline) {
    const op = entry.action?.operation;
    const step = !op
      ? 0
      : op.Hook?.[0] === 1
        ? 2
        : op.Hook?.[0] === 2
          ? 3
          : op.Kill
            ? 4
            : op.Wait === 20
              ? 1
              : null;
    if (step !== null) steps.push({ ...entry, originalStep: entry.step, step });
    if (op?.Kill) break;
  }
  if (!steps.some((s) => s.step === 1)) steps.push({ ...steps[0], step: 1 });
  steps.sort((a, b) => a.step - b.step);
  return { report, replay, events, steps };
}
export function atTime(evidence, time) {
  return evidence.events.filter((e) => e.time <= time);
}
export function heat(events, time, line) {
  return events
    .filter((e) => e.line === line && e.time <= time)
    .reduce((sum, e) => sum + Math.exp(-(time - e.time) / 0.35), 0);
}
export function command(branch, step, action) {
  if (!/^[a-z][a-z0-9-]*$/.test(branch) || !Number.isInteger(step) || step < 0)
    throw new Error("Invalid branch point");
  const options = {
    trace: "--exec '/app/control trace' --name investigation",
    quorum: "--exec '/app/control require-quorum' --name quorum",
    shell: "--shell --name inspected",
  };
  if (!options[action]) throw new Error("Unknown action");
  return `harmony branch ${branch} --step ${step} ${options[action]}`;
}
