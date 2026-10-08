// SPDX-License-Identifier: AGPL-3.0-or-later
export function viewCenter(
  width,
  zoom,
  point,
  height = 224,
  viewport = { width, height },
) {
  const scale =
    Math.min(viewport.width / width, viewport.height / height) * zoom;
  const clamp = (size, half, value) =>
    half * 2 >= size ? size / 2 : Math.max(half, Math.min(size - half, value));
  return {
    x: clamp(width, viewport.width / (2 * scale), point.x),
    y: clamp(height, viewport.height / (2 * scale), point.y),
  };
}
