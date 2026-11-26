// SPDX-License-Identifier: MIT
// The 10 canonical Excalidraw scenes, as .excalidraw files the app opens through
// its own open path. Deterministic ids, seeds and order, so the Chromium
// baselines are reproducible.

import fs from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { PNG } from "pngjs";

const __dirname = path.dirname(fileURLToPath(import.meta.url));
export const fixturesDir = path.resolve(__dirname, "../fixtures");

const NOW = 1735689600000; // 2025-01-01T00:00:00Z, fixed for determinism

let counter = 0;
function id(prefix) {
  counter += 1;
  return `${prefix}-${String(counter).padStart(4, "0")}`;
}

function base(type, overrides) {
  return {
    id: id(type),
    type,
    x: 0,
    y: 0,
    width: 100,
    height: 100,
    angle: 0,
    strokeColor: "#1e1e1e",
    backgroundColor: "transparent",
    fillStyle: "hachure",
    strokeWidth: 2,
    strokeStyle: "solid",
    roughness: 1,
    opacity: 100,
    groupIds: [],
    frameId: null,
    roundness: null,
    seed: 100000 + counter * 7919,
    version: 1,
    versionNonce: counter * 104729,
    isDeleted: false,
    boundElements: null,
    updated: NOW,
    link: null,
    locked: false,
    ...overrides,
  };
}

function rectangle(x, y, width, height, overrides = {}) {
  return base("rectangle", {
    x,
    y,
    width,
    height,
    roundness: { type: 3 },
    backgroundColor: "#a5d8ff",
    fillStyle: "solid",
    ...overrides,
  });
}

function ellipse(x, y, width, height, overrides = {}) {
  return base("ellipse", {
    x,
    y,
    width,
    height,
    roundness: { type: 2 },
    backgroundColor: "#b2f2bb",
    fillStyle: "solid",
    ...overrides,
  });
}

function diamond(x, y, width, height, overrides = {}) {
  return base("diamond", {
    x,
    y,
    width,
    height,
    roundness: { type: 2 },
    backgroundColor: "#ffec99",
    fillStyle: "solid",
    ...overrides,
  });
}

function text(x, y, value, overrides = {}) {
  const fontSize = overrides.fontSize ?? 24;
  const width = Math.max(40, value.length * fontSize * 0.55);
  const height = fontSize * 1.25;
  return base("text", {
    x,
    y,
    width,
    height,
    strokeColor: "#1e1e1e",
    backgroundColor: "transparent",
    fillStyle: "solid",
    text: value,
    originalText: value,
    fontSize,
    fontFamily: 1,
    textAlign: "left",
    verticalAlign: "top",
    containerId: null,
    lineHeight: 1.25,
    baseline: fontSize,
    autoResize: true,
    ...overrides,
  });
}

function arrow(x, y, points, overrides = {}) {
  const xs = points.map(([px]) => px);
  const ys = points.map(([, py]) => py);
  const minX = Math.min(...xs);
  const minY = Math.min(...ys);
  const width = Math.max(...xs) - minX;
  const height = Math.max(...ys) - minY;
  return base("arrow", {
    x: x + minX,
    y: y + minY,
    width,
    height,
    strokeColor: "#1971c2",
    strokeWidth: 2,
    points: points.map(([px, py]) => [px - minX, py - minY]),
    lastCommittedPoint: null,
    startBinding: null,
    endBinding: null,
    startArrowhead: null,
    endArrowhead: "arrow",
    ...overrides,
  });
}

function freedraw(x, y, points, overrides = {}) {
  const xs = points.map(([px]) => px);
  const ys = points.map(([, py]) => py);
  const minX = Math.min(...xs);
  const minY = Math.min(...ys);
  return base("freedraw", {
    x: x + minX,
    y: y + minY,
    width: Math.max(...xs) - minX,
    height: Math.max(...ys) - minY,
    strokeColor: "#e03131",
    strokeWidth: 3,
    fillStyle: "solid",
    points: points.map(([px, py]) => [px - minX, py - minY]),
    pressures: [],
    simulatePressure: true,
    lastCommittedPoint: null,
    ...overrides,
  });
}

function frame(x, y, width, height) {
  return base("frame", {
    x,
    y,
    width,
    height,
    strokeColor: "#868e96",
    backgroundColor: "transparent",
    fillStyle: "solid",
    roundness: { type: 3 },
    name: "Scene frame",
  });
}

function scene(elements, appState = {}, files = {}) {
  return {
    type: "excalidraw",
    version: 2,
    source: "strange-quark",
    elements,
    appState: {
      gridSize: null,
      viewBackgroundColor: "#ffffff",
      theme: "light",
      scrollX: 0,
      scrollY: 0,
      zoom: { value: 1 },
      ...appState,
    },
    files,
  };
}

/** A tiny deterministic PNG (16×16) for the image scene and the clipboard. */
export function tinyPngBase64() {
  // 16x16 RGBA PNG generated from a fixed pattern, base64-encoded.
  const png = new PNG({ width: 16, height: 16 });
  for (let y = 0; y < 16; y += 1) {
    for (let x = 0; x < 16; x += 1) {
      const at = (y * 16 + x) * 4;
      png.data[at] = x < 8 ? 220 : 50;
      png.data[at + 1] = y < 8 ? 120 : 200;
      png.data[at + 2] = (x + y) % 2 === 0 ? 60 : 180;
      png.data[at + 3] = 255;
    }
  }
  return PNG.sync.write(png).toString("base64");
}

function imageElement(x, y, width, height) {
  const fileId = "fixture-image";
  return {
    element: base("image", {
      x,
      y,
      width,
      height,
      strokeColor: "transparent",
      backgroundColor: "transparent",
      fileId,
      status: "saved",
      scale: [1, 1],
      roundness: null,
    }),
    file: {
      id: fileId,
      mimeType: "image/png",
      dataURL: `data:image/png;base64,${tinyPngBase64()}`,
      created: NOW,
      lastRetrieved: NOW,
    },
  };
}

/** The ten scenes, in a fixed order. */
export function canonicalScenes() {
  const scenes = [];

  scenes.push({
    id: "01-basic-shapes",
    note: "rectangle, ellipse and diamond",
    data: scene([
      rectangle(160, 160, 240, 150, { backgroundColor: "#a5d8ff" }),
      ellipse(460, 160, 220, 150, { backgroundColor: "#b2f2bb" }),
      diamond(740, 150, 220, 170, { backgroundColor: "#ffec99" }),
    ]),
  });

  scenes.push({
    id: "02-arrows-labels",
    note: "arrows with text labels",
    data: scene([
      rectangle(140, 260, 180, 100),
      ellipse(880, 250, 200, 120),
      arrow(330, 300, [
        [0, 0],
        [180, -40],
        [340, 10],
      ]),
      text(360, 200, "annotate", { fontSize: 28, strokeColor: "#1971c2" }),
      arrow(700, 300, [
        [0, 0],
        [200, 30],
      ], { strokeColor: "#e03131", endArrowhead: "triangle" }),
    ]),
  });

  scenes.push({
    id: "03-text-styles",
    note: "text sizes and colours",
    data: scene([
      text(160, 140, "Heading one", { fontSize: 48, strokeColor: "#1e1e1e" }),
      text(160, 240, "Body copy at twenty-four", { fontSize: 24, strokeColor: "#495057" }),
      text(160, 320, "Small caption", { fontSize: 16, strokeColor: "#868e96" }),
      text(160, 400, "Accent line", { fontSize: 28, strokeColor: "#1971c2" }),
      text(160, 480, "Multiline first", { fontSize: 22 }),
      text(160, 515, "Multiline second", { fontSize: 22 }),
    ]),
  });

  scenes.push({
    id: "04-freedraw",
    note: "two freedraw strokes",
    data: scene([
      freedraw(180, 260, [
        [0, 60],
        [60, 0],
        [130, 50],
        [210, 10],
        [300, 70],
      ]),
      freedraw(520, 340, [
        [0, 0],
        [60, 80],
        [130, 20],
        [200, 90],
        [280, 30],
      ], { strokeColor: "#7048e8", strokeWidth: 4 }),
    ]),
  });

  scenes.push({
    id: "05-dark-mode",
    note: "dark theme scene",
    data: scene(
      [
        rectangle(180, 200, 260, 160, { backgroundColor: "#4dabf7", strokeColor: "#e9ecef" }),
        ellipse(520, 200, 220, 160, { backgroundColor: "#69db7c", strokeColor: "#e9ecef" }),
        text(200, 120, "Dark mode", { fontSize: 36, strokeColor: "#f8f9fa" }),
      ],
      { theme: "dark", viewBackgroundColor: "#121212" },
    ),
  });

  scenes.push({
    id: "06-groups",
    note: "grouped elements",
    data: scene([
      rectangle(200, 220, 200, 120, { groupIds: ["group-a"] }),
      rectangle(420, 220, 200, 120, { groupIds: ["group-a"], backgroundColor: "#ffec99" }),
      text(240, 380, "A group of two", { fontSize: 26 }),
    ]),
  });

  scenes.push({
    id: "07-rotated-opacity",
    note: "rotation and opacity",
    data: scene([
      rectangle(260, 240, 220, 140, { angle: Math.PI / 9, opacity: 80 }),
      ellipse(620, 250, 220, 150, { angle: -Math.PI / 12, opacity: 55 }),
      text(280, 460, "rotated and faded", { fontSize: 26, opacity: 70 }),
    ]),
  });

  {
    const { element, file } = imageElement(240, 220, 240, 240);
    scenes.push({
      id: "08-image",
      note: "embedded image element",
      data: scene([element, text(540, 260, "embedded image", { fontSize: 28 })], {}, { [file.id]: file }),
    });
  }

  {
    const frameElement = frame(160, 160, 420, 300);
    scenes.push({
      id: "09-frames",
      note: "frame with children",
      data: scene([
        frameElement,
        rectangle(220, 220, 140, 100, { frameId: frameElement.id }),
        text(220, 350, "inside a frame", { fontSize: 22, frameId: frameElement.id }),
      ]),
    });
  }

  scenes.push({
    id: "10-mixed",
    note: "a mixed composition",
    data: scene([
      rectangle(160, 180, 220, 140),
      ellipse(440, 180, 200, 140),
      diamond(700, 170, 180, 160),
      arrow(380, 250, [
        [0, 0],
        [60, 0],
      ], { strokeColor: "#2f9e44" }),
      freedraw(300, 420, [
        [0, 20],
        [80, 0],
        [160, 30],
      ]),
      text(560, 420, "everything together", { fontSize: 26, strokeColor: "#c92a2a" }),
    ]),
  });

  return scenes;
}

export function writeFixtures() {
  fs.mkdirSync(fixturesDir, { recursive: true });
  const written = [];
  for (const entry of canonicalScenes()) {
    const file = path.join(fixturesDir, `${entry.id}.excalidraw`);
    fs.writeFileSync(file, `${JSON.stringify(entry.data, null, 2)}\n`);
    written.push(file);
  }
  return written;
}

const isMain = process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url);
if (isMain) {
  const files = writeFixtures();
  console.log(`wrote ${files.length} fixtures to ${fixturesDir}`);
}
