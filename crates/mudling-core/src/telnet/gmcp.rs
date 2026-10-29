//! Writing GMCP: frames, and the messages every client sends.

use super::{command, escape_into, option};

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
