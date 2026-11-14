// The one Move or copy to… dialog (MoveServer.svelte, shown by Shell).
// The server page's "…" menu and the server list's row menu open it.

export const moveDialog = $state({ server: null });

/** Opens the dialog for a server summary of this panel. */
export function openMove(server) {
  moveDialog.server = server;
}

export function closeMove() {
  moveDialog.server = null;
}
