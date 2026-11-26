// SPDX-License-Identifier: MIT
// Shared helpers for the R3 canvas reftests.

/** Builds an offscreen canvas, the way Excalidraw caches per-element scenes. */
export function offscreen(width, height) {
  const canvas = document.createElement("canvas");
  canvas.width = width;
  canvas.height = height;
  return canvas;
}

/** Draws a 2D checkerboard into a context. */
export function checker(ctx, width, height, cell = 8, a = "#f8fafc", b = "#cbd5e1") {
  for (let y = 0; y < height; y += cell) {
    for (let x = 0; x < width; x += cell) {
      ctx.fillStyle = ((x / cell) | 0) % 2 === ((y / cell) | 0) % 2 ? a : b;
      ctx.fillRect(x, y, cell, cell);
    }
  }
}

/** Converts a perfect-freehand outline to a Path2D. */
export function outlineToPath(outline) {
  const path = new Path2D();
  outline.forEach(([x, y], index) => {
    if (index === 0) path.moveTo(x, y);
    else path.lineTo(x, y);
  });
  path.closePath();
  return path;
}
