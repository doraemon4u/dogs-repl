//! ogs-tui — terminal client for the Online Go Server.
//!
//! Layering (design §3): Presentation → Session → Engine/Transport. Engine has
//! no I/O. The TUI and the text REPL are two front ends over the same Session
//! actor and command grammar.

mod config;
mod engine;
mod session;
mod transport;
mod ui;

use crate::session::{Command, SessionConfig, SessionHandle};
use crate::transport::auth::{self, AuthSession};
use crate::transport::realtime_client::{RealtimeClient, RealtimeEvent};
use crate::transport::rest_client::RestClient;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Tui,
    Repl,
    Smoke,
}

#[derive(Debug, Default)]
struct Args {
    mode: Option<Mode>,
    username: Option<String>,
    password: Option<String>,
    game: Option<u64>,
    config_dir: Option<PathBuf>,
    lang: Option<String>,
    no_persist: bool,
    trace: bool,
    help: bool,
    version: bool,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args::default();
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        let mut value = |flag: &str| -> Result<String, String> {
            iter.next()
                .ok_or_else(|| format!("{flag} needs a value"))
        };
        match arg.as_str() {
            "--tui" | "tui" => args.mode = Some(Mode::Tui),
            "--repl" | "repl" => args.mode = Some(Mode::Repl),
            "--smoke" | "smoke" => args.mode = Some(Mode::Smoke),
            "--user" | "-u" => args.username = Some(value("--user")?),
            "--password" | "-p" => args.password = Some(value("--password")?),
            "--game" | "-g" => {
                let raw = value("--game")?;
                args.game = Some(
                    raw.trim_start_matches('#')
                        .parse::<u64>()
                        .map_err(|_| format!("--game expects a numeric id (got {raw})"))?,
                );
            }
            "--config-dir" => args.config_dir = Some(PathBuf::from(value("--config-dir")?)),
            "--lang" => args.lang = Some(value("--lang")?),
            "--no-persist" => args.no_persist = true,
            "--trace" => args.trace = true,
            "--help" | "-h" => args.help = true,
            "--version" | "-V" => args.version = true,
            other => return Err(format!("unknown argument '{other}' (try --help)")),
        }
    }
    Ok(args)
}

fn usage() -> &'static str {
    "ogs-tui — OGS terminal client

USAGE:
  ogs-tui [options]              TUI on a terminal (default)
  ogs-tui --repl [options]       plain-text REPL
  ogs-tui --smoke [options]      verify live OGS auth/REST/socket

OPTIONS:
  -u, --user <name>              OGS username
  -p, --password <password>      password (prefer the OGS_PASSWORD env var)
  -g, --game <id>                open a game after login
      --config-dir <path>        override the config/credential directory
      --lang <code>              world-chat language channel (default en)
      --no-persist               do not read or write credentials
      --trace                    verbose socket tracing (OGS_TUI_TRACE=all)
  -h, --help                     show this help
  -V, --version                  print the version

ENVIRONMENT:
  OGS_USERNAME, OGS_PASSWORD, OGS_CLIENT_ID, OGS_LANG,
  OGS_TUI_CONFIG_DIR, OGS_TUI_TRACE

NOTES:
  Credentials are stored atomically with mode 0600 in the config directory.
  A command-line password is visible in the process list; prefer OGS_PASSWORD.
  `estimate` is a local tool. Official OGS scoring is negotiated through
  dead/dame + `score accept|reject` on the server."
}

#[tokio::main]
async fn main() -> ExitCode {
    let args = match parse_args() {
        Ok(args) => args,
        Err(error) => {
            eprintln!("error: {error}");
            eprintln!("{}", usage());
            return ExitCode::from(2);
        }
    };
    if args.help {
        println!("{}", usage());
        return ExitCode::SUCCESS;
    }
    if args.version {
        println!("ogs-tui {}", env!("CARGO_PKG_VERSION"));
        return ExitCode::SUCCESS;
    }

    if let Some(dir) = &args.config_dir {
        std::env::set_var("OGS_TUI_CONFIG_DIR", dir);
    }
    if let Some(lang) = &args.lang {
        std::env::set_var("OGS_LANG", lang);
    }
    if args.trace {
        std::env::set_var("OGS_TUI_TRACE", "all");
    }
    if let Some(username) = &args.username {
        if std::env::var_os("OGS_USERNAME").is_none() {
            std::env::set_var("OGS_USERNAME", username);
        }
    }
    if args.password.is_some() {
        eprintln!("[warning] --password is visible in the process list; OGS_PASSWORD is safer");
    }

    let mode = args.mode.unwrap_or_else(|| {
        if std::io::stdin().is_terminal() && std::io::stdout().is_terminal() {
            Mode::Tui
        } else {
            Mode::Repl
        }
    });

    if mode == Mode::Smoke {
        return run_smoke_command(&args).await;
    }

    let persist_path = if args.no_persist {
        None
    } else {
        Some(config::credentials_path())
    };
    if let Some(path) = &persist_path {
        if config::credentials_are_world_readable(path) == Some(true) {
            eprintln!(
                "[warning] {} is readable by group/other; run `chmod 600` to protect the refresh token",
                path.display()
            );
        }
    }
    let world_channel = std::env::var("OGS_LANG").unwrap_or_else(|_| "en".to_string());
    let session = SessionHandle::spawn(SessionConfig {
        world_channel,
        persist_path: persist_path.clone(),
    });

    // Automatic sign-in: explicit credentials win, then a saved session.
    let env_username = std::env::var("OGS_USERNAME").ok();
    let env_password = std::env::var("OGS_PASSWORD").ok();
    let username = args.username.clone().or(env_username);
    let password = args.password.clone().or(env_password);
    let stored = if args.no_persist {
        None
    } else {
        config::load_credentials()
    };

    if let (Some(username), Some(password)) = (username.clone(), password) {
        session.send(Command::Login { username, password });
    } else if let Some(username) = username {
        let matching = stored.filter(|stored| {
            stored.username.eq_ignore_ascii_case(&username)
        });
        if let Some(stored) = matching {
            session.send(Command::Resume(stored));
        }
    } else if let Some(stored) = stored {
        session.send(Command::Resume(stored));
    }

    let result = match mode {
        Mode::Tui => {
            ui::tui::run(
                session,
                ui::tui::Startup {
                    initial_game: args.game,
                },
            )
            .await
        }
        Mode::Repl => {
            ui::repl::run(
                session,
                ui::repl::ReplOptions {
                    initial_game: args.game,
                },
            )
            .await
        }
        Mode::Smoke => unreachable!(),
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("error: {error}");
            ExitCode::from(1)
        }
    }
}

async fn run_smoke_command(args: &Args) -> ExitCode {
    let username = args
        .username
        .clone()
        .or_else(|| std::env::var("OGS_USERNAME").ok());
    let password = args
        .password
        .clone()
        .or_else(|| std::env::var("OGS_PASSWORD").ok());
    let (Some(username), Some(password)) = (username, password) else {
        eprintln!(
            "error: --smoke needs credentials: --user/--password or OGS_USERNAME/OGS_PASSWORD"
        );
        return ExitCode::from(2);
    };
    match smoke(&username, &password).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("FAIL: {error}");
            ExitCode::from(1)
        }
    }
}

/// Stage-0-style live verification: OAuth2 → REST → user_jwt → socket auth.
async fn smoke(username: &str, password: &str) -> Result<(), String> {
    println!("OGS connectivity smoke test");
    println!("[1/4] OAuth2 password grant");
    let tokens = auth::login(username, password).await?;
    println!(
        "      access token {} chars, refresh token {} chars",
        tokens.access_token.len(),
        tokens.refresh_token.len()
    );
    let auth_session = AuthSession::new(tokens, None);
    let rest = RestClient::new(auth_session.clone());

    println!("[2/4] REST GET /api/v1/me");
    let me = rest.me().await?;
    let user_id = me.get("id").and_then(|value| value.as_i64()).unwrap_or(0);
    let name = me
        .get("username")
        .and_then(|value| value.as_str())
        .unwrap_or(username);
    if user_id == 0 {
        return Err("/me returned no numeric user id".to_string());
    }
    auth_session.set_user_id(user_id).await;
    println!("      authenticated as {name} (id {user_id})");

    println!("[3/4] REST GET /api/v1/ui/config (user_jwt)");
    let config = rest.ui_config().await?;
    let jwt = config
        .get("user_jwt")
        .and_then(|value| value.as_str())
        .filter(|jwt| !jwt.is_empty())
        .map(str::to_string)
        .ok_or_else(|| "/ui/config returned no user_jwt".to_string())?;
    match auth::jwt_is_anonymous(&jwt) {
        Some(true) => {
            return Err("user_jwt is anonymous; REST token was not accepted by /ui/config"
                .to_string())
        }
        Some(false) => println!("      user_jwt claims: anonymous=false"),
        None => println!("      user_jwt is opaque (no decodable claims), continuing"),
    }

    println!("[4/4] WebSocket connect + authenticate (up to 45 s)");
    let mut client = RealtimeClient::new(jwt);
    let mut events = client
        .take_event_receiver()
        .ok_or_else(|| "event receiver already taken".to_string())?;
    let client = Arc::new(client);
    let connector = Arc::clone(&client);
    tokio::spawn(async move { connector.connect().await });

    let deadline = tokio::time::Instant::now() + Duration::from_secs(45);
    let mut last_error: Option<String> = None;
    let mut authenticated = false;
    while tokio::time::Instant::now() < deadline {
        tokio::select! {
            maybe = events.recv() => {
                match maybe {
                    Some(RealtimeEvent::Connected) => println!("      socket connected"),
                    Some(RealtimeEvent::Authenticated { user_id, username }) => {
                        println!("      socket authenticated as {username} ({user_id})");
                        authenticated = true;
                        break;
                    }
                    Some(RealtimeEvent::Heartbeat) => println!("      net/pong"),
                    Some(RealtimeEvent::Error(error)) => {
                        println!("      socket error: {error}");
                        last_error = Some(error);
                    }
                    Some(_) => {}
                    None => break,
                }
            }
            _ = tokio::time::sleep(Duration::from_millis(500)) => {}
        }
    }
    client.disconnect().await;
    if authenticated {
        println!("PASS: OAuth2 + REST + user_jwt + socket authentication all work");
        let _ = std::io::stdout().flush();
        Ok(())
    } else {
        Err(format!(
            "socket authentication did not complete within 45 s{}",
            last_error
                .map(|error| format!(" (last error: {error})"))
                .unwrap_or_default()
        ))
    }
}
