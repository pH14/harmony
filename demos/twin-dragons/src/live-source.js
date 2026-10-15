// SPDX-License-Identifier: AGPL-3.0-or-later
const STATE_LIMIT = 12;

function decodeBase64(text) {
  const bytes = atob(text), out = new Uint8Array(bytes.length);
  for (let i = 0; i < bytes.length; i++) out[i] = bytes.charCodeAt(i);
  return out;
}

function decodeState(state) {
  return { ...state, snapshot: state.snapshot ? decodeBase64(state.snapshot) : null };
}

export function liveSearch(source, gameId) {
  const base = new URL(source ? `${source.replace(/\/$/, "")}/` : ".", location.href);
  const events = new EventSource(new URL("live/events", base));
  const port = { onmessage: null, onerror: null };
  let paused = false;
  const deliver = (data) => port.onmessage?.({ data });
  const fail = (message) => deliver({ type: "error", message });
  events.onmessage = ({ data }) => {
    const message = JSON.parse(data);
    if (message.type === "ready") {
      if (message.game !== gameId) return fail(`The live campaign is running ${message.game}. Open this page with game=${message.game}.`);
      message.state = decodeState(message.state);
    } else if (message.type === "batch") {
      if (paused) Object.assign(message, { points: [], motion: [], finishes: [], exits: [] });
      message.motion = message.motion.map((trail) => Uint16Array.from(trail));
    }
    deliver(message);
  };
  events.onerror = () => {
    if (events.readyState === EventSource.CLOSED) fail("The live campaign closed its stream.");
  };
  port.postMessage = (message) => {
    if (message.type === "states") {
      const ids = message.ids.slice(0, STATE_LIMIT).join(",");
      fetch(new URL(`live/states?ids=${ids}&request=${message.request}`, base))
        .then((response) => (response.ok ? response.json() : { states: [], missing: message.ids }))
        .then((reply) => deliver({ type: "states", request: message.request, states: reply.states.map(decodeState), missing: reply.missing ?? [] }))
        .catch(() => deliver({ type: "states", request: message.request, states: [], missing: [] }));
    } else if (message.type === "pause") {
      paused = true;
      deliver({ type: "paused" });
    } else if (message.type === "resume") {
      paused = false;
    } else if (message.type === "fork") {
      deliver({ type: "branch-error", message: "Branches run in this browser. A live campaign view cannot branch." });
    }
  };
  port.terminate = () => events.close();
  return port;
}
