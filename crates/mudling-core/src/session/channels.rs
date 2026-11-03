//! Lines of the chat that come as GMCP, such as a game's channels, where a
//! plugin's Text point says.

use crate::line::{Line, LinePipeline};
use crate::plugin::PluginHost;
use crate::telnet::gmcp;
use serde_json::{Map, Value};

/// The lines a message of `package` holds for the chat, colored by the ANSI
/// codes in them.
pub(super) fn lines(plugins: &PluginHost, package: &str, json: &str) -> Vec<Line> {
    let mut lines = Vec::new();
    let mut message = None;
    let texts = plugins.declared(|points| points.text.as_ref());
    for (_, text) in texts {
        for channel in text.channels.iter().filter(|c| c.message.is_in(package)) {
            let message = message.get_or_insert_with(|| {
                gmcp::from_json::<Map<String, Value>>(json).unwrap_or_default()
            });
            let Some(said) = message.get(&channel.message.key).and_then(Value::as_str) else {
                continue;
            };
            let said = said.trim_end_matches(['\r', '\n']);
            LinePipeline::default().push_line(said.as_bytes(), &mut lines);
        }
    }
    lines
}
