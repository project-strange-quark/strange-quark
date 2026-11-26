// SPDX-License-Identifier: MIT

import assert from "node:assert/strict";
import { test } from "node:test";

import { createDocument, flushDocument, hasPendingRecords, installCanvas, installDOM } from "../dist/index.js";
import { Journal, Opcode } from "../dist/journal.js";

function commands(view, offset = 12) {
  const records = [];
  while (offset < view.byteLength) {
    const op = view.getUint8(offset);
    assert.equal(view.getUint8(offset + 1), 0);
    assert.equal(view.getUint16(offset + 2, true), 0);
    const length = view.getUint32(offset + 4, true);
    const payload = new DataView(view.buffer, view.byteOffset + offset + 8, length);
    const end = offset + 8 + Math.ceil(length / 8) * 8;
    for (let i = offset + 8 + length; i < end; i++) assert.equal(view.getUint8(i), 0);
    records.push({ op, payload });
    offset = end;
  }
  assert.equal(offset, view.byteLength);
  return records;
}

function stream(bytes) {
  const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
  assert.equal(String.fromCharCode(...bytes.slice(0, 4)), "JCV1");
  assert.equal(view.getUint32(4, true), bytes.length);
  const records = commands(view);
  assert.equal(view.getUint32(8, true), records.length);
  return records;
}

function floats(view, count, start = 0) {
  return Array.from({ length: count }, (_, i) => view.getFloat64(start + i * 8, true));
}

function words(view, count, start = 0) {
  return Array.from({ length: count }, (_, i) => view.getUint32(start + i * 4, true));
}

function text(view, count, start = 0) {
  let result = "";
  for (let i = 0; i < count; i++) result += String.fromCharCode(view.getUint16(start + i * 2, true));
  return result;
}

function setup() {
  const submissions = [];
  const journalRecords = [];
  const calls = [];
  const tasks = [];
  const buffers = new Map();
  let stageId = 0;
  let reject = false;
  const journal = new Journal((batch, length) => {
    if (reject) {
      reject = false;
      throw new Error("fixture rejects batch");
    }
    const view = new DataView(batch, 0, length);
    const recordBytes = view.getUint32(0, true);
    for (let i = 16; i < 16 + recordBytes; i += 16) {
      const op = view.getUint8(i), [a, b, c] = words(view, 3, i + 4);
      journalRecords.push([op, a, b, c]);
      if (op === Opcode.CanvasSubmit) {
        const bytes = buffers.get(b);
        assert.equal(bytes?.length, c);
        submissions.push({ owner: a, bytes });
        buffers.delete(b);
      }
    }
    return recordBytes / 16;
  });
  const global = { Blob };
  const document = createDocument(journal);
  installDOM(global, document);
  installCanvas(global, document, {
    stageBytes(bytes) { buffers.set(++stageId, bytes.slice()); return stageId; },
    cssParseColor(value, id) {
      calls.push(["color", value, id]);
      if (value === "red") return [255, 0, 0, 255];
      if (value === "currentColor") return [17, 34, 51, 141];
      return null;
    },
    cssValidateFilter(value) {
      calls.push(["filter", value]);
      return value.trim().startsWith("invert(");
    },
    cssParseCanvasFont(value, id) {
      calls.push(["font", value, id]);
      return value.trim() === "bold 16px serif" ? value.trim() : null;
    },
    canvasImageSize(id) {
      calls.push(["image size", id]);
      return [6, 12];
    },
    measureText(font, value) {
      calls.push(["measure", font, value, submissions.length]);
      return { width: 12, actualBoundingBoxAscent: 9, actualBoundingBoxDescent: 3 };
    },
    canvasReadback(id, x, y, width, height) {
      calls.push(["read", id, x, y, width, height, submissions.length]);
      return new Uint8ClampedArray(width * height * 4).fill(127);
    },
    canvasEncodeSync(id, mime, quality) {
      calls.push(["encodeSync", id, mime, quality, submissions.length]);
      return Uint8Array.of(1, 2, 3, 4).buffer;
    },
    async canvasEncode(id, mime, quality) {
      calls.push(["encode", id, mime, quality, submissions.length]);
      return Uint8Array.of(8, 9).buffer;
    },
    postTask(callback) { tasks.push(callback); },
  });
  const canvas = document.createElement("canvas");
  const context = canvas.getContext("2d", { willReadFrequently: true });
  return { global, document, canvas, context, calls, tasks, submissions, journalRecords,
    journal, rejectNext: () => { reject = true; } };
}

test("canvas dimensions, state restoration and geometry are recorded in JCV1", () => {
  const { global, document, canvas, context: ctx, calls, submissions, journalRecords } = setup();
  assert.equal(canvas.getContext("webgl"), null);
  assert.equal(canvas.getContext("2d"), ctx);
  assert.equal(ctx.canvas, canvas);
  assert.ok(canvas instanceof global.HTMLCanvasElement && ctx instanceof global.CanvasRenderingContext2D);
  assert.deepEqual([canvas.width, canvas.height], [300, 150]);
  assert.equal(ctx.fillStyle, "#000000");
  ctx.fillStyle = "currentColor";
  assert.equal(ctx.fillStyle, "rgba(17, 34, 51, 0.553)");
  ctx.fillStyle = "invalid";
  ctx.strokeStyle = "red";
  ctx.filter = "  invert(93%) hue-rotate(180deg)  ";
  ctx.filter = "invalid";
  ctx.font = "  bold 16px serif  ";
  ctx.font = "nonsense";
  ctx.textAlign = "center";
  ctx.textBaseline = "top";
  ctx.lineCap = "round";
  ctx.lineJoin = "bevel";
  ctx.lineWidth = 5;
  ctx.lineDashOffset = -2;
  ctx.setLineDash([2, 3, 4]);
  ctx.setLineDash([2, -1]);
  ctx.getLineDash().push(100);
  ctx.globalAlpha = 0.5;
  ctx.imageSmoothingEnabled = false;
  ctx.imageSmoothingQuality = "high";
  ctx.save();
  ctx.fillStyle = "red";
  ctx.globalAlpha = 0;
  ctx.imageSmoothingQuality = "medium";
  ctx.setLineDash([]);
  ctx.rotate(Math.PI / 2);
  ctx.restore();
  ctx.restore();
  assert.deepEqual([ctx.fillStyle, ctx.strokeStyle, ctx.lineWidth, ctx.lineCap, ctx.lineJoin,
    ctx.lineDashOffset, ctx.getLineDash(), ctx.globalAlpha, ctx.imageSmoothingEnabled, ctx.imageSmoothingQuality,
    ctx.filter, ctx.font, ctx.textAlign, ctx.textBaseline],
  ["rgba(17, 34, 51, 0.553)", "#ff0000", 5, "round", "bevel", -2, [2, 3, 4, 2, 3, 4], 0.5, false, "high",
    "  invert(93%) hue-rotate(180deg)  ", "bold 16px serif", "center", "top"]);

  ctx.setTransform(1, 0, 0, 1, 10, 20);
  ctx.scale(2, 3);
  ctx.translate(4, 5);
  ctx.fillRect(1, 2, 3, 4);
  ctx.strokeRect(4, 3, -2, 0);
  ctx.clearRect(0, 0, 5, 5);
  ctx.beginPath();
  ctx.moveTo(1, 2);
  ctx.lineTo(3, 4);
  ctx.quadraticCurveTo(1, 2, 3, 4);
  ctx.bezierCurveTo(1, 2, 3, 4, 5, 6);
  ctx.closePath();
  ctx.arc(4, 6, 3, 0, Math.PI);
  ctx.ellipse(4, 6, 2, 1, 0.5, 0, Math.PI, true);
  ctx.clip("evenodd");
  ctx.fill();
  ctx.stroke();
  ctx.fillText("🖊\ud800", 8, 9);
  ctx.fillText("short", 4, 6, 22);
  ctx.fillText("zero", 4, 6, 0);
  assert.deepEqual(ctx.measureText("\ud800T"), { width: 12, actualBoundingBoxAscent: 9, actualBoundingBoxDescent: 3 });

  const [{ owner, bytes }] = submissions;
  const records = stream(bytes);
  assert.ok(journalRecords.some(([op, id, w, h]) => op === Opcode.CanvasSize && id === owner && w === 300 && h === 150));
  assert.deepEqual(calls.at(-1), ["measure", "bold 16px serif", "\ud800T", 1]);
  assert.deepEqual(calls[0], ["color", "currentColor", owner]);
  assert.deepEqual(records.slice(0, 4).map(({ op }) => op), [20, 18, 20, 19]);
  assert.deepEqual(words(records[0].payload, 2), [1, 0]);
  assert.deepEqual(Array.from(new Uint8Array(records[0].payload.buffer, records[0].payload.byteOffset + 8, 4)), [17, 34, 51, 141]);
  assert.deepEqual(words(records[1].payload, 1), [1]);
  const filter = records.find(({ op }) => op === 27).payload;
  assert.equal(text(filter, filter.getUint32(0, true), 4), ctx.filter);
  const stroke = records.filter(({ op }) => op === 22)[4].payload;
  assert.deepEqual(floats(stroke, 3), [5, 10, -2]);
  assert.deepEqual(words(stroke, 3, 24), [1, 2, 6]);
  assert.deepEqual(floats(stroke, 6, 36), [2, 3, 4, 2, 3, 4]);
  const sampling = records.filter(({ op }) => op === 21)[1].payload;
  assert.deepEqual(floats(sampling, 1), [0.5]);
  assert.deepEqual(words(sampling, 2, 8), [0, 0]);
  assert.deepEqual(floats(records.filter(({ op }) => op === 10).at(-1).payload, 6), [2, 0, 0, 3, 18, 35]);
  assert.deepEqual(records.filter(({ op }) => op >= 14 && op <= 16).map(({ payload }) => floats(payload, 4)),
    [[1, 2, 3, 4], [4, 3, -2, 0], [0, 0, 5, 5]]);
  const textOps = records.filter(({ op }) => op === 26);
  assert.equal(textOps.length, 2);
  const first = textOps[0].payload;
  assert.deepEqual(floats(first, 3), [8, 9, NaN]);
  assert.deepEqual(words(first, 5, 24), [4, 0, 2, ctx.font.length, 3]);
  assert.equal(text(first, ctx.font.length + 3, 44), "bold 16px serif🖊\ud800");
  assert.deepEqual(floats(textOps[1].payload, 3), [4, 6, 22]);

  ctx.lineWidth = 3;
  canvas.width = canvas.width;
  assert.deepEqual([ctx.lineWidth, ctx.fillStyle, ctx.getLineDash(), ctx.font, ctx.filter], [1, "#000000", [], "10px sans-serif", "none"]);
  assert.equal(canvas.getContext("2d"), ctx);
  canvas.setAttribute("height", "  +42pixels");
  assert.equal(canvas.height, 42);
  canvas.removeAttribute("height");
  assert.equal(canvas.height, 150);
  assert.ok(hasPendingRecords(document));
  flushDocument(document);
  assert.equal(hasPendingRecords(document), false);
  assert.deepEqual(journalRecords.filter(([op]) => op === Opcode.CanvasSize).map(record => record.slice(2)),
    [[300, 150], [300, 150], [300, 42], [300, 150]]);
});

test("Path2D lowers SVG arcs, shorthand curves and scaled roundRect corners", () => {
  const { global, document, context: ctx, submissions } = setup();
  const { Path2D } = global;
  const path = new Path2D("M2,3 4,5 h2 v3 c2 0 3 1 4 1 s3 -1 4 1 q1 2 3 4 t3 2 A5 5 0 0124 16z L1");
  const copy = new Path2D(path);
  path.lineTo(99, 100);
  ctx.fill(copy, "evenodd");
  ctx.roundRect(0, 0, -20, 10, [{ x: 9, y: 8 }, 9]);
  ctx.rect(0, 0, 3, 4);
  assert.throws(() => ctx.roundRect(0, 0, 10, 10, []), RangeError);
  assert.throws(() => ctx.roundRect(0, 0, 10, 10, [-1]), RangeError);
  assert.throws(() => ctx.arc(0, 0, -1, 0, 3), { name: "IndexSizeError" });
  ctx.lineTo(NaN, 10);
  flushDocument(document);
  const records = stream(submissions[0].bytes);
  const pathRecord = records[0].payload;
  assert.equal(records[0].op, 17);
  assert.deepEqual(words(pathRecord, 2), [1, pathRecord.byteLength - 8]);
  const pathOps = commands(pathRecord, 8);
  assert.deepEqual(pathOps.map(({ op }) => op), [4, 5, 5, 5, 7, 7, 6, 6, 9, 8]);
  assert.deepEqual(floats(pathOps[5].payload, 6), [11, 9, 13, 8, 14, 10]);
  assert.equal(pathOps[8].payload.getUint32(56, true), 0);
  assert.deepEqual(words(records[1].payload, 2), [1, 1]);
  const arc = records.find(({ op }) => op === 9).payload;
  assert.equal(arc.getUint32(56, true), 1);
  const [, , rx, ry] = floats(arc, 7);
  assert.ok(rx <= 5.3 && ry <= 5.3);
});

test("miterLimit preserves valid values and writes them into stroke options", () => {
  const { context: ctx, document, canvas, submissions } = setup();
  assert.equal(ctx.miterLimit, 10);
  ctx.miterLimit = 4;
  for (const invalid of [0, -1, NaN, Infinity, -Infinity]) ctx.miterLimit = invalid;
  assert.equal(ctx.miterLimit, 4);
  ctx.save();
  ctx.miterLimit = 0.5;
  ctx.restore();
  assert.equal(ctx.miterLimit, 4);
  ctx.lineWidth = 8;
  ctx.strokeRect(0, 0, 5, 5);
  flushDocument(document);
  const strokes = stream(submissions[0].bytes).filter(({ op }) => op === 22);
  assert.deepEqual(strokes.map(({ payload }) => floats(payload, 3)), [[1, 4, 0], [1, 0.5, 0], [8, 4, 0]]);
  canvas.width = canvas.width;
  assert.equal(ctx.miterLimit, 10);
});

test("Canvas transforms return mutable matrix snapshots and record the current transform", () => {
  const { global, context: ctx, document, submissions } = setup();
  const { DOMMatrix } = global;
  const components = ({ a, b, c, d, e, f }) => [a, b, c, d, e, f];
  const identity = [1, 0, 0, 1, 0, 0];
  assert.ok(ctx.getTransform() instanceof DOMMatrix);
  assert.ok(ctx.getTransform().is2D && ctx.getTransform().isIdentity);

  ctx.translate(10, 20);
  ctx.transform(2, 0, 1, 3, 4, 5);
  const composed = [2, 0, 1, 3, 14, 25];
  const snapshot = ctx.getTransform();
  assert.deepEqual(components(snapshot), composed);
  snapshot.m41 = 80;
  assert.equal(snapshot.e, 80);
  assert.deepEqual(components(ctx.getTransform()), composed);
  ctx.save();
  ctx.setTransform(snapshot);
  assert.deepEqual(components(ctx.getTransform()), [2, 0, 1, 3, 80, 25]);
  ctx.restore();
  ctx.transform(1, 0, 0, 1, NaN, 0);
  assert.deepEqual(components(ctx.getTransform()), composed);
  assert.throws(() => ctx.setTransform({ a: 2, m11: 3 }), TypeError);
  for (const method of ["transform", "rotate", "translate", "scale"]) assert.throws(() => ctx[method](), TypeError);
  ctx.resetTransform();
  assert.deepEqual(components(ctx.getTransform()), identity);
  ctx.setTransform(new DOMMatrix([1, 0, 0, 1, 4, 5]));
  assert.deepEqual(components(ctx.getTransform()), [1, 0, 0, 1, 4, 5]);
  ctx.setTransform({ a: null });
  assert.equal(ctx.getTransform().a, 0);
  flushDocument(document);
  const transforms = stream(submissions[0].bytes).filter(({ op }) => op === 10);
  assert.deepEqual(transforms.map(({ payload }) => floats(payload, 6)), [
    [1, 0, 0, 1, 10, 20], composed, [2, 0, 1, 3, 80, 25], identity, [1, 0, 0, 1, 4, 5], [0, 0, 0, 1, 0, 0],
  ]);
});

test("createImageData allocates transparent pixels for dimensions, settings and existing ImageData", () => {
  const { global, context: ctx } = setup();
  const describe = image => [image.width, image.height, image.colorSpace, image.pixelFormat];
  const plain = ctx.createImageData(-2, 3.5);
  assert.ok(plain instanceof global.ImageData);
  assert.deepEqual(describe(plain), [2, 3, "srgb", "rgba-unorm8"]);
  assert.deepEqual(plain.data, new Uint8ClampedArray(24));
  const wide = ctx.createImageData(3, 1, { colorSpace: "display-p3", pixelFormat: "rgba-float16" });
  wide.data[0] = 0.5;
  const blank = ctx.createImageData(wide);
  assert.deepEqual(describe(blank), [3, 1, "display-p3", "rgba-float16"]);
  assert.deepEqual(blank.data, new Float16Array(12));
  assert.notStrictEqual(blank.data, wide.data);
  assert.throws(() => ctx.createImageData(0, 2), { name: "IndexSizeError" });
  assert.throws(() => ctx.createImageData(NaN, 2), TypeError);
  assert.throws(() => ctx.createImageData(2), TypeError);
  assert.throws(() => ctx.createImageData(1, 1, { pixelFormat: "invalid" }), TypeError);
});

test("putImageData snapshots straight RGBA8 pixels and clips dirty rows in source coordinates", () => {
  const { global, document, canvas, context: ctx, submissions } = setup();
  canvas.width = 4;
  canvas.height = 4;
  const source = Uint8ClampedArray.from({ length: 48 }, (_, i) => i + 1);
  const image = new global.ImageData(source, 4, 3);
  ctx.putImageData(image, 0, 1);
  ctx.putImageData(image, -1, 0, 3, 5, -5, -4);
  ctx.putImageData(image, 0, 0, 100, 0, 1, 1);
  ctx.putImageData(image, 0, 0, 0, 0, 0, 1);
  ctx.putImageData(image, 20, 0);
  const expected = [
    { box: [0, 1, 4, 3], pixels: [...source] },
    { box: [0, 1, 2, 2], pixels: [...source.slice(20, 28), ...source.slice(36, 44)] },
  ];
  source.fill(0);
  flushDocument(document);
  const records = stream(submissions[0].bytes);
  assert.deepEqual(records.map(record => record.op), [25, 25]);
  assert.deepEqual(records.map(({ payload }) => ({
    box: [payload.getInt32(0, true), payload.getInt32(4, true), ...words(payload, 2, 8)],
    pixels: [...new Uint8Array(payload.buffer, payload.byteOffset + 16, payload.byteLength - 16)],
  })), expected);
});

test("putImageData validates arguments and converts other pixel formats to sRGB bytes", () => {
  const { document, context: ctx, submissions } = setup();
  const image = ctx.createImageData(1, 1);
  assert.throws(() => ctx.putImageData({}, 0, 0), TypeError);
  assert.throws(() => ctx.putImageData(image, 0), TypeError);
  assert.throws(() => ctx.putImageData(image, 0, 0, 0, 0, 1), TypeError);
  assert.throws(() => ctx.putImageData(image, Infinity, 0), TypeError);
  assert.throws(() => ctx.putImageData(image, 0, 0, 0, 0, 2 ** 31, 1), TypeError);
  image.data.buffer.transfer();
  assert.throws(() => ctx.putImageData(image, 0, 0, 0, 0, 0, 0), { name: "InvalidStateError" });
  flushDocument(document);
  assert.equal(submissions.length, 0);

  const p3 = ctx.createImageData(1, 1, { colorSpace: "display-p3" });
  p3.data.set([180, 102, 55, 255]);
  const fp16 = ctx.createImageData(1, 1, { pixelFormat: "rgba-float16" });
  fp16.data.set([0.25, 0.5, 1, 0.75]);
  ctx.putImageData(p3, 0, 0);
  ctx.putImageData(fp16, 0, 0);
  flushDocument(document);
  const records = stream(submissions[0].bytes);
  assert.deepEqual(records.map(({ payload }) => [
    ...new Uint8Array(payload.buffer, payload.byteOffset + 16, payload.byteLength - 16),
  ]), [[192, 97, 41, 255], [64, 128, 255, 191]]);
});

test("drawImage observes ordered sources and readback and encoders flush pending commands", async () => {
  const { global, document, canvas, context: ctx, calls, tasks, submissions, journalRecords } = setup();
  const cache = document.createElement("canvas");
  cache.width = 30;
  cache.height = 20;
  const source = cache.getContext("2d");
  source.fillRect(1, 1, 2, 2);
  ctx.drawImage(cache, 3, 4);
  source.clearRect(0, 0, 1, 1);
  ctx.drawImage(cache, 4, 5, -20, 10);
  const img = document.createElement("img");
  img.src = "/initial.png";
  ctx.drawImage(img, -2, 0, 4, 10, 20, 30, 40, 50);
  img.src = "/next.png";
  ctx.drawImage(canvas, 0, 0, 10, 10, 1, 1, 10, 10);
  const pixels = ctx.getImageData(4, 5, -2, -3);
  assert.ok(pixels instanceof global.ImageData);
  assert.deepEqual([pixels.width, pixels.height, pixels.colorSpace, pixels.data.length, pixels.data[0]], [2, 3, "srgb", 24, 127]);
  assert.throws(() => ctx.getImageData(0, 0, 0, 1), { name: "IndexSizeError" });
  assert.throws(() => ctx.getImageData(0, 0, Infinity, 1), TypeError);
  const [cacheId, canvasId] = submissions.map(({ owner }) => owner);
  assert.deepEqual(submissions.map(({ owner }) => owner), [cacheId, canvasId, cacheId, canvasId, canvasId, canvasId]);
  const images = submissions.flatMap(({ bytes }) => stream(bytes)).filter(({ op }) => op === 23);
  assert.deepEqual(words(images[0].payload, 2), [1, cacheId]);
  assert.deepEqual(floats(images[0].payload, 8, 8), [0, 0, 30, 20, 3, 4, 30, 20]);
  assert.deepEqual(floats(images[1].payload, 8, 8), [0, 0, 30, 20, -16, 5, 20, 10]);
  const [, imgId] = words(images[2].payload, 2);
  assert.ok(journalRecords.findIndex(([op, id]) => op === Opcode.SetAttr && id === imgId) <
    journalRecords.findLastIndex(([op, id]) => op === Opcode.SetAttr && id === imgId));
  const sourceRecords = journalRecords.filter(([op, id]) =>
    op === Opcode.SetAttr && id === imgId || op === Opcode.CanvasSubmit && id === canvasId);
  assert.deepEqual(sourceRecords.map(([op]) => op),
    [Opcode.CanvasSubmit, Opcode.CanvasSubmit, Opcode.SetAttr, Opcode.CanvasSubmit, Opcode.SetAttr, Opcode.CanvasSubmit]);
  assert.deepEqual(floats(images[2].payload, 8, 8), [0, 0, 2, 10, 40, 30, 20, 50]);
  assert.deepEqual(words(images[3].payload, 2), [1, canvasId]);
  assert.deepEqual(calls.at(-1), ["read", canvasId, 2, 2, 2, 3, 6]);

  ctx.fillRect(0, 0, 4, 4);
  assert.equal(canvas.toDataURL("not-an-image"), "data:image/png;base64,AQIDBA==");
  assert.deepEqual(calls.at(-1), ["encodeSync", canvasId, "image/png", undefined, 7]);
  let blob;
  ctx.fillRect(0, 0, 3, 3);
  assert.equal(canvas.toBlob(value => { blob = value; }, "image/jpeg", 0.5), undefined);
  assert.equal(blob, undefined);
  assert.deepEqual(calls.at(-1), ["encode", canvasId, "image/jpeg", 0.5, 8]);
  await Promise.resolve();
  tasks.shift()();
  assert.equal(blob.type, "image/jpeg");
  assert.deepEqual(new Uint8Array(await blob.arrayBuffer()), Uint8Array.of(8, 9));
  canvas.width = 0;
  assert.equal(canvas.toDataURL(), "data:,");
  canvas.toBlob(value => { blob = value; });
  tasks.shift()();
  assert.equal(blob, null);
  assert.throws(() => source.drawImage(canvas, 0, 0), { name: "InvalidStateError" });
});


test("an automatic journal flush can retry a sealed canvas submission", () => {
  const { document, context, journal, rejectNext, submissions } = setup();
  flushDocument(document);
  for (let i = 0; i < 4095; i++) journal.record(Opcode.Listeners, 0, 0, 0);
  context.fillRect(1, 2, 3, 4);
  rejectNext();
  // This write first seals the stream, filling the record region.
  assert.throws(() => { document.createElement("div"); }, /fixture rejects batch/);
  assert.equal(hasPendingRecords(document), true);
  assert.deepEqual(submissions, []);
  flushDocument(document);
  assert.equal(hasPendingRecords(document), false);
  assert.equal(submissions.length, 1);
  assert.deepEqual(stream(submissions[0].bytes).map(({ op }) => op), [14]);
});
