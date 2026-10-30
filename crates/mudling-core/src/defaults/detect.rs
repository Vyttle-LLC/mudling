//! Game detection: which game plugin suits a server, as a suggestion the
//! player turns on. Each game plugin's manifest lists its `[servers]`.

use crate::plugin::{Layer, Plugin, PluginId, ServerInfo};

/// What the core learned about a server on connecting.
#[derive(Clone, Copy, Debug, Default)]
pub struct ServerSignals<'a> {
    pub host: &'a str,
    /// MSSP `NAME`.
    pub mssp_name: Option<&'a str>,
    /// The game's name from the server's GMCP `Core.Hello`.
    pub gmcp_hello: Option<&'a str>,
}

/// Which signal named the game.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Mssp,
    GmcpHello,
    Host,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Suggestion {
    pub plugin: PluginId,
    pub source: Source,
}

/// The first game plugin the server's own name for its game matches (MSSP,
/// then the GMCP hello, each against `mssp_names`), or else its host.
pub fn suggest<'a>(
    plugins: impl IntoIterator<Item = &'a Plugin>,
    signals: ServerSignals<'_>,
) -> Option<Suggestion> {
    let games: Vec<&Plugin> = plugins
        .into_iter()
        .filter(|plugin| plugin.layer() == Layer::Game)
        .collect();
    fn by_name(name: &str) -> ServerInfo<'_> {
        ServerInfo {
            host: "",
            mssp_name: Some(name),
        }
    }
    let tries = [
        (Source::Mssp, signals.mssp_name.map(by_name)),
        (Source::GmcpHello, signals.gmcp_hello.map(by_name)),
        (
            Source::Host,
            Some(ServerInfo {
                host: signals.host,
                mssp_name: None,
            }),
        ),
    ];
    tries.into_iter().find_map(|(source, server)| {
        let server = server?;
        let plugin = games
            .iter()
            .find(|plugin| plugin.manifest.matches_server(server))?;
        Some(Suggestion {
            plugin: plugin.id().clone(),
            source,
        })
    })
}
