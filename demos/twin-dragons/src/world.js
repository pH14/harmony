// SPDX-License-Identifier: AGPL-3.0-or-later
export function project(observation, map) {
  if (!map) return observation;
  return { ...observation, x: observation.x + 8, y: observation.y + 8 };
}
export function completedLevels(observation, from = 0) {
  const result = [];
  for (let id = from; id < (observation?.cleared ?? 0); id++) result.push(id);
  return result;
}
export function mergeProgress(progress, observation, from = 0) {
  for (const id of completedLevels(observation, from)) progress.add(id);
  return progress;
}
export function isMapEvidence(observation, levels) {
  return (
    !observation.loading &&
    observation.health > 0 &&
    levels.some((level) => level.rooms.includes(observation.level))
  );
}

export function finishedLevel(observation, level) {
  return level != null && (observation?.cleared ?? 0) > level ? level : null;
}

export function followExit(path, from, to, levels) {
  if (from !== path.level || to === from || !levels.some((level) => level.id === to)) return false;
  path.level = to;
  if (!path.levels.includes(to)) path.levels.push(to);
  return true;
}
