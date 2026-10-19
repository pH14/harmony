// SPDX-License-Identifier: AGPL-3.0-or-later
const LOOPBACK = new Set(["127.0.0.1", "localhost", "[::1]"]);
const REFRESH_MS = 3000;
const LAUNCHER_HEADER = "X-Harmony-Launcher";

function element(tag, props = {}, ...children) {
  const node = Object.assign(document.createElement(tag), props);
  node.append(...children);
  return node;
}

function field(label, input) {
  return element("label", { className: "native-field" }, element("span", { textContent: label }), input);
}

async function post(path, body = {}) {
  const response = await fetch(path, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(body) });
  const reply = await response.json().catch(() => ({}));
  if (!response.ok) throw new Error(reply.error || `The launcher answered ${response.status}`);
  return reply;
}

function describe(run) {
  const minutes = Math.round((Date.now() / 1000 - run.started) / 60);
  if (run.external) return `${run.game} on ${run.machine} · ${run.directory} · ${minutes} min`;
  const memory = `${(run.memory_mib / 1024).toFixed(1)} GiB`;
  return `${run.game} on ${run.machine} · ${run.workers} workers · ${memory} · seed ${run.seed} · ${minutes} min`;
}

export async function nativeRuns(gameId, mount) {
  if (!LOOPBACK.has(location.hostname)) return;
  const page = await fetch(location.href, { method: "HEAD" }).catch(() => null);
  if (!page?.headers.has(LAUNCHER_HEADER)) return;
  const options = await fetch("/launcher/machines").then((response) => (response.ok ? response.json() : null)).catch(() => null);
  if (!options?.machines?.length) return;
  const machine = element("select"), game = element("select");
  for (const entry of options.machines) machine.append(element("option", { value: entry.name, textContent: entry.name }));
  for (const id of options.games) game.append(element("option", { value: id, textContent: id, selected: id === gameId }));
  const workers = element("input", { type: "number", min: 1, value: 4 }),
    memory = element("input", { type: "number", min: 0.25, step: 0.25, value: 4 }),
    seed = element("input", { type: "number", min: 0, value: 1 }),
    hours = element("input", { type: "number", min: 0.5, max: 72, step: 0.5, value: 12 }),
    start = element("button", { type: "submit", textContent: "Start run" }),
    message = element("p", { className: "native-message", role: "status" }),
    list = element("ul", { className: "native-runs" });
  const limits = () => {
    const entry = options.machines.find((m) => m.name === machine.value);
    workers.max = entry.workers;
    memory.max = entry.memory_mib / 1024;
  };
  machine.onchange = limits;
  limits();
  const form = element(
    "form",
    { className: "native-form" },
    field("Machine", machine),
    field("Game", game),
    field("Workers", workers),
    field("Memory cap (GiB)", memory),
    field("Seed", seed),
    field("Time limit (hours)", hours),
    start,
  );
  const dialog = element(
    "dialog",
    { id: "native-run", className: "native-run" },
    element("button", { className: "close", type: "button", ariaLabel: "Close native runs", textContent: "×", onclick: () => dialog.close() }),
    element("h2", { textContent: "Native runs" }),
    form,
    message,
    list,
  );
  document.body.append(dialog);
  const render = (runs) => {
    list.replaceChildren(
      ...runs.map((run) => {
        const actions = element("span", { className: "native-actions" });
        if (run.watch && run.state === "running") actions.append(element("a", { href: run.watch, textContent: "Watch" }));
        if (!run.external && (run.state === "running" || run.state === "starting")) {
          actions.append(
            element("button", {
              type: "button",
              textContent: "Stop",
              onclick: () => post(`/launcher/runs/${run.id}/stop`).then(refresh, (error) => (message.textContent = error.message)),
            }),
          );
        }
        const state = run.message ? `${run.state}: ${run.message}` : run.state;
        return element("li", {}, element("span", { textContent: describe(run) }), element("span", { className: "native-state", textContent: state }), actions);
      }),
    );
  };
  const refresh = () =>
    fetch("/launcher/runs")
      .then((response) => response.json())
      .then(({ runs }) => render(runs))
      .catch(() => (message.textContent = "The launcher stopped answering."));
  let timer = 0;
  dialog.addEventListener("close", () => clearInterval(timer));
  form.onsubmit = (event) => {
    event.preventDefault();
    message.textContent = "";
    start.disabled = true;
    post("/launcher/runs", {
      machine: machine.value,
      game: game.value,
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
