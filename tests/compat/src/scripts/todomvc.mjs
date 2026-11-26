// SPDX-License-Identifier: MIT
// Shared interaction script for apps/todomvc (runs identically on ChromiumDriver and SquarkDriver).

import assert from "node:assert/strict";
import path from "node:path";

async function snapshotTodoState(driver) {
  const itemCount = await driver.queryCount('[data-testid="todo-item"]');
  const labels = await driver.eval(
    `Array.from(document.querySelectorAll('[data-testid="todo-label"]'), (el) => el.textContent.trim())`,
  );
  const completed = await driver.eval(
    `Array.from(document.querySelectorAll('[data-testid="todo-toggle"]'), (el) => Boolean(el.checked))`,
  );
  const countText = (await driver.queryText('[data-testid="todo-count"]'))?.trim() ?? null;
  const newTodoValue = await driver.queryValue('[data-testid="new-todo"]');
  return { itemCount, labels, completed, countText, newTodoValue };
}

export async function runTodoMvcScript(driver, { screenshotDir = null } = {}) {
  const screenshots = {};
  const checkpoints = [];
  const snap = async (name) => {
    if (!screenshotDir) return;
    const file = path.join(screenshotDir, `${name}.png`);
    await driver.screenshot(file);
    screenshots[name] = file;
  };
  const record = async (name) => {
    const state = await snapshotTodoState(driver);
    checkpoints.push({ name, state });
    return state;
  };

  await driver.waitForSelector('[data-testid="new-todo"]');
  await driver.waitForIdle();
  const initial = await record("initial");
  assert.equal(initial.itemCount, 0);

  async function addItem(text) {
    await driver.click('[data-testid="new-todo"]');
    await driver.type(text);
    await driver.press("Enter");
    await driver.waitForIdle();
  }

  // Add three items
  await addItem("Buy milk");
  await addItem("Write reftests");
  await addItem("Ship Excalidraw");

  const added = await record("added-3");
  assert.equal(added.itemCount, 3);
  assert.equal(added.newTodoValue, "");
  assert.equal(added.countText, "3 items left");
  await snap("todomvc-added");

  // Complete the second item ("Write reftests")
  await driver.click('[data-testid="todo-item"]:nth-child(2) [data-testid="todo-toggle"]');
  await driver.waitForIdle();
  const toggled = await record("toggled-2nd");
  assert.equal(toggled.countText, "2 items left");

  // Filter: Active -> 2 items
  await driver.click('[data-testid="filter-active"]');
  await driver.waitForIdle();
  const active = await record("filter-active");
  assert.equal(active.itemCount, 2);

  // Filter: Completed -> 1 item ("Write reftests")
  await driver.click('[data-testid="filter-completed"]');
  await driver.waitForIdle();
  const completedFilter = await record("filter-completed");
  assert.equal(completedFilter.itemCount, 1);
  assert.equal(completedFilter.labels[0], "Write reftests");
  await snap("todomvc-completed");

  // Back to All, clear completed -> 2 active items remain
  await driver.click('[data-testid="filter-all"]');
  await driver.waitForIdle();
  await driver.click('[data-testid="clear-completed"]');
  await driver.waitForIdle();
  const cleared = await record("cleared-completed");
  assert.equal(cleared.itemCount, 2);

  // Double-click first item to edit inline
  await driver.dblclick('[data-testid="todo-item"]:nth-child(1) [data-testid="todo-label"]');
  await driver.waitForSelector('[data-testid="todo-edit"]');
  await driver.press("a", { primary: true });
  await driver.type("Buy oat milk");
  await driver.press("Enter");
  await driver.waitForIdle();
  const edited = await record("edited-1st");
  assert.equal(edited.labels[0], "Buy oat milk");

  // Delete second item ("Ship Excalidraw")
  await driver.click('[data-testid="todo-item"]:nth-child(2) [data-testid="todo-destroy"]');
  await driver.waitForIdle();
  const deleted = await record("deleted-2nd");
  assert.equal(deleted.itemCount, 1);
  assert.equal(deleted.countText, "1 item left");
  await snap("todomvc-final");

  // Toggle all -> 0 items left
  await driver.click('[data-testid="toggle-all"]');
  await driver.waitForIdle();
  const toggledAll = await record("toggle-all");
  assert.equal(toggledAll.countText, "0 items left");

  return {
    remainingItems: 1,
    checkpoints,
    screenshots,
  };
}
