import { invoke } from "@tauri-apps/api/core";
import { actionDialog } from "../ui/dialogParts";
import { promptAction } from "../ui/confirm";
import type { FolderInfo, Project } from "../platform/types";
import { loadProjects } from "./daemon";
import { S } from "./state";
import { refocusTerminal, selectProject } from "../workspace/view";

/** The default folder. It does not follow the agent's name, so a rename keeps its history. */
const FOLDER = "~/skiff-agent";

/**
 * Writes the Skiff agent template into an empty folder the user names, then
 * offers to add that folder as the first project. Nothing is added unless
 * the user confirms.
 */
export function setUpSkiffAgent() {
  let dir: string | null = null;
  promptAction({
    title: "Set up Skiff agent",
    body: "Skiff writes the agent's instructions, its task ledger and a results folder into an empty folder. It does not overwrite files.",
    placeholder: "a folder",
    value: FOLDER,
    action: "Set up",
    submit: async (path) => {
      dir = await invoke<string>("setup_skiff_agent", { path });
    },
    onClose: () => (dir ? offerProject(dir) : refocusTerminal()),
  });
}

function offerProject(dir: string) {
  actionDialog({
    title: "Add Skiff agent project",
    body: `Skiff wrote the agent's files to ${dir}. Add this folder as the first project in the side panel? Skiff starts a git repository in it first.`,
    action: "Add project",
    tone: "go",
    submit: async () => {
      // The folder gets its own repository, also inside another one.
      let folder = await invoke<FolderInfo>("inspect_folder", { path: dir });
      if (folder.root !== dir) {
        await invoke("init_skiff_agent_repository", { path: dir });
        folder = await invoke<FolderInfo>("inspect_folder", { path: dir });
      }
      if (folder.error) throw folder.error;
      if (folder.root !== dir) throw `Skiff could not start a git repository in ${dir}.`;
      const p = await invoke<Project>("add_project", { path: dir, name: folder.name, short: folder.short || null, color: folder.color, icon: null });
      const order = [p.name, ...S.projects.map((x) => x.name).filter((n) => n !== p.name)];
      await invoke("reorder_projects", { order });
      S.justAdded = p.name;
      await loadProjects();
      selectProject(p.name);
    },
    onClose: refocusTerminal,
  });
}
