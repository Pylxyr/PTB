//! Command-line parsing: tiny and dependency-free, with its own tests.

pub const USAGE: &str = "\
pyxeetb - a fast, self-hosted Twitch chat bot, alerts and overlay engine

USAGE:
    pyxeetb <COMMAND> [OPTIONS]

COMMANDS:
    probe      Measure this machine and its path to Twitch, and write a report to paste back
    version    Print the version

PROBE OPTIONS:
    --samples <N>         Connections per Twitch endpoint (1-200, default 20)
    --burst-seconds <S>   Length of the CPU throttling test (0-120, default 20; 0 skips it)
    --out <FILE>          Where to write the report (default probe-report.txt)

GLOBAL OPTIONS:
    -h, --help            Show this help
    -V, --version         Print the version
";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbeOptions {
    pub samples: usize,
    pub burst_seconds: u64,
    pub out: String,
}

impl Default for ProbeOptions {
    fn default() -> Self {
        Self {
            samples: 20,
            burst_seconds: 20,
            out: "probe-report.txt".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help,
    Version,
    Probe(ProbeOptions),
}

/// Split `--name=value` into its two halves; a bare `--name` has no value yet.
fn split_flag(arg: &str) -> (&str, Option<&str>) {
    match arg.split_once('=') {
        Some((name, value)) => (name, Some(value)),
        None => (arg, None),
    }
}

fn number<T: std::str::FromStr + PartialOrd + std::fmt::Display>(
    name: &str,
    raw: &str,
    min: T,
    max: T,
) -> Result<T, String> {
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

pub fn parse(args: &[String]) -> Result<Command, String> {
    let Some(first) = args.first() else {
        return Ok(Command::Help);
    };
    match first.as_str() {
        "-h" | "--help" | "help" => Ok(Command::Help),
        "-V" | "--version" | "version" => Ok(Command::Version),
        "probe" => parse_probe(&args[1..]),
        other => Err(format!("unknown command '{other}'")),
    }
}

fn parse_probe(args: &[String]) -> Result<Command, String> {
    let mut options = ProbeOptions::default();
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        let (name, inline) = split_flag(arg);
        if matches!(name, "-h" | "--help") {
            return Ok(Command::Help);
        }
        if !matches!(name, "--samples" | "--burst-seconds" | "--out") {
            return Err(format!("unknown option '{arg}' for probe"));
        }
        let value = match inline {
            Some(v) => v.to_string(),
            None => iter
                .next()
                .cloned()
                .ok_or_else(|| format!("{name} needs a value"))?,
        };
        match name {
            "--samples" => options.samples = number(name, &value, 1, 200)?,
            "--burst-seconds" => options.burst_seconds = number(name, &value, 0, 120)?,
            _ => {
                if value.is_empty() {
                    return Err("--out needs a file name".to_string());
                }
                options.out = value;
            }
        }
    }
    Ok(Command::Probe(options))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn no_arguments_show_help() {
        assert_eq!(parse(&[]), Ok(Command::Help));
    }

    #[test]
    fn version_in_every_spelling() {
        for spelling in ["version", "--version", "-V"] {
            assert_eq!(
                parse(&args(&[spelling])),
                Ok(Command::Version),
                "{spelling}"
            );
        }
    }

    #[test]
    fn probe_defaults() {
        assert_eq!(
            parse(&args(&["probe"])),
            Ok(Command::Probe(ProbeOptions::default()))
        );
    }

    #[test]
    fn probe_options_in_both_styles() {
        let parsed = parse(&args(&[
            "probe",
            "--samples",
            "5",
            "--burst-seconds=0",
            "--out",
            "r.txt",
        ]));
        assert_eq!(
            parsed,
            Ok(Command::Probe(ProbeOptions {
                samples: 5,
                burst_seconds: 0,
                out: "r.txt".into()
            }))
        );
    }

    #[test]
    fn out_of_range_and_bad_values_are_refused_with_a_reason() {
        let err = parse(&args(&["probe", "--samples", "0"])).unwrap_err();
        assert!(err.contains("between 1 and 200"), "{err}");
        let err = parse(&args(&["probe", "--burst-seconds", "999"])).unwrap_err();
        assert!(err.contains("between 0 and 120"), "{err}");
        let err = parse(&args(&["probe", "--samples", "many"])).unwrap_err();
        assert!(err.contains("whole number"), "{err}");
    }

    #[test]
    fn missing_values_and_unknown_things_are_refused() {
        assert!(parse(&args(&["probe", "--samples"]))
            .unwrap_err()
            .contains("needs a value"));
        assert!(parse(&args(&["probe", "--out="]))
            .unwrap_err()
            .contains("file name"));
        assert!(parse(&args(&["probe", "--nope"]))
            .unwrap_err()
            .contains("unknown option"));
        assert!(parse(&args(&["serve"]))
            .unwrap_err()
            .contains("unknown command"));
    }

    #[test]
    fn help_after_probe_shows_help() {
        assert_eq!(parse(&args(&["probe", "--help"])), Ok(Command::Help));
    }
}
