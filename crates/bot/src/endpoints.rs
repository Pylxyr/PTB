//! The addresses of everything the bot talks to. The defaults are Twitch's; tests point them at localhost.

use pyxeetb_twitch::{eventsub, helix, irc, oauth};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Endpoints {
    pub id_base: String,
    pub helix_base: String,
    pub eventsub_url: String,
    pub irc_host: String,
    pub irc_port: u16,
    pub irc_tls: bool,
}

impl Default for Endpoints {
    fn default() -> Self {
        Self {
            id_base: oauth::ID_BASE.to_owned(),
            helix_base: helix::HELIX_BASE.to_owned(),
            eventsub_url: eventsub::WS_URL.to_owned(),
            irc_host: irc::HOST.to_owned(),
            irc_port: irc::PORT_TLS,
            irc_tls: true,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_twitchs_own_https_addresses() {
        let e = Endpoints::default();
        assert!(e.id_base.starts_with("https://id.twitch.tv/"));
        assert!(e.helix_base.starts_with("https://api.twitch.tv/"));
        assert!(e.eventsub_url.starts_with("wss://"));
        assert!(e.irc_tls);
    }
}
