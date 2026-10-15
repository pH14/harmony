// SPDX-License-Identifier: AGPL-3.0-or-later
import { CampaignSearch, CampaignWorker, campaign_genesis } from "../rust/pkg/twin_dragons_browser.js";
import { QUICKNES_REVISION } from "./cores/quicknes.js";

const CORE = 0;

export function coreIdentity(coreSha256) {
  return { label: `quicknes-browser:${QUICKNES_REVISION}`, sha256: coreSha256 };
}

function attach(engine) {
  (globalThis.harmonyCores ??= {})[CORE] = engine;
}

export function campaignGenesis(engine) {
  attach(engine);
  return campaign_genesis(engine.game.id, CORE, engine.rom);
}

export function campaignWorker(engine, identity) {
  attach(engine);
  return new CampaignWorker(engine.game.id, CORE, engine.rom, identity.label, identity.sha256);
}

export function campaignSearch(engine, identity, { seed, root, prefix = [], memoryMiB = 0, executions = 0, record = false }) {
  attach(engine);
  return new CampaignSearch(
    engine.game.id, CORE, engine.rom, identity.label, identity.sha256, seed >>> 0, root,
    JSON.stringify(prefix.map(({ buttons, frames }) => ({ buttons, frames }))),
    memoryMiB, executions, record,
  );
}

export function runCampaign(search, worker, { jobs = Infinity, onJob } = {}) {
  for (let done = 0; done < jobs; done++) {
    const job = search.plan();
    if (!job) return false;
    const outcome = worker.run(job);
    search.admit(outcome, worker.note());
    onJob?.(worker);
  }
  return true;
}
