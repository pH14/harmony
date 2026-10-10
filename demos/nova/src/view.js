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

export function roomPanels(width, height, availableWidth) {
  const vertical = height > 672;
  const span = Math.max(512, Math.min(1280, Math.floor(availableWidth / 256) * 256));
  const columns = width > 1536 || (vertical && width > span) ? Math.ceil(width / span) : 1;
  const rows = vertical ? Math.ceil(height / 448) : 1;
  const w = Math.ceil(width / columns / 32) * 32;
  const h = Math.ceil(height / rows / 224) * 224;
  return Array.from({ length: columns * rows }, (_, index) => {
    const x = (index % columns) * w, y = Math.floor(index / columns) * h;
    return { index, x, y, width: Math.min(w, width - x), height: Math.min(h, height - y), vertical };
  });
}
export function panelContains(panel, point) {
  return point.x >= panel.x && point.x < panel.x + panel.width &&
    point.y >= panel.y && point.y < panel.y + panel.height;
}
export function panelCenter(panel, zoom, point, viewport) {
  const center = viewCenter(panel.width, zoom, { x: point.x - panel.x, y: point.y - panel.y }, panel.height, viewport);
  return { x: center.x + panel.x, y: center.y + panel.y };
}
