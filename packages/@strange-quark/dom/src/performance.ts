// SPDX-License-Identifier: MIT

// The Performance interface with the User Timing marks and measures of the
// Performance Timeline. Strange Quark records no navigation, resource or
// paint timing, so those entry types stay empty and PerformanceObserver and
// performance.timing are not defined.

import { DOMException, EventTarget } from "./events.js";

type PerformanceHost = {
  now(): number;
  cloneMessage(data: unknown): unknown;
};
type MarkOptions = { detail?: unknown; startTime?: number };
type MeasureOptions = { detail?: unknown; start?: string | number; duration?: number; end?: string | number };

const token = Symbol("Performance constructor");

// The read-only attributes of PerformanceTiming, which a Window's marks may not be named after.
const timingAttributes = new Set(`navigationStart unloadEventStart unloadEventEnd redirectStart redirectEnd fetchStart
  domainLookupStart domainLookupEnd connectStart connectEnd secureConnectionStart requestStart responseStart responseEnd
  domLoading domInteractive domContentLoadedEventStart domContentLoadedEventEnd domComplete loadEventStart loadEventEnd`
  .split(/\s+/));

type EntryState = { name: string; entryType: string; startTime: number; duration: number; detail?: unknown };
const entries = new WeakMap<object, EntryState>();

export class PerformanceEntry {
  constructor(check: symbol, state?: EntryState) {
    if (check !== token || state === undefined) throw new TypeError("Illegal constructor");
    entries.set(this, state);
  }
  get name(): string { return entries.get(this)!.name; }
  get entryType(): string { return entries.get(this)!.entryType; }
  get startTime(): number { return entries.get(this)!.startTime; }
  get duration(): number { return entries.get(this)!.duration; }
  toJSON(): object {
    const { name, entryType, startTime, duration } = entries.get(this)!;
    return { name, entryType, startTime, duration };
  }
}

function timestamp(value: unknown, name: string): number {
  const number = Number(value);
  if (!Number.isFinite(number)) throw new TypeError(`${name} is not a finite number`);
  if (number < 0) throw new TypeError(`${name} cannot be negative`);
  return number;
}

export function installPerformance(global: Record<PropertyKey, unknown>, host: PerformanceHost): void {
  const timeOrigin = Date.now() - host.now();
  const marks: PerformanceMark[] = [];
  const measures: PerformanceMeasure[] = [];

  function cloneDetail(detail: unknown): unknown {
    if (detail === undefined || detail === null) return null;
    try {
      return host.cloneMessage(detail);
    } catch (error) {
      if ((error as Error).name === "DataCloneError") throw new DOMException((error as Error).message, "DataCloneError");
      throw error;
    }
  }

  class PerformanceMark extends PerformanceEntry {
    constructor(markName: string, markOptions?: MarkOptions | null) {
      if (arguments.length === 0) throw new TypeError("PerformanceMark requires a name");
      const name = String(markName);
      if (timingAttributes.has(name)) throw new DOMException(`"${name}" is a PerformanceTiming attribute`, "SyntaxError");
      const startTime = markOptions?.startTime === undefined ? host.now() : timestamp(markOptions.startTime, "startTime");
      super(token, { name, entryType: "mark", startTime, duration: 0, detail: cloneDetail(markOptions?.detail) });
    }
    get detail(): unknown { return entries.get(this)!.detail; }
    override toJSON(): object { return { ...super.toJSON(), detail: this.detail }; }
  }

  class PerformanceMeasure extends PerformanceEntry {
    get detail(): unknown { return entries.get(this)!.detail; }
    override toJSON(): object { return { ...super.toJSON(), detail: this.detail }; }
  }

  function markTime(mark: string | number): number {
    if (typeof mark === "number") return timestamp(mark, "The timestamp");
    const name = String(mark);
    for (let index = marks.length - 1; index >= 0; index--) if (marks[index].name === name) return marks[index].startTime;
    if (name === "navigationStart") return 0;
    throw new DOMException(`The mark "${name}" does not exist`, "SyntaxError");
  }

  function sorted(list: readonly PerformanceEntry[]): PerformanceEntry[] {
    return [...list].sort((left, right) => left.startTime - right.startTime);
  }

  class Performance extends EventTarget {
    constructor(check: symbol) {
      if (check !== token) throw new TypeError("Illegal constructor");
      super();
    }

    get timeOrigin(): number { return timeOrigin; }
    now(): number { return host.now(); }
    toJSON(): object { return { timeOrigin }; }

    mark(markName: string, markOptions?: MarkOptions | null): PerformanceMark {
      if (arguments.length === 0) throw new TypeError("performance.mark requires a name");
      const mark = new PerformanceMark(markName, markOptions);
      marks.push(mark);
      return mark;
    }

    measure(measureName: string, startOrOptions?: string | MeasureOptions | null, endMark?: string): PerformanceMeasure {
      if (arguments.length === 0) throw new TypeError("performance.measure requires a name");
      const name = String(measureName);
      const options = typeof startOrOptions === "object" && startOrOptions !== null ? startOrOptions : null;
      const given = (key: keyof MeasureOptions) => options !== null && options[key] !== undefined;
      const dictionary = options !== null && (given("start") || given("end") || given("duration") || given("detail"));
      if (dictionary) {
        if (endMark !== undefined) throw new TypeError("performance.measure takes an end mark or measure options, not both");
        if (!given("start") && !given("end")) throw new TypeError("Measure options need a start or an end");
        if (given("start") && given("end") && given("duration")) throw new TypeError("Measure options cannot set start, end and duration");
      }
      const duration = () => {
        const value = Number(options!.duration);
        if (!Number.isFinite(value)) throw new TypeError("duration is not a finite number");
        return value;
      };
      let end: number;
      if (endMark !== undefined) end = markTime(String(endMark));
      else if (dictionary && given("end")) end = markTime(options!.end!);
      else if (dictionary && given("start") && given("duration")) end = markTime(options!.start!) + duration();
      else end = host.now();
      let start: number;
      if (dictionary && given("start")) start = markTime(options!.start!);
      else if (dictionary && given("duration") && given("end")) start = end - duration();
      else if (startOrOptions != null && options === null) start = markTime(String(startOrOptions));
      else start = 0;
      const detail = dictionary ? cloneDetail(options!.detail) : null;
      const measure = new PerformanceMeasure(token, { name, entryType: "measure", startTime: start, duration: end - start, detail });
      measures.push(measure);
      return measure;
    }

    clearMarks(markName?: string): void { remove(marks, markName); }
    clearMeasures(measureName?: string): void { remove(measures, measureName); }

    getEntries(): PerformanceEntry[] { return sorted([...marks, ...measures]); }
    getEntriesByType(type: string): PerformanceEntry[] {
      type = String(type);
      return type === "mark" ? sorted(marks) : type === "measure" ? sorted(measures) : [];
    }
    getEntriesByName(name: string, type?: string): PerformanceEntry[] {
      name = String(name);
      const list = type === undefined ? [...marks, ...measures] : this.getEntriesByType(type);
      return sorted(list.filter(entry => entry.name === name));
    }
  }

  function remove(list: PerformanceEntry[], name: string | undefined): void {
    const kept = name === undefined ? [] : list.filter(entry => entry.name !== String(name));
    list.splice(0, list.length, ...kept);
  }

  for (const type of [PerformanceEntry, PerformanceMark, PerformanceMeasure, Performance]) {
    Object.defineProperty(type.prototype, Symbol.toStringTag, { value: type.name, configurable: true });
  }
  Object.assign(global, { performance: new Performance(token), Performance, PerformanceEntry, PerformanceMark, PerformanceMeasure });
}
