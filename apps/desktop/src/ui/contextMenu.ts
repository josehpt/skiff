import { createMenu } from "./menu";
import { refocusTerminal } from "../workspace/view";

/** Esc gives the keys back: to Mission control while it is open, else to the terminal. */
export const ctxMenu = createMenu(() => {
  const board = document.getElementById("mission-control");
  if (board && !board.hidden) board.focus();
  else refocusTerminal();
});
