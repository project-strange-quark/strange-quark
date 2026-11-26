// SPDX-License-Identifier: MIT
// Deterministic RGBA PNG pixel comparator with per-channel and ratio tolerances.

import { PNG } from "pngjs";

export const DEFAULT_TOLERANCE = Object.freeze({
  maxChannelDelta: 2,
  maxDiffRatio: 0.005,
  maxDiffPixels: null,
});

/**
 * Compares two PNG buffers pixel-by-pixel in sRGB RGBA space.
 * Returns pass/fail metrics plus an optional diff PNG buffer highlighting
 * differing pixels in magenta over a dimmed grayscale baseline.
 */
export function comparePngBuffers(expectedBuf, actualBuf, options = {}) {
  const maxChannelDelta = options.maxChannelDelta ?? DEFAULT_TOLERANCE.maxChannelDelta;
  const maxDiffRatio = options.maxDiffRatio ?? DEFAULT_TOLERANCE.maxDiffRatio;
  const maxDiffPixels = options.maxDiffPixels ?? DEFAULT_TOLERANCE.maxDiffPixels;
  const generateDiff = options.generateDiff ?? true;

  const expected = PNG.sync.read(expectedBuf);
  const actual = PNG.sync.read(actualBuf);

  if (expected.width !== actual.width || expected.height !== actual.height) {
    return {
      pass: false,
      reason: `dimension mismatch: expected ${expected.width}x${expected.height}, got ${actual.width}x${actual.height}`,
      width: actual.width,
      height: actual.height,
      totalPixels: expected.width * expected.height,
      diffPixels: expected.width * expected.height,
      diffRatio: 1,
      maxObservedDelta: 255,
      meanChannelError: 255,
      diffBounds: null,
      diffPng: null,
    };
  }

  const { width, height } = expected;
  const totalPixels = width * height;
  const diffImg = generateDiff ? new PNG({ width, height }) : null;

  let diffPixels = 0;
  let maxObservedDelta = 0;
  let totalChannelError = 0;
  let minX = width;
  let minY = height;
  let maxX = -1;
  let maxY = -1;

  for (let y = 0; y < height; y++) {
    for (let x = 0; x < width; x++) {
      const idx = (y * width + x) * 4;
      const dr = Math.abs(expected.data[idx] - actual.data[idx]);
      const dg = Math.abs(expected.data[idx + 1] - actual.data[idx + 1]);
      const db = Math.abs(expected.data[idx + 2] - actual.data[idx + 2]);
      const da = Math.abs(expected.data[idx + 3] - actual.data[idx + 3]);
      const delta = Math.max(dr, dg, db, da);

      if (delta > maxObservedDelta) maxObservedDelta = delta;
      totalChannelError += (dr + dg + db + da) / 4;

      const isDiff = delta > maxChannelDelta;
      if (isDiff) {
        diffPixels++;
        if (x < minX) minX = x;
        if (y < minY) minY = y;
        if (x > maxX) maxX = x;
        if (y > maxY) maxY = y;
      }

      if (diffImg) {
        if (isDiff) {
          // Highlight mismatched pixels in vivid magenta.
          diffImg.data[idx] = 255;
          diffImg.data[idx + 1] = 0;
          diffImg.data[idx + 2] = 255;
          diffImg.data[idx + 3] = 255;
        } else {
          // Dimmed grayscale baseline so diff location is clear.
          const lum = Math.round(
            (expected.data[idx] * 0.299 +
              expected.data[idx + 1] * 0.587 +
              expected.data[idx + 2] * 0.114) *
              0.3,
          );
          diffImg.data[idx] = lum;
          diffImg.data[idx + 1] = lum;
          diffImg.data[idx + 2] = lum;
          diffImg.data[idx + 3] = 255;
        }
      }
    }
  }

  const diffRatio = totalPixels === 0 ? 0 : diffPixels / totalPixels;
  const meanChannelError = totalPixels === 0 ? 0 : totalChannelError / totalPixels;
  const withinRatio = diffRatio <= maxDiffRatio;
  const withinCount = maxDiffPixels == null || diffPixels <= maxDiffPixels;
  const pass = withinRatio && withinCount;

  const diffBounds =
    diffPixels > 0
      ? { x: minX, y: minY, width: maxX - minX + 1, height: maxY - minY + 1 }
      : null;

  return {
    pass,
    reason: pass
      ? null
      : `diffPixels=${diffPixels} (${(diffRatio * 100).toFixed(3)}%), maxDelta=${maxObservedDelta}`,
    width,
    height,
    totalPixels,
    diffPixels,
    diffRatio,
    maxObservedDelta,
    meanChannelError,
    diffBounds,
    diffPng: diffImg && !pass ? PNG.sync.write(diffImg) : null,
  };
}

/**
 * Reads a PNG buffer and returns the pixel RGBA tuple at (x, y).
 */
export function samplePngPixel(pngBuf, x, y) {
  const png = PNG.sync.read(pngBuf);
  if (x < 0 || y < 0 || x >= png.width || y >= png.height) {
    throw new RangeError(`(${x}, ${y}) out of bounds for ${png.width}x${png.height}`);
  }
  const idx = (y * png.width + x) * 4;
  return [png.data[idx], png.data[idx + 1], png.data[idx + 2], png.data[idx + 3]];
}
