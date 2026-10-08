// SPDX-License-Identifier: AGPL-3.0-or-later
export function viewCenter(width, zoom, point) {
  const halfWidth = width / (2 * zoom),
    halfHeight = 112 / zoom;
  return {
    x: Math.max(halfWidth, Math.min(width - halfWidth, point.x)),
    y: Math.max(halfHeight, Math.min(224 - halfHeight, point.y)),
  };
}
