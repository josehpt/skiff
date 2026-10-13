/**
 * How mission control (#88) comes and goes: open, close, go to a session,
 * maximize one, and come back from that maximize. missionControl.ts is the board.
 */

import { S } from "../app/state";
import { drop, maxReturn, toggleMaximize } from "../app/modes";
import { board, missionOpen } from "./missionControl";
import { refocusTerminal, revealSession } from "./view";

let returnTo: HTMLElement | null = null;
let leaving = false;
let starting = false;

const still = () => matchMedia("(prefers-reduced-motion: reduce)").matches;
const EASE = "cubic-bezier(0.2, 0.8, 0.2, 1)";
const FLY_MS = 200;

/** Where a session's pane is on screen: its cell, else the terminal area. */
function paneRect(id: string): DOMRect {
  const el = document.querySelector(`.cell[data-session="${CSS.escape(id)}"]`) ?? document.getElementById("terminal-host");
  return el?.getBoundingClientRect() ?? new DOMRect(0, 0, innerWidth, innerHeight);
}

/** The transform that puts a box at `at` over the box at `to`. */
function onto(at: DOMRect, to: DOMRect): string {
  return `translate(${to.left - at.left}px, ${to.top - at.top}px) scale(${to.width / at.width}, ${to.height / at.height})`;
}

/** A copy of the screen, fixed where the screen is, to fly over the lanes, which clip. */
function flier(tile: HTMLElement): { fly: HTMLElement; at: DOMRect } {
  const at = tile.getBoundingClientRect();
  const fly = tile.cloneNode(true) as HTMLElement;
  fly.classList.add("mission-fly", "lit");
  Object.assign(fly.style, { left: `${at.left}px`, top: `${at.top}px`, width: `${at.width}px`, height: `${at.height}px` });
  board.root.appendChild(fly);
  return { fly, at };
}

export async function openMission() {
  if (missionOpen() || starting) return;
  starting = true;
  returnTo = document.activeElement as HTMLElement | null;
  const pane = S.focused && document.querySelector(`.cell[data-session="${CSS.escape(S.focused)}"]`) ? paneRect(S.focused) : null;
  await board.prepare(S.focused);
  starting = false;
  const root = board.root;
  root.hidden = false;
  root.classList.remove("opening");
  void root.offsetWidth;
  root.classList.add("opening");
  window.setTimeout(() => root.classList.remove("opening"), 600);
  root.focus();
  board.start();
  const tile = board.tile(board.lit());
  if (!pane || !tile || still()) return;
  const { fly, at } = flier(tile);
  tile.style.visibility = "hidden";
  fly.animate([{ transform: onto(at, pane) }, { transform: "none" }], { duration: FLY_MS, easing: EASE }).onfinish = () => {
    fly.remove();
    tile.style.visibility = "";
  };
}

function finish() {
  leaving = false;
  board.root.classList.remove("leaving", "opening");
  board.clear();
}

function flyHome(id: string, show?: () => void, target?: () => DOMRect) {
  const tile = board.tile(id);
  if (!tile || still()) {
    show?.();
    return finish();
  }
  const { fly, at } = flier(tile);
  board.root.classList.add("leaving");
  show?.();
  const go = (to: DOMRect) =>
    (fly.animate([{ transform: "none", opacity: 1 }, { transform: onto(at, to), opacity: 1, offset: 0.8 }, { transform: onto(at, to), opacity: 0 }], {
      duration: FLY_MS,
      easing: EASE,
      fill: "forwards",
    }).onfinish = finish);
  if (target) return void go(target());
  requestAnimationFrame(() => requestAnimationFrame(() => go(paneRect(id))));
}

/** `go`: the session to show. `max`: maximize its pane. Without `go`, back to the panes as they were. */
export function closeMission(go?: string, max = false) {
  if (!missionOpen() || leaving) return;
  leaving = true;
  board.stop();
  if (go) {
    const host = () => document.getElementById("terminal-host")!.getBoundingClientRect();
    return flyHome(
      go,
      () => {
        revealSession(go);
        if (max && !S.maximized) {
          toggleMaximize();
          drop();
          maxReturn.to = () => void openMission();
        }
      },
      max ? host : undefined,
    );
  }
  if (returnTo?.isConnected) returnTo.focus();
  else refocusTerminal();
  const home = S.focused && document.querySelector(`.cell[data-session="${CSS.escape(S.focused)}"]`) ? S.focused : null;
  if (home) return flyHome(home);
  if (still()) return finish();
  board.root.animate([{ opacity: 1 }, { opacity: 0, transform: "scale(1.015)" }], { duration: 140, easing: "ease-in" }).onfinish = finish;
}

/** Ctrl+Shift+Enter here: the lit session, maximized. */
export function maximizeLit() {
  const lit = board.lit();
  if (lit) closeMission(lit, true);
}

export function toggleMission() {
  if (missionOpen()) closeMission();
  else void openMission();
}
