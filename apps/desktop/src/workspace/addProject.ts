import { invoke } from "@tauri-apps/api/core";
import { open as openFolder } from "@tauri-apps/plugin-dialog";
import { showError } from "../ui/alerts";
import type { FolderInfo, Project } from "../platform/types";

/**
 * Pick a folder, check it with the daemon, and add it with the defaults: the
 * repo's name, letters or its own icon, a random free color and theme. The
 * project's right-click menu changes them afterwards.
 */
/** `fleet`: the fleet the new project joins, from the + on a fleet head. */
export function createAddProject(onAdded: (p: Project, fleet: string | null) => void, onClose: () => void) {
  let busy = false;
  let fleet: string | null = null;
  async function add(folder: FolderInfo) {
    if (!folder.root || folder.error) throw new Error(folder.error ?? "Pick a folder in a git repository.");
    const p = await invoke<Project>("add_project", {
      path: folder.root,
      name: folder.name,
      short: folder.short || null,
      color: folder.color,
      icon: null,
    });
    await onAdded(p, fleet);
  }
  return {
    async open(into: string | null = null) {
      if (busy) return;
      busy = true;
      fleet = into;
      try {
        const dir = await openFolder({ directory: true, title: "Pick a project folder" });
        if (typeof dir !== "string") return onClose();
        const folder = await invoke<FolderInfo>("inspect_folder", { path: dir });
        if (!folder.root) {
          showError(folder.error ?? `${dir} is not inside a git repository`, {
            label: "Initialize repository",
            run: async () => {
              if (busy) throw new Error("Add project is busy. Try again.");
              busy = true;
              try {
                await invoke("init_repository", { path: dir });
                await add(await invoke<FolderInfo>("inspect_folder", { path: dir }));
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
