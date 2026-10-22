/** The project's background image: one picture behind every pane of the terminal area. */
import { hostInvoke } from "../app/hostCaps";
import { host } from "../terminal/terminalHost";
import { S, sessions } from "../app/state";
import { place } from "../app/stateQueries";
import type { Project } from "../platform/types";

/** How opaque the panes are over a background image, in percent, when the project sets none. */
export const OPACITY_DEFAULT = 85;
export const OPACITY_MIN = 40;

/** A value the opacity dialog shows while the slider moves. Null: the project's own. */
let trial: number | null = null;

export function previewOpacity(percent: number | null) {
  trial = percent;
  apply();
}

/**
 * Blob URLs by project and the Host's background path. The bytes come from
 * the selected Host (`project_assets@1`), checked there as an image: This Mac
 * and a remote Host alike, never a path read here. A key that failed to load
 * waits before it is tried again, so a missing file is not asked on every render.
 */
const urls = new Map<string, string>();
const loading = new Set<string>();
const failedAt = new Map<string, number>();
const RETRY_MS = 30_000;
let shown: string | null = null;

async function load(key: string, project: string) {
  if (urls.has(key) || loading.has(key) || Date.now() - (failedAt.get(key) ?? -Infinity) < RETRY_MS) return;
  loading.add(key);
  try {
    const bytes = await hostInvoke<ArrayBuffer>("project_assets", "project_background", { project });
    urls.set(key, URL.createObjectURL(new Blob([bytes])));
    failedAt.delete(key);
  } catch (e) {
    failedAt.set(key, Date.now());
    console.error(e);
  } finally {
    loading.delete(key);
  }
  if (key === wanted()) apply();
}

/** The focused session's project, else the selected one. */
function shownProject(): Project | undefined {
  const s = S.focused ? sessions.get(S.focused) : undefined;
  return (s && place(s)?.project) ?? S.projects.find((x) => x.name === S.selectedProject);
}

/** The shown project's background, as a key: its name and the Host's path for it. */
function wanted(): string | null {
  const p = shownProject();
  return p?.background ? `${p.name}\n${p.background}` : null;
}

function apply() {
  const key = wanted();
  const url = key ? urls.get(key) ?? null : null;
  const percent = trial ?? shownProject()?.background_opacity ?? OPACITY_DEFAULT;
  host.classList.toggle("has-bg", !!url);
  host.style.setProperty("--pane-alpha", url ? String(percent / 100) : "1");
  if (url !== shown) {
    shown = url;
    host.style.setProperty("--bg-image", url ? `url("${url}")` : "none");
  }
}

/** Call on every render. Cheap when nothing changed. */
export function applyBackdrop() {
  const key = wanted();
  const p = shownProject();
  if (key && p && !urls.has(key)) void load(key, p.name);
  apply();
}
