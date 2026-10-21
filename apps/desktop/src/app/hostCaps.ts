/** The selected Host's capabilities. Every session and terminal call goes
 *  through `hostInvoke`, which refuses a call the Host does not offer, and
 *  every surface asks `hostCan` before it offers an action. The Host
 *  connection checks each request again; nothing falls back to local data. */
import { invoke, type InvokeArgs } from "@tauri-apps/api/core";

export type HostCap =
  | "session_inventory"
  | "session_screen"
  | "session_updates"
  | "session_terminal"
  | "session_lifecycle"
  | "session_groups";

/** The major of each capability that this page speaks. */
const MAJOR: Record<HostCap, number> = {
  session_inventory: 1,
  session_screen: 1,
  session_updates: 1,
  session_terminal: 1,
  session_lifecycle: 1,
  session_groups: 1,
};

/** What each capability offers, for the refusal. */
const OFFERS: Record<HostCap, string> = {
  session_inventory: "a session list",
  session_screen: "session screens",
  session_updates: "session updates",
  session_terminal: "terminals",
  session_lifecycle: "new, ended or renamed sessions",
  session_groups: "groups",
};

/** `name@major` of the selected Host. `null` until the first answer. */
let caps: Set<string> | null = null;
let loading: Promise<void> | null = null;

/** Asks the selected Host again: at launch and after the daemon reloads. */
export function loadHostCaps(): Promise<void> {
  const load = invoke<string[]>("host_capabilities").then((list) => {
    caps = new Set(list);
  });
  loading = load.finally(() => {
    if (loading === load) loading = null;
  });
  return load;
}

/** The Host offers `cap`. Before the first answer, a surface stays as it is;
 *  the call itself still waits for the answer in `hostInvoke`. */
export function hostCan(cap: HostCap): boolean {
  return !caps || caps.has(`${cap}@${MAJOR[cap]}`);
}

/** `invoke`, only when the selected Host offers `cap`. Once the
 *  capabilities are known it calls at once, so keystrokes keep their order. */
export async function hostInvoke<T = void>(cap: HostCap, cmd: string, args?: InvokeArgs): Promise<T> {
  if (!caps) await (loading ?? loadHostCaps());
  if (!hostCan(cap)) throw new Error(`This Host does not offer ${OFFERS[cap]}.`);
  return invoke<T>(cmd, args);
}
