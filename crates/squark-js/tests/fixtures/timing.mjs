// SPDX-License-Identifier: MIT

import { installTiming } from "./index.js";

const events = [];
const note = (value) => events.push(value);

export function install(host) {
  return installTiming(globalThis, host);
}

export function seed() {
  Promise.resolve().then(() => note("startup promise"));
  queueMicrotask(() => note("startup microtask"));

  setTimeout(() => {
    note("timer one");
    Promise.resolve().then(() => note("timer microtask"));
  }, 5);
  setTimeout(() => note("timer two"), 5);
  clearTimeout(setTimeout(() => note("unexpected timer"), 1));

  const channel = new MessageChannel();
  const message = { text: "first" };
  channel.port1.postMessage(message);
  message.text = "mutated after send";
  channel.port1.postMessage({ text: "second" });
  channel.port2.onmessage = ({ data }) => {
    note(data.text);
    queueMicrotask(() => note(data.text + " microtask"));
  };

  let cancel;
  requestAnimationFrame((time) => {
    note("frame one " + time);
    queueMicrotask(() => {
      note("frame microtask");
      cancelAnimationFrame(cancel);
    });
    requestAnimationFrame((next) => note("next frame " + next));
  });
  cancel = requestAnimationFrame(() => note("unexpected frame"));
  requestAnimationFrame((time) => note("frame two " + time));
}

export function seedNesting() {
  const repeat = setInterval(() => {
    note("interval");
    clearTimeout(repeat);
  }, 1);
  let depth = 0;
  function nested() {
    note(++depth);
    if (depth < 7) setTimeout(nested, 0);
  }
  setTimeout(nested, 0);
}

export function seedFailures() {
  setTimeout(() => {
    queueMicrotask(() => note("throwing task microtask"));
    throw Error("timer error");
  }, 0);
  setTimeout(() => note("timer after error"), 0);
  requestAnimationFrame(() => { throw Error("frame error"); });
  requestAnimationFrame(() => note("frame after error"));
}

const idleLog = (name) => (deadline) => {
  note(`${name} ${deadline.timeRemaining()} ${deadline.didTimeout}`);
  queueMicrotask(() => note(`${name} microtask`));
};

export function seedIdle() {
  requestIdleCallback(idleLog("first"));
  cancelIdleCallback(requestIdleCallback(idleLog("cancelled")));
  requestIdleCallback((deadline) => {
    idleLog("second")(deadline);
    requestIdleCallback(idleLog("next period"));
  });
}

export function seedIdleTimeout() {
  requestIdleCallback(idleLog("timed out"), { timeout: 10 });
  setTimeout(() => note("timer"), 4);
}

export function drain() {
  return events.splice(0).join("|");
}
