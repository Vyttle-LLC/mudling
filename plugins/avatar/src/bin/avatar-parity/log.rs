//! A session log, back into what the prototype received, in order.
//!
//! `SessionLogger` writes server text as it arrives and wraps each of its
//! notes in newlines of its own, so what lies between two notes is the
//! server's text exactly, and a prompt it left unended stays unended. Logs
//! hold other players' chat: they're read at runtime and never committed.

/// One thing a log recorded.
#[derive(Debug, PartialEq)]
pub enum Happened<'log> {
    Text(Vec<u8>),
    Gmcp {
        package: &'log str,
        json: &'log str,
    },
    /// A move or look the player sent, as the map note gave it.
    Command(&'log str),
}

fn is_dropped_note(line: &[u8]) -> bool {
    line == b"[connected]"
        || [
            b"[telnet] ".as_slice(),
            b"[auto-login] ",
            b"[disconnected] ",
        ]
        .iter()
        .any(|prefix| line.starts_with(prefix))
}

/// Splits on `\n` alone: `\r` is the server's, and stays in the text.
pub fn events(log: &str) -> Vec<Happened<'_>> {
    let mut happened = Vec::new();
    let mut text: Vec<&[u8]> = Vec::new();
    let end_text = |text: &mut Vec<&[u8]>, happened: &mut Vec<Happened<'_>>| {
        let joined = text.join(&b'\n');
        if !joined.is_empty() {
            happened.push(Happened::Text(joined));
        }
        text.clear();
    };
    for line in log.split('\n') {
        if let Some(message) = line.strip_prefix("[GMCP] ") {
            end_text(&mut text, &mut happened);
            let (package, json) = message.split_once(' ').unwrap_or((message, ""));
            happened.push(Happened::Gmcp {
                package,
                json: json.trim(),
            });
        } else if let Some(note) = line.strip_prefix("[MAP] ") {
            end_text(&mut text, &mut happened);
            happened.push(Happened::Command(
                note.trim_end()
                    .strip_prefix("move ")
                    .unwrap_or(note.trim_end()),
            ));
        } else if is_dropped_note(line.trim_end().as_bytes()) {
            end_text(&mut text, &mut happened);
        } else {
            text.push(line.as_bytes());
        }
    }
    end_text(&mut text, &mut happened);
    happened
}

/// Who played the log: the first welcome names them, as the prototype took
/// it, and Char.Status otherwise.
pub fn character(log: &str) -> Option<String> {
    const WELCOME: &str = "Welcome back to the AVATAR System";
    if let Some(at) = log.find(WELCOME) {
        let rest = log[at + WELCOME.len()..].trim_start_matches(',');
        let words = rest
            .split(|c: char| !c.is_alphanumeric() && c != '_' && c != ' ')
            .next()
            .unwrap_or("");
        if let Some(name) = words.split_whitespace().last() {
            return Some(name.to_owned());
        }
    }
    log.match_indices("\"character_name\"")
        .filter_map(|(at, key)| {
            let rest = log[at + key.len()..].trim_start().strip_prefix(':')?;
            let rest = rest.trim_start().strip_prefix('"')?;
            Some(rest[..rest.find('"')?].to_owned())
        })
        // Char.StatusVars gives the field's label under the same key.
        .find(|name| name != "Character Name")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn notes_come_out_and_text_keeps_its_own_endings() {
        let log = "[connected]\nHello\r\n\n[GMCP] Char.Vitals {\"hp\":\"1\"}\n(1h-2m-3v-4)(-/-)\n[MAP] move east\n\n[telnet] verb 251 option 1\nRoom\r\n";
        assert_eq!(
            events(log),
            vec![
                Happened::Text(b"Hello\r\n".to_vec()),
                Happened::Gmcp {
                    package: "Char.Vitals",
                    json: "{\"hp\":\"1\"}"
                },
                Happened::Text(b"(1h-2m-3v-4)(-/-)".to_vec()),
                Happened::Command("east"),
                Happened::Text(b"Room\r\n".to_vec()),
            ]
        );
    }

    #[test]
    fn a_pager_line_is_the_servers_text() {
        let log = "[B]ack,[H]elp,[R]efresh, or [C]ontinue: ";
        assert_eq!(events(log), vec![Happened::Text(log.as_bytes().to_vec())]);
    }

    #[test]
    fn the_character_is_who_was_welcomed() {
        let hero = "Welcome back to the AVATAR System, Hero Pat.\r\n";
        assert_eq!(character(hero).as_deref(), Some("Pat"));
        let low = "Welcome back to the AVATAR System Sam, hope you get beyond level 3 today!";
        assert_eq!(character(low).as_deref(), Some("Sam"));
        let status = "[GMCP] Char.StatusVars {\"character_name\": \"Character Name\"}\n\
                      [GMCP] Char.Status {\"character_name\": \"Lee\"}\n";
        assert_eq!(character(status).as_deref(), Some("Lee"));
        assert_eq!(character("nothing"), None);
    }
}
