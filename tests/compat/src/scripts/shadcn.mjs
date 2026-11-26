// SPDX-License-Identifier: MIT
// Shared interaction script for apps/shadcn (runs identically on ChromiumDriver and SquarkDriver).
// Exercises Tailwind v4, Radix Tabs, Dialog, Popover, DropdownMenu, and React Hook Form validation.

import assert from "node:assert/strict";
import path from "node:path";

async function snapshotShadcnState(driver) {
  const dialogStatus = (await driver.queryText('[data-testid="dialog-status"]'))?.trim() ?? null;
  const density = (await driver.queryText('[data-testid="density-value"]'))?.trim() ?? null;
  const menuAction = (await driver.queryText('[data-testid="menu-action"]'))?.trim() ?? null;
  const activeTab = await driver.eval(
    `document.querySelector('[role="tab"][data-state="active"]')?.getAttribute("data-testid") ?? null`,
  );
  const dialogVisible = (await driver.queryCount('[data-testid="dialog-content"]')) > 0;
  const popoverVisible = (await driver.queryCount('[data-testid="popover-content"]')) > 0;
  const menuVisible = (await driver.queryCount('[data-testid="menu-content"]')) > 0;
  const nameError = (await driver.queryText('[data-testid="error-name"]'))?.trim() ?? null;
  const emailError = (await driver.queryText('[data-testid="error-email"]'))?.trim() ?? null;
  const formResult = (await driver.queryText('[data-testid="form-result"]'))?.trim() ?? null;
  return {
    dialogStatus,
    density,
    menuAction,
    activeTab,
    dialogVisible,
    popoverVisible,
    menuVisible,
    nameError,
    emailError,
    formResult,
  };
}

export async function runShadcnScript(driver, { screenshotDir = null } = {}) {
  const screenshots = {};
  const checkpoints = [];
  const snap = async (name) => {
    if (!screenshotDir) return;
    const file = path.join(screenshotDir, `${name}.png`);
    await driver.screenshot(file);
    screenshots[name] = file;
  };
  const record = async (name) => {
    const state = await snapshotShadcnState(driver);
    checkpoints.push({ name, state });
    return state;
  };

  await driver.waitForSelector('[data-testid="profile-form"]');
  await driver.waitForIdle();

  const initial = await record("initial");
  assert.equal(initial.activeTab, "tab-account");
  assert.equal(initial.dialogStatus, "idle");
  assert.equal(initial.density, "comfortable");
  assert.equal(initial.menuAction, "none");
  await snap("shadcn-initial");

  // 1. Radix Tabs: Account -> Security -> Preferences -> Account
  await driver.click('[data-testid="tab-security"]');
  await driver.waitForIdle();
  const secTab = await record("tab-security");
  assert.equal(secTab.activeTab, "tab-security");

  await driver.click('[data-testid="tab-prefs"]');
  await driver.waitForIdle();
  const prefsTab = await record("tab-prefs");
  assert.equal(prefsTab.activeTab, "tab-prefs");

  await driver.click('[data-testid="tab-account"]');
  await driver.waitForIdle();

  // 2. Radix Dialog: open -> screenshot -> confirm -> closed
  await driver.click('[data-testid="open-dialog"]');
  await driver.waitForSelector('[data-testid="dialog-content"]');
  await driver.waitForIdle();
  const dialogOpen = await record("dialog-open");
  assert.equal(dialogOpen.dialogVisible, true);
  await snap("shadcn-dialog");

  await driver.click('[data-testid="dialog-confirm"]');
  await driver.waitForIdle();
  const dialogConfirmed = await record("dialog-confirmed");
  assert.equal(dialogConfirmed.dialogVisible, false);
  assert.equal(dialogConfirmed.dialogStatus, "deployed");

  // 3. Radix Popover: open -> toggle density -> Escape to dismiss
  await driver.click('[data-testid="open-popover"]');
  await driver.waitForSelector('[data-testid="popover-content"]');
  await driver.waitForIdle();
  await driver.click('[data-testid="popover-toggle"]');
  await driver.waitForIdle();
  await driver.press("Escape");
  await driver.waitForIdle();
  const afterPopover = await record("popover-toggled");
  assert.equal(afterPopover.popoverVisible, false);
  assert.equal(afterPopover.density, "compact");

  // 4. Radix DropdownMenu: open -> select Duplicate
  await driver.click('[data-testid="open-menu"]');
  await driver.waitForSelector('[data-testid="menu-content"]');
  await driver.waitForIdle();
  await driver.click('[data-testid="menu-item-duplicate"]');
  await driver.waitForIdle();
  const afterMenu = await record("menu-selected");
  assert.equal(afterMenu.menuVisible, false);
  assert.equal(afterMenu.menuAction, "duplicated");

  // 5. React Hook Form: empty submit -> validation errors -> valid submit
  await driver.click('[data-testid="submit-form"]');
  await driver.waitForIdle();
  const invalidForm = await record("form-invalid");
  assert.equal(invalidForm.nameError, "Name is required");
  assert.equal(invalidForm.emailError, "Email is required");
  assert.equal(invalidForm.formResult, null);

  await driver.click('[data-testid="input-name"]');
  await driver.type("Ada Lovelace");
  await driver.click('[data-testid="input-email"]');
  await driver.type("ada@squark.dev");
  await driver.click('[data-testid="submit-form"]');
  await driver.waitForIdle();
  const validForm = await record("form-submitted");
  assert.equal(validForm.nameError, null);
  assert.equal(validForm.emailError, null);
  assert.equal(validForm.formResult, "Saved: Ada Lovelace (ada@squark.dev) - engineer");
  await snap("shadcn-submitted");

  return {
    checkpoints,
    screenshots,
  };
}
