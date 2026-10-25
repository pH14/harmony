// SPDX-License-Identifier: AGPL-3.0-or-later
import { run } from "./main.js";

const SOFT_SWITCHES_BEFORE_RELOAD = 12;

const inertWorker = () => ({ postMessage() {}, terminate() {}, addEventListener() {}, removeEventListener() {}, onmessage: null, onerror: null });
const inertObserver = () => ({ observe() {}, unobserve() {}, disconnect() {} });
const FIND_ONE = new Set(["getElementById", "querySelector"]);
const FIND_MANY = new Set(["querySelectorAll", "getElementsByClassName", "getElementsByTagName", "getElementsByName"]);

function readThrough(target, prop) {
  const value = Reflect.get(target, prop, target);
  return typeof value === "function" ? value.bind(target) : value;
}

export class Scope {
  constructor(navigate) {
    this.alive = true;
    this.navigate = navigate;
    this.cleanups = [];
    this.hooks = [];
    this.timers = new Set();
    this.frames = new Set();
    this.workers = new Set();
    this.observers = new Set();
    const scope = this;
    const listen = (target) => {
      const add = target.addEventListener.bind(target), remove = target.removeEventListener.bind(target);
      return (type, listener, options) => {
        if (!scope.alive) return;
        add(type, listener, options);
        scope.cleanups.push(() => remove(type, listener, options));
      };
    };
    this.window = new Proxy(window, {
      get: (target, prop) => (prop === "addEventListener" ? listen(target) : readThrough(target, prop)),
      set(target, prop, value) {
        if (!(prop in target)) scope.cleanups.push(() => delete target[prop]);
        target[prop] = value;
        return true;
      },
    });
    this.document = new Proxy(document, {
      get: (target, prop) => {
        if (prop === "addEventListener") return listen(target);
        if (!scope.alive && FIND_ONE.has(prop)) return () => document.createElement("div");
        if (!scope.alive && FIND_MANY.has(prop)) return () => [];
        return readThrough(target, prop);
      },
      set(target, prop, value) {
        target[prop] = value;
        return true;
      },
    });
    this.addEventListener = listen(window);
    this.setTimeout = (callback, delay, ...rest) => {
      if (!scope.alive) return 0;
      const id = setTimeout(() => {
        scope.timers.delete(id);
        callback(...rest);
      }, delay);
      scope.timers.add(id);
      return id;
    };
    this.clearTimeout = (id) => {
      scope.timers.delete(id);
      clearTimeout(id);
    };
    this.requestAnimationFrame = (callback) => {
      if (!scope.alive) return 0;
      const id = requestAnimationFrame((time) => {
        scope.frames.delete(id);
        callback(time);
      });
      scope.frames.add(id);
      return id;
    };
    this.cancelAnimationFrame = (id) => {
      scope.frames.delete(id);
      cancelAnimationFrame(id);
    };
    this.matchMedia = (query) => {
      const list = matchMedia(query);
      list.addEventListener = listen(list);
      return list;
    };
    this.Worker = function Worker(...args) {
      if (!scope.alive) return inertWorker();
      const worker = new globalThis.Worker(...args);
      scope.workers.add(worker);
      return worker;
    };
    this.ResizeObserver = function ResizeObserver(callback) {
      if (!scope.alive) return inertObserver();
      const observer = new globalThis.ResizeObserver(callback);
      scope.observers.add(observer);
      return observer;
    };
  }

  onDispose(hook) {
    this.hooks.push(hook);
  }

  dispose() {
    if (!this.alive) return;
    this.alive = false;
    for (const id of this.timers) clearTimeout(id);
    for (const id of this.frames) cancelAnimationFrame(id);
    for (const worker of this.workers) worker.terminate();
    for (const observer of this.observers) observer.disconnect();
    for (const cleanup of [...this.cleanups].reverse()) cleanup();
    for (const hook of this.hooks) hook();
  }
}

let scope = null;
let softSwitches = 0;
const root = document.documentElement;

async function boot() {
  scope = new Scope(navigate);
  try {
    await run(scope);
  } catch (error) {
    if (scope.alive) throw error;
  }
}

function reboot(url) {
  root.dataset.switching = "";
  scope?.dispose();
  if (url) history.pushState(null, "", url);
  boot().catch(() => location.reload()).finally(() => delete root.dataset.switching);
}

function navigate(target, { replace = false } = {}) {
  const url = new URL(target, location.href);
  if (url.origin !== location.origin || softSwitches >= SOFT_SWITCHES_BEFORE_RELOAD) {
    location.assign(url);
    return;
  }
  softSwitches++;
  if (replace) history.replaceState(null, "", url);
  reboot(replace ? null : url);
}

addEventListener("popstate", () => reboot(null));
boot();
