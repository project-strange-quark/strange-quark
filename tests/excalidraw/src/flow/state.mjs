// SPDX-License-Identifier: MIT
// Reads Excalidraw's own state without patching the app.
//
// The app persists the scene to localStorage under "excalidraw" (key from
// app_constants.ts) on every change, so the probe reads exactly what a reload
// would restore: elements, the persisted appState subset and files. DOM facts
// cover what persistence does not expose (toolbar, theme class, the WYSIWYG
// textarea, zoom UI). History is asserted through its effects (undo/redo
// change the scene), because the app keeps the undo stack in memory only.

export const STATE_PROBE = `(() => {
  const safe = (fn, fallback = null) => { try { return fn(); } catch (e) { return fallback; } };
  // excalidraw-app persists elements under "excalidraw" (an array) and the
  // appState subset under "excalidraw-state" (app_constants.ts STORAGE_KEYS).
  const rawElements = safe(() => localStorage.getItem("excalidraw"));
  const rawState = safe(() => localStorage.getItem("excalidraw-state"));
  let scene = null;
  let sceneError = null;
  try {
    const elements = rawElements ? JSON.parse(rawElements) : [];
    const appState = rawState ? JSON.parse(rawState) : null;
    scene = {
      elementCount: Array.isArray(elements) ? elements.length : 0,
      elements: (elements || []).map((element) => ({
        id: element.id,
        type: element.type,
        x: Math.round(element.x),
        y: Math.round(element.y),
        width: Math.round(element.width),
        height: Math.round(element.height),
        text: element.text ?? null,
        strokeColor: element.strokeColor ?? null,
        backgroundColor: element.backgroundColor ?? null,
        opacity: element.opacity ?? 100,
        isDeleted: element.isDeleted ?? false,
        groupIds: element.groupIds ?? [],
        containerId: element.containerId ?? null,
        fileId: element.fileId ?? null,
      })),
      appState: appState
        ? {
            theme: appState.theme ?? null,
            viewBackgroundColor: appState.viewBackgroundColor ?? null,
            zoom: appState.zoom ? { value: appState.zoom.value } : null,
            scrollX: Math.round(appState.scrollX ?? 0),
            scrollY: Math.round(appState.scrollY ?? 0),
            selectedElementIds: Object.keys(appState.selectedElementIds ?? {}),
            activeTool: appState.activeTool ? appState.activeTool.type : null,
          }
        : null,
      fileCount: null,
    };
  } catch (error) {
    sceneError = String(error && error.message ? error.message : error);
  }
  const canvas = document.querySelector("canvas");
  const rect = canvas ? canvas.getBoundingClientRect() : null;
  return {
    scene,
    sceneRawBytes: (rawElements ? rawElements.length : 0) + (rawState ? rawState.length : 0),
    sceneError,
    dom: {
      excalidraw: !!document.querySelector(".excalidraw"),
      canvases: document.querySelectorAll("canvas").length,
      toolbarButtons: document.querySelectorAll('[data-testid^="toolbar-"]').length,
      dark:
        document.documentElement.classList.contains("dark") ||
        !!document.querySelector(".excalidraw.theme--dark") ||
        !!document.querySelector(".theme--dark"),
      theme: safe(() => localStorage.getItem("excalidraw-theme")),
      libraryOpen: !!document.querySelector(".sidebar:not(.sidebar--floating)") ||
        !!document.querySelector(".default-sidebar-trigger[aria-expanded='true']"),
      wysiwyg: safe(() => {
        const el = document.querySelector(".excalidraw-wysiwyg");
        return el ? el.value : null;
      }),
      zoomText: safe(() => document.querySelector(".zoom-button")?.textContent ?? null),
      canvas: rect
        ? { x: rect.x, y: rect.y, width: rect.width, height: rect.height }
        : null,
    },
  };
})()`;

export function emptyState() {
  return {
    scene: null,
    sceneError: null,
    dom: {
      excalidraw: false,
      canvases: 0,
      toolbarButtons: 0,
      dark: false,
      libraryOpen: false,
      wysiwyg: null,
      zoomText: null,
      canvas: null,
    },
  };
}

/** Normalised element summary used by assertions. */
export function elementTypes(state) {
  return (state.scene?.elements ?? []).filter((element) => !element.isDeleted).map((element) => element.type);
}

/**
 * Normalised DOM and persisted scene checkpoint recorded at every flow step so
 * the native run can be compared against Chromium step by step.
 */
export function checkpointState(state) {
  const dom = state?.dom ?? emptyState().dom;
  const canvas = dom.canvas
    ? {
        x: Math.round(dom.canvas.x),
        y: Math.round(dom.canvas.y),
        width: Math.round(dom.canvas.width),
        height: Math.round(dom.canvas.height),
      }
    : null;
  return {
    dom: {
      excalidraw: Boolean(dom.excalidraw),
      canvases: dom.canvases ?? 0,
      toolbarButtons: dom.toolbarButtons ?? 0,
      dark: Boolean(dom.dark),
      theme: dom.theme ?? null,
      libraryOpen: Boolean(dom.libraryOpen),
      wysiwyg: dom.wysiwyg ?? null,
      zoomText: typeof dom.zoomText === "string" ? dom.zoomText.trim() : null,
      canvasFilled: Boolean(
        canvas && canvas.x === 0 && canvas.y === 0 && canvas.width > 0 && canvas.height > 0,
      ),
      canvas,
    },
    elements: elementTypes(state),
    texts: (state?.scene?.elements ?? [])
      .filter((element) => element.type === "text" && !element.isDeleted)
      .map((element) => element.text ?? null),
    appTheme: state?.scene?.appState?.theme ?? null,
  };
}

/**
 * Compares Chromium and native checkpoint snapshots at every step in the flow.
 * Returns `{ passed, total, matched, mismatched, mismatches }`.
 *
 * `checkCanvasSize` compares exact canvas pixel dimensions (used headless,
 * where both engines own a fixed 1280x800 viewport; tiling window managers
 * resize the native window in windowed runs, where `canvasFilled` still
 * verifies a full origin-aligned non-zero canvas).
 */
export function compareCheckpoints(chromiumSteps, nativeSteps, { checkCanvasSize = true } = {}) {
  const cSteps = Array.isArray(chromiumSteps) ? chromiumSteps : [];
  const nSteps = Array.isArray(nativeSteps) ? nativeSteps : [];
  const total = Math.max(cSteps.length, nSteps.length);
  const mismatches = [];
  let matched = 0;

  for (let index = 0; index < total; index += 1) {
    const cStep = cSteps[index];
    const nStep = nSteps[index];
    const id = cStep?.id ?? nStep?.id ?? `step-${index}`;
    if (!cStep || !nStep || cStep.id !== nStep.id) {
      mismatches.push({
        id,
        reason: "step mismatch",
        chromiumId: cStep?.id ?? null,
        nativeId: nStep?.id ?? null,
      });
      continue;
    }
    if (!cStep.checkpoint || !nStep.checkpoint) {
      mismatches.push({
        id,
        reason: "missing checkpoint",
        chromiumStatus: cStep.status,
        nativeStatus: nStep.status,
      });
      continue;
    }
    const diffs = [];
    for (const [key, cVal] of Object.entries(cStep.checkpoint.dom ?? {})) {
      if (key === "canvas" && !checkCanvasSize) continue;
      const nVal = nStep.checkpoint.dom?.[key];
      if (JSON.stringify(cVal) !== JSON.stringify(nVal)) {
        diffs.push({ field: `dom.${key}`, chromium: cVal, native: nVal });
      }
    }
    for (const key of ["elements", "texts", "appTheme"]) {
      const cVal = cStep.checkpoint[key];
      const nVal = nStep.checkpoint[key];
      if (JSON.stringify(cVal) !== JSON.stringify(nVal)) {
        diffs.push({ field: key, chromium: cVal, native: nVal });
      }
    }
    if (diffs.length === 0) {
      matched += 1;
    } else {
      mismatches.push({ id, diffs });
    }
  }

  return {
    passed: total > 0 && mismatches.length === 0 && matched === total,
    total,
    matched,
    mismatched: mismatches.length,
    mismatches,
  };
}

/** Waits until the persisted scene satisfies `predicate`. */
export async function waitForScene(driver, predicate, { label, timeoutMs = 10_000, intervalMs = 50 } = {}) {
  const deadline = performance.now() + timeoutMs;
  let last = null;
  while (performance.now() < deadline) {
    last = await driver.state();
    try {
      if (predicate(last)) return last;
    } catch {
      // Keep polling.
    }
    await new Promise((resolve) => setTimeout(resolve, intervalMs));
  }
  throw new Error(
    `timed out waiting for ${label ?? "the scene"} (last: ${JSON.stringify({
      elements: elementTypes(last ?? emptyState()),
      dom: last?.dom,
      sceneError: last?.sceneError,
    }).slice(0, 800)})`,
  );
}
