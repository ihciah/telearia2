mod aria2;
mod config;
mod format;
mod state;
mod utils;

use aria2_rs::{status::TaskStatus, SmallVec};
use bytes::Bytes;
use clap::Parser;
use config::{Config, MAX_TORRENT_SIZE};
use format::{make_download_confirm_keyboard, make_single_task_keyboard, make_tasks_keyboard};
use smol_str::SmolStr;
use state::State;
use std::{error::Error, str::FromStr, sync::Arc};
use teloxide::{
    payloads::SendMessageSetters,
    prelude::*,
    types::{Me, ParseMode},
    utils::command::BotCommands,
};

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
        .branch(Update::filter_message().endpoint(message_handler))
        .branch(Update::filter_callback_query().endpoint(callback_handler));

    tracing::info!("Bot created and running");
    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![state])
        .enable_ctrlc_handler()
        .build()
        .dispatch()
        .await;
    Ok(())
}

/// Parse the text wrote on Telegram and check if that text is a valid command
/// or not, then match the command. If the command is `/start` it writes a
/// markup with the `InlineKeyboardMarkup`.
async fn message_handler(
    bot: Bot,
    msg: Message,
    me: Me,
    state: Arc<State>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    if let Some(text) = msg.text() {
        let parsed = BotCommands::parse(text, me.username());
        // Auth check for task and purge command.
        if matches!(parsed, Ok(Command::Task) | Ok(Command::Purge)) && !state.auth(msg.chat.id.0) {
            bot.send_message(
                msg.chat.id,
                format!(
                    "User or group({}) are not authorized to use this command!",
                    msg.chat.id.0
                ),
            )
            .await?;
            return Ok(());
        }
        // If parse command successfully, handle the command and return.
        if let Ok(cmd) = parsed {
            match cmd {
                Command::Help => {
                    // Just send the description of all commands.
                    bot.send_message(msg.chat.id, Command::descriptions().to_string())
                        .await?;
                }
                Command::Start => {
                    bot.send_message(msg.chat.id, "Welcome to ihciah's aria2 bot!\nUse /help to get help.\nUse /task to get task list.\nTo download, send magnet link, torrent file or http(s) link to me!")
                        .await?;
                }
                Command::Id => {
                    bot.send_message(msg.chat.id, format!("`{}`", msg.chat.id))
                        .parse_mode(ParseMode::MarkdownV2)
                        .await?;
                }
                Command::Task => {
                    state.refresh().await;
                    let tasks = state.tasks_cache.read().fmt_tasks();
                    let keyboard = make_tasks_keyboard(tasks);
                    let msg = bot.send_message(msg.chat.id, "Tasks:\nThis page will be updated automatically within 3mins.\nUse /task to refresh again.")
                        .reply_markup(keyboard)
                        .await?;
                    state
                        .tasks_cache
                        .write()
                        .add_list_subscriber(msg.chat.id, msg.id);
                }
                Command::Purge => match state.client.purge_downloaded().await {
                    Ok(()) => {
                        bot.send_message(msg.chat.id, "Purge downloaded results successfully!")
                            .reply_to_message_id(msg.id)
                            .await?;
                    }
                    Err(e) => {
                        bot.send_message(
                            msg.chat.id,
                            format!("Purge downloaded results failed: {e}"),
                        )
                        .reply_to_message_id(msg.id)
                        .await?;
                    }
                },
            }
            return Ok(());
        }
    }

    // handle other messages
    match handle_message(&bot, &msg, state).await {
        Ok(Some(_)) => (),
        Ok(None) => {
            bot.send_message(msg.chat.id, "Command not found!").await?;
        }
        Err(e) => {
            bot.send_message(msg.chat.id, format!("Error: {e}")).await?;
        }
    }

    Ok(())
}

// Handle message with magnet link or torrent file.
async fn handle_message(bot: &Bot, msg: &Message, state: Arc<State>) -> anyhow::Result<Option<()>> {
    if !state.auth(msg.chat.id.0) {
        bot.send_message(
            msg.chat.id,
            format!(
                "User or group({}) are not authorized to download.",
                msg.chat.id.0
            ),
        )
        .await?;
        return Ok(Some(()));
    }

    // extract all magnet links with regexp to Vec<String>.
    let magnet_re = regex::Regex::new(r"magnet:\?xt=urn:btih:([0-9a-fA-F]{40})")?;
    let mut magnets: SmallVec<String> = magnet_re
        .captures_iter(msg.text().unwrap_or(""))
        .map(|cap| format!("magnet:?xt=urn:btih:{}", &cap[1].to_ascii_lowercase()))
        .collect();
    magnets.sort_unstable();
    magnets.dedup();

    // if message length is 40 and all chars are hex, it may be a magnet link.
    if let Some(text) = msg.text() {
        if text.len() == 40 && text.chars().all(|c| c.is_ascii_hexdigit()) {
            magnets.push(format!("magnet:?xt=urn:btih:{}", text));
        }
    }

    if !magnets.is_empty() {
        let uuid = uuid::Uuid::new_v4().to_string();
        let text = if magnets.len() == 1 {
            format!("Confirm download {}?", magnets[0])
        } else {
            format!("Confirm download {} magnets?", magnets.len())
        };
        let keyboard = make_download_confirm_keyboard(
            &state.download_config.magnet_dirs,
            &state.download_config.default_dir,
            |dir| format!("uri|{dir}|{uuid}"),
        );
        state.uri_cache.lock().insert(uuid, magnets);

        bot.send_message(msg.chat.id, text)
            .reply_markup(keyboard)
            .await?;
        return Ok(Some(()));
    }

    // extract all http or https links(not magnet) with regexp to Vec<String>.
    let http_re = regex::Regex::new(r"((?:https|http)://[^\s]*)")?;
    let mut http_links: SmallVec<String> = http_re
        .captures_iter(msg.text().unwrap_or(""))
        .map(|cap| cap[1].to_string())
        .collect();
    http_links.sort_unstable();
    http_links.dedup();
    if !http_links.is_empty() {
        let uuid = uuid::Uuid::new_v4().to_string();
        let text = if http_links.len() == 1 {
            format!("Confirm download {}?", http_links[0])
        } else {
            format!("Confirm download {} links?", http_links.len())
        };
        let keyboard = make_download_confirm_keyboard(
            &state.download_config.link_dirs,
            &state.download_config.default_dir,
            |dir| format!("uri|{dir}|{uuid}"),
        );
        state.uri_cache.lock().insert(uuid, http_links);

        bot.send_message(msg.chat.id, text)
            .reply_markup(keyboard)
            .await?;
        return Ok(Some(()));
    }

    // extract all torrent files.
    if let Some(document) = msg.document() {
        if document.file.size > MAX_TORRENT_SIZE {
            bot.send_message(msg.chat.id, "File size too large!")
                .await?;
            return Ok(Some(()));
        }
        let uuid = uuid::Uuid::new_v4().to_string();
        let text = format!(
            "Confirm download torrent {}?",
            document
                .file_name
                .clone()
                .unwrap_or_else(|| format!("file_{}", document.file.id))
        );
        let keyboard = make_download_confirm_keyboard(
            &state.download_config.torrent_dirs,
            &state.download_config.default_dir,
            |dir| format!("t|{dir}|{uuid}"),
        );
        state
            .file_cache
            .lock()
            .insert(uuid.into(), document.file.id.clone());

        bot.send_message(msg.chat.id, text)
            .reply_markup(keyboard)
            .await?;
        return Ok(Some(()));
    }

    Ok(None)
}

async fn callback_handler(
    bot: Bot,
    q: CallbackQuery,
    state: Arc<State>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let (Some(user_data), Some(Message { id, chat, .. })) = (q.data, q.message) else {
        return Ok(());
    };
    if !state.auth(chat.id.0) {
        bot.send_message(
            chat.id,
            format!(
                "User or group({}) are not authorized to use this command!",
                chat.id.0
            ),
        )
        .await?;
        return Ok(());
    }

    let user_data = UserData::from_str(&user_data)?;
    match user_data {
        UserData::Task(gid) => {
            let Some((task_desc, task_status)) =
                state
                    .tasks_cache
                    .read()
                    .fmt_task(&gid)
                    .map(|(task_desc, task_status)| {
                        (task_desc, task_status.status.unwrap_or(TaskStatus::Removed))
                    })
            else {
                bot.send_message(chat.id, format!("Task {gid} not found!"))
                    .await?;
                return Ok(());
            };

            let keyboard = make_single_task_keyboard(&gid, task_status);
            let msg = bot
                .send_message(chat.id, task_desc)
                .reply_markup(keyboard)
                .reply_to_message_id(id)
                .await?;
            state
                .tasks_cache
                .write()
                .add_task_subscriber(gid, chat.id, msg.id);
        }
        UserData::PauseTask(gid) => {
            let res = state.client.pause(&gid).await;
            bot.edit_message_text(
                chat.id,
                id,
                format!("Pause task {gid} {}.", fmt_result(&res)),
            )
            .await?;
        }
        UserData::ResumeTask(gid) => {
            let res = state.client.resume(&gid).await;
            bot.edit_message_text(
                chat.id,
                id,
                format!("Resume task {gid} {}.", fmt_result(&res)),
            )
            .await?;
        }
        UserData::RemoveTask(gid) => {
            let res = state.client.remove(&gid).await;
            bot.edit_message_text(
                chat.id,
                id,
                format!("Remove task {gid} {}.", fmt_result(&res)),
            )
            .await?;
        }
        UserData::AddUri(dir, uris_key) => {
            let Some(uris) = state.uri_cache.lock().remove(&uris_key) else {
                bot.edit_message_text(chat.id, id, format!("Uri cache {uris_key} not found!"))
                    .await?;
                return Ok(());
            };
            let mut text = format!("Add download uris task to {dir} successfully:\n");
            let res = state.client.add_uris(uris.as_slice(), Some(dir)).await;
            let gids = match res {
                Ok(gids) => gids,
                Err(e) => {
                    bot.edit_message_text(chat.id, id, format!("Push add uris task failed: {e}"))
                        .await?;
                    return Ok(());
                }
            };
            for (uri, gid) in uris.into_iter().zip(gids.into_iter()) {
                text.push_str(&format!("{uri}: {gid}\n"));
            }
            bot.edit_message_text(chat.id, id, text).await?;
        }
        UserData::AddTorrent(dir, file_id_key) => {
            let Some(file_id) = state.file_cache.lock().remove(&file_id_key) else {
                bot.edit_message_text(
                    chat.id,
                    id,
                    format!("File cache {} not found!", file_id_key),
                )
                .await?;
                return Ok(());
            };
            let file = match get_telegram_file(&bot, file_id.as_str()).await {
                Ok(file) => file,
                Err(e) => {
                    bot.edit_message_text(
                        chat.id,
                        id,
                        format!("Download torrent file failed: {e}"),
                    )
                    .await?;
                    return Ok(());
                }
            };

            let res = state.client.add_torrent(&file, Some(dir.clone())).await;
            let gid = match res {
                Ok(gid) => gid,
                Err(e) => {
                    bot.edit_message_text(
                        chat.id,
                        id,
                        format!("Push add torrent task failed: {e}"),
                    )
                    .await?;
                    return Ok(());
                }
            };

            let text = format!("Add download torrent task to {dir} successfully:\nGID: {gid}");
            bot.edit_message_text(chat.id, id, text).await?;
        }
    }

    Ok(())
}

fn fmt_result(res: &anyhow::Result<()>) -> String {
    match res {
        Ok(_) => "success".to_string(),
        Err(e) => format!("failed: {e}"),
    }
}

async fn get_telegram_file(bot: &Bot, file_id: &str) -> anyhow::Result<Bytes> {
    let file = bot.get_file(file_id).await?;
    let url = bot
        .api_url()
        .join(&format!("file/bot{}/{}", bot.token(), file.path))
        .expect("failed to format file url");
    let data = reqwest::Client::new()
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    Ok(data)
}

enum UserData {
    Task(SmolStr),
    PauseTask(SmolStr),
    ResumeTask(SmolStr),
    RemoveTask(SmolStr),
    AddUri(SmolStr, String),
    AddTorrent(SmolStr, SmolStr),
}

impl FromStr for UserData {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.split_once('|') {
            Some((action, gid)) => match action {
                "task" => Ok(UserData::Task(gid.into())),
                "pause" => Ok(UserData::PauseTask(gid.into())),
                "resume" => Ok(UserData::ResumeTask(gid.into())),
                "remove" => Ok(UserData::RemoveTask(gid.into())),
                "uri" => {
                    let mut parts = gid.split('|');
                    let dir = parts
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("Invalid action"))?;
                    let uris_key = parts
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("Invalid action"))?;
                    Ok(UserData::AddUri(dir.into(), uris_key.into()))
                }
                "t" => {
                    let mut parts = gid.split('|');
                    let dir = parts
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("Invalid action"))?;
                    let torrent_id = parts
                        .next()
                        .ok_or_else(|| anyhow::anyhow!("Invalid action"))?;
                    Ok(UserData::AddTorrent(dir.into(), torrent_id.into()))
                }
                _ => Err(anyhow::anyhow!("Invalid action")),
            },
            None => Err(anyhow::anyhow!("Invalid action")),
        }
    }
}
