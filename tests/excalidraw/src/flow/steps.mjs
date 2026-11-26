// SPDX-License-Identifier: MIT
// One description of the Excalidraw end-to-end scripted flow. The same steps run in
// Chromium and in Strange Quark; each step asserts Excalidraw's own persisted
// scene/app state through the probe in ./state.mjs, plus DOM facts. No app
// patches and no engine-specific branches.

import assert from "node:assert/strict";

import { checkpointState, elementTypes, waitForScene } from "./state.mjs";

const SETTLE = 1500;

function countOf(state, type) {
  return elementTypes(state).filter((entry) => entry === type).length;
}

function textElements(state) {
  return (state.scene?.elements ?? []).filter((element) => element.type === "text" && !element.isDeleted);
}

export function flowSteps({ savedFile, exportedPng, exportedSvg, clipboardPng }) {
  return [
    {
      id: "empty-scene",
      feature: "draw the elements",
      run: async () => {},
      // Also wait for the app's first debounced save of its state: the
      // checkpoint compares the persisted theme, and a fast start can reach
      // it before that save (native on macOS does, in under 200 ms).
      wait: (state) =>
        state.dom.excalidraw && state.dom.toolbarButtons >= 5 && state.scene?.appState != null,
      check: (state) => {
        assert.equal(countOf(state, "rectangle") + countOf(state, "ellipse"), 0);
        assert.ok(state.dom.canvases >= 1);
      },
    },
    {
      id: "draw-rectangle",
      feature: "draw the elements",
      run: async (driver) => {
        await driver.selectTool("rectangle");
        await driver.dragOnCanvas({ x: 0.32, y: 0.35 }, { x: 0.46, y: 0.55 });
      },
      wait: (state) => countOf(state, "rectangle") === 1,
      check: (state) => assert.equal(countOf(state, "rectangle"), 1),
    },
    {
      id: "draw-ellipse",
      feature: "draw the elements",
      run: async (driver) => {
        await driver.selectTool("ellipse");
        await driver.dragOnCanvas({ x: 0.5, y: 0.33 }, { x: 0.64, y: 0.53 });
      },
      wait: (state) => countOf(state, "ellipse") === 1,
      check: (state) => {
        assert.equal(countOf(state, "ellipse"), 1);
        assert.equal(countOf(state, "rectangle"), 1);
      },
    },
    {
      id: "draw-arrow",
      feature: "draw the elements",
      run: async (driver) => {
        await driver.selectTool("arrow");
        await driver.dragOnCanvas({ x: 0.68, y: 0.34 }, { x: 0.8, y: 0.5 });
      },
      wait: (state) => countOf(state, "arrow") === 1,
      check: (state) => assert.equal(countOf(state, "arrow"), 1),
    },
    {
      id: "draw-freedraw",
      feature: "draw the elements",
      run: async (driver) => {
        await driver.selectTool("freedraw");
        const rect = await driver.canvasRect();
        const { x, y, width, height } = rect;
        const points = [
          [0.34, 0.72],
          [0.4, 0.66],
          [0.46, 0.74],
          [0.52, 0.67],
          [0.58, 0.73],
        ].map(([fx, fy]) => ({ x: x + width * fx, y: y + height * fy }));
        await driver.dragPath(points);
      },
      wait: (state) => countOf(state, "freedraw") === 1,
      check: (state) => assert.equal(countOf(state, "freedraw"), 1),
    },
    {
      id: "draw-text",
      feature: "draw the elements",
      run: async (driver) => {
        await driver.selectTool("text");
        const rect = await driver.canvasRect();
        await driver.clickPoint({ x: rect.x + rect.width * 0.6, y: rect.y + rect.height * 0.78 });
        await driver.type("Hello Excalidraw");
        await driver.press("Escape");
      },
      wait: (state) => textElements(state).length === 1 && textElements(state)[0].text === "Hello Excalidraw",
      check: (state) => {
        assert.equal(textElements(state).length, 1);
        assert.equal(textElements(state)[0].text, "Hello Excalidraw");
        assert.equal(elementTypes(state).length, 5);
      },
    },
    {
      id: "edit-text",
      feature: "edit the text in the WYSIWYG textarea",
      run: async (driver) => {
        await driver.selectTool("selection");
        const rect = await driver.canvasRect();
        // Double-click enters the WYSIWYG editor on the text element, which was
        // drawn at this same canvas point (scene has not been zoomed yet).
        await driver.dblclick({ x: rect.x + rect.width * 0.6, y: rect.y + rect.height * 0.78 });
        await driver.press("a", { primary: true });
        await driver.type("Edited text");
        await driver.press("Escape");
      },
      wait: (state) => textElements(state)[0]?.text === "Edited text",
      check: (state) => assert.equal(textElements(state)[0]?.text, "Edited text"),
    },
    {
      id: "ime-composition",
      feature: "edit the text in the WYSIWYG textarea, including IME composition",
      run: async (driver) => {
        const rect = await driver.canvasRect();
        await driver.dblclick({ x: rect.x + rect.width * 0.6, y: rect.y + rect.height * 0.78 });
        await driver.press("End");
        await driver.composeIme("日本");
        await driver.press("Escape");
      },
      wait: (state) => (textElements(state)[0]?.text ?? "").includes("日本"),
      check: (state) => assert.ok((textElements(state)[0]?.text ?? "").includes("日本")),
    },
    {
      id: "undo",
      feature: "undo and redo",
      run: async (driver) => {
        await driver.press("z", { primary: true });
      },
      wait: (state) => !(textElements(state)[0]?.text ?? "").includes("日本"),
      check: (state) => assert.equal(textElements(state)[0]?.text, "Edited text"),
    },
    {
      id: "redo",
      feature: "undo and redo",
      run: async (driver) => {
        await driver.press("z", { primary: true, shift: true });
      },
      wait: (state) => (textElements(state)[0]?.text ?? "").includes("日本"),
      check: (state) => assert.ok((textElements(state)[0]?.text ?? "").includes("日本")),
    },
    {
      id: "zoom",
      feature: "zoom and pan",
      run: async (driver) => {
        // Zoom in twice with the app's own shortcut (Ctrl++, Cmd++ on macOS). Wheel zooming
        // depends on how each engine's input path carries ctrlKey.
        await driver.press("=", { primary: true });
        await driver.press("=", { primary: true });
      },
      wait: (state) => (state.scene?.appState?.zoom?.value ?? 1) > 1.02,
      check: (state) => assert.ok((state.scene?.appState?.zoom?.value ?? 1) > 1.02),
    },
    {
      id: "pan",
      feature: "zoom and pan",
      run: async (driver, ctx) => {
        const before = (await driver.state()).scene?.appState ?? {};
        ctx.panBefore = { scrollX: before.scrollX ?? 0, scrollY: before.scrollY ?? 0 };
        // The hand tool pans with a real pointer drag, the same gesture in both
        // engines (wheel panning depends on the app's wheel handler ordering).
        await driver.selectTool("hand");
        await driver.dragOnCanvas({ x: 0.5, y: 0.5 }, { x: 0.58, y: 0.58 });
        await driver.selectTool("selection");
      },
      wait: (state, ctx) => {
        const after = state.scene?.appState ?? {};
        return (
          after.scrollX !== ctx.panBefore?.scrollX || after.scrollY !== ctx.panBefore?.scrollY
        );
      },
      check: (state, ctx) => {
        const after = state.scene?.appState ?? {};
        assert.ok(
          after.scrollX !== ctx.panBefore.scrollX || after.scrollY !== ctx.panBefore.scrollY,
          `the scene did not pan (before ${JSON.stringify(ctx.panBefore)}, after ${JSON.stringify({ scrollX: after.scrollX, scrollY: after.scrollY })})`,
        );
      },
    },
    {
      id: "dark-mode",
      feature: "toggle dark mode",
      run: async (driver) => {
        // The app's own theme shortcut (Shift+Alt+D); the menu's theme control
        // is a radio group without stable test ids.
        await driver.press("d", { shift: true, alt: true });
      },
      wait: (state) => state.dom.dark && state.scene?.appState?.theme === "dark",
      check: (state) => {
        assert.equal(state.dom.dark, true);
        assert.equal(state.scene?.appState?.theme, "dark");
      },
    },
    {
      id: "library-sidebar",
      feature: "open the library sidebar",
      run: async (driver) => driver.openLibrary(),
      wait: (state) => state.dom.libraryOpen,
      check: (state) => assert.equal(state.dom.libraryOpen, true),
    },
    {
      id: "save-file",
      feature: "save a .excalidraw file and open it again",
      run: async (driver) => {
        await driver.saveFile(savedFile);
      },
      wait: () => true,
      check: (state, ctx) => {
        assert.ok(ctx.fs.existsSync(savedFile), `expected ${savedFile}`);
        const saved = JSON.parse(ctx.fs.readFileSync(savedFile, "utf8"));
        assert.equal(saved.type, "excalidraw");
        assert.ok(saved.elements.length >= 5);
      },
    },
    {
      id: "open-file",
      feature: "save a .excalidraw file and open it again",
      run: async (driver) => {
        await driver.press("a", { primary: true });
        await driver.press("Backspace");
        await driver.openFile(savedFile);
      },
      wait: (state) => elementTypes(state).length >= 5,
      check: (state) => {
        assert.ok(elementTypes(state).length >= 5);
        assert.equal(textElements(state).length, 1);
      },
    },
    {
      id: "export-png",
      feature: "export PNG and SVG",
      run: async (driver) => driver.exportPng(exportedPng),
      wait: () => true,
      check: (_state, ctx) => {
        assert.ok(ctx.fs.existsSync(exportedPng));
        const bytes = ctx.fs.readFileSync(exportedPng);
        assert.deepEqual([...bytes.subarray(0, 8)], [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]);
      },
    },
    {
      id: "export-svg",
      feature: "export PNG and SVG",
      run: async (driver) => driver.exportSvg(exportedSvg),
      wait: () => true,
      check: (_state, ctx) => {
        assert.ok(ctx.fs.existsSync(exportedSvg));
        const text = ctx.fs.readFileSync(exportedSvg, "utf8");
        assert.match(text, /<svg[\s>]/);
      },
    },
    {
      id: "paste-image",
      feature: "paste an image from the clipboard",
      run: async (driver) => driver.pasteImage(clipboardPng),
      wait: (state) => countOf(state, "image") === 1,
      check: (state) => assert.equal(countOf(state, "image"), 1),
    },
    {
      id: "restart-persistence",
      feature: "restart, and the scene comes back from local storage",
      run: async (driver) => driver.restart(),
      wait: (state) => elementTypes(state).length >= 5 && state.scene?.appState?.theme === "dark",
      check: (state) => {
        assert.ok(elementTypes(state).length >= 5);
        assert.equal(countOf(state, "image"), 1);
        assert.equal(state.scene?.appState?.theme, "dark");
        assert.ok((textElements(state)[0]?.text ?? "").includes("日本"));
      },
    },
  ];
}

/**
 * Runs the flow on one driver. Stops at the first failing step and records the
 * rest as blocked, so the two engines compare step-for-step.
 */
export async function runFlow(driver, { engine, ctx, continueOnFailure = false }) {
  const steps = [];
  let failure = null;
  for (const step of flowSteps(ctx)) {
    if (failure) {
      steps.push({ id: step.id, feature: step.feature, status: "blocked", reason: `after ${failure}` });
      continue;
    }
    const started = performance.now();
    try {
      await step.run(driver, ctx, engine);
      const state = await waitForScene(driver, (candidate) => step.wait(candidate, ctx), {
        label: `${step.id} scene`,
        timeoutMs: step.id === "restart-persistence" ? 20_000 : 12_000,
      });
      await step.check(state, ctx);
      steps.push({
        id: step.id,
        feature: step.feature,
        status: "pass",
        durationMs: Number((performance.now() - started).toFixed(2)),
        checkpoint: checkpointState(state),
      });
    } catch (error) {
      if (!continueOnFailure) failure = step.id;
      steps.push({
        id: step.id,
        feature: step.feature,
        status: "fail",
        durationMs: Number((performance.now() - started).toFixed(2)),
        error: error.message,
      });
    }
  }
  return {
    engine,
    passed: steps.filter((step) => step.status === "pass").length,
    failed: steps.filter((step) => step.status === "fail").length,
    blocked: steps.filter((step) => step.status === "blocked").length,
    total: steps.length,
    steps,
  };
}
