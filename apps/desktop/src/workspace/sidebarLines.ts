import { sessionsOf } from "./layout";

import { byStart } from "./model";
import type { Group, SessionInfo, Worktree } from "../platform/types";

import { S, sessions } from "../app/state";
import { groupedIds, place, visitors, worktreeSessions } from "../app/stateQueries";

import { groupHome } from "./view";
import { skiffAgentSession } from "../app/skiffAgent";
/** One line in a session list: a session, a group's header, or a pointer to a session from another worktree that works here. */
export type Line =
  | { session: SessionInfo; branch?: boolean; group?: undefined; pointer?: undefined; in?: Group; groupRail?: boolean }
  | { group: Group; hasPrevious: boolean; hasNext: boolean; pointer?: undefined }
  | { pointer: SessionInfo; group?: undefined };

/**
 * A worktree's lines: its groups (a group lives in the worktree of its first
 * session) with their members, then its sessions in no group.
 */
export function worktreeLines(w: Worktree | null): Line[] {
  const lines: Line[] = [];
  const groups = S.groups.filter((g) => groupHome(g) === w);
  for (const [groupIndex, g] of groups.entries()) {
    const members: Array<{ session: SessionInfo; branch: boolean }> = [];
    for (const id of sessionsOf(g.layout)) {
      const m = sessions.get(id);
      if (m) members.push({ session: m, branch: (place(m)?.worktree ?? null) !== w });
    }
    lines.push({ group: g, hasPrevious: groupIndex > 0, hasNext: groupIndex < groups.length - 1 });
    members.forEach(({ session, branch }) => {
      lines.push({ session, in: g, branch, groupRail: groupIndex < groups.length - 1 });
    });
  }
  // The Skiff agent has its own row above the projects. In a group it stays there too.
  const grouped = groupedIds();
  const agent = skiffAgentSession()?.id;
  const loose = w
    ? worktreeSessions(w)
    : [...sessions.values()].filter((x) => !place(x)).sort(byStart);
  for (const m of loose) if (!grouped.has(m.id) && m.id !== agent) lines.push({ session: m });
  if (w) for (const m of visitors(w)) lines.push({ pointer: m });
  return lines;
}
