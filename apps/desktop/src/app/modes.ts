/**
 * Focus mode and maximize. Focus mode hides the rail, the tree, the top bar
 * and the status bar: the panes of the current view fill the window. Maximize
 * shows only the focused pane, with the same chrome hidden. Ctrl+Shift+Up/Down
 * step through the panes of the view, as they step through its rows in the
 * tree. While maximized, the next pane is maximized in its place.
 * Ctrl+Shift+Left/Right do nothing. Keys that go to another view (Next
 * waiting, the palette, Back, a project key) work, and going there ends the
 * mode.
 */
import { isSlot } from "../workspace/layoutSlots";
import { runAction } from "./actions";
import { host } from "../terminal/terminalHost";
import { currentRow } from "./keyboard";
import type { Action } from "./keys";
import { render } from "./render";
import { S } from "./state";
import { shownIds } from "./stateQueries";

import { fitShown } from "../terminal/terminal";

import { focusPane } from "../workspace/view";

/** Keys that move in the rail or the tree, which focus mode hides. */
const TREE_KEYS = new Set<Action>(["session-next", "session-prev", "list-project", "list-back", "list-main", "region-next", "region-prev", "project-menu"]);
/** Keys that move between panes or add one: maximize shows one pane only. */
const PANE_KEYS = new Set<Action>(["focus-left", "focus-right", "focus-up", "focus-down", "split-right", "split-down"]);
/** Keys whose menus open at the rail or a tree row. The chrome comes back first. */
const CHROME_KEYS = new Set<Action>(["new-session", "new-worktree", "add-project", "project-color", "project-changes", "project-files"]);

/** The sessions shown at the last render. Focus mode ends when none of them stays. */
let seen: string[] = [];

const hidden = () => S.focusMode || !!S.maximized;

/**
 * Entering a mode needs a session to act on. Focus mode takes a session row or
 * a group row. Maximize takes a session row only: a group has no one pane. A
 * worktree, Changes, Files, a project row or the rail is not a session. Leaving
 * a mode always works, since the tree is hidden then.
 */
export function modeKeyBlocked(a: Action): boolean {
  if (a !== "focus-mode" && a !== "maximize") return false;
  if (hidden()) return false;
  if (S.atRail) return true;
  const row = currentRow();
  if (!row) return false;
  if (row.dataset.session) return false;
  return !(a === "focus-mode" && row.dataset.group);
}

/** True when the mode takes the key: it does nothing, or already ran. */
export function modeGate(a: Action): boolean {
  if (!hidden()) return false;
  if (S.maximized && PANE_KEYS.has(a)) return true;
  // Ctrl+Shift+Up/Down step through the group's panes, as they step through its rows in the tree.
  if (a === "session-next" || a === "session-prev") {
    stepPane(a === "session-next" ? 1 : -1);
    return true;
  }
  if (TREE_KEYS.has(a)) return true;
  if (CHROME_KEYS.has(a)) {
    leaveModes();
    // The menu measures the rail and the tree, so they must be laid out first.
    requestAnimationFrame(() => runAction(a));
    return true;
  }
  return false;
}

/** The next or previous pane of the view, in tree order, wrapping at the ends. Maximize moves with it. */
function stepPane(dir: 1 | -1) {
  const ids = shownIds().filter((id) => !isSlot(id));
  const i = S.focused ? ids.indexOf(S.focused) : -1;
  if (ids.length < 2 || i < 0) return;
  const next = ids[(i + dir + ids.length) % ids.length];
  if (S.maximized) S.maximized = next;
  focusPane(next);
}

/** The view has one pane: focus mode and maximize look the same, so they are one state. */
const onePane = () => shownIds().filter((id) => !isSlot(id)).length === 1;

export function toggleFocusMode() {
  // One pane: focus mode is maximize. A second press leaves it, never a step back to the other.
  if (onePane() && !maxReturn.to) return hidden() ? leaveModes() : toggleMaximize();
  // From maximize, focus mode shows the view's panes again with the chrome still hidden.
  if (S.maximized) {
    S.focusMode = true;
    return restore();
  }
  if (hidden()) return leaveModes();
  if (!S.focused || !shownIds().includes(S.focused)) return;
  S.focusMode = true;
  enter();
}

/**
 * Where a restore goes: set when mission control maximized the pane, so a
 * restore reopens it. Mission control opens over the maximized pane first and
 * calls `restore` when it covers it. Another way out forgets it.
 */
export const maxReturn = { to: null as ((restore: () => void) => void) | null };

export function toggleMaximize() {
  if (S.maximized) {
    const back = maxReturn.to;
    maxReturn.to = null;
    // One pane: out of maximize is out of focus mode too.
    if (!back) return onePane() ? leaveModes() : restore();
    back(() => {
      stillNext();
      restore();
    });
    return;
  }
  if (!S.focused || !shownIds().includes(S.focused)) return;
  // One pane in focus mode already fills the window: the key leaves it.
  if (onePane() && S.focusMode) return leaveModes();
  S.maximized = S.focused;
  enter();
}

/** Back from maximize: the pane shrinks into its slot and the other panes fade in. */
function restore() {
  S.maximized = null;
  focusPane(S.focused!);
}

const SLIDE_MS = 180;
const EASE = "cubic-bezier(.2, .7, .2, 1)";
const reduced = () => matchMedia("(prefers-reduced-motion: reduce)").matches;
const chrome = () => [...document.querySelectorAll<HTMLElement>("#topbar, #rail, #sidebar, #statusbar")];
const cells = () => [...host.querySelectorAll<HTMLElement>(".layout .cell, .mode-lift > .cell")];
const motion: { animation: Animation; clean: () => void }[] = [];

function animate(el: HTMLElement, frames: Keyframe[], clean = () => {}) {
  const animation = el.animate(frames, { duration: SLIDE_MS, easing: EASE });
  const entry = { animation, clean };
  motion.push(entry);
  animation.onfinish = () => {
    const i = motion.indexOf(entry);
    if (i < 0) return;
    motion.splice(i, 1);
    animation.cancel();
    clean();
  };
}

/** Ends the mode animations at once: under mission control nobody sees them. */
export function drop() {
  for (const { animation, clean } of motion.splice(0)) {
    animation.cancel();
    clean();
  }
}

// A window resize needs the live layout, rather than a lift with fixed dimensions.
window.addEventListener("resize", drop);

type ChromeBox = { rect: DOMRect; opacity: string; visible: boolean };
type Before = {
  area: DOMRect;
  panes: Map<string, DOMRect>;
  chrome: Map<HTMLElement, ChromeBox>;
  ghosts: { el: HTMLElement; rect: DOMRect }[];
};
let before: Before | null = null;
let quiet = false;

/** The next render changes the mode without a slide: mission control covers the panes. */
export function stillNext() {
  quiet = true;
}
let wasHidden = false;
let wasMaximized: string | null = null;

/** A still image preserves the fade after a terminal goes to the park. */
function snapshot(cell: HTMLElement): HTMLElement {
  const copy = cell.cloneNode(true) as HTMLElement;
  const canvases = copy.querySelectorAll("canvas");
  cell.querySelectorAll("canvas").forEach((canvas, i) => {
    canvases[i].getContext("2d")?.drawImage(canvas, 0, 0);
  });
  copy.removeAttribute("data-session");
  copy.querySelectorAll("[id]").forEach((el) => el.removeAttribute("id"));
  copy.inert = true;
  copy.setAttribute("aria-hidden", "true");
  return copy;
}

/** Final terminal dimensions stay fixed while the surrounding box moves and clips. */
function lift(cell: HTMLElement, from: DOMRect, to: DOMRect, area: DOMRect, dx: number, dy: number) {
  const slot = document.createElement("div");
  slot.style.flex = cell.style.flex;
  cell.before(slot);
  const box = document.createElement("div");
  box.className = "mode-lift";
  const width = Math.max(from.width, to.width);
  const height = Math.max(from.height, to.height);
  Object.assign(box.style, {
    left: `${to.left - area.left}px`, top: `${to.top - area.top}px`,
    width: `${width}px`, height: `${height}px`,
  });
  cell.style.width = `${to.width}px`;
  cell.style.height = `${to.height}px`;
  box.appendChild(cell);
  host.appendChild(box);
  animate(box, [
    { transform: `translate(${from.left - to.left - dx}px, ${from.top - to.top - dy}px)`, clipPath: `inset(0 ${width - from.width}px ${height - from.height}px 0)` },
    { transform: "translate(0, 0)", clipPath: `inset(0 ${width - to.width}px ${height - to.height}px 0)` },
  ], () => {
    slot.replaceWith(cell);
    cell.style.removeProperty("width");
    cell.style.removeProperty("height");
    box.remove();
  });
}

function enter() {
  seen = shownIds();
  // The rail and the tree go away, so the keys go to the pane.
  S.atRail = false;
  focusPane(S.focused!);
}

export function leaveModes() {
  S.focusMode = false;
  S.maximized = null;
  maxReturn.to = null;
  if (S.focused) focusPane(S.focused);
  else render();
}

/** Runs before each render: a move away from the view ends the mode. */
export function settleModes() {
  const still = quiet;
  quiet = false;
  const shown = shownIds();
  if (S.maximized && (S.maximized !== S.focused || !shown.includes(S.maximized))) {
    S.maximized = null;
    maxReturn.to = null;
  }
  // A split or a closed pane keeps some of the sessions. Another view keeps none.
  if (S.focusMode && !shown.some((id) => seen.includes(id))) S.focusMode = false;
  seen = shown;
  if (hidden() === wasHidden && S.maximized === wasMaximized) return;
  // Read the visible positions before cancelling an interrupted transition.
  const oldCells = cells();
  before = still || reduced() ? null : {
    area: host.getBoundingClientRect(),
    panes: new Map(oldCells.map((cell) => [cell.dataset.session!, cell.getBoundingClientRect()])),
    chrome: new Map(chrome().map((el) => {
      const style = getComputedStyle(el);
      return [el, { rect: el.getBoundingClientRect(), opacity: style.opacity, visible: style.visibility === "visible" }];
    })),
    ghosts: S.maximized ? oldCells.filter((cell) => cell.dataset.session !== S.maximized).map((cell) => ({ el: snapshot(cell), rect: cell.getBoundingClientRect() })) : [],
  };
  drop();
}

/** Runs after the layout: hides the chrome while a mode is on. */
export function renderModes() {
  const on = hidden();
  const chromeChanged = on !== wasHidden;
  const paneChanged = S.maximized !== wasMaximized;
  if (!chromeChanged && !paneChanged) return;
  wasHidden = on;
  wasMaximized = S.maximized;
  const old = before;
  before = null;
  for (const el of chrome()) {
    if (on && chromeChanged) {
      const r = old?.chrome.get(el)?.rect ?? el.getBoundingClientRect();
      Object.assign(el.style, { left: `${r.left}px`, top: `${r.top}px`, width: `${r.width}px`, height: `${r.height}px` });
    } else if (!on) {
      for (const prop of ["left", "top", "width", "height"]) el.style.removeProperty(prop);
    }
    el.inert = on;
  }
  document.body.classList.toggle("focus-mode", on);
  // Fit before the first animation frame. Animation never changes these dimensions.
  fitShown();
  if (!old || reduced()) return;
  const area = host.getBoundingClientRect();
  const dx = old.area.left - area.left;
  const dy = old.area.top - area.top;
  const finalCells = cells().map((cell) => ({ cell, rect: cell.getBoundingClientRect() }));
  if (chromeChanged) {
    animate(host, [
      { transform: `translate(${dx}px, ${dy}px)` },
      { transform: "translate(0, 0)" },
    ]);
    for (const el of chrome()) {
      const r = el.getBoundingClientRect();
      const away = el.id === "topbar" ? `translateY(${-r.bottom}px)`
        : el.id === "statusbar" ? `translateY(${innerHeight - r.top}px)` : `translateX(${-r.right}px)`;
      const previous = old.chrome.get(el)!;
      const from = previous.visible ? `translate(${previous.rect.left - r.left}px, ${previous.rect.top - r.top}px)` : away;
      animate(el, [
        { transform: from, opacity: previous.opacity, visibility: "visible" },
        { transform: on ? away : "translate(0, 0)", opacity: on ? 0 : 1, visibility: "visible" },
      ]);
    }
  }
  if (paneChanged) {
    for (const { cell, rect } of finalCells) {
      const from = old.panes.get(cell.dataset.session!);
      if (from) lift(cell, from, rect, area, chromeChanged ? dx : 0, chromeChanged ? dy : 0);
      else animate(cell, [{ opacity: 0 }, { opacity: 1 }]);
    }
    for (const { el, rect } of old.ghosts) {
      el.classList.add("mode-ghost");
      Object.assign(el.style, {
        left: `${rect.left - area.left - (chromeChanged ? dx : 0)}px`, top: `${rect.top - area.top - (chromeChanged ? dy : 0)}px`,
        width: `${rect.width}px`, height: `${rect.height}px`,
      });
      host.appendChild(el);
      animate(el, [{ opacity: 1 }, { opacity: 0 }], () => el.remove());
    }
  }
}
