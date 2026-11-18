//! Command-line parsing: small, dependency-free, and tested.

use pyxeetb_twitch::Secret;

pub const USAGE: &str = "\
pyxeetb - a fast, self-hosted Twitch chat bot, alerts and overlay engine

USAGE:
    pyxeetb <COMMAND> [OPTIONS]

COMMANDS:
    probe      Measure this machine and its path to Twitch, and write a report to paste back
    auth       Log in to Twitch (device code flow) and save the login
    latency    Measure real chat latency over IRC and EventSub (needs `auth` first)
    version    Print the version

PROBE OPTIONS:
    --samples <N>          Connections per Twitch endpoint (1-200, default 20)
    --burst-seconds <S>    Length of the CPU throttling test (0-120, default 20; 0 skips it)
    --jitter-seconds <S>   Length of the idle wake-up test (0-120, default 10; 0 skips it)
    --out <FILE>           Where to write the report (default probe-report.txt)

AUTH AND LATENCY OPTIONS:
    --client-id <ID>       Your Twitch app's client id (or set PYXEETB_CLIENT_ID)
    --state-dir <DIR>      Where the login is kept (default ~/.config/pyxeetb, or PYXEETB_STATE_DIR)
    PYXEETB_CLIENT_SECRET  Environment variable: the app's secret, only if it is a confidential app

LATENCY OPTIONS:
    --channel <LOGIN>      Channel to test in (default: your own)
    --count <N>            Test messages to send (1-50, default 10)
    --gap-ms <MS>          Pause between test messages (500-30000, default 2500)

GLOBAL OPTIONS:
    -h, --help             Show this help
    -V, --version          Print the version
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeOptions {
    pub samples: usize,
    pub burst_seconds: u64,
    pub jitter_seconds: u64,
    pub out: String,
}

impl Default for ProbeOptions {
    fn default() -> Self {
        Self {
            samples: 20,
            burst_seconds: 20,
            jitter_seconds: 10,
            out: "probe-report.txt".to_owned(),
        }
    }
}

/// What the commands that talk to Twitch on your behalf share.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub client_id: String,
    pub client_secret: Option<Secret>,
    pub state_dir: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LatencyOptions {
    pub account: Account,
    pub channel: Option<String>,
    pub count: usize,
    pub gap_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help,
    Version,
    Probe(ProbeOptions),
    Auth(Account),
    Latency(LatencyOptions),
}

/// `--name value` and `--name=value` pairs, each readable once.
struct Flags(Vec<(String, String)>);

impl Flags {
    fn parse(args: &[String], allowed: &[&str]) -> Result<Option<Self>, String> {
        let mut pairs = Vec::new();
        let mut iter = args.iter();
        while let Some(arg) = iter.next() {
            if matches!(arg.as_str(), "-h" | "--help") {
                return Ok(None);
            }
            let (name, inline) = arg
                .split_once('=')
                .map_or((arg.as_str(), None), |(n, v)| (n, Some(v)));
            if !allowed.contains(&name) {
                return Err(format!("unknown option '{arg}'"));
            }
            let value = match inline {
                Some(v) => v.to_owned(),
                None => iter
                    .next()
                    .cloned()
                    .ok_or_else(|| format!("{name} needs a value"))?,
            };
            pairs.push((name.to_owned(), value));
        }
        Ok(Some(Self(pairs)))
    }

    fn text(&self, name: &str) -> Option<String> {
        self.0
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.clone())
    }

    fn number<T>(&self, name: &str, default: T, min: T, max: T) -> Result<T, String>
    where
        T: std::str::FromStr + PartialOrd + std::fmt::Display + Copy,
    {
        let Some(raw) = self.text(name) else {
            return Ok(default);
        };
        let value: T = raw
            .parse()
            .map_err(|_| format!("{name} needs a whole number, got '{raw}'"))?;
        if value < min || value > max {
            return Err(format!(
                "{name} must be between {min} and {max}, got {value}"
            ));
        }
        Ok(value)
    }
}

fn account(flags: &Flags, env: &dyn Fn(&str) -> Option<String>) -> Result<Account, String> {
    let client_id = flags
        .text("--client-id")
        .or_else(|| env("PYXEETB_CLIENT_ID"))
        .filter(|id| !id.is_empty())
        .ok_or("a client id is needed: pass --client-id or set PYXEETB_CLIENT_ID")?;
    Ok(Account {
        client_id,
        client_secret: env("PYXEETB_CLIENT_SECRET")
            .filter(|s| !s.is_empty())
            .map(Secret::new),
        state_dir: flags.text("--state-dir"),
    })
}

/// Parse the arguments after the program name. `env` reads an environment variable.
///
/// # Errors
/// A message for the user when the arguments are wrong.
pub fn parse(args: &[String], env: &dyn Fn(&str) -> Option<String>) -> Result<Command, String> {
    let Some((first, rest)) = args.split_first() else {
        return Ok(Command::Help);
    };
    let flags = |allowed: &[&str]| Flags::parse(rest, allowed);
    match first.as_str() {
        "-h" | "--help" | "help" => Ok(Command::Help),
        "-V" | "--version" | "version" => Ok(Command::Version),
        "probe" => {
            let Some(f) = flags(&["--samples", "--burst-seconds", "--jitter-seconds", "--out"])?
            else {
                return Ok(Command::Help);
            };
            let defaults = ProbeOptions::default();
            Ok(Command::Probe(ProbeOptions {
                samples: f.number("--samples", defaults.samples, 1, 200)?,
                burst_seconds: f.number("--burst-seconds", defaults.burst_seconds, 0, 120)?,
                jitter_seconds: f.number("--jitter-seconds", defaults.jitter_seconds, 0, 120)?,
                out: match f.text("--out") {
                    Some(out) if out.is_empty() => return Err("--out needs a file name".to_owned()),
                    Some(out) => out,
                    None => defaults.out,
                },
            }))
        }
        "auth" => {
            let Some(f) = flags(&["--client-id", "--state-dir"])? else {
                return Ok(Command::Help);
            };
            Ok(Command::Auth(account(&f, env)?))
        }
        "latency" => {
            let Some(f) = flags(&[
                "--client-id",
                "--state-dir",
                "--channel",
                "--count",
                "--gap-ms",
            ])?
            else {
                return Ok(Command::Help);
            };
            Ok(Command::Latency(LatencyOptions {
                account: account(&f, env)?,
                channel: f
                    .text("--channel")
                    .map(|c| c.trim_start_matches('#').to_ascii_lowercase()),
                count: f.number("--count", 10, 1, 50)?,
                gap_ms: f.number("--gap-ms", 2500, 500, 30_000)?,
            }))
        }
        other => Err(format!("unknown command '{other}'")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| (*s).to_owned()).collect()
    }

    fn no_env(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn no_arguments_and_help_in_every_spelling_show_help() {
        assert_eq!(parse(&[], &no_env), Ok(Command::Help));
        for spelling in ["help", "--help", "-h"] {
            assert_eq!(
                parse(&args(&[spelling]), &no_env),
                Ok(Command::Help),
                "{spelling}"
            );
        }
        assert_eq!(
            parse(&args(&["probe", "--help"]), &no_env),
            Ok(Command::Help)
        );
        assert_eq!(parse(&args(&["latency", "-h"]), &no_env), Ok(Command::Help));
    }

    #[test]
    fn version_in_every_spelling() {
        for spelling in ["version", "--version", "-V"] {
            assert_eq!(
                parse(&args(&[spelling]), &no_env),
                Ok(Command::Version),
                "{spelling}"
            );
        }
    }

    #[test]
    fn probe_defaults_and_options_in_both_styles() {
        assert_eq!(
            parse(&args(&["probe"]), &no_env),
            Ok(Command::Probe(ProbeOptions::default()))
        );
        let parsed = parse(
            &args(&[
                "probe",
                "--samples",
                "5",
                "--burst-seconds=0",
                "--jitter-seconds",
                "3",
                "--out",
                "r.txt",
            ]),
            &no_env,
        );
        assert_eq!(
            parsed,
            Ok(Command::Probe(ProbeOptions {
                samples: 5,
                burst_seconds: 0,
                jitter_seconds: 3,
                out: "r.txt".into()
            }))
        );
    }

    #[test]
    fn out_of_range_and_bad_values_say_why() {
        let err = |list: &[&str]| parse(&args(list), &no_env).unwrap_err();
        assert!(err(&["probe", "--samples", "0"]).contains("between 1 and 200"));
        assert!(err(&["probe", "--burst-seconds", "999"]).contains("between 0 and 120"));
        assert!(err(&["probe", "--samples", "many"]).contains("whole number"));
        assert!(err(&["probe", "--samples"]).contains("needs a value"));
        assert!(err(&["probe", "--out="]).contains("file name"));
        assert!(err(&["probe", "--nope"]).contains("unknown option"));
        assert!(err(&["serve"]).contains("unknown command"));
    }

    #[test]
    fn auth_needs_a_client_id_from_a_flag_or_the_environment() {
        assert!(
            parse(&args(&["auth"]), &no_env)
                .unwrap_err()
                .contains("client id")
        );
        let from_flag = parse(
            &args(&["auth", "--client-id", "abc", "--state-dir", "/s"]),
            &no_env,
        )
        .unwrap();
        assert_eq!(
            from_flag,
            Command::Auth(Account {
                client_id: "abc".into(),
                client_secret: None,
                state_dir: Some("/s".into())
            })
        );
        let env = |name: &str| match name {
            "PYXEETB_CLIENT_ID" => Some("from-env".to_owned()),
            "PYXEETB_CLIENT_SECRET" => Some("shh".to_owned()),
            _ => None,
        };
        let Command::Auth(account) = parse(&args(&["auth"]), &env).unwrap() else {
            panic!("not auth")
        };
        assert_eq!(
            (
                account.client_id.as_str(),
                account.client_secret.as_ref().map(Secret::expose)
            ),
            ("from-env", Some("shh"))
        );
        let overridden = parse(&args(&["auth", "--client-id", "flag"]), &env).unwrap();
        assert!(matches!(overridden, Command::Auth(a) if a.client_id == "flag"));
    }

    #[test]
    fn latency_options_have_limits_and_a_normalised_channel() {
        let ok = parse(
            &args(&[
                "latency",
                "--client-id",
                "c",
                "--channel",
                "#PyLxyr",
                "--count",
                "3",
                "--gap-ms=600",
            ]),
            &no_env,
        )
        .unwrap();
        let Command::Latency(options) = ok else {
            panic!("not latency")
        };
        assert_eq!(
            (options.channel.as_deref(), options.count, options.gap_ms),
            (Some("pylxyr"), 3, 600)
        );
        let defaults = parse(&args(&["latency", "--client-id", "c"]), &no_env).unwrap();
        assert!(
            matches!(defaults, Command::Latency(o) if o.count == 10 && o.gap_ms == 2500 && o.channel.is_none())
        );
        assert!(
            parse(
                &args(&["latency", "--client-id", "c", "--count", "51"]),
                &no_env
            )
            .is_err()
        );
        assert!(
            parse(
                &args(&["latency", "--client-id", "c", "--gap-ms", "100"]),
                &no_env
            )
            .is_err()
        );
    }

    #[test]
    fn an_empty_secret_counts_as_none() {
        let env = |name: &str| (name == "PYXEETB_CLIENT_SECRET").then(String::new);
        let Command::Auth(account) = parse(&args(&["auth", "--client-id", "c"]), &env).unwrap()
        else {
            panic!("not auth")
        };
        assert_eq!(account.client_secret, None);
    }
}
