// SPDX-License-Identifier: MIT

const { readFile } = require("node:fs/promises");
const { dirname, resolve } = require("node:path");
const { SourceTextModule, runInContext } = require("node:vm");
const { TestEnvironment } = require(require.resolve("jest-environment-node", {
  paths: [process.env.SQUARK_REACT_SOURCE ?? process.cwd()],
}));
const dist = resolve(__dirname, "../../packages/@strange-quark/dom/dist");

module.exports = class SquarkDomEnvironment extends TestEnvironment {
  async setup() {
    await super.setup();
    const context = this.getVmContext();
    const modules = new Map();
    const load = async (file) => {
      if (!modules.has(file)) {
        modules.set(file, readFile(file, "utf8").then(
          source => new SourceTextModule(source, { identifier: file, context }),
          error => { throw new Error("Build squark-dom with npm run build --workspace @strange-quark/dom", { cause: error }); },
        ));
      }
      return modules.get(file);
    };
    const main = await load(resolve(dist, "index.js"));
    await main.link((specifier, parent) => load(resolve(dirname(parent.identifier), specifier)));
    await main.evaluate();

    const global = runInContext("globalThis", context);
    main.namespace.installDOM(global, undefined, { url: "http://localhost/" });
    const timing = {};
    const host = {
      now: () => global.performance.now(),
      armTimer() {},
      queueMicrotask: callback => global.queueMicrotask(callback),
      postTask: callback => global.setImmediate(callback),
      cloneMessage: value => structuredClone(value),
    };
    const frames = main.namespace.installTiming(timing, host);
    global.MessageChannel = timing.MessageChannel;
    global.MessagePort = timing.MessagePort;
    global.MessageEvent = timing.MessageEvent;
    const rendered = () => {
      frames.beginFrame();
      const time = host.now();
      for (let next = frames.nextAnimation(); next !== undefined; next = frames.nextAnimation()) next(time);
    };
    let frame = null;
    global.requestAnimationFrame = callback => {
      const handle = timing.requestAnimationFrame(callback);
      frame ??= global.setTimeout(() => {
        frame = null;
        rendered();
      }, 16);
      return handle;
    };
    global.cancelAnimationFrame = timing.cancelAnimationFrame;
    this.clearFrame = () => { if (frame !== null) global.clearTimeout(frame); };
  }

  async teardown() {
    this.clearFrame?.();
    await super.teardown();
  }
};
