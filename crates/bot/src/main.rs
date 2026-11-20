//! PyxeeTB. Phase 1a ships four commands: `probe` measures the server, `auth` logs in to Twitch, `latency`
//! measures real chat latency, and `version`.

mod auth;
mod cli;
mod endpoints;
mod error;
mod http;
mod latency;
mod net;
mod probe;
mod state;
#[cfg(test)]
mod testkit;

use std::io::{self, Write};
use std::process::ExitCode;
use std::thread;
use std::time::Duration;

use pyxeetb_twitch::oauth::{self, Tokens};

use crate::cli::{Account, AuthOptions, Command, LatencyOptions, SendVia};
use crate::endpoints::Endpoints;
use crate::error::{Context, Error, Result};
use crate::http::Http;
use crate::state::{TokenStore, state_dir, unix_now};

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

fn store_for(account: &Account) -> TokenStore {
    TokenStore::new(state_dir(account.state_dir.as_deref(), env))
}

fn run_probe(options: &cli::ProbeOptions) -> Result<()> {
    let text = probe::run(options);
    print!("{text}");
    std::fs::write(&options.out, &text).context(&format!("writing {}", options.out))?;
    println!("\nReport written to {}", options.out);
    Ok(())
}

fn run_auth(options: &AuthOptions) -> Result<()> {
    let account = &options.account;
    let scopes = oauth::scopes_for(options.irc_send);
    let (http, endpoints, store) = (Http::new(), Endpoints::default(), store_for(account));
    let client = auth::Client {
        http: &http,
        endpoints: &endpoints,
        app_id: &account.client_id,
        app_secret: account.client_secret.as_ref(),
    };
    let tokens = client.login(&scopes, &mut io::stdout(), &thread::sleep)?;
    store.save(&tokens)?;
    println!("\nLogged in as {} (id {}).", tokens.login, tokens.user_id);
    println!(
        "Access valid for about {} minutes; the login is saved in {}",
        tokens.expires_at.saturating_sub(unix_now()) / 60,
        store.path().display()
    );
    let missing = tokens.missing_scopes(&scopes);
    if !missing.is_empty() {
        println!(
            "Warning: Twitch did not grant these permissions: {}",
            missing.join(", ")
        );
    }
    Ok(())
}

/// The saved login, renewed if it is about to expire, and checked for the permissions `required` lists.
fn fresh_login(
    account: &Account,
    client: &auth::Client<'_>,
    store: &TokenStore,
    required: &[&str],
) -> Result<Tokens> {
    let tokens = store.load()?.context(&format!(
        "no saved login in {}; run `pyxeetb auth` first",
        store.path().display()
    ))?;
    let tokens = client.ensure_fresh(store, tokens, unix_now())?;
    let missing = tokens.missing_scopes(required);
    if missing.is_empty() {
        Ok(tokens)
    } else {
        Err(Error::new(format!(
            "the saved login lacks permissions ({}); run `pyxeetb auth{}` again",
            missing.join(", "),
            if missing.contains(&oauth::IRC_SEND_SCOPE) {
                " --with-irc-send"
            } else {
                ""
            }
        )))
        .context(&format!("app {}", account.client_id))
    }
}

fn channel_id(client: &auth::Client<'_>, tokens: &Tokens, channel: &str) -> Result<String> {
    if channel.eq_ignore_ascii_case(&tokens.login) {
        return Ok(tokens.user_id.clone());
    }
    let url = format!("{}/users?login={channel}", client.endpoints.helix_base);
    let bearer = format!("Bearer {}", tokens.access_token.expose());
    let reply = client.http.get(
        &url,
        &[
            ("Authorization", bearer.as_str()),
            ("Client-Id", tokens.client_id.as_str()),
        ],
    )?;
    pyxeetb_twitch::helix::parse_user_id(&reply.body)
        .context(&format!("Twitch does not know a channel called {channel}"))
}

fn run_latency(options: &LatencyOptions) -> Result<()> {
    let (http, endpoints, store) = (
        Http::new(),
        Endpoints::default(),
        store_for(&options.account),
    );
    let account = &options.account;
    let client = auth::Client {
        http: &http,
        endpoints: &endpoints,
        app_id: &account.client_id,
        app_secret: account.client_secret.as_ref(),
    };
    let required = oauth::scopes_for(options.send_via == SendVia::Irc);
    let tokens = fresh_login(account, &client, &store, &required)?;
    let channel = options
        .channel
        .clone()
        .unwrap_or_else(|| tokens.login.clone());
    let broadcaster_id = channel_id(&client, &tokens, &channel)?;

    eprintln!(
        "About to post {} short test messages in #{channel} as {} (sent over {}), {} ms apart.",
        options.count,
        tokens.login,
        if options.send_via == SendVia::Irc {
            "IRC"
        } else {
            "Helix"
        },
        options.gap_ms
    );
    eprintln!("Viewers will see them. Press Ctrl+C within 5 seconds to cancel.");
    io::stderr().flush().context("writing to the terminal")?;
    thread::sleep(Duration::from_secs(5));

    let plan = latency::Plan {
        endpoints: &endpoints,
        http: &http,
        tokens: &tokens,
        channel: &channel,
        broadcaster_id: &broadcaster_id,
        send_via: options.send_via,
        count: options.count,
        gap: Duration::from_millis(options.gap_ms),
    };
    let outcome = latency::run(&plan);
    let report = latency::build_report(&plan, &outcome).render();
    print!("{report}");
    std::fs::write("latency-report.txt", &report).context("writing latency-report.txt")?;
    println!("\nReport written to latency-report.txt");
    Ok(())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let result = match cli::parse(&args, &env) {
        Ok(Command::Help) => {
            print!("{}", cli::USAGE);
            Ok(())
        }
        Ok(Command::Version) => {
            println!("pyxeetb {}", pyxeetb_core::VERSION);
            Ok(())
        }
        Ok(Command::Probe(options)) => run_probe(&options),
        Ok(Command::Auth(options)) => run_auth(&options),
        Ok(Command::Latency(options)) => run_latency(&options),
        Err(message) => {
            eprintln!("error: {message}\n\n{}", cli::USAGE);
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}
