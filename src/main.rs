mod aria2;
mod config;
mod format;
mod state;
mod utils;

use aria2::AddUrisResult;
use aria2_rs::{status::TaskStatus, SmallVec};
use bytes::Bytes;
use clap::Parser;
use config::{Config, MAX_TORRENT_SIZE};
use format::{
    make_download_confirm_keyboard, make_single_task_keyboard, make_switch_server_keyboard,
    make_tasks_keyboard,
    msg::{
        MsgCatchError, MsgDownloadLinkConfirm, MsgDownloadMagnetConfirm, MsgDownloadTorrentConfirm,
        MsgStart, MsgSwitchPrompt, MsgSwitchResult, MsgTask, MsgTaskActionResult, MsgTaskNotFound,
        MsgUnauthorized,
    },
};
use smol_str::SmolStr;
use state::{State, TasksCache};
use std::{error::Error, str::FromStr, sync::Arc, sync::LazyLock};

static MAGNET_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"magnet:\?xt=urn:btih:((?:[0-9a-fA-F]{40})|(?:[a-zA-Z2-7]{32}))")
        .expect("invalid magnet regex")
});

static HTTP_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"((?:https|http)://[^\s]*)").expect("invalid http regex")
});
use teloxide::{
    payloads::SendMessageSetters,
    prelude::*,
    types::{MaybeInaccessibleMessage, Me, MessageId, ParseMode, ReplyParameters},
    utils::command::BotCommands,
};
use utils::SendMessageSettersExt;

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
    // Try to parse the message as a command.
    if let Some(cmd) = msg
        .text()
        .and_then(|text| BotCommands::parse(text, me.username()).ok())
    {
        match cmd {
            Command::Help => {
                // Just send the description of all commands.
                bot.send_message(msg.chat.id, Command::descriptions().to_string())
                    .reply_parameters(ReplyParameters::new(msg.id))
                    .await?;
                return Ok(());
            }
            Command::Start => {
                bot.send_message(msg.chat.id, MsgStart).await?;
                return Ok(());
            }
            Command::Id => {
                bot.send_message(msg.chat.id, format!("`{}`", msg.chat.id))
                    .parse_mode(ParseMode::MarkdownV2)
                    .reply_parameters(ReplyParameters::new(msg.id))
                    .await?;
                return Ok(());
            }
            Command::Switch => {
                select_or_unauthorized(&bot, msg.chat.id, Some(msg.id), &state).await?;
                return Ok(());
            }
            _ => (),
        }

        let Some(server_selected) = state.selected(msg.chat.id.0) else {
            select_or_unauthorized(&bot, msg.chat.id, Some(msg.id), &state).await?;
            return Ok(());
        };
        // Auth checked for Task and Purge command.
        match cmd {
            Command::Task => {
                TasksCache::refresh(&server_selected.tasks_cache, &server_selected.client).await;
                let tasks = server_selected.tasks_cache.read().fmt_tasks();
                let keyboard = make_tasks_keyboard(tasks);
                let msg = bot
                    .send_message(msg.chat.id, MsgTask)
                    .reply_markup(keyboard)
                    .await?;
                server_selected
                    .tasks_cache
                    .write()
                    .add_list_subscriber(msg.chat.id, msg.id);
            }
            Command::Purge => {
                bot.send_message(
                    msg.chat.id,
                    MsgTaskActionResult::Purge(&server_selected.client.purge_downloaded().await),
                )
                .reply_parameters(ReplyParameters::new(msg.id))
                .await?;
            }
            _ => unreachable!(),
        }
        return Ok(());
    }

    // handle other messages
    let text = match handle_message(&bot, &msg, state).await {
        Ok(ControlFlow::Break(_)) => return Ok(()),
        Ok(ControlFlow::Continue(_)) => MsgCatchError::InvalidCommand,
        Err(e) => MsgCatchError::Error { error: e },
    };
    bot.send_message(msg.chat.id, text).await?;

    Ok(())
}

// Handle message with magnet link or torrent file.
async fn handle_message(
    bot: &Bot,
    msg: &Message,
    state: Arc<State>,
) -> anyhow::Result<ControlFlow<()>> {
    let Some(server_selected) = state.selected(msg.chat.id.0) else {
        select_or_unauthorized(bot, msg.chat.id, Some(msg.id), &state).await?;
        return Ok(ControlFlow::Break(()));
    };

    // extract all magnet links with regexp to Vec<String>.
    // TODO: extract and pass more query parameters.
    let mut magnets: SmallVec<String> = MAGNET_RE
        .captures_iter(msg.text().unwrap_or(""))
        .map(|cap| format!("magnet:?xt=urn:btih:{}", &cap[1].to_ascii_lowercase()))
        .collect();
    magnets.sort_unstable();
    magnets.dedup();

    // if message length is 40 and all chars are hex, it may be a magnet link.
    // base32 format is also considered as valid magnet link.
    if let Some(text) = msg.text() {
        if text.len() == 40 && text.chars().all(|c| c.is_ascii_hexdigit()) {
            magnets.push(format!("magnet:?xt=urn:btih:{text}"));
        }
        if text.len() == 32
            && text
                .chars()
                .all(|c| matches!(c, 'a'..='z' | 'A'..='Z' | '2'..='7'))
        {
            magnets.push(format!("magnet:?xt=urn:btih:{text}"));
        }
    }

    if !magnets.is_empty() {
        let uuid = uuid::Uuid::new_v4().to_string();
        let text: String = MsgDownloadMagnetConfirm { magnets: &magnets }.into();
        let keyboard = make_download_confirm_keyboard(
            &server_selected.download_config.magnet_dirs,
            &server_selected.download_config.default_dir,
            |dir| format!("uri|{dir}|{uuid}"),
        );
        state.uri_cache.lock().insert(uuid, magnets);

        bot.send_message(msg.chat.id, text)
            .reply_markup(keyboard)
            .await?;
        return Ok(ControlFlow::Break(()));
    }

    // extract all http or https links(not magnet) with regexp to Vec<String>.
    let mut http_links: SmallVec<String> = HTTP_RE
        .captures_iter(msg.text().unwrap_or(""))
        .map(|cap| cap[1].to_string())
        .collect();
    http_links.sort_unstable();
    http_links.dedup();
    if !http_links.is_empty() {
        let uuid = uuid::Uuid::new_v4().to_string();
        let text: String = MsgDownloadLinkConfirm { links: &http_links }.into();
        let keyboard = make_download_confirm_keyboard(
            &server_selected.download_config.link_dirs,
            &server_selected.download_config.default_dir,
            |dir| format!("uri|{dir}|{uuid}"),
        );
        state.uri_cache.lock().insert(uuid, http_links);

        bot.send_message(msg.chat.id, text)
            .reply_markup(keyboard)
            .await?;
        return Ok(ControlFlow::Break(()));
    }

    // extract all torrent files.
    if let Some(document) = msg.document() {
        if document.file.size > MAX_TORRENT_SIZE {
            bot.send_message(msg.chat.id, "File size too large!")
                .await?;
            return Ok(ControlFlow::Break(()));
        }
        let uuid = uuid::Uuid::new_v4().to_string();
        let text = MsgDownloadTorrentConfirm { document };
        let keyboard = make_download_confirm_keyboard(
            &server_selected.download_config.torrent_dirs,
            &server_selected.download_config.default_dir,
            |dir| format!("t|{dir}|{uuid}"),
        );
        state
            .file_cache
            .lock()
            .insert(uuid.into(), document.file.id.clone());

        bot.send_message(msg.chat.id, text)
            .reply_markup(keyboard)
            .await?;
        return Ok(ControlFlow::Break(()));
    }

    Ok(ControlFlow::Continue(()))
}

async fn callback_handler(
    bot: Bot,
    q: CallbackQuery,
    state: Arc<State>,
) -> Result<(), Box<dyn Error + Send + Sync>> {
    let (Some(user_data), Some(MaybeInaccessibleMessage::Regular(q))) = (q.data, q.message) else {
        return Ok(());
    };
    let id = q.id;
    let chat = q.chat;

    let user_data = UserData::from_str(&user_data)?;
    if let UserData::SwitchServer(server_name) = user_data {
        let msg = match state.try_select(chat.id.0, &server_name) {
            state::SelectResult::Success => MsgSwitchResult::Success {
                server_name: &server_name,
            },
            state::SelectResult::NoNeed => MsgSwitchResult::NoNeed,
            state::SelectResult::Failure => MsgSwitchResult::Failure,
        };
        bot.edit_message_text(chat.id, id, msg).await?;
        return Ok(());
    };

    let Some(server_selected) = state.selected(chat.id.0) else {
        select_or_unauthorized(&bot, chat.id, None, &state).await?;
        return Ok(());
    };

    match user_data {
        UserData::Task(gid) => {
            let Some((task_desc, task_status)) =
                server_selected.tasks_cache.read().fmt_task(&gid).map(
                    |(task_desc, task_status)| {
                        (task_desc, task_status.status.unwrap_or(TaskStatus::Removed))
                    },
                )
            else {
                bot.send_message(chat.id, MsgTaskNotFound { gid: &gid })
                    .await?;
                return Ok(());
            };

            let keyboard = make_single_task_keyboard(&gid, task_status);
            let msg = bot
                .send_message(chat.id, task_desc)
                .reply_markup(keyboard)
                .reply_parameters(ReplyParameters::new(id))
                .await?;
            server_selected
                .tasks_cache
                .write()
                .add_task_subscriber(gid, chat.id, msg.id);
        }
        UserData::PauseTask(gid) => {
            let res = server_selected.client.pause(&gid).await;
            bot.edit_message_text(chat.id, id, MsgTaskActionResult::Pause(&gid, &res))
                .await?;
        }
        UserData::ResumeTask(gid) => {
            let res = server_selected.client.resume(&gid).await;
            bot.edit_message_text(chat.id, id, MsgTaskActionResult::Resume(&gid, &res))
                .await?;
        }
        UserData::RemoveTask(gid) => {
            let res = server_selected.client.remove(&gid).await;
            bot.edit_message_text(chat.id, id, MsgTaskActionResult::Remove(&gid, &res))
                .await?;
        }
        UserData::AddUri(dir, uris_key) => {
            let Some(uris) = state.uri_cache.lock().remove(&uris_key) else {
                bot.edit_message_text(chat.id, id, format!("Uri cache {uris_key} not found!"))
                    .await?;
                return Ok(());
            };
            let AddUrisResult { gids, error } = server_selected
                .client
                .add_uris(uris.as_slice(), Some(dir.clone()))
                .await;
            let mut text = if gids.is_empty() {
                String::new()
            } else {
                format!("Add download uris task to {dir} successfully:\n")
            };
            for (uri, gid) in uris.iter().zip(gids.iter()) {
                text.push_str(&format!("{uri}: {gid}\n"));
            }
            if let Some(e) = error {
                if !gids.is_empty() {
                    text.push_str(&format!("\nPartially failed at uri[{}]: {e}", gids.len()));
                } else {
                    text = format!("Push add uris task failed: {e}");
                }
            }
            bot.edit_message_text(chat.id, id, text).await?;
        }
        UserData::AddTorrent(dir, file_id_key) => {
            let Some(file_id) = state.file_cache.lock().remove(&file_id_key) else {
                bot.edit_message_text(chat.id, id, format!("File cache {file_id_key} not found!"))
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

            let res = server_selected
                .client
                .add_torrent(&file, Some(dir.clone()))
                .await;
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
        _ => (),
    }

    Ok(())
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

async fn select_or_unauthorized(
    bot: &Bot,
    chat_id: ChatId,
    msg_id: Option<MessageId>,
    state: &State,
) -> anyhow::Result<()> {
    if let Some(authorized) = state.authorized(chat_id.0) {
        if authorized.size_hint().1 == Some(1) {
            bot.send_message(
                chat_id,
                "No need to switch server, there is only one server.",
            )
            .reply_to_message_id_opt(msg_id)
            .await?;
            return Ok(());
        }
        let keyboard = make_switch_server_keyboard(authorized);
        let selected = state.selected(chat_id.0);
        bot.send_message(
            chat_id,
            MsgSwitchPrompt {
                current_server_name: selected.as_ref().map(|s| s.name.as_str()),
            },
        )
        .reply_markup(keyboard)
        .reply_to_message_id_opt(msg_id)
        .await?;
    } else {
        bot.send_message(chat_id, MsgUnauthorized { user_id: chat_id.0 })
            .reply_to_message_id_opt(msg_id)
            .await?;
    }
    Ok(())
}

enum UserData {
    Task(SmolStr),
    PauseTask(SmolStr),
    ResumeTask(SmolStr),
    RemoveTask(SmolStr),
    AddUri(SmolStr, String),
    AddTorrent(SmolStr, SmolStr),
    SwitchServer(SmolStr),
}

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
        let Some((action, gid)) = s.split_once('|') else {
            return Err(UserDataError);
        };
        match action {
            "task" => Ok(UserData::Task(gid.into())),
            "pause" => Ok(UserData::PauseTask(gid.into())),
            "resume" => Ok(UserData::ResumeTask(gid.into())),
            "remove" => Ok(UserData::RemoveTask(gid.into())),
            "uri" => {
                let mut parts = gid.split('|');
                let dir = parts.next().ok_or(UserDataError)?;
                let uris_key = parts.next().ok_or(UserDataError)?;
                Ok(UserData::AddUri(dir.into(), uris_key.into()))
            }
            "t" => {
                let mut parts = gid.split('|');
                let dir = parts.next().ok_or(UserDataError)?;
                let torrent_id = parts.next().ok_or(UserDataError)?;
                Ok(UserData::AddTorrent(dir.into(), torrent_id.into()))
            }
            "switch" => {
                let mut parts = gid.split('|');
                let server = parts.next().ok_or(UserDataError)?;
                Ok(UserData::SwitchServer(server.into()))
            }
            _ => Err(UserDataError),
        }
    }
}
