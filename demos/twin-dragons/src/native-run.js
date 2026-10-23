// SPDX-License-Identifier: AGPL-3.0-or-later
const LOOPBACK = new Set(["127.0.0.1", "localhost", "[::1]"]);
const REFRESH_MS = 3000;
const LAUNCHER_HEADER = "X-Harmony-Launcher";

function element(tag, props = {}, ...children) {
  const node = Object.assign(document.createElement(tag), props);
  node.append(...children);
  return node;
}

function field(label, input, hint = "") {
  return element("label", { className: "native-field" }, element("span", { textContent: label }), input, element("small", { textContent: hint }));
}

async function post(path, body = {}) {
  const response = await fetch(path, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
  const reply = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(reply.error || `The launcher answered ${response.status}`);
  return reply;
}

function segments(items, selected, pick) {
  const group = element("div", { className: "segments", role: "group" });
  for (const [id, label] of items) {
    group.append(element("button", { type: "button", textContent: label, ariaPressed: String(id === selected), onclick: () => pick(id) }));
  }
  return group;
}

function statusChip(run) {
  const node = element("span", { className: "native-state", textContent: run.message ? `${run.state}: ${run.message}` : run.state });
  node.setAttribute("data-state", run.state);
  return node;
}

function age(started) {
  const minutes = Math.max(0, Math.round((Date.now() / 1000 - started) / 60));
  return minutes < 1 ? "just now" : minutes < 90 ? `${minutes} min ago` : `${Math.round(minutes / 60)} h ago`;
}

export async function nativeRuns(gameId, mount, catalog) {
  if (!LOOPBACK.has(location.hostname)) return;
  const page = await fetch(location.href, { method: "HEAD" }).catch(() => null);
  if (!page?.headers.has(LAUNCHER_HEADER)) return;
  const options = await fetch("/launcher/machines").then((response) => (response.ok ? response.json() : null)).catch(() => null);
  if (!options?.machines?.length) return;
  const available = new Set(options.games),
    byId = new Map(catalog.map((choice) => [choice.id, choice])),
    members = (choice) => (choice.family ? catalog.filter((member) => member.family === choice.family) : [choice]),
    titles = catalog.filter((choice) => (!choice.family || choice.id === choice.family) && members(choice).some((member) => available.has(member.id))),
    label = (id) => {
      const choice = byId.get(id);
      return choice?.group ? `${choice.title} · ${choice.group}` : (choice?.title ?? id);
    };
  const state = { machine: options.machines[0].name, game: available.has(gameId) ? gameId : options.games[0] };
  const workers = element("input", { type: "number", min: 1, value: 8 }),
    memory = element("input", { type: "number", min: 0.25, step: 0.25, value: 4 }),
    seed = element("input", { type: "number", min: 0, value: 1 }),
    hours = element("input", { type: "number", min: 0.5, max: 72, step: 0.5, value: 12 }),
    start = element("button", { type: "submit", className: "native-start", textContent: "Start native search" }),
    ceiling = element("p", { className: "native-note" }),
    message = element("p", { className: "native-message", role: "status" }),
    list = element("ul", { className: "native-runs" }),
    machinePick = element("section", { className: "game-group" }),
    gamePick = element("div", { className: "game-choice" });
  const entry = () => options.machines.find((machine) => machine.name === state.machine);
  const limits = () => {
    const machine = entry();
    workers.max = machine.workers;
    workers.value = Math.min(Number(workers.value) || 1, machine.workers);
    memory.max = machine.memory_mib / 1024;
    memory.value = Math.min(Number(memory.value) || 1, machine.memory_mib / 1024);
    ceiling.textContent = `Up to ${machine.workers} workers and ${(machine.memory_mib / 1024).toFixed(0)} GiB of archive on ${machine.name}. The run, its workers and the live view share ${(machine.memory_max_mib / 1024).toFixed(0)} GiB.`;
  };
  const render = () => {
    machinePick.replaceChildren(
      ...(options.machines.length > 1
        ? [element("h3", { textContent: "Machine" }), segments(options.machines.map((machine) => [machine.name, machine.name]), state.machine, (name) => { state.machine = name; limits(); render(); })]
        : []),
    );
    const top = titles.find((choice) => members(choice).some((member) => member.id === state.game)),
      homebrew = titles.filter((choice) => !choice.rom.upload),
      owned = titles.filter((choice) => choice.rom.upload),
      choose = (choice) => (state.game = members(choice).find((member) => available.has(member.id)).id),
      row = (heading, choices) =>
        choices.length
          ? element("section", { className: "game-group" }, element("h3", { textContent: heading }), segments(choices.map((choice) => [choice.id, choice.title]), top?.id, (id) => { choose(byId.get(id)); render(); }))
          : null;
    const sections = members(top).filter((member) => available.has(member.id));
    gamePick.replaceChildren(
      ...[row("Homebrew", homebrew), row("Bring your own ROM", owned)].filter(Boolean),
      ...(sections.length > 1
        ? [element("section", { className: "game-group" }, element("h3", { textContent: "Section" }), segments(sections.map((member) => [member.id, member.group]), state.game, (id) => { state.game = id; render(); }))]
        : []),
    );
  };
  limits();
  render();
  const form = element(
    "form",
    { className: "native-form" },
    machinePick,
    gamePick,
    element(
      "div",
      { className: "native-fields" },
      field("Workers", workers, "Parallel emulators"),
      field("Archive (GiB)", memory, "Saved-state memory"),
      field("Seed", seed, "Same seed, same search"),
      field("Time limit (h)", hours, "Stops itself after this"),
    ),
    element("div", { className: "native-submit" }, start, ceiling),
    message,
  );
  const dialog = element(
    "dialog",
    { id: "native-run", className: "native-run", ariaLabelledby: "native-run-title" },
    element(
      "div",
      { className: "level-picker-heading" },
      element("button", { className: "close", type: "button", ariaLabel: "Close native runs", textContent: "×", onclick: () => dialog.close() }),
      element("h2", { id: "native-run-title", textContent: "Native search" }),
    ),
    form,
    element("h3", { className: "native-runs-title", textContent: "Runs" }),
    list,
  );
  document.body.append(dialog);
  const draw = (runs) => {
    list.replaceChildren(
      ...(runs.length ? runs.slice(0, 8) : [{ empty: true }]).map((run) => {
        if (run.empty) return element("li", { className: "native-empty", textContent: "No runs yet." });
        const actions = element("span", { className: "native-actions" });
        if (run.watch && run.state === "running") actions.append(element("a", { className: "native-watch", href: run.watch, textContent: "Watch" }));
        if (!run.external && (run.state === "running" || run.state === "starting")) {
          actions.append(
            element("button", {
              type: "button",
              textContent: "Stop",
              onclick: () => post(`/launcher/runs/${run.id}/stop`).then(refresh, (error) => (message.textContent = error.message)),
            }),
          );
        }
        const detail = run.external
          ? `${run.machine} · ${run.directory}`
          : `${run.machine} · ${run.workers} workers · ${(run.memory_mib / 1024).toFixed(1)} GiB · seed ${run.seed}`;
        return element(
          "li",
          { className: "native-run-card" },
          element("span", { className: "native-run-name", textContent: label(run.game) }),
          statusChip(run),
          element("span", { className: "native-run-meta", textContent: `${detail} · ${age(run.started)}` }),
          actions,
        );
      }),
    );
  };
  const refresh = () =>
    fetch("/launcher/runs")
      .then((response) => response.json())
      .then(({ runs }) => draw(runs))
      .catch(() => (message.textContent = "The launcher stopped answering."));
  let timer = 0;
  dialog.addEventListener("close", () => clearInterval(timer));
  form.onsubmit = (event) => {
    event.preventDefault();
    message.textContent = "";
    start.disabled = true;
    post("/launcher/runs", {
      machine: state.machine,
      game: state.game,
      workers: Number(workers.value),
      memory_mib: Math.round(Number(memory.value) * 1024),
      seed: Number(seed.value),
      hours: Number(hours.value),
    })
      .then(refresh, (error) => (message.textContent = error.message))
      .finally(() => (start.disabled = false));
  };
  const open = element("button", { type: "button", className: "native-open", textContent: "Native runs" });
  open.onclick = () => {
    dialog.showModal();
    refresh();
    timer = setInterval(refresh, REFRESH_MS);
  };
  mount.append(open);
}
