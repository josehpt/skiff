import { invoke } from "@tauri-apps/api/core";
import { open as openFolder } from "@tauri-apps/plugin-dialog";
import { showError } from "../ui/alerts";
import { promptAction } from "../ui/confirm";
import { hostInvoke } from "../app/hostCaps";
import { hostName, isRemote } from "../app/hosts";
import type { FolderInfo, Project } from "../platform/types";

/**
 * Pick a folder, check it with the Host, and add it with the defaults: the
 * repo's name, letters or its own icon, a random free color and theme. The
 * project's right-click menu changes them afterwards. The Host checks and
 * adds it (`project_config@1`): on This Mac a folder picked here, on a remote
 * Host a folder path typed for that Host, never one of this Mac.
 */
/** `fleet`: the fleet the new project joins, from the + on a fleet head. */
export function createAddProject(onAdded: (p: Project, fleet: string | null) => void, onClose: () => void) {
  let busy = false;
  let fleet: string | null = null;
  async function add(folder: FolderInfo) {
    if (!folder.root || folder.error) throw new Error(folder.error ?? "Pick a folder in a git repository.");
    const p = await hostInvoke<Project>("project_config", "add_project", {
      path: folder.root,
      name: folder.name,
      short: folder.short || null,
      color: folder.color,
    });
    await onAdded(p, fleet);
  }
  /** A remote Host: its folder, typed. Errors stay in the dialog. */
  function addRemote() {
    promptAction({
      title: `Add a project on ${hostName()}`,
      body: `A folder in a git repository on ${hostName()}, as a full path from / or ~/.`,
      placeholder: "a folder path on that Host",
      action: "Add project",
      submit: async (path) => {
        const folder = await hostInvoke<FolderInfo>("project_config", "inspect_folder", { path });
        if (!folder.root) throw new Error(folder.error ?? `${path} is not inside a git repository on ${hostName()}.`);
        await add(folder);
      },
      onClose,
    });
  }
  return {
    async open(into: string | null = null) {
      if (busy) return;
      fleet = into;
      if (isRemote()) return addRemote();
      busy = true;
      try {
        const dir = await openFolder({ directory: true, title: "Pick a project folder" });
        if (typeof dir !== "string") return onClose();
        const folder = await hostInvoke<FolderInfo>("project_config", "inspect_folder", { path: dir });
        if (!folder.root) {
          showError(folder.error ?? `${dir} is not inside a git repository`, {
            label: "Initialize repository",
            run: async () => {
              if (busy) throw new Error("Add project is busy. Try again.");
              busy = true;
              try {
                await invoke("init_repository", { path: dir });
                await add(await hostInvoke<FolderInfo>("project_config", "inspect_folder", { path: dir }));
              } finally {
                busy = false;
              }
            },
          });
          return onClose();
        }
        await add(folder);
      } catch (e) {
        showError(e);
        onClose();
      } finally {
        busy = false;
      }
    },
    get isOpen() {
      return busy;
    },
  };
}
