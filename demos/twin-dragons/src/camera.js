// SPDX-License-Identifier: AGPL-3.0-or-later
import { panelCenter } from "./view.js";
export function resumeFollowing(camera) {
  Object.assign(camera, { overview: false, manual: 0, held: 0, only: null,
    targets: new Map(), follow: true, focus: null });
}

export function resetRetreat(camera, activeRoom = null) {
  for (const [room, target] of camera.targets) if (room !== activeRoom) target.behind = 0;
}

export function trackingGoal(held, point, points, now, progress, retreatMs = 1500) {
  if (!held) return point;
  if (!point) {
    held.behind = 0;
    return held.goal ?? null;
  }
  if (!held.goal || progress(point) >= progress(held.goal) - 32) {
    held.behind = 0;
    return point;
  }
  const goal = held.goal, ahead = progress(goal);
  const near = points.filter((p) => Math.hypot(p.x + 8 - goal.x, p.y - goal.y) < 192 || progress({ x: p.x + 8, y: p.y }) > ahead).length;
  if (near >= 2) held.behind = 0;
  else held.behind ||= now;
  return !held.behind || now - held.behind < retreatMs ? goal : point;
}

export function followView(view, point, panel, viewport, targetZoom, seconds, fixedZoom = false) {
  const dt = Math.max(0, Math.min(0.1, seconds));
  if (!dt) return { ...view };
  if (fixedZoom) {
    const zoom = view.zoom + (targetZoom - view.zoom) * (1 - Math.exp(-dt / 0.6)), ease = 1 - Math.exp(-dt / 0.25);
    return { ...panelCenter(panel, zoom, { x: view.x + (point.x - view.x) * ease, y: view.y + (point.y - view.y) * ease }, viewport), zoom };
  }
  const fit = Math.min(viewport.width / panel.width, viewport.height / panel.height);
  const reach = panelCenter(panel, view.zoom, point, viewport);
  const distance = Math.hypot((reach.x - view.x) * fit / viewport.width,
    (reach.y - view.y) * fit / viewport.height);
  const framing = Math.max(1, Math.min(targetZoom, distance ? 0.4 / distance : targetZoom));
  const zoom = view.zoom + (framing - view.zoom) * (1 - Math.exp(-dt / (framing < view.zoom ? 0.15 : 0.6)));
  const ease = framing < view.zoom && distance * view.zoom > 0.5 ? 0 : 1 - Math.exp(-dt / 0.35);
  const growth = Math.max(0, view.zoom / zoom - 1) / (2 * fit * view.zoom);
  const anchored = (from, to, span) => from + Math.sign(to - from) * Math.min(Math.abs(to - from), growth * span);
  const base = { x: anchored(view.x, point.x, viewport.width), y: anchored(view.y, point.y, viewport.height) };
  const center = panelCenter(panel, zoom,
    { x: base.x + (point.x - base.x) * ease, y: base.y + (point.y - base.y) * ease }, viewport);
  const oldScale = fit * view.zoom, nextScale = fit * zoom;
  let movement = 0;
  for (const x of [-0.5, 0.5]) for (const y of [-0.5, 0.5]) {
    const worldX = view.x + x * viewport.width / oldScale;
    const worldY = view.y + y * viewport.height / oldScale;
    movement = Math.max(movement, Math.hypot(
      ((worldX - center.x) * nextScale - x * viewport.width) / viewport.width,
      ((worldY - center.y) * nextScale - y * viewport.height) / viewport.height));
  }
  const mix = movement ? Math.min(1, 2 * dt / movement) : 1;
  const scale = oldScale + (nextScale - oldScale) * mix;
  return {
    x: (view.x * oldScale + (center.x * nextScale - view.x * oldScale) * mix) / scale,
    y: (view.y * oldScale + (center.y * nextScale - view.y * oldScale) * mix) / scale,
    zoom: scale / fit,
  };
}
