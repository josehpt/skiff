/** render() and the throttled render for daemon events. */
import { syncMenu } from "./commands";
import { host } from "../terminal/terminalHost";
import { applyTabOrder } from "./keyboard";
import { syncPreview } from "../terminal/peek";
import { renderModes, settleModes } from "./modes";
import { renderSelectBar } from "../workspace/selection";
import { renderSidebar } from "../workspace/sidebar";
import { renderRail } from "../workspace/rail";
import { renderHeader } from "../workspace/header";
import { renderCounts } from "../workspace/status";
import { S } from "./state";
import { hasKeys, markSeen } from "./seen";

import { savePlace } from "./stored";
import { renderLayout } from "../terminal/terminal";

import { applyBackdrop } from "../appearance/backdrop";
import { applyApp, applyThemes } from "../appearance/themes";
import { step } from "../diagnostics/stalls";

/**
 * Daemon events render at most every RENDER_MS, and never while a mouse
 * button is down: a row rebuilt between press and release loses the click.
 */
const RENDER_MS = 80;

let renderTimer: number | undefined;
let renderQueued = false;
let pointerDown = false;

export function scheduleRender() {
  renderQueued = true;
  if (pointerDown || renderTimer !== undefined) return;
  renderTimer = window.setTimeout(() => {
    renderTimer = undefined;
    if (renderQueued && !pointerDown) render();
  }, RENDER_MS);
}

function releasePointer() {
  if (!pointerDown) return;
  pointerDown = false;
  // After the click this release produces has run on the old rows.
  if (renderQueued) window.setTimeout(scheduleRender, 0);
}

window.addEventListener("pointerdown", () => (pointerDown = true), true);

window.addEventListener("pointerup", releasePointer, true);

window.addEventListener("pointercancel", releasePointer, true);

window.addEventListener("blur", releasePointer);

// A release outside the window sends no pointerup.
window.addEventListener("pointermove", (e) => {
  if (pointerDown && e.buttons === 0) releasePointer();
}, true);

/** Each step is timed for the latency trace's stall report. */
export function render() {
  step("render", renderAll);
}

function renderAll() {
  renderQueued = false;
  settleModes();
  // App colors first: the sidebar's marks are inked against them.
  step("render.app", applyApp);
  host.classList.toggle("group-picked", !!S.groupPicked && S.groupPicked === S.activeGroup);
  // The keys can reach a pane without a focus change: the group row or the rail lets go.
  if (S.focused && hasKeys(S.focused)) markSeen(S.focused);
  step("render.rail", renderRail);
  step("render.sidebar", renderSidebar);
  renderSelectBar();
  step("render.layout", renderLayout);
  renderModes();
  step("render.themes", applyThemes);
  step("render.backdrop", applyBackdrop);
  step("render.header", renderHeader);
  renderCounts();
  applyTabOrder();
  // The current row may have changed: a preview whose row it no longer is ends.
  syncPreview();
  step("render.menu", syncMenu);
  savePlace({
    project: S.selectedProject,
    session: S.focused,
    group: S.activeGroup,
    picked: !!S.groupPicked && S.groupPicked === S.activeGroup,
    mode: S.maximized ? "maximize" : S.focusMode ? "focus" : null,
  });
}
