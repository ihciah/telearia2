mod aria2;
mod config;
mod constants;
mod format;
mod handlers;
mod state;
mod utils;

use clap::Parser;
use config::Config;
use smol_str::SmolStr;
use state::State;
use std::{error::Error, str::FromStr, sync::Arc, sync::LazyLock};

static MAGNET_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"magnet:\?xt=urn:btih:((?:[0-9a-fA-F]{40})|(?:[a-zA-Z2-7]{32}))")
        .expect("invalid magnet regex")
});

static HTTP_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"((?:https|http)://[^\s]*)").expect("invalid http regex"));

use teloxide::{prelude::*, utils::command::BotCommands};

/// These commands are supported:
#[derive(BotCommands)]
#[command(rename_rule = "lowercase")]
enum Command {
    /// Display this text
    Help,
    /// Start
    Start,
    /// Id
    Id,
    /// Switch server
    Switch,
    /// Task list
    Task,
    /// Purge all downloaded results
    Purge,
}

#[derive(Parser, Debug, Default, Clone)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Path of config toml file.
    #[arg(short, long)]
    pub config: Option<String>,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt::init();

    let config_file = Args::parse()
        .config
        .or_else(|| {
            std::env::var("CONFIG_PATH")
                .ok()
                .and_then(|s| if s.is_empty() { None } else { Some(s) })
        })
        .unwrap_or_else(|| "config.toml".to_string());
    tracing::info!("Use config file: {config_file}");
    let config = Config::load_from(&config_file).expect("unable to load config");
    tracing::info!("Config file {config_file} load successfully");

    let bot = Bot::new(&config.telegram.token);
    let state = Arc::new(State::new(&config, bot.clone()).await?);

    let handler = dptree::entry()
        .branch(Update::filter_message().endpoint(handlers::message_handler))
        .branch(Update::filter_callback_query().endpoint(handlers::callback_handler));

    tracing::info!("Bot created and running");
    let mut dispatcher = Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![state])
        .enable_ctrlc_handler()
        .build();

    #[cfg(unix)]
    {
        let shutdown_token = dispatcher.shutdown_token();
        tokio::spawn(async move {
            let mut sigterm =
                tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                    .expect("failed to register SIGTERM handler");
            sigterm.recv().await;
            tracing::info!("Received SIGTERM, shutting down...");
            shutdown_token.shutdown().ok();
        });
    }

    dispatcher.dispatch().await;
    Ok(())
}

/// User action data parsed from callback query.
pub enum UserData {
    Task(SmolStr),
    PauseTask(SmolStr),
    ResumeTask(SmolStr),
    RemoveTask(SmolStr),
    AddUri(String),
    AddTorrent(String),
    SwitchServer(SmolStr),
    RefreshList,
    RefreshTask(SmolStr),
}

/// Error type for parsing user data.
#[derive(Debug)]
pub struct UserDataError;

impl std::fmt::Display for UserDataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Invalid action")
    }
}
impl std::error::Error for UserDataError {}

impl FromStr for UserData {
    type Err = UserDataError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        if s == "rlist" {
            return Ok(UserData::RefreshList);
        }
        let Some((action, data)) = s.split_once('|') else {
            return Err(UserDataError);
        };
        match action {
            "task" => Ok(UserData::Task(data.into())),
            "pause" => Ok(UserData::PauseTask(data.into())),
            "resume" => Ok(UserData::ResumeTask(data.into())),
            "remove" => Ok(UserData::RemoveTask(data.into())),
            "uri" => Ok(UserData::AddUri(data.into())),
            "t" => Ok(UserData::AddTorrent(data.into())),
            "rtask" => Ok(UserData::RefreshTask(data.into())),
            "switch" => {
                let mut parts = data.split('|');
                let server = parts.next().ok_or(UserDataError)?;
                Ok(UserData::SwitchServer(server.into()))
            }
            _ => Err(UserDataError),
        }
    }
}
