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

export const CAMERA_VIEWPORT = { phone: { width: 640, height: 320 }, desktop: { width: 1280, height: 320 } };

export function roomPanels(width, height) {
  return [{ index: 0, x: 0, y: 0, width, height, viewport: true }];
}
export function panelContains(panel, point) {
  return point.x >= panel.x && point.x < panel.x + panel.width &&
    point.y >= panel.y && point.y < panel.y + panel.height;
}
export function panelCenter(panel, zoom, point, viewport) {
  const center = viewCenter(panel.width, zoom, { x: point.x - panel.x, y: point.y - panel.y }, panel.height, viewport);
  return { x: center.x + panel.x, y: center.y + panel.y };
}
