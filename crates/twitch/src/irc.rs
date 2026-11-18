//! Twitch chat over IRC: framing, parsing and the lines to send. No I/O.

use crate::Secret;

pub const HOST: &str = "irc.chat.twitch.tv";
pub const PORT_TLS: u16 = 6697;

/// A line longer than this is not IRC; the buffer is dropped instead of growing without bound.
const MAX_LINE_BYTES: usize = 64 * 1024;

/// Turns arbitrary chunks of bytes (a read can end mid-line) into complete lines.
#[derive(Debug, Default)]
pub struct LineBuffer {
    pending: Vec<u8>,
}

impl LineBuffer {
    /// Add bytes; get back every line they completed, without the CRLF.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.pending.extend_from_slice(bytes);
        let mut lines = Vec::new();
        while let Some(end) = self.pending.windows(2).position(|w| w == b"\r\n") {
            let line: Vec<u8> = self.pending.drain(..end + 2).take(end).collect();
            lines.push(String::from_utf8_lossy(&line).into_owned());
        }
        if self.pending.len() > MAX_LINE_BYTES {
            self.pending.clear();
        }
        lines
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub tags: Vec<(String, String)>,
    pub prefix: Option<String>,
    pub command: String,
    pub params: Vec<String>,
}

impl Message {
    #[must_use]
    pub fn tag(&self, key: &str) -> Option<&str> {
        self.tags
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }

    /// The last parameter, which is the text of a PRIVMSG or NOTICE.
    #[must_use]
    pub fn trailing(&self) -> Option<&str> {
        self.params.last().map(String::as_str)
    }

    /// The nickname in the prefix `nick!user@host`.
    #[must_use]
    pub fn nick(&self) -> Option<&str> {
        let prefix = self.prefix.as_deref()?;
        Some(prefix.split_once('!').map_or(prefix, |(nick, _)| nick))
    }
}

fn unescape_tag(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some(':') => out.push(';'),
            Some('s') => out.push(' '),
            Some('r') => out.push('\r'),
            Some('n') => out.push('\n'),
            Some('\\') | None => out.push('\\'),
            Some(other) => out.push(other),
        }
    }
    out
}

/// Parse one IRC line (with IRCv3 tags). `None` for an empty or malformed line.
#[must_use]
pub fn parse(line: &str) -> Option<Message> {
    let mut rest = line.trim_end_matches(['\r', '\n']);
    let mut tags = Vec::new();
    if let Some(after) = rest.strip_prefix('@') {
        let (raw, remainder) = after.split_once(' ')?;
        tags = raw
            .split(';')
            .filter(|t| !t.is_empty())
            .map(|t| t.split_once('=').map_or((t, ""), |(k, v)| (k, v)))
            .map(|(k, v)| (k.to_owned(), unescape_tag(v)))
            .collect();
        rest = remainder.trim_start();
    }
    let mut prefix = None;
    if let Some(after) = rest.strip_prefix(':') {
        let (p, remainder) = after.split_once(' ')?;
        prefix = Some(p.to_owned());
        rest = remainder.trim_start();
    }
    let (head, trailing) = rest
        .split_once(" :")
        .map_or((rest, None), |(h, t)| (h, Some(t)));
    let mut words = head.split_whitespace();
    let command = words.next()?.to_owned();
    let mut params: Vec<String> = words.map(str::to_owned).collect();
    params.extend(trailing.map(str::to_owned));
    Some(Message {
        tags,
        prefix,
        command,
        params,
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Privmsg {
    pub id: String,
    pub login: String,
    pub channel: String,
    pub text: String,
}

impl Message {
    /// A chat message, if this is one that carries an id.
    #[must_use]
    pub fn as_privmsg(&self) -> Option<Privmsg> {
        if self.command != "PRIVMSG" {
            return None;
        }
        Some(Privmsg {
            id: self.tag("id")?.to_owned(),
            login: self.nick()?.to_owned(),
            channel: self.params.first()?.trim_start_matches('#').to_owned(),
            text: self.trailing()?.to_owned(),
        })
    }

    /// True once the server has finished joining a channel (end of the NAMES list, or the ROOMSTATE).
    #[must_use]
    pub fn is_join_done(&self) -> bool {
        self.command == "ROOMSTATE" || self.command == "366"
    }

    /// The text of a NOTICE that says the login failed, if this is one.
    #[must_use]
    pub fn login_failure(&self) -> Option<&str> {
        let text = self.trailing()?;
        (self.command == "NOTICE" && text.to_ascii_lowercase().contains("authentication failed"))
            .then_some(text)
    }
}

/// Everything sent after connecting: request tags, log in, join one channel. Contains the token.
#[must_use]
pub fn login_lines(login: &str, token: &Secret, channel: &str) -> String {
    format!(
        "CAP REQ :twitch.tv/tags twitch.tv/commands\r\nPASS oauth:{}\r\nNICK {}\r\nJOIN #{}\r\n",
        token.expose(),
        login.to_ascii_lowercase(),
        channel.to_ascii_lowercase()
    )
}

/// The reply to `PING :server`, if this message is one.
#[must_use]
pub fn pong_for(message: &Message) -> Option<String> {
    (message.command == "PING").then(|| {
        format!(
            "PONG :{}\r\n",
            message.trailing().unwrap_or("tmi.twitch.tv")
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const PRIVMSG: &str = "@badge-info=;color=#FF0000;display-name=Chatter;id=8d1f-77;tmi-sent-ts=1700000000123;user-id=2 :chatter!chatter@chatter.tmi.twitch.tv PRIVMSG #pylxyr :hello world :)";

    #[test]
    fn lines_are_cut_across_reads() {
        let mut buffer = LineBuffer::default();
        assert_eq!(buffer.push(b"PING :tmi.tw"), Vec::<String>::new());
        assert_eq!(buffer.push(b"itch.tv\r\nPONG"), ["PING :tmi.twitch.tv"]);
        assert_eq!(buffer.push(b" x\r\n\r\nlast\r\n"), ["PONG x", "", "last"]);
    }

    #[test]
    fn a_split_crlf_is_still_one_line_ending() {
        let mut buffer = LineBuffer::default();
        assert_eq!(buffer.push(b"abc\r"), Vec::<String>::new());
        assert_eq!(buffer.push(b"\ndef\r\n"), ["abc", "def"]);
    }

    #[test]
    fn an_endless_line_is_dropped_not_stored() {
        let mut buffer = LineBuffer::default();
        assert_eq!(
            buffer.push(&vec![b'x'; MAX_LINE_BYTES + 1]),
            Vec::<String>::new()
        );
        assert_eq!(buffer.push(b"ok\r\n"), ["ok"]);
    }

    #[test]
    fn a_chat_message_is_read_with_its_tags() {
        let m = parse(PRIVMSG).unwrap();
        assert_eq!(
            (m.command.as_str(), m.nick(), m.tag("tmi-sent-ts")),
            ("PRIVMSG", Some("chatter"), Some("1700000000123"))
        );
        assert_eq!(m.tag("color"), Some("#FF0000"));
        assert_eq!(m.tag("badge-info"), Some(""));
        assert_eq!(
            m.as_privmsg().unwrap(),
            Privmsg {
                id: "8d1f-77".into(),
                login: "chatter".into(),
                channel: "pylxyr".into(),
                text: "hello world :)".into()
            }
        );
    }

    #[test]
    fn tag_escapes_are_undone() {
        let m = parse(r"@msg=a\sb\:c\\d\nz;empty= :tmi.twitch.tv NOTICE * :x").unwrap();
        assert_eq!(m.tag("msg"), Some("a b;c\\d\nz"));
        assert_eq!(m.tag("empty"), Some(""));
    }

    #[test]
    fn lines_without_tags_or_prefix_parse() {
        let m = parse("PING :tmi.twitch.tv").unwrap();
        assert_eq!(
            (m.command.as_str(), m.params.as_slice(), m.prefix.as_deref()),
            ("PING", ["tmi.twitch.tv".to_string()].as_slice(), None)
        );
        assert_eq!(pong_for(&m).as_deref(), Some("PONG :tmi.twitch.tv\r\n"));
        let numeric = parse(":tmi.twitch.tv 001 mybot :Welcome, GLHF!").unwrap();
        assert_eq!((numeric.command.as_str(), numeric.params.len()), ("001", 2));
        assert_eq!(pong_for(&numeric), None);
    }

    #[test]
    fn empty_and_broken_lines_are_none() {
        assert!(parse("").is_none());
        assert!(parse("@tags-only").is_none());
        assert!(parse(":prefix-only").is_none());
    }

    #[test]
    fn a_privmsg_without_an_id_is_not_usable() {
        assert!(
            parse(":a!a@a PRIVMSG #c :hi")
                .unwrap()
                .as_privmsg()
                .is_none()
        );
        assert!(
            parse("@id=1 :a!a@a JOIN #c")
                .unwrap()
                .as_privmsg()
                .is_none()
        );
    }

    #[test]
    fn joining_and_failing_are_recognised() {
        assert!(
            parse(":mybot.tmi.twitch.tv 366 mybot #c :End of /NAMES list")
                .unwrap()
                .is_join_done()
        );
        assert!(
            parse("@room-id=1 :tmi.twitch.tv ROOMSTATE #c")
                .unwrap()
                .is_join_done()
        );
        assert!(!parse(":a!a@a PRIVMSG #c :366").unwrap().is_join_done());
        let fail = parse(":tmi.twitch.tv NOTICE * :Login authentication failed").unwrap();
        assert_eq!(fail.login_failure(), Some("Login authentication failed"));
        assert_eq!(
            parse(":tmi.twitch.tv NOTICE #c :slow mode on")
                .unwrap()
                .login_failure(),
            None
        );
    }

    #[test]
    fn login_lines_lowercase_the_names_and_carry_the_token() {
        let lines = login_lines("MyBot", &Secret::new("tok"), "PyLxyr");
        assert_eq!(
            lines,
            "CAP REQ :twitch.tv/tags twitch.tv/commands\r\nPASS oauth:tok\r\nNICK mybot\r\nJOIN #pylxyr\r\n"
        );
    }
}
