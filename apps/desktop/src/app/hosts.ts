/** The Host this window shows, and the picker. "This Mac" is the local Host.
 *  A remote Host is an SSH destination from ssh config and its skiffd socket.
 *  Picking another Host reloads the page: no view mixes two Hosts. */
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import { ctxMenu } from "../ui/contextMenu";
import type { MenuEntry } from "../ui/menu";
import { confirmAction } from "../ui/confirm";
import { actionDialog } from "../ui/dialogParts";
import { h } from "../ui/dom";
import { showError } from "../ui/alerts";

export interface KnownHost {
  destination: string;
  socket: string;
  /** OSC 52 clipboard writes from this Host's terminals reach this Mac. */
  clipboard: boolean;
}

interface HostList {
  /** `null`: this Mac. */
  selected: string | null;
  hosts: KnownHost[];
}

let list: HostList = { selected: null, hosts: [] };
let connected = true;

/** Reads the known Hosts and the selected one. Call it before anything loads. */
export async function loadHosts(): Promise<void> {
  list = await invoke<HostList>("list_hosts");
  renderHostLabel();
}

/** The window shows a remote Host: this Mac's projects, files, ledger and
 *  Skipper are not its data, and are not shown. */
export const isRemote = () => list.selected !== null;

export const hostName = () => list.selected ?? "This Mac";

/** A program in a terminal may write the clipboard: always on this Mac, and
 *  on a remote Host only after the person allows it for that Host. */
export function clipboardWrites(): boolean {
  if (!isRemote()) return true;
  return list.hosts.some((x) => x.destination === list.selected && x.clipboard);
}

/** The link to the shown Host is up or down. While down, input is off. */
export function setHostConnected(up: boolean) {
  connected = up;
  renderHostLabel();
}

export const hostConnected = () => connected;

function renderHostLabel() {
  const name = hostName();
  document.title = isRemote() ? `Skiff — ${name}` : "Skiff";
  let cell = document.getElementById("host-cell");
  const settings = document.getElementById("open-settings");
  if (!cell && settings) {
    cell = h("button", "tb-cell host-cell");
    cell.id = "host-cell";
    (cell as HTMLButtonElement).type = "button";
    cell.setAttribute("aria-haspopup", "menu");
    cell.addEventListener("click", () => openHostMenu(cell!));
    settings.before(cell);
  }
  if (!cell) return;
  const state = connected ? "connected" : "error";
  cell.replaceChildren(h("span", `dot ${state}`), h("span", "host-name", name));
  cell.classList.toggle("remote", isRemote());
  cell.title = connected
    ? `Host: ${name}. Pick another Host.`
    : `Host: ${name}, disconnected. Input is off. Skiff connects again on its own.`;
  cell.setAttribute("aria-label", cell.title);
}

function pick(destination: string | null) {
  if (destination === list.selected) return;
  invoke("select_host", { destination })
    .then(() => location.reload())
    .catch(showError);
}

/** The Host menu: pick a Host, add one, or forget one. */
export function openHostMenu(anchor: HTMLElement) {
  const r = anchor.getBoundingClientRect();
  const mark = (on: boolean) => (on ? "indicators-check" : undefined);
  const entries: MenuEntry[] = [
    { head: "HOST" },
    { icon: mark(!isRemote()), label: "This Mac", run: () => pick(null) },
    ...list.hosts.map((x) => ({ icon: mark(x.destination === list.selected), label: x.destination, run: () => pick(x.destination) })),
    { icon: "indicators-plus", label: "Add SSH Host…", run: addHostDialog },
  ];
  const others = list.hosts.filter((x) => x.destination !== list.selected);
  if (others.length) {
    entries.push({
      icon: "tools-trash-2",
      label: "Forget Host…",
      sub: others.map((x) => ({ label: x.destination, danger: true, run: () => void forget(x) })),
    });
  }
  if (isRemote()) {
    entries.push({
      icon: "code-copy",
      label: clipboardWrites() ? "Block clipboard writes from this Host" : "Allow clipboard writes from this Host…",
      run: () => void toggleClipboard(),
    });
  }
  ctxMenu.open(r.left, r.bottom, "Host", entries);
}

async function forget(x: KnownHost) {
  const ok = await confirmAction({
    title: `Forget ${x.destination}?`,
    body: "Skiff removes it from the Host list. Its sessions keep running on that Host.",
    action: "Forget Host",
  });
  if (!ok) return;
  await invoke("remove_host", { destination: x.destination }).catch(showError);
  await loadHosts().catch(showError);
}

async function toggleClipboard() {
  const allow = !clipboardWrites();
  if (allow) {
    const ok = await confirmAction({
      title: `Allow clipboard writes from ${hostName()}?`,
      body: "A program in a terminal on this Host can then replace this Mac's clipboard with OSC 52. It cannot read the clipboard.",
      action: "Allow writes",
    });
    if (!ok) return;
  }
  await invoke("set_host_clipboard", { destination: list.selected, allow }).catch(showError);
  await loadHosts().catch(showError);
}

/** Two fields: what `ssh` takes, and the skiffd socket on that Host. */
function addHostDialog() {
  const field = (placeholder: string, label: string) => {
    const input = h("input", "prompt-input") as HTMLInputElement;
    input.type = "text";
    input.spellcheck = false;
    input.placeholder = placeholder;
    input.setAttribute("aria-label", label);
    return input;
  };
  const destination = field("devbox", "SSH destination");
  const socket = field("/run/user/1000/skiff/skiffd.sock", "skiffd socket on that Host");
  actionDialog({
    title: "Add SSH Host",
    body: "Type the Host as you type it after ssh, such as an alias from ssh config. Its host key must be in known_hosts. On that Host, `skiff info` prints the socket.",
    content: [destination, socket],
    action: "Add Host",
    tone: "go",
    focus: destination,
    submit: async () => {
      if (!destination.value.trim()) {
        destination.focus();
        throw "Type the SSH destination.";
      }
      if (!socket.value.trim()) {
        socket.focus();
        throw "Type the socket path.";
      }
      await invoke("add_host", { destination: destination.value, socket: socket.value });
      await loadHosts();
    },
  });
}

/** Rust says the remote link ended. `reconnect` runs until it is back. */
export function watchHostLink(reconnect: () => Promise<void>) {
  void listen("skiff:host-lost", () => {
    if (!connected) return;
    setHostConnected(false);
    void reconnect();
  });
}
