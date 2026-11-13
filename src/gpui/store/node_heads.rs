//! The nodes behind each paired Hopper Server (hopperd's multi-panel A3):
//! each one's own state, version and whether this PC may update it, from
//! the main's `GET /nodes/live`. Read with the panel's server list, so the
//! node groups' lights and lines follow the same changes.

use super::{Msg, Store};
use hopper::hopperd::Client;
use hopper::hopperd_nodes::{NodeHead, NodeHeads};

impl Store {
    /// Reads panel `id`'s node heads off the UI thread.
    pub(super) fn list_node_heads(&self, id: String, p: &hopper::settings::HopperPanel) {
        let Some(client) = Client::for_panel(p) else { return };
        self.spawn(move || Msg::NodeHeads(id, client.node_heads().ok()));
    }

    /// Panel `panel`'s answer: `None` before it came, or from a panel too
    /// old to say (its node groups then show the panel's own light).
    pub fn node_heads(&self, panel: &str) -> Option<&NodeHeads> {
        self.node_heads.get(panel)
    }

    /// Node `node` behind panel `panel`, when the panel named it.
    pub fn node_head(&self, panel: &str, node: &str) -> Option<&NodeHead> {
        self.node_heads(panel)?.nodes.iter().find(|n| n.id == node)
    }

    /// Asks panel `panel` to update node `node`; its line then follows the
    /// update (the panel sends a change for each step).
    pub fn update_node(&self, panel: &str, node: &str) {
        let Some(client) = self.settings.hopper_panels.iter().find(|p| p.id == panel).and_then(Client::for_panel) else { return };
        let (id, node) = (panel.to_string(), node.to_string());
        self.spawn(move || {
            let r = client.update_node(&node).map_err(|e| format!("{e:#}"));
            Msg::NodeUpdateAsked(id, client.node_heads().ok(), r.err())
        });
    }
}
