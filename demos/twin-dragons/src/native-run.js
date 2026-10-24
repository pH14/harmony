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

export function segments(items, selected, pick) {
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

export async function detectLauncher() {
  if (!LOOPBACK.has(location.hostname)) return null;
  const page = await fetch(location.href, { method: "HEAD" }).catch(() => null);
  if (!page?.headers.has(LAUNCHER_HEADER)) return null;
  const options = await fetch("/launcher/machines").then((response) => (response.ok ? response.json() : null)).catch(() => null);
  return options?.machines?.length ? options : null;
}

export function nativePanel(options, catalog, gameId) {
  const available = new Set(options.games),
    byId = new Map(catalog.map((choice) => [choice.id, choice])),
    label = (id) => {
      const choice = byId.get(id);
      return choice?.group ? `${choice.title} · ${choice.group}` : (choice?.title ?? id);
    };
  const state = { machine: options.machines[0].name, game: gameId, stage: null };
  const workers = element("input", { type: "number", min: 1, value: 8 }),
    memory = element("input", { type: "number", min: 0.25, step: 0.25, value: 4 }),
    seed = element("input", { type: "number", min: 0, value: 1 }),
    hours = element("input", { type: "number", min: 0.5, max: 72, step: 0.5, value: 12 }),
    start = element("button", { type: "submit", className: "native-start", textContent: "Start native search" }),
    ceiling = element("p", { className: "native-note" }),
    message = element("p", { className: "native-message", role: "status" }),
    list = element("ul", { className: "native-runs" }),
    preview = element("div", { className: "native-preview" }),
    summary = element("p", { className: "native-summary-text" }),
    back = element("button", { type: "button", className: "native-back", textContent: "← Levels" });
  const limits = () => {
    const machine = options.machines.find((entry) => entry.name === state.machine);
    workers.max = machine.workers;
    workers.value = Math.min(Number(workers.value) || 1, machine.workers);
    memory.max = machine.memory_mib / 1024;
    memory.value = Math.min(Number(memory.value) || 1, machine.memory_mib / 1024);
    ceiling.textContent = `Up to ${machine.workers} workers and ${(machine.memory_mib / 1024).toFixed(0)} GiB of archive on ${machine.name}. The run, its workers and the live view share ${(machine.memory_max_mib / 1024).toFixed(0)} GiB.`;
  };
  limits();
  const form = element(
    "form",
    { className: "native-form" },
    element("div", { className: "native-summary" }, preview, element("div", {}, summary, back)),
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
  const title = element("h3", { className: "native-runs-title", textContent: "Running now" });
  const root = element("div", { className: "native-panel-body" }, form, title, list);
  let timer = 0, launched = null;
  const draw = (runs) => {
    const mine = runs.find((run) => run.id === launched);
    if (mine?.state === "running" && mine.watch) location.assign(mine.watch);
    if (mine?.state === "failed") message.textContent = mine.message;
    const active = runs.filter((run) => run.state === "running" || run.state === "starting");
    title.hidden = list.hidden = !active.length;
    list.replaceChildren(
      ...active.map((run) => {
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
  form.onsubmit = (event) => {
    event.preventDefault();
    message.textContent = "";
    start.disabled = true;
    post("/launcher/runs", {
      machine: state.machine,
      game: state.game,
      ...(state.stage === null ? {} : { stage: state.stage }),
      workers: Number(workers.value),
      memory_mib: Math.round(Number(memory.value) * 1024),
      seed: Number(seed.value),
      hours: Number(hours.value),
    })
      .then((run) => ((launched = run.id), refresh()), (error) => (message.textContent = error.message))
      .finally(() => (start.disabled = false));
  };
  return {
    element: root,
    machines: options.machines.map((machine) => machine.name),
    available: (id) => available.has(id),
    setLevel(id, level, image, onBack) {
      state.game = id;
      state.stage = id.startsWith("famidash") && level ? level.id : null;
      const picture = image?.toDataURL ? element("img", { src: image.toDataURL(), alt: "" }) : image?.cloneNode(true);
      preview.replaceChildren(...(picture ? [picture] : []));
      summary.textContent = level ? `${label(id)} · from ${level.label}` : label(id);
      back.onclick = onBack;
    },
    setMachine(name) {
      state.machine = name;
      limits();
    },
    activate() {
      this.deactivate();
      refresh();
      timer = setInterval(refresh, REFRESH_MS);
    },
    deactivate() {
      clearInterval(timer);
    },
  };
}
