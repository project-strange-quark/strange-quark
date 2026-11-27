// SPDX-License-Identifier: MIT

import { NodeList } from "./collections.js";
import type { Node } from "./document.js";
import { dispatchTrustedEvent, Event } from "./events.js";

type Options = {
  childList?: boolean;
  attributes?: boolean;
  characterData?: boolean;
  subtree?: boolean;
  attributeOldValue?: boolean;
  characterDataOldValue?: boolean;
  attributeFilter?: readonly string[];
};
type RecordType = "attributes" | "characterData" | "childList";

export class MutationRecord {
  readonly type: RecordType;
  readonly target: Node;
  readonly addedNodes: NodeList<Node>;
  readonly removedNodes: NodeList<Node>;
  readonly previousSibling: Node | null;
  readonly nextSibling: Node | null;
  readonly attributeName: string | null;
  readonly attributeNamespace: string | null;
  readonly oldValue: string | null;

  constructor(target: Node, change: Change, oldValue: string | null) {
    if (target === undefined) throw new TypeError("Illegal MutationRecord constructor");
    this.type = change.type;
    this.target = target;
    this.addedNodes = new NodeList(() => change.type === "childList" ? change.added : []);
    this.removedNodes = new NodeList(() => change.type === "childList" ? change.removed : []);
    this.previousSibling = change.type === "childList" ? change.previous : null;
    this.nextSibling = change.type === "childList" ? change.next : null;
    this.attributeName = change.type === "attributes" ? change.name : null;
    this.attributeNamespace = change.type === "attributes" ? change.namespace : null;
    this.oldValue = oldValue;
  }
}

type Change =
  | { type: "attributes"; name: string; namespace: string | null; oldValue: string | null }
  | { type: "characterData"; oldValue: string }
  | { type: "childList"; added: Node[]; removed: Node[]; previous: Node | null; next: Node | null };
type ObserverState = {
  callback(records: MutationRecord[], observer: MutationObserver): void;
  nodes: Set<Node>;
  records: MutationRecord[];
  transient: Set<Registration>;
};
type Registration = {
  observer: MutationObserver;
  options: Options;
  node: Node;
  source?: Registration;
};

const states = new WeakMap<MutationObserver, ObserverState>();
const observed = new WeakMap<Node, Set<Registration>>();
const pending = new Set<MutationObserver>();
// DOM's signal slots: slots whose assigned nodes changed, notified with the
// mutation observers.
const signalSlots = new Set<Node>();
let queued = false;

function removeTransient(state: ObserverState, source?: Registration): void {
  for (const registration of state.transient) {
    if (source === undefined || source === registration.source) {
      observed.get(registration.node)?.delete(registration);
      state.transient.delete(registration);
    }
  }
}

export class MutationObserver {
  constructor(callback: ObserverState["callback"]) {
    if (typeof callback !== "function") throw new TypeError("MutationObserver requires a callback");
    states.set(this, { callback, nodes: new Set(), records: [], transient: new Set() });
  }

  observe(target: Node, init: Options): void {
    if (typeof target !== "object" || target === null || typeof target.nodeType !== "number" ||
        typeof target.getRootNode !== "function") throw new TypeError("Expected a Node");
    const options: Options = {
      attributeFilter: init?.attributeFilter === undefined ? undefined : Array.from(init.attributeFilter, String),
      attributeOldValue: init?.attributeOldValue === undefined ? undefined : Boolean(init.attributeOldValue),
      attributes: init?.attributes === undefined ? undefined : Boolean(init.attributes),
      characterData: init?.characterData === undefined ? undefined : Boolean(init.characterData),
      characterDataOldValue: init?.characterDataOldValue === undefined ? undefined : Boolean(init.characterDataOldValue),
      childList: Boolean(init?.childList), subtree: Boolean(init?.subtree),
    };
    options.attributes ??= options.attributeFilter !== undefined || options.attributeOldValue !== undefined;
    options.characterData ??= options.characterDataOldValue !== undefined;
    if (!options.attributes && !options.characterData && !options.childList ||
        !options.attributes && (options.attributeFilter !== undefined || options.attributeOldValue) ||
        !options.characterData && options.characterDataOldValue) {
      throw new TypeError("Inconsistent mutation observer options");
    }

    const state = states.get(this)!;
    let registrations = observed.get(target);
    if (registrations === undefined) observed.set(target, registrations = new Set());
    const previous = [...registrations].find(registration => registration.observer === this && registration.source === undefined);
    if (previous === undefined) {
      registrations.add({ observer: this, options, node: target });
      state.nodes.add(target);
    } else {
      removeTransient(state, previous);
      previous.options = options;
    }
  }

  disconnect(): void {
    const state = states.get(this)!;
    for (const node of state.nodes) {
      const registrations = observed.get(node)!;
      for (const registration of registrations) {
        if (registration.observer === this) registrations.delete(registration);
      }
    }
    state.nodes.clear();
    state.records = [];
    removeTransient(state);
  }

  takeRecords(): MutationRecord[] {
    const state = states.get(this)!;
    const records = state.records;
    state.records = [];
    return records;
  }
}

function schedule(): void {
  if (queued) return;
  queued = true;
  const global = globalThis as unknown as { queueMicrotask(callback: () => void): void; reportError?: (error: unknown) => void; console?: { error(error: unknown): void } };
  global.queueMicrotask(() => {
    queued = false;
    const current = [...pending];
    pending.clear();
    const slots = [...signalSlots];
    signalSlots.clear();
    for (const observer of current) {
      const state = states.get(observer)!;
      const records = observer.takeRecords();
      removeTransient(state);
      if (records.length === 0) continue;
      try {
        state.callback.call(observer, records, observer);
      } catch (error) {
        if (global.reportError !== undefined) global.reportError(error);
        else global.console?.error(error);
      }
    }
    for (const slot of slots) dispatchTrustedEvent(slot, new Event("slotchange", { bubbles: true }));
  });
}

/** DOM's "signal a slot change": slotchange fires after the next mutation observer callbacks. */
export function signalSlotChange(slot: Node): void {
  signalSlots.add(slot);
  schedule();
}

export function queueMutation(target: Node, change: Change): void {
  let interested: Map<MutationObserver, string | null> | undefined;
  for (let node: Node | null = target; node !== null; node = node.parentNode) {
    const registrations = observed.get(node);
    if (registrations === undefined) continue;
    for (const { observer, options } of registrations) {
      if (node !== target && !options.subtree || !options[change.type]) continue;
      if (change.type === "attributes" && options.attributeFilter !== undefined &&
          (change.namespace !== null || !options.attributeFilter.includes(change.name))) continue;
      const wantsOld = change.type === "attributes" && options.attributeOldValue ||
        change.type === "characterData" && options.characterDataOldValue;
      interested ??= new Map();
      if (wantsOld) interested.set(observer, change.oldValue);
      else if (!interested.has(observer)) interested.set(observer, null);
    }
  }
  if (interested === undefined) return;
  for (const [observer, oldValue] of interested) {
    states.get(observer)!.records.push(new MutationRecord(target, change, oldValue));
    pending.add(observer);
  }
  schedule();
}

export function observeDetachedSubtree(target: Node, parent: Node): void {
  for (let node: Node | null = parent; node !== null; node = node.parentNode) {
    const sources = observed.get(node);
    if (sources === undefined) continue;
    for (const registration of sources) {
      if (!registration.options.subtree) continue;
      const { observer, options, source } = registration;
      let registrations = observed.get(target);
      if (registrations === undefined) observed.set(target, registrations = new Set());
      const transient = { observer, options, node: target, source: source ?? registration };
      registrations.add(transient);
      states.get(observer)!.transient.add(transient);
      pending.add(observer);
      schedule();
    }
  }
}
