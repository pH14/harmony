// SPDX-License-Identifier: AGPL-3.0-or-later
export function project(observation, map) {
  if (!map) return observation;
  return {
    ...observation,
    x: observation.x % map.width,
    y: Math.floor(observation.x / map.width) * 224 + observation.y,
  };
}
export function completedLevels(bytes = []) {
  const result = [];
  for (let id = 0; id < 40; id++)
    if ((bytes[Math.floor(id / 8)] || 0) & (1 << (id % 8))) result.push(id);
  return result;
}
export function mergeProgress(progress, observation) {
  for (const id of completedLevels(observation.cleared_levels))
    progress.add(id);
  return progress;
}
