// SPDX-License-Identifier: AGPL-3.0-or-later
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
