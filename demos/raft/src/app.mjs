// SPDX-License-Identifier: AGPL-3.0-or-later
import { readEvidence, atTime, heat, command } from "./evidence.mjs";
const $ = (s) => document.querySelector(s),
  escape = (s) =>
    s.replace(
      /[&<>"']/g,
      (c) =>
        ({
          "&": "&amp;",
          "<": "&lt;",
          ">": "&gt;",
          '"': "&quot;",
          "'": "&#39;",
        })[c],
    );
let branch = "failure",
  time = 0,
  playing = !matchMedia("(prefers-reduced-motion: reduce)").matches,
  last = performance.now(),
  selected = 0,
  runtime,
  liveCount = 0;
const reports = Object.fromEntries(
  await Promise.all(
    ["failure", "investigation", "quorum"].map(async (name) => [
      name,
      readEvidence(await (await fetch(`public/evidence/${name}.json`)).json()),
    ]),
  ),
);
const source = await (await fetch("workload/raft.c")).text();
$("#code").innerHTML = source
  .split("\n")
  .map(
    (text, i) =>
      `<button class="code-line" data-line="${i + 1}" aria-label="Line ${i + 1}: ${escape(text)}"><span class="ln">${i + 1}</span><span class="source">${escape(text)}</span></button>`,
  )
  .join("");
const rows = [...document.querySelectorAll(".code-line")];
function stepTime(step) {
  const t = reports[branch].steps.find((s) => s.step === step);
  return t ? Number(t.observation.moment) / 1e9 : 0;
}
function phase() {
  return (
    reports[branch].steps.filter((s) => stepTime(s.step) <= time).at(-1)
      ?.step || 0
  );
}
function showCommand(action, step = 1) {
  const source = action === "shell" ? branch : "failure";
  if (action === "shell")
    step =
      reports[branch].steps.find((s) => s.step === step)?.originalStep ?? 0;
  $("#command").textContent = command(source, step, action);
}
function focus(line) {
  rows[line - 1]?.scrollIntoView({ block: "center" });
}
function render() {
  const e = reports[branch],
    visible = atTime(e, time),
    p = phase();
  $("#scrub").value = time;
  $("#clock").textContent = `${time.toFixed(2)}s`;
  $("#time").textContent = `${time.toFixed(2)} / 3.00s`;
  $("#play").textContent = playing ? "Ⅱ" : "▶";
  $("#play").setAttribute(
    "aria-label",
    playing ? "Pause playback" : "Play execution",
  );
  $("#location").textContent = `${branch} / after action ${p}`;
  const bad = e.replay.violations.length > 0 && p >= 4;
  $("#assertion").classList.toggle("failed", bad);
  $("#verdict").textContent = bad
    ? "Violated · counter=1 is missing"
    : branch.startsWith("quorum") && p >= 4
      ? "No violation in this replay · write never acknowledged"
      : "Watching this execution";
  $("#cluster").classList.toggle("partition", p >= 2);
  $("#cluster").classList.toggle("paused", !playing);
  $("#client strong").textContent = visible.some(
    (x) => x.kind === "acknowledged",
  )
    ? "counter=1 acknowledged"
    : "no acknowledgment";
  for (const id of ["A", "B", "C"]) {
    const n = $(`[data-node="${id}"]`),
      events = visible.filter((x) => x.node === id),
      state = events.at(-1),
      dead =
        id === "A" && time > stepTime(3) && e.steps.some((s) => s.step === 4);
    const role = state?.role ?? (id === "A" ? 2 : 0);
    n.classList.toggle("dead", dead);
    n.classList.toggle("leader", !dead && role === 2);
    n.querySelector("small").textContent = dead
      ? "kill injected"
      : state
        ? ["follower", "candidate", "leader"][role]
        : "starting";
    n.querySelector("em").textContent =
      `counter ${events.some((x) => ["append", "replicated"].includes(x.kind)) ? 1 : 0}`;
  }
  const logs = selected ? visible.filter((x) => x.line === selected) : visible;
  $("#count").textContent = selected ? `line ${selected}` : `${logs.length}`;
  const html = logs
    .slice(-70)
    .map(
      (x) =>
        `<p class="${escape(x.kind)}"><b>${x.time.toFixed(3)} ${escape(x.node)}</b> ${escape(x.message)}</p>`,
    )
    .join("");
  if ($("#logs").innerHTML !== html) {
    $("#logs").innerHTML = html || "<p>No captured events at this point.</p>";
    $("#logs").scrollTop = $("#logs").scrollHeight;
  }
  for (const row of rows) {
    const line = Number(row.dataset.line),
      h = heat(e.events, time, line),
      seen = visible.some((x) => x.line === line);
    row.style.background =
      h > 0.03
        ? `hsla(${Math.max(5, 135 - h * 65)},65%,45%,${Math.min(0.43, 0.09 + h * 0.13)})`
        : seen
          ? "#172d24"
          : "";
    row.classList.toggle("active", line === selected);
  }
  document
    .querySelectorAll("#phases button")
    .forEach((b) =>
      b.classList.toggle("selected", Number(b.dataset.step) === p),
    );
  $("#metrics").textContent =
    `${visible.length} events · ${new Set(visible.map((x) => x.line)).size} source sites`;
}
function jump(t) {
  playing = false;
  time = Math.min(3, Math.max(0, t));
  render();
}
$("#play").onclick = () => {
  if (time >= 3) time = 0;
  playing = !playing;
  render();
};
$("#scrub").oninput = (e) => jump(Number(e.target.value));
document.querySelectorAll("#phases button").forEach(
  (b) =>
    (b.onclick = () => {
      selected = 0;
      jump(stepTime(Number(b.dataset.step)));
    }),
);
document
  .querySelectorAll(".file")
  .forEach((b) => (b.onclick = () => focus(Number(b.dataset.line))));
$("#code").onclick = (e) => {
  const row = e.target.closest("[data-line]");
  if (!row) return;
  selected =
    selected === Number(row.dataset.line) ? 0 : Number(row.dataset.line);
  playing = false;
  render();
};
async function investigate(action) {
  showCommand(action, action === "shell" ? phase() : 1);
  playing = false;
  if (runtime && !runtime.ready) {
    $("#explanation").textContent =
      runtime.error ||
      "The live machine is running a command. Its output is in the terminal below.";
    return;
  }
  if (runtime?.ready) {
    $("#command").textContent = $("#command").textContent.replace(
      /--name ([a-z-]+)$/,
      `--name $1-${++liveCount}`,
    );
    document
      .querySelectorAll(".actions button")
      .forEach((b) => (b.disabled = true));
    $("#explanation").textContent =
      "Restoring a real machine and running the displayed command. Software emulation can take several minutes; follow the terminal below.";
    try {
      await runtime.command($("#command").textContent);
    } catch (error) {
      $("#explanation").textContent = error.message;
    } finally {
      document
        .querySelectorAll(".actions button")
        .forEach((b) => (b.disabled = false));
    }
    return;
  }
  if (action === "shell") {
    $("#explanation").textContent =
      "A shell requires the live Linux runtime. Start live Linux above; recorded playback cannot execute commands.";
    return;
  }
  branch = action === "trace" ? "investigation" : "quorum";
  selected = 0;
  time = stepTime(1);
  playing = true;
  $("#explanation").textContent =
    action === "trace"
      ? "Recorded branch: the real CLI restored action 1, ran /app/control trace, and replayed the suffix. Orange trace events expose the missing replication acknowledgments."
      : "Recorded counterfactual: the real CLI enabled the quorum check before replay. A never acknowledges the isolated write, so this execution has no durability violation. This is one demonstrated case, not proof of Raft correctness.";
  focus(action === "trace" ? 94 : 106);
  render();
}
$("#original").onclick = () => {
  branch = "failure";
  selected = 0;
  time = 0;
  playing = true;
  showCommand("trace");
  $("#explanation").textContent =
    "Original failure: follow the acknowledged write through the leader change.";
  render();
};
$("#trace").onclick = () => investigate("trace");
$("#quorum").onclick = () => investigate("quorum");
$("#shell").onclick = () => investigate("shell");
$("#copy").onclick = async () => {
  await navigator.clipboard.writeText($("#command").textContent);
  $("#copy").textContent = "Copied";
  setTimeout(() => ($("#copy").textContent = "Copy"), 1500);
};
$("#boot").onclick = async () => {
  $(".console").hidden = false;
  $("#boot").disabled = true;
  $("#runtime-status").textContent = "Loading browser Linux…";
  try {
    const { boot } = await import("./runtime.mjs");
    runtime = await boot(
      $("#terminal"),
      (text) => ($("#runtime-status").textContent = text),
      (name, report) => {
        reports[name] = readEvidence(report);
        branch = name;
        time = 0;
        selected = 0;
        playing = true;
        $("#explanation").textContent =
          `Live report received from ${name}. All commands and machine state are local to this browser.`;
        render();
      },
    );
    if (!runtime) return;
    $("#mode").textContent = "Live Linux · local to this browser";
  } catch (error) {
    $("#runtime-status").textContent = error.message;
    $("#boot").disabled = false;
    $("#boot").textContent = "Reload Linux";
    $("#boot").onclick = () => {
      sessionStorage.removeItem("harmony-isolation-attempt");
      sessionStorage.setItem("harmony-start-linux", "1");
      location.reload();
    };
  }
};
document.querySelectorAll(".tip").forEach(
  (b) =>
    (b.onclick = () => {
      if (runtime) runtime.send(b.dataset.command);
    }),
);
function frame(now) {
  const dt = Math.min((now - last) / 1000, 0.1);
  last = now;
  if (playing) {
    time += dt * 0.18;
    if (time >= 3) {
      time = 3;
      playing = false;
    }
    render();
  }
  requestAnimationFrame(frame);
}
render();
focus(97);
requestAnimationFrame(frame);

if (sessionStorage.getItem("harmony-start-linux")) {
  sessionStorage.removeItem("harmony-start-linux");
  $("#boot").click();
}
