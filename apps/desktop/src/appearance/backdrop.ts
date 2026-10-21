/** The project's background image: one picture behind every pane of the terminal area. */
import { invoke } from "@tauri-apps/api/core";
import { isRemote } from "../app/hosts";
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

/** Blob URLs by file path. A path that failed to load stays out, so it is tried again. */
const urls = new Map<string, string>();
const loading = new Set<string>();
let shown: string | null = null;

async function load(path: string) {
  if (urls.has(path) || loading.has(path)) return;
  loading.add(path);
  try {
    const bytes = await invoke<ArrayBuffer>("read_image", { path });
    urls.set(path, URL.createObjectURL(new Blob([bytes])));
  } catch (e) {
    console.error(e);
  } finally {
    loading.delete(path);
  }
  if (path === wanted()) apply();
}

/** The focused session's project, else the selected one. */
function shownProject(): Project | undefined {
  const s = S.focused ? sessions.get(S.focused) : undefined;
  return (s && place(s)?.project) ?? S.projects.find((x) => x.name === S.selectedProject);
}

// A remote project's background is a path on that Host: never read here (M5 serves bytes).
const wanted = () => (isRemote() ? null : shownProject()?.background ?? null);

function apply() {
  const path = wanted();
  const url = path ? urls.get(path) ?? null : null;
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
  const path = wanted();
  if (path && !urls.has(path)) void load(path);
  apply();
}
