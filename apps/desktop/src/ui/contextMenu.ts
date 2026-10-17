import { createMenu } from "./menu";
import { refocusTerminal } from "../workspace/view";

export const ctxMenu = createMenu(() => refocusTerminal());
