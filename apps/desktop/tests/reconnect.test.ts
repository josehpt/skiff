import { test } from "node:test";
import assert from "node:assert/strict";
import { flush, moduleFromSource } from "./workspaceHarness.ts";

/** The real daemon module's remote recovery. The Host, its link and the
 *  page around it are fakes; the Host's agent list can change between
 *  connections, as it does when its daemon restarts with a new config. */
function recovery() {
  const S: any = { agents: [{ id: "agent", command: "agent --old", enabled: true }], groups: [] };
  const sessions = new Map();
  const host = { agents: [{ id: "agent", command: "agent --new", enabled: true }], fail: "" };
  const calls: string[] = [];
  const created: any[] = [];
  let agentsDuringConnect: unknown = null;
  const hostInvoke = async (_cap: string, cmd: string, args: any) => {
    calls.push(cmd);
    if (cmd === host.fail) throw new Error(`${cmd} failed`);
    if (cmd === "list_sessions") return [];
    if (cmd === "list_agents") return host.agents.map((a) => ({ ...a }));
    if (cmd === "create_session") {
      created.push(args.spec);
      return { id: "s1" };
    }
  };
  const el = { querySelector: () => ({}), replaceChildren() {} };
  const mod = moduleFromSource(
    "../src/app/daemon.ts",
    {
      "@tauri-apps/api/core": {
        invoke: async () => {
          agentsDuringConnect = [...S.agents.map((a: any) => a.command)];
          return { connected: true };
        },
        Channel: class {},
      },
      "@tauri-apps/api/event": { listen: async () => {} },
      "../ui/dom": { $: () => el },
      "./daemonRestart": { daemonBadge() {} },
      "./skiffAgent": { noteDaemon() {} },
      "./hosts": { isRemote: () => true, hostName: () => "devbox" },
      "./render": { render() {} },
      "./state": { S, sessions, born: new Set(), gone: new Set(), upsert: (s: any) => sessions.set(s.id, s) },
      "./seen": { quietSeen() {} },
      "../terminal/terminalRuntime": { muteInput() {}, reattachPanes() {} },
      "./groups": { loadGroups: async () => {} },
      "../terminal/peek": { previewToken: () => "" },
      "../terminal/terminalState": { panes: new Map() },
      "../workspace/view": { showSingle() {} },
      "../appearance/agentNames": { agentCallsign: () => "Ada" },
      "./hostCaps": { loadHostCaps: async () => {}, hostCan: () => true, hostInvoke },
    },
    { window: { addEventListener() {} }, document: { hasFocus: () => true }, Promise, Map, Set, console },
    "",
  );
  return { mod, S, host, calls, created, agentsDuringConnect: () => agentsDuringConnect };
}

test("a reconnect reads the Host's agents again, and a launch runs the new command", async () => {
  const r = recovery();
  await r.mod.reconnectRemote();
  // While the link was down, the old commands were no longer offered.
  assert.deepEqual(r.agentsDuringConnect(), []);
  assert.equal(r.calls.filter((c) => c === "list_agents").length, 1);
  assert.deepEqual([...r.S.agents.map((a: any) => a.command)], ["agent --new"]);
  // The launch menu passes the listed command to newSession.
  await r.mod.newSession("/srv", r.S.agents[0].command, "agent", "agent");
  await flush();
  assert.equal(r.created[0].command, "agent");
  assert.deepEqual([...r.created[0].args], ["--new"]);
});

test("a recovery that cannot read the agents fails, and offers none from before", async () => {
  const r = recovery();
  r.host.fail = "list_agents";
  await assert.rejects(r.mod.reconnectRemote(), /list_agents failed/);
  assert.deepEqual([...r.S.agents], []);
});
