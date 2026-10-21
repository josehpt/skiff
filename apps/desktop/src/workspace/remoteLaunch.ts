/**
 * New session on a remote Host. Its projects are not reachable yet (M4), so
 * the folder is one the Host reported (a folder of its sessions or groups),
 * its home folder, or one the person types. The choices are the Host's own
 * agents. A folder here is a path on that Host: it goes only into the
 * session spec, never to an opener on this Mac.
 */
import type { Group, SessionInfo } from "../platform/types";
import type { MenuEntry } from "../ui/menu";
import { ctxMenu } from "../ui/contextMenu";
import { actionDialog } from "../ui/dialogParts";
import { h } from "../ui/dom";
import { showError } from "../ui/alerts";
import { newSession } from "../app/daemon";
import { hostConnected, hostName } from "../app/hosts";
import { S, sessions } from "../app/state";
import { launchChoices, launchDigit } from "../app/stateQueries";
import { launchIcon } from "../appearance/agentIcon";
import { basename } from "./model";
import { refocusTerminal } from "./view";

/** The folders the Host reported: its sessions' and groups' folders, once each, `first` before the rest. */
export function hostFolders(list: Iterable<SessionInfo>, groups: Group[], first?: string | null): string[] {
  const seen = new Set<string>();
  for (const s of list) if (s.cwd) seen.add(s.cwd);
  for (const g of groups) if (g.cwd) seen.add(g.cwd);
  const rest = [...seen].filter((f) => f !== first).sort();
  return first ? [first, ...rest] : rest;
}

/** A typed folder: an absolute path on the Host, or `null` (empty) for its home folder. */
export function typedFolder(typed: string, host: string): string | null {
  const t = typed.trim();
  if (!t) return null;
  if (!t.startsWith("/")) throw `Type an absolute path on ${host}, such as /home/me/repo. Leave it empty for the home folder.`;
  return t;
}

/** Shell, then the Host's agents, as the other start menus list them. */
function startEntries(cwd: string | null): MenuEntry[] {
  return launchChoices().map((a, i) => ({
    glyph: launchIcon(a),
    label: a?.id ?? "Shell",
    hint: launchDigit(i),
    run: () => start(cwd, a?.id ?? null),
  }));
}

/** A menu can stay open across a lost link, and the Host may list other
 *  agents once it is back. So the pick is read again when it runs: the
 *  agent's command as the Host lists it now, or nothing. */
function start(cwd: string | null, id: string | null) {
  const host = hostName();
  if (!hostConnected()) return showError(`${host} is not connected. Try again once it is back.`);
  const a = id === null ? null : launchChoices().find((c) => c?.id === id);
  if (a === undefined) return showError(`${host} no longer offers ${id}. Open New session again.`);
  void newSession(cwd, a?.command ?? null, a?.id ?? "", a ? "agent" : "shell").catch(showError);
}

/** The folder menu. `near`: the highlighted or focused session's folder, listed first. */
export function openRemoteNewSession(x: number, y: number, near?: string | null) {
  const host = hostName();
  const entries: MenuEntry[] = [
    { head: `FOLDER ON ${host.toUpperCase()}` },
    ...hostFolders(sessions.values(), S.groups, near).map((f) => ({ icon: "documents-folder-open", label: f, sub: startEntries(f) })),
    { icon: "documents-folder-open", label: "Home folder", sub: startEntries(null) },
    { icon: "tools-pencil", label: "Type a folder…", run: () => typeFolder(x, y) },
  ];
  ctxMenu.open(x, y, "New session", entries);
}

function typeFolder(x: number, y: number) {
  const host = hostName();
  const input = h("input", "prompt-input") as HTMLInputElement;
  input.type = "text";
  input.spellcheck = false;
  input.placeholder = "/home/me/repo";
  input.setAttribute("aria-label", `Folder on ${host}`);
  let picked: string | null | undefined;
  actionDialog({
    title: `New session on ${host}`,
    body: `Type a folder on ${host}. Leave it empty for its home folder.`,
    content: [input],
    action: "Next",
    tone: "go",
    focus: input,
    submit: () => {
      picked = typedFolder(input.value, host);
    },
    onClose: () => {
      if (picked === undefined) return refocusTerminal();
      const where = picked ? basename(picked) : "home";
      ctxMenu.open(x, y, "New session", [{ head: `NEW IN ${where.toUpperCase()}` }, ...startEntries(picked)]);
    },
  });
}
