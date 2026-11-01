//! GMCP: writing frames and the messages every client sends, and reading
//! message bodies.

use super::{command, escape_into, option};
use serde::de::DeserializeOwned;

/// Reads a GMCP message's JSON. Some games put raw control codes (ANSI
/// colors) inside JSON strings, which strict JSON rejects; those are escaped
/// and read again, so the strings still hold them. `None` when it doesn't
/// fit `T`.
pub fn from_json<T: DeserializeOwned>(json: &str) -> Option<T> {
    serde_json::from_str(json)
        .or_else(|_| serde_json::from_str(&escape_control_characters(json)))
        .ok()
}

fn escape_control_characters(json: &str) -> String {
    let mut escaped = String::with_capacity(json.len() + 16);
    for c in json.chars() {
        if c < '\u{20}' && !matches!(c, '\t' | '\n' | '\r') {
            escaped.push_str(&format!("\\u{:04x}", c as u32));
        } else {
            escaped.push(c);
        }
    }
    escaped
}

/// Appends `IAC SB GMCP <package> <data> IAC SE` to `out`. An empty `data`
/// sends the package alone.
pub fn encode(package: &str, data: &str, out: &mut Vec<u8>) {
    out.extend_from_slice(&[command::IAC, command::SB, option::GMCP]);
    escape_into(package.as_bytes(), out);
    if !data.is_empty() {
        out.push(b' ');
        escape_into(data.as_bytes(), out);
    }
    out.extend_from_slice(&[command::IAC, command::SE]);
}

/// `Core.Hello`, naming the client.
pub fn encode_hello(client: &str, version: &str, out: &mut Vec<u8>) {
    let data = serde_json::json!({ "client": client, "version": version });
    encode("Core.Hello", &data.to_string(), out);
}

/// `Core.Supports.Set`, subscribing to `modules` such as `"Char 1"`.
pub fn encode_supports_set(modules: &[impl AsRef<str>], out: &mut Vec<u8>) {
    let modules: Vec<&str> = modules.iter().map(AsRef::as_ref).collect();
    let data = serde_json::to_string(&modules).expect("a list of strings serializes");
    encode("Core.Supports.Set", &data, out);
}
