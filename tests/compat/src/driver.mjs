// SPDX-License-Identifier: MIT
// Abstract AppDriver contract shared between ChromiumDriver (Playwright) and
// SquarkDriver (NDJSON --test-control client).

/**
 * Base driver interface for cross-engine app interaction scripts.
 * Subclasses must implement all interaction, query, and screenshot methods.
 */
export class AppDriver {
  /** @param {string | { x: number, y: number }} _target */
  async click(_target) {
    throw new Error("AppDriver.click not implemented");
  }

  /** @param {string | { x: number, y: number }} _target */
  async dblclick(_target) {
    throw new Error("AppDriver.dblclick not implemented");
  }

  /** @param {number} _x @param {number} _y */
  async move(_x, _y) {
    throw new Error("AppDriver.move not implemented");
  }

  /**
   * @param {{ x: number, y: number }} _from
   * @param {{ x: number, y: number }} _to
   * @param {number} [_steps]
   */
  async drag(_from, _to, _steps = 5) {
    throw new Error("AppDriver.drag not implemented");
  }

  /**
   * @param {number} _x @param {number} _y
   * @param {number} _dx @param {number} _dy
   * @param {{ shift?: boolean, ctrl?: boolean, alt?: boolean, meta?: boolean }} [_modifiers]
   */
  async wheel(_x, _y, _dx, _dy, _modifiers = {}) {
    throw new Error("AppDriver.wheel not implemented");
  }

  /** @param {string} _text */
  async type(_text) {
    throw new Error("AppDriver.type not implemented");
  }

  /**
   * @param {string} _key
   * @param {{ shift?: boolean, ctrl?: boolean, alt?: boolean, meta?: boolean, primary?: boolean }} [_modifiers]
   */
  async press(_key, _modifiers = {}) {
    throw new Error("AppDriver.press not implemented");
  }

  /** @param {string} _selector @param {number} [_timeoutMs] */
  async waitForSelector(_selector, _timeoutMs = 10_000) {
    throw new Error("AppDriver.waitForSelector not implemented");
  }

  /** @param {number} [_frames] */
  async waitForIdle(_frames = 2) {
    throw new Error("AppDriver.waitForIdle not implemented");
  }

  /** @param {string} _selector @returns {Promise<string | null>} */
  async queryText(_selector) {
    throw new Error("AppDriver.queryText not implemented");
  }

  /** @param {string} _selector @param {string} _name @returns {Promise<string | null>} */
  async queryAttr(_selector, _name) {
    throw new Error("AppDriver.queryAttr not implemented");
  }

  /** @param {string} _selector @returns {Promise<number>} */
  async queryCount(_selector) {
    throw new Error("AppDriver.queryCount not implemented");
  }

  /** @param {string} _selector @returns {Promise<string | null>} */
  async queryValue(_selector) {
    throw new Error("AppDriver.queryValue not implemented");
  }

  /**
   * @param {string} _selector
   * @returns {Promise<{ x: number, y: number, width: number, height: number } | null>}
   */
  async queryRect(_selector) {
    throw new Error("AppDriver.queryRect not implemented");
  }

  /** @param {string} _expr @returns {Promise<unknown>} */
  async eval(_expr) {
    throw new Error("AppDriver.eval not implemented");
  }

  /** @param {string} _outPath @returns {Promise<Buffer>} */
  async screenshot(_outPath) {
    throw new Error("AppDriver.screenshot not implemented");
  }

  async close() {
    throw new Error("AppDriver.close not implemented");
  }
}

export function platformModifiers(modifiers = {}, platform = process.platform) {
  const { primary, ...rest } = modifiers;
  if (!primary) return rest;
  return platform === "darwin" ? { ...rest, meta: true } : { ...rest, ctrl: true };
}
