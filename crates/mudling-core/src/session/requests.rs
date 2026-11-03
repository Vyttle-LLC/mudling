//! The GMCP requests plugins declare: each goes out when its field's value
//! changes, from a plugin the player approved. Only what a plugin's TOML
//! says is ever sent, and only as GMCP: see [`GmcpMessage`].

use crate::plugin::points::GmcpMessage;
use crate::plugin::{PluginHost, PluginId};
use crate::telnet::gmcp;
use serde_json::{Map, Value};
use std::collections::HashMap;

/// What each request's field last held, this connection.
#[derive(Debug, Default)]
pub(super) struct Requested(HashMap<(PluginId, usize), Value>);

impl Requested {
    /// The requests a message of `package` calls for: those of `approved`
    /// plugins whose field it holds with a new value.
    pub(super) fn due<'a>(
        &mut self,
        plugins: &'a PluginHost,
        approved: &[PluginId],
        package: &str,
        json: &str,
    ) -> Vec<&'a GmcpMessage> {
        let mut due = Vec::new();
        let mut message = None;
        let connections = plugins.declared(|points| points.connection.as_ref());
        for (plugin, connection) in connections.filter(|(id, _)| approved.contains(id)) {
            for (at, request) in connection.requests.iter().enumerate() {
                if !request.when.is_in(package) {
                    continue;
                }
                let message = message.get_or_insert_with(|| {
                    gmcp::from_json::<Map<String, Value>>(json).unwrap_or_default()
                });
                let Some(value) = message.get(&request.when.key) else {
                    continue;
                };
                let last = self.0.insert((plugin.clone(), at), value.clone());
                if last.as_ref() != Some(value) {
                    due.push(&request.send);
                }
            }
        }
        due
    }
}
