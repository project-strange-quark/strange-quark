// SPDX-License-Identifier: MIT

import {
  beginPointerSelection, changeStateChain, connectedInputTarget, disabledControl, Document, Element, elementChain, focusFromPointer,
  inputComposition, isComposing, keyboardDefault, noteKeyboardModality, notePointerModality, onInputDetach, spaceActivates, updatePointerSelection,
} from "./document.js";
import type { ImeKind, PointerSelection } from "./document.js";
import {
  dispatchTrustedEvent, DOMException, KeyboardEvent, MouseEvent, PointerEvent, setPointerSamples, WheelEvent,
} from "./events.js";
import type { MouseInit } from "./events.js";
import { noteUserActivation } from "./activation.js";
import { ElementState } from "./journal.js";

type Modifiers = { altKey: boolean; ctrlKey: boolean; metaKey: boolean; shiftKey: boolean };
type Sample = { x: number; y: number; pressure: number; time: number };
type PointerFields = Modifiers & Omit<Sample, "time"> & {
  pointerId: number; pointerType: "mouse" | "pen" | "touch"; isPrimary: boolean;
  button: number; buttons: number; samples: Sample[]; boundaryUpdate: boolean;
  clickCount?: number | null;
};
type KeyFields = Modifiers & { key: string; code: string; location: number; repeat: boolean; isComposing: boolean };
type WheelFields = Modifiers & { x: number; y: number; deltaX: number; deltaY: number; deltaZ: number; deltaMode: number };
type InputMessage = { id: number; target: number; related: number; time: number; cancelable: boolean } & (
  { type: "pointerdown" | "pointerup" | "pointermove" | "pointercancel"; fields: PointerFields } |
  { type: "keydown" | "keyup"; fields: KeyFields } |
  { type: "wheel"; fields: WheelFields } |
  { type: "ime"; fields: { kind: ImeKind; text: string; selection: [number, number] | null } }
);
type ClickPress = { target: Element; button: number; x: number; y: number; time: number; detail: number; modifiers: number };
type Pointer = {
  hover: Element[];
  active: Element[];
  down: Element | null;
  clickPress: ClickPress | null;
  /** The press of a non-primary button, which an auxclick completes. */
  auxPress: ClickPress | null;
  selection: PointerSelection | null;
  buttons: number;
  suppressMouse: boolean;
  capture: Element | null;
  pendingCapture: Element | null;
  /** The last position this pointer reported, for movementX and movementY. */
  last: { x: number; y: number } | null;
};

const controllers = new WeakMap<Document, InputController>();
const CONTEXT_MENU_BUTTON = 2;
// Chromium's desktop repeated-click window and per-axis movement tolerance.
const DOUBLE_CLICK_MS = 500;
const DOUBLE_CLICK_TOLERANCE = 2;
const pointerEvents = { bubbles: true, cancelable: true, composed: true };

class InputController {
  readonly pointers = new Map<number, Pointer>();
  #spaceChain: Element[] = [];
  #lastClick: ClickPress | null = null;

  // Chromium shows the context menu, and fires contextmenu, when the right
  // button goes down, except on Windows, where it waits for the button to rise.
  constructor(readonly document: Document, readonly contextMenuOnRelease: boolean) {
    onInputDetach(document, child => {
      const hover = this.chain("hover");
      const active = this.chain("active");
      if (this.#spaceChain[0] !== undefined && child.contains(this.#spaceChain[0])) this.#spaceChain = [];
      if (this.#lastClick !== null && child.contains(this.#lastClick.target)) this.#lastClick = null;
      for (const pointer of this.pointers.values()) {
        if (pointer.hover[0] !== undefined && child.contains(pointer.hover[0])) pointer.hover = [];
        if (pointer.down !== null && child.contains(pointer.down)) {
          pointer.down = null;
          pointer.clickPress = null;
          pointer.active = [];
        }
        if (pointer.auxPress !== null && child.contains(pointer.auxPress.target)) pointer.auxPress = null;
        if (pointer.pendingCapture !== null && child.contains(pointer.pendingCapture)) pointer.pendingCapture = null;
        if (pointer.selection !== null && child.contains(pointer.selection.control)) pointer.selection = null;
      }
      changeStateChain(hover, this.chain("hover"), ElementState.Hover);
      changeStateChain(active, this.chain("active"), ElementState.Active);
    });
  }

  chain(kind: "hover" | "active"): Element[] {
    const elements = [...this.pointers.values()].flatMap(pointer => pointer[kind]);
    if (kind === "active") elements.push(...this.#spaceChain);
    return [...new Set(elements)];
  }

  active(pointer: Pointer, target: Element | null): void {
    const previous = this.chain("active");
    pointer.active = elementChain(target);
    changeStateChain(previous, this.chain("active"), ElementState.Active);
  }

  hover(pointer: Pointer, target: Element | null, fields: PointerFields, time: number, related: Element | null = null): void {
    const previous = pointer.hover;
    if ((previous[0] ?? null) === target) return;
    const next = elementChain(target);
    const all = this.chain("hover");
    pointer.hover = next;
    changeStateChain(all, this.chain("hover"), ElementState.Hover);

    const left = previous.filter(node => !next.includes(node));
    const entered = next.filter(node => !previous.includes(node)).reverse();
    const old = previous[0] ?? null;
    const mouse = fields.isPrimary && fields.pointerType !== "touch";
    const base = { ...mouseInit(fields, this.document), ...pointerEvents, button: fields.button };
    const out = { ...base, relatedTarget: target };
    const over = { ...base, relatedTarget: old ?? related };
    const direct = { bubbles: false, cancelable: false, composed: false };
    if (old?.isConnected) {
      dispatchTrustedEvent(old, new PointerEvent("pointerout", out), time);
      for (const node of left) {
        dispatchTrustedEvent(node, new PointerEvent("pointerleave", { ...out, ...direct }), time);
      }
    }
    if (target?.isConnected) {
      dispatchTrustedEvent(target, new PointerEvent("pointerover", over), time);
      for (const node of entered) {
        dispatchTrustedEvent(node, new PointerEvent("pointerenter", { ...over, ...direct }), time);
      }
    }
    if (!mouse) return;
    if (old?.isConnected) {
      dispatchTrustedEvent(old, new MouseEvent("mouseout", { ...out, button: 0 }), time);
      for (const node of left) {
        dispatchTrustedEvent(node, new MouseEvent("mouseleave", { ...out, ...direct, button: 0 }), time);
      }
    }
    if (target?.isConnected) {
      dispatchTrustedEvent(target, new MouseEvent("mouseover", { ...over, button: 0 }), time);
      for (const node of entered) {
        dispatchTrustedEvent(node, new MouseEvent("mouseenter", { ...over, ...direct, button: 0 }), time);
      }
    }
  }

  capture(pointer: Pointer, fields: PointerFields, time: number): void {
    const before = pointer.capture;
    const next = pointer.pendingCapture?.isConnected ? pointer.pendingCapture : null;
    if (before === next) return;
    pointer.capture = next;
    pointer.pendingCapture = next;
    const init = { ...mouseInit(fields, this.document), bubbles: true, composed: true, button: fields.button };
    if (before !== null) {
      dispatchTrustedEvent(before.isConnected ? before : this.document, new PointerEvent("lostpointercapture", init), time);
    }
    if (next?.isConnected) dispatchTrustedEvent(next, new PointerEvent("gotpointercapture", init), time);
  }

  beginClick(target: Element | null, fields: PointerFields, time: number): ClickPress | null {
    const previous = this.#lastClick;
    this.#lastClick = null;
    if (target === null) return null;
    const { x, y, button } = fields;
    const modifiers = Number(fields.ctrlKey) | Number(fields.shiftKey) << 1 |
      Number(fields.altKey) << 2 | Number(fields.metaKey) << 3;
    const repeated = fields.clickCount == null && previous !== null && previous.target === target && previous.button === button &&
      previous.modifiers === modifiers && time >= previous.time && time - previous.time <= DOUBLE_CLICK_MS &&
      Math.abs(previous.x - x) <= DOUBLE_CLICK_TOLERANCE && Math.abs(previous.y - y) <= DOUBLE_CLICK_TOLERANCE;
    const detail = fields.clickCount ?? (repeated ? Math.min(previous.detail + 1, 3) : 1);
    return { target, button, x, y, time, modifiers, detail };
  }

  dispatchPointer(type: "pointerdown" | "pointerup" | "pointermove" | "pointercancel",
    hit: Element | null, stale: boolean, related: Element | null, fields: PointerFields, time: number): boolean {
    const id = fields.pointerId;
    let pointer = this.pointers.get(id);
    if (pointer === undefined) {
      if (stale || type === "pointercancel" || type === "pointerup") return false;
      pointer = { hover: [], active: [], down: null, clickPress: null, auxPress: null, selection: null, buttons: 0,
        suppressMouse: false, capture: null, pendingCapture: null, last: null };
      this.pointers.set(id, pointer);
    }
    if (stale && type !== "pointerup" && type !== "pointercancel") return false;

    if (fields.boundaryUpdate) {
      const target = pointer.capture?.isConnected ? pointer.capture : hit;
      this.hover(pointer, target, fields, time, related);
      if (pointer.hover.length === 0 && pointer.buttons === 0 && pointer.pendingCapture === null) this.pointers.delete(id);
      return false;
    }

    const touch = fields.pointerType === "touch";
    const pressed = type === "pointerdown";
    const released = type === "pointerup" || type === "pointercancel";
    if (pressed) notePointerModality(this.document);
    pointer.buttons = fields.buttons;
    this.capture(pointer, fields, time);
    const captured = pointer.capture;
    if (type !== "pointercancel") this.hover(pointer, captured ?? hit, fields, time, related);
    const target = captured?.isConnected ? captured : hit?.isConnected ? hit : null;
    const init = mouseInit(fields, this.document);
    const origin = pointer.last ?? { x: fields.x, y: fields.y };
    const movement = type === "pointermove" ? { movementX: fields.x - origin.x, movementY: fields.y - origin.y } : {};
    if (type !== "pointercancel") pointer.last = { x: fields.x, y: fields.y };
    if (pressed && fields.isPrimary && fields.pointerType === "mouse") {
      const press = this.beginClick(target, fields, time);
      if (fields.button === 0) pointer.clickPress = press;
      else pointer.auxPress = press;
    }
    const press = fields.button === 0 ? pointer.clickPress : pointer.auxPress?.button === fields.button ? pointer.auxPress : null;
    const detail = fields.clickCount ?? press?.detail ?? 1;
    if (pressed && fields.button === 0) {
      pointer.down = target;
      this.active(pointer, target);
      if (touch) pointer.pendingCapture = target;
    } else if (released) {
      this.active(pointer, null);
    }

    let canceled = false;
    if (target !== null) {
      const event = new PointerEvent(type, {
        ...init, ...pointerEvents, ...movement, cancelable: type !== "pointercancel", button: fields.button,
      });
      if (type === "pointermove") {
        let previous = origin;
        setPointerSamples(event, fields.samples.map(sample => {
          const coalesced = new PointerEvent(type, {
            ...init, clientX: sample.x, clientY: sample.y, screenX: sample.x, screenY: sample.y, pressure: sample.pressure,
            movementX: sample.x - previous.x, movementY: sample.y - previous.y, button: fields.button,
          });
          previous = sample;
          return coalesced;
        }), fields.samples.map(sample => sample.time), target);
      }
      canceled = !dispatchTrustedEvent(target, event, time);
    }
    if (pressed && fields.isPrimary) pointer.suppressMouse = canceled;
    const mouse = fields.isPrimary && !pointer.suppressMouse;
    const disabled = target !== null && elementChain(target).some(disabledControl);
    if (target !== null && mouse && type === "pointermove" && !touch) {
      canceled = !dispatchTrustedEvent(target, new MouseEvent("mousemove", { ...init, ...pointerEvents, ...movement, button: 0 }), time) || canceled;
    }
    if (target !== null && !disabled && mouse && (pressed && !touch || type === "pointerup" && touch && pointer.down !== null)) {
      const allowed = dispatchTrustedEvent(target, new MouseEvent("mousedown", { ...init, ...pointerEvents, detail,
        button: touch ? 0 : fields.button, buttons: touch ? 1 : fields.buttons,
      }), time);
      if (allowed && fields.button === 0 && target.isConnected) {
        focusFromPointer(this.document, target, time);
        if (!touch) pointer.selection = beginPointerSelection(this.document, target, fields.x, fields.y, fields.shiftKey);
      }
      canceled ||= !allowed;
    }
    if (pressed && !this.contextMenuOnRelease) canceled = this.contextMenu(target, disabled, fields, init, time) || canceled;
    if (target !== null && type === "pointerup" && !disabled && mouse) {
      canceled = !dispatchTrustedEvent(target, new MouseEvent("mouseup", { ...init, ...pointerEvents, detail, button: fields.button }), time) || canceled;
    }
    if (type === "pointermove" && mouse && !canceled && (fields.buttons & 1) !== 0 && pointer.selection !== null) {
      updatePointerSelection(pointer.selection, fields.x, fields.y);
    }
    if (released) {
      pointer.selection = null;
      pointer.pendingCapture = null;
      this.capture(pointer, fields, time);
      if (type === "pointerup" && fields.button === 0 && fields.isPrimary && target?.isConnected && pointer.down?.isConnected && !disabled) {
        const clickTarget = captured === target ? target : elementChain(target).find(node => node.contains(pointer.down!));
        if (clickTarget !== undefined && !elementChain(clickTarget).some(disabledControl)) {
          const press = pointer.clickPress;
          const count = fields.clickCount ?? (press?.target === clickTarget ? detail : 1);
          const clickInit = { ...init, ...pointerEvents, button: 0, detail: count };
          if (press !== null) this.#lastClick = { ...press, target: clickTarget, detail: count };
          canceled = !dispatchTrustedEvent(clickTarget, new PointerEvent("click", clickInit), time) || canceled;
          if (count === 2 && clickTarget.isConnected) {
            canceled = !dispatchTrustedEvent(clickTarget, new MouseEvent("dblclick", clickInit), time) || canceled;
          }
        }
      }
      const aux = pointer.auxPress;
      if (type === "pointerup" && fields.button !== 0 && fields.isPrimary && aux?.button === fields.button &&
          target?.isConnected && aux.target.isConnected && !disabled) {
        const auxTarget = captured === target ? target : elementChain(target).find(node => node.contains(aux.target));
        if (auxTarget !== undefined && !elementChain(auxTarget).some(disabledControl)) {
          const count = fields.clickCount ?? aux.detail;
          this.#lastClick = { ...aux, target: auxTarget, detail: count };
          canceled = !dispatchTrustedEvent(auxTarget, new PointerEvent("auxclick", {
            ...init, ...pointerEvents, button: fields.button, detail: count,
          }), time) || canceled;
        }
      }
      if (type === "pointerup" && aux?.button === fields.button || type === "pointercancel") pointer.auxPress = null;
      if (type === "pointerup" && this.contextMenuOnRelease) canceled = this.contextMenu(target, disabled, fields, init, time) || canceled;
      pointer.down = null;
      pointer.clickPress = null;
      pointer.suppressMouse = false;
      this.hover(pointer, touch || type === "pointercancel" ? null : hit?.isConnected ? hit : null, fields, time);
    }
    if (pointer.hover.length === 0 && pointer.buttons === 0 && pointer.pendingCapture === null) this.pointers.delete(id);
    return canceled;
  }

  /** Fires contextmenu for a right button change; the mouse events a canceled pointerdown suppresses do not gate it. */
  contextMenu(target: Element | null, disabled: boolean, fields: PointerFields, init: ReturnType<typeof mouseInit>, time: number): boolean {
    if (!target?.isConnected || disabled || !fields.isPrimary || fields.pointerType === "touch" || fields.button !== CONTEXT_MENU_BUTTON) {
      return false;
    }
    return !dispatchTrustedEvent(target, new PointerEvent("contextmenu", { ...init, ...pointerEvents, button: CONTEXT_MENU_BUTTON }), time);
  }

  dispatch(message: InputMessage): boolean {
    const { time } = message;
    if (activates(message)) noteUserActivation(this.document);
    switch (message.type) {
      case "pointerdown": case "pointerup": case "pointermove": case "pointercancel": {
        const hit = connectedInputTarget(this.document, message.target);
        const target = hit instanceof Element ? hit : hit?.parentElement ?? null;
        const previous = connectedInputTarget(this.document, message.related);
        return this.dispatchPointer(message.type, target, message.target !== 0 && hit === null,
          previous instanceof Element && previous !== target ? previous : null, message.fields, time);
      }
      case "wheel": {
        const target = connectedInputTarget(this.document, message.target);
        if (target === null) return false;
        const { x, y, ...init } = message.fields;
        return !dispatchTrustedEvent(target, new WheelEvent("wheel", {
          ...init, clientX: x, clientY: y, screenX: x, screenY: y, bubbles: true, composed: true, cancelable: message.cancelable,
          view: this.document.defaultView,
        }), time);
      }
      case "keydown": case "keyup":
        return this.keyboard(message.type, message.fields, time);
      case "ime":
        return inputComposition(this.document, message.target, message.fields.kind, message.fields.text, message.fields.selection, time);
    }
  }

  keyboard(type: "keydown" | "keyup", fields: KeyFields, time: number): boolean {
    const target = this.document.activeElement;
    const previousSpaceTarget = this.#spaceChain[0];
    if (type === "keyup" && fields.key === " ") this.setSpaceTarget(null);
    if (type === "keydown" && !fields.altKey && !fields.ctrlKey && !fields.metaKey &&
        !["Shift", "Control", "Alt", "Meta"].includes(fields.key)) noteKeyboardModality(this.document);
    const composing = fields.isComposing || isComposing(this.document);
    const keyCode = legacyKeyCode(fields, composing);
    const init = { ...fields, isComposing: composing, ...pointerEvents, keyCode };
    const event = new KeyboardEvent(type, init);
    if (!dispatchTrustedEvent(target ?? this.document, event, time)) return true;
    const charCode = fields.key === "Enter" ? 13 : [...fields.key].length === 1 ? fields.key.codePointAt(0)! : 0;
    if (type === "keydown" && charCode !== 0 && !composing && !fields.altKey && !fields.ctrlKey && !fields.metaKey &&
        !dispatchTrustedEvent(target ?? this.document, new KeyboardEvent("keypress", { ...init, keyCode: charCode, charCode }), time)) return true;
    if (fields.key === " " && spaceActivates(target)) {
      if (type === "keydown" && !composing && !fields.altKey && !fields.ctrlKey && !fields.metaKey &&
          target === this.document.activeElement && target.isConnected && !disabledControl(target)) {
        this.setSpaceTarget(target);
      }
      if (type === "keyup" && target !== previousSpaceTarget) return false;
    }
    return target !== null && keyboardDefault(this.document, target, event, time);
  }

  setSpaceTarget(target: Element | null): void {
    const previous = this.chain("active");
    this.#spaceChain = elementChain(target);
    changeStateChain(previous, this.chain("active"), ElementState.Active);
  }
}

// The host reports no window position, so screen coordinates equal client
// coordinates, as Chromium reports them on Wayland.
function mouseInit(fields: PointerFields, document: Document) {
  return {
    pointerId: fields.pointerId, pointerType: fields.pointerType, isPrimary: fields.isPrimary,
    clientX: fields.x, clientY: fields.y, screenX: fields.x, screenY: fields.y, pressure: fields.pressure, buttons: fields.buttons,
    altKey: fields.altKey, ctrlKey: fields.ctrlKey, metaKey: fields.metaKey, shiftKey: fields.shiftKey,
    view: document.defaultView,
  } satisfies MouseInit & { pointerId: number; pointerType: string; isPrimary: boolean; pressure: number };
}

// keyCode follows Chromium: the Windows virtual-key code for a key value
// that is not printable (ui/events/keycodes/keyboard_code_conversion.cc),
// otherwise the letter or digit typed, otherwise the US-layout code of the
// physical key, and numpad keys always by their position.
const keyCodes: Record<string, number> = {
  Backspace: 8, Tab: 9, Clear: 12, Enter: 13, Shift: 16, Control: 17, Alt: 18, Pause: 19, CapsLock: 20,
  KanaMode: 21, HangulMode: 21, JunjaMode: 23, FinalMode: 24, HanjaMode: 25, KanjiMode: 25, Escape: 27,
  Convert: 28, NonConvert: 29, Accept: 30, ModeChange: 31, " ": 32, PageUp: 33, PageDown: 34, End: 35, Home: 36,
  ArrowLeft: 37, ArrowUp: 38, ArrowRight: 39, ArrowDown: 40, Select: 41, Print: 42, Execute: 43, PrintScreen: 44,
  Insert: 45, Delete: 46, Help: 47, Meta: 91, ContextMenu: 93, Standby: 95, NumLock: 144, ScrollLock: 145,
  BrowserBack: 166, BrowserForward: 167, BrowserRefresh: 168, BrowserStop: 169, BrowserSearch: 170,
  BrowserFavorites: 171, BrowserHome: 172, AudioVolumeMute: 173, AudioVolumeDown: 174, AudioVolumeUp: 175,
  MediaTrackNext: 176, MediaTrackPrevious: 177, MediaStop: 178, MediaPlayPause: 179, LaunchMail: 180,
  LaunchMediaPlayer: 181, LaunchApplication1: 182, LaunchApplication2: 183, AltGraph: 225, Process: 229,
  Attn: 246, CrSel: 247, ExSel: 248, EraseEof: 249, Play: 250, ZoomToggle: 251,
};
const numpadCodes: Record<string, number> = {
  NumpadMultiply: 106, NumpadAdd: 107, NumpadSubtract: 109, NumpadDecimal: 110, NumpadDivide: 111,
};
const punctuationCodes: Record<string, number> = {
  Space: 32, Semicolon: 186, Equal: 187, Comma: 188, Minus: 189, Period: 190, Slash: 191, Backquote: 192,
  BracketLeft: 219, Backslash: 220, BracketRight: 221, Quote: 222, IntlBackslash: 226,
};

function legacyKeyCode({ key, code }: KeyFields, composing: boolean): number {
  if (composing) return 229;
  if (Object.hasOwn(keyCodes, key)) return keyCodes[key];
  const functionKey = /^F([1-9]|1[0-9]|2[0-4])$/.exec(key);
  if (functionKey !== null) return 111 + Number(functionKey[1]);
  const numpad = /^Numpad([0-9])$/.exec(code);
  if (numpad !== null && [...key].length === 1) return 96 + Number(numpad[1]);
  if (Object.hasOwn(numpadCodes, code)) return numpadCodes[code];
  if (/^[A-Za-z0-9]$/.test(key)) return key.toUpperCase().charCodeAt(0);
  if (/^(Key[A-Z]|Digit[0-9])$/.test(code)) return code.charCodeAt(code.length - 1);
  return Object.hasOwn(punctuationCodes, code) ? punctuationCodes[code] : 0;
}

function setPointerCapture(this: Element, pointerId: number): void {
  const pointer = controllers.get(this.ownerDocument)?.pointers.get(pointerId | 0);
  if (pointer === undefined) throw new DOMException("Unknown pointer ID", "NotFoundError");
  if (!this.isConnected) throw new DOMException("The element is detached", "InvalidStateError");
  if (pointer.buttons !== 0) pointer.pendingCapture = this;
}

function releasePointerCapture(this: Element, pointerId: number): void {
  const pointer = controllers.get(this.ownerDocument)?.pointers.get(pointerId | 0);
  if (pointer === undefined) throw new DOMException("Unknown pointer ID", "NotFoundError");
  if (pointer.pendingCapture === this) pointer.pendingCapture = null;
}

function hasPointerCapture(this: Element, pointerId: number): boolean {
  return controllers.get(this.ownerDocument)?.pointers.get(pointerId | 0)?.pendingCapture === this;
}

// HTML's activation-triggering input events: keydown, a mouse or pen press, and
// the end of a touch. The pinned Chromium grants activation to every trusted
// keydown it receives through DevTools input, Escape included.
function activates(message: InputMessage): boolean {
  switch (message.type) {
    case "keydown": return true;
    case "pointerdown": return message.fields.pointerType !== "touch";
    case "pointerup": return message.fields.pointerType === "touch";
    default: return false;
  }
}

export function installInput(document: Document, host: { os?: string; inputResult(id: number, defaultPrevented: boolean): void }) {
  const controller = new InputController(document, host.os === "windows");
  controllers.set(document, controller);
  for (const [name, value] of Object.entries({ setPointerCapture, releasePointerCapture, hasPointerCapture })) {
    Object.defineProperty(Element.prototype, name, { value, configurable: true, writable: true });
  }
  return (message: InputMessage): void => {
    const prevented = controller.dispatch(message);
    host.inputResult(message.id, prevented);
  };
}
