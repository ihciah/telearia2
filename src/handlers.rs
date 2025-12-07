//! Message and callback handlers for the Telegram bot.
//!
//! This module contains the main handler functions that process
//! incoming messages and callback queries from Telegram.

use std::ops::ControlFlow;
use std::str::FromStr;
use std::sync::Arc;

use aria2_rs::status::TaskStatus;
use aria2_rs::SmallVec;
use bytes::Bytes;
use smol_str::SmolStr;
use teloxide::{
    payloads::SendMessageSetters,
    prelude::*,
    types::{MaybeInaccessibleMessage, Me, MessageId, ParseMode, ReplyParameters},
    utils::command::BotCommands,
    Bot,
};

use crate::aria2::AddUrisResult;
use crate::constants::{ARIA2_OP_TIMEOUT, MAX_TORRENT_SIZE};
use crate::format::{
    make_download_confirm_keyboard, make_refresh_list_keyboard, make_refresh_task_keyboard,
    make_retry_keyboard, make_single_task_keyboard, make_switch_server_keyboard,
    make_tasks_keyboard,
    msg::{
        MsgCatchError, MsgDownloadLinkConfirm, MsgDownloadMagnetConfirm, MsgDownloadTorrentConfirm,
        MsgStart, MsgSwitchPrompt, MsgSwitchResult, MsgTaskActionResult, MsgTaskList,
        MsgTaskNotFound, MsgUnauthorized,
    },
};
use crate::state::{State, TasksCache};
use crate::utils::SendMessageSettersExt;
use crate::{Command, UserData, HTTP_RE, MAGNET_RE};

/// Handle incoming messages from Telegram.
///
/// Processes commands and various types of content (magnets, links, torrents).
pub async fn message_handler(
    bot: Bot,
    msg: Message,
    me: Me,
    state: Arc<State>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
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
                if let Err(e) =
                    TasksCache::refresh(&server_selected.tasks_cache, &server_selected.client).await
                {
                    bot.send_message(msg.chat.id, format!("Failed to fetch tasks: {e}"))
                        .reply_parameters(ReplyParameters::new(msg.id))
                        .await?;
                    return Ok(());
                }
                let tasks = server_selected.tasks_cache.read().fmt_tasks();
                let keyboard = make_tasks_keyboard(tasks);
                let reply = bot
                    .send_message(msg.chat.id, MsgTaskList)
                    .reply_markup(keyboard)
                    .reply_parameters(ReplyParameters::new(msg.id))
                    .await?;
                server_selected
                    .tasks_cache
                    .write()
                    .add_list_subscriber(reply.chat.id, reply.id);
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
    let text = match handle_message_content(&bot, &msg, state).await {
        Ok(ControlFlow::Break(_)) => return Ok(()),
        Ok(ControlFlow::Continue(_)) => MsgCatchError::InvalidCommand,
        Err(e) => MsgCatchError::Error { error: e },
    };
    bot.send_message(msg.chat.id, text).await?;

    Ok(())
}

/// Handle message content (magnets, links, torrents).
async fn handle_message_content(
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
        let text: String = MsgDownloadMagnetConfirm { magnets: &magnets }.into();
        let keyboard = make_download_confirm_keyboard(
            &server_selected.download_config.magnet_dirs,
            &server_selected.download_config.default_dir,
            |dir| {
                let uuid = uuid::Uuid::new_v4().simple().to_string();
                let callback = format!("uri|{uuid}");
                state
                    .uri_cache
                    .lock()
                    .insert(uuid, (dir.into(), magnets.clone()));
                callback
            },
        );

        bot.send_message(msg.chat.id, text)
            .reply_markup(keyboard)
            .reply_parameters(ReplyParameters::new(msg.id))
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
        let text: String = MsgDownloadLinkConfirm { links: &http_links }.into();
        let keyboard = make_download_confirm_keyboard(
            &server_selected.download_config.link_dirs,
            &server_selected.download_config.default_dir,
            |dir| {
                let uuid = uuid::Uuid::new_v4().simple().to_string();
                let callback = format!("uri|{uuid}");
                state
                    .uri_cache
                    .lock()
                    .insert(uuid, (dir.into(), http_links.clone()));
                callback
            },
        );

        bot.send_message(msg.chat.id, text)
            .reply_markup(keyboard)
            .reply_parameters(ReplyParameters::new(msg.id))
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
        let file_id = document.file.id.to_string();
        let text = MsgDownloadTorrentConfirm { document };
        let keyboard = make_download_confirm_keyboard(
            &server_selected.download_config.torrent_dirs,
            &server_selected.download_config.default_dir,
            |dir| {
                let uuid = uuid::Uuid::new_v4().simple().to_string();
                let callback = format!("t|{uuid}");
                state
                    .file_cache
                    .lock()
                    .insert(uuid, (dir.into(), file_id.clone()));
                callback
            },
        );

        bot.send_message(msg.chat.id, text)
            .reply_markup(keyboard)
            .reply_parameters(ReplyParameters::new(msg.id))
            .await?;
        return Ok(ControlFlow::Break(()));
    }

    Ok(ControlFlow::Continue(()))
}

/// Handle callback queries from inline keyboards.
pub async fn callback_handler(
    bot: Bot,
    q: CallbackQuery,
    state: Arc<State>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let (Some(user_data), Some(MaybeInaccessibleMessage::Regular(q))) = (q.data, q.message) else {
        return Ok(());
    };
    let id = q.id;
    let chat = q.chat;

    let user_data = UserData::from_str(&user_data)?;
    if let UserData::SwitchServer(server_name) = user_data {
        let msg = match state.try_select(chat.id.0, &server_name) {
            crate::state::SelectResult::Success => MsgSwitchResult::Success {
                server_name: &server_name,
            },
            crate::state::SelectResult::NoNeed => MsgSwitchResult::NoNeed,
            crate::state::SelectResult::Failure => MsgSwitchResult::Failure,
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
            handle_task_view(&bot, &server_selected, chat.id, id, &gid).await?;
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
        UserData::AddUri(uuid) => {
            handle_add_uri(&bot, &state, &server_selected, chat.id, id, uuid).await?;
        }
        UserData::AddTorrent(uuid) => {
            handle_add_torrent(&bot, &state, &server_selected, chat.id, id, uuid).await?;
        }
        UserData::RefreshList => {
            handle_refresh_list(&bot, &server_selected, chat.id, id).await?;
        }
        UserData::RefreshTask(gid) => {
            handle_refresh_task(&bot, &server_selected, chat.id, id, &gid).await?;
        }
        _ => (),
    }

    Ok(())
}

/// Handle viewing a single task's details.
async fn handle_task_view(
    bot: &Bot,
    server: &crate::state::ServerState,
    chat_id: ChatId,
    msg_id: MessageId,
    gid: &str,
) -> anyhow::Result<()> {
    let Some((task_desc, task_status)) =
        server
            .tasks_cache
            .read()
            .fmt_task(gid)
            .map(|(task_desc, task_status)| {
                (task_desc, task_status.status.unwrap_or(TaskStatus::Removed))
            })
    else {
        bot.send_message(chat_id, MsgTaskNotFound { gid }).await?;
        return Ok(());
    };

    let keyboard = make_single_task_keyboard(gid, task_status);
    let msg = bot
        .send_message(chat_id, task_desc)
        .reply_markup(keyboard)
        .reply_parameters(ReplyParameters::new(msg_id))
        .await?;
    server
        .tasks_cache
        .write()
        .add_task_subscriber(gid.into(), chat_id, msg.id);
    Ok(())
}

/// Handle adding URIs (magnets or http links) with retry support.
async fn handle_add_uri(
    bot: &Bot,
    state: &State,
    server: &crate::state::ServerState,
    chat_id: ChatId,
    msg_id: MessageId,
    uuid: String,
) -> anyhow::Result<()> {
    let Some((dir, uris)) = state.uri_cache.lock().remove(&uuid) else {
        bot.edit_message_text(chat_id, msg_id, format!("Uri cache {uuid} not found!"))
            .await?;
        return Ok(());
    };

    let add_result = tokio::time::timeout(
        ARIA2_OP_TIMEOUT,
        server.client.add_uris(uris.as_slice(), Some(dir.clone())),
    )
    .await;

    let AddUrisResult { gids, error } = match add_result {
        Ok(result) => result,
        Err(_) => {
            let retry_uuid = uuid::Uuid::new_v4().simple().to_string();
            let keyboard = make_retry_keyboard(format!("uri|{retry_uuid}"));
            state.uri_cache.lock().insert(retry_uuid, (dir, uris));
            bot.edit_message_text(chat_id, msg_id, "Add uris task timeout")
                .reply_markup(keyboard)
                .await?;
            return Ok(());
        }
    };

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
        // Store failed URIs for retry
        let failed_uris: SmallVec<String> = uris.into_iter().skip(gids.len()).collect();
        let retry_uuid = uuid::Uuid::new_v4().simple().to_string();
        let keyboard = make_retry_keyboard(format!("uri|{retry_uuid}"));
        state
            .uri_cache
            .lock()
            .insert(retry_uuid, (dir, failed_uris));
        bot.edit_message_text(chat_id, msg_id, text)
            .reply_markup(keyboard)
            .await?;
    } else {
        text.push_str("\nUse /task to list all tasks.");
        bot.edit_message_text(chat_id, msg_id, text).await?;
    }

    Ok(())
}

/// Handle adding torrent files with retry support.
async fn handle_add_torrent(
    bot: &Bot,
    state: &State,
    server: &crate::state::ServerState,
    chat_id: ChatId,
    msg_id: MessageId,
    uuid: String,
) -> anyhow::Result<()> {
    let Some((dir, file_id)) = state.file_cache.lock().remove(&uuid) else {
        bot.edit_message_text(chat_id, msg_id, format!("File cache {uuid} not found!"))
            .await?;
        return Ok(());
    };

    // Download the torrent file from Telegram
    let file = match tokio::time::timeout(
        ARIA2_OP_TIMEOUT,
        get_telegram_file(bot, file_id.as_str(), &state.http_client),
    )
    .await
    {
        Ok(Ok(file)) => file,
        Ok(Err(e)) => {
            store_file_and_show_retry(
                bot,
                state,
                chat_id,
                msg_id,
                &format!("Download torrent file failed: {e}"),
                dir,
                file_id,
            )
            .await?;
            return Ok(());
        }
        Err(_) => {
            store_file_and_show_retry(
                bot,
                state,
                chat_id,
                msg_id,
                "Download torrent file timeout",
                dir,
                file_id,
            )
            .await?;
            return Ok(());
        }
    };

    // Add torrent to aria2
    let res = tokio::time::timeout(
        ARIA2_OP_TIMEOUT,
        server.client.add_torrent(&file, Some(dir.clone())),
    )
    .await;

    let gid = match res {
        Ok(Ok(gid)) => gid,
        Ok(Err(e)) => {
            store_file_and_show_retry(
                bot,
                state,
                chat_id,
                msg_id,
                &format!("Push add torrent task failed: {e}"),
                dir,
                file_id,
            )
            .await?;
            return Ok(());
        }
        Err(_) => {
            store_file_and_show_retry(
                bot,
                state,
                chat_id,
                msg_id,
                "Add torrent task timeout",
                dir,
                file_id,
            )
            .await?;
            return Ok(());
        }
    };

    let text = format!(
        "Add download torrent task to {dir} successfully:\nGID: {gid}\n\nUse /task to list all tasks."
    );
    bot.edit_message_text(chat_id, msg_id, text).await?;

    Ok(())
}

/// Store file info and show retry button.
async fn store_file_and_show_retry(
    bot: &Bot,
    state: &State,
    chat_id: ChatId,
    msg_id: MessageId,
    error_msg: &str,
    dir: SmolStr,
    file_id: String,
) -> anyhow::Result<()> {
    let retry_uuid = uuid::Uuid::new_v4().simple().to_string();
    let keyboard = make_retry_keyboard(format!("t|{retry_uuid}"));
    state.file_cache.lock().insert(retry_uuid, (dir, file_id));
    bot.edit_message_text(chat_id, msg_id, error_msg)
        .reply_markup(keyboard)
        .await?;
    Ok(())
}

/// Handle refreshing the task list.
async fn handle_refresh_list(
    bot: &Bot,
    server: &crate::state::ServerState,
    chat_id: ChatId,
    msg_id: MessageId,
) -> anyhow::Result<()> {
    if let Err(e) = TasksCache::refresh(&server.tasks_cache, &server.client).await {
        bot.edit_message_text(chat_id, msg_id, format!("Failed to fetch tasks: {e}"))
            .reply_markup(make_refresh_list_keyboard())
            .await?;
        return Ok(());
    }
    let tasks = server.tasks_cache.read().fmt_tasks();
    let keyboard = make_tasks_keyboard(tasks);
    bot.edit_message_text(chat_id, msg_id, MsgTaskList)
        .reply_markup(keyboard)
        .await?;
    server
        .tasks_cache
        .write()
        .add_list_subscriber(chat_id, msg_id);
    Ok(())
}

/// Handle refreshing a single task.
async fn handle_refresh_task(
    bot: &Bot,
    server: &crate::state::ServerState,
    chat_id: ChatId,
    msg_id: MessageId,
    gid: &str,
) -> anyhow::Result<()> {
    if let Err(e) = TasksCache::refresh(&server.tasks_cache, &server.client).await {
        bot.edit_message_text(chat_id, msg_id, format!("Failed to fetch tasks: {e}"))
            .reply_markup(make_refresh_task_keyboard(gid))
            .await?;
        return Ok(());
    }
    let Some((task_desc, task_status)) =
        server
            .tasks_cache
            .read()
            .fmt_task(gid)
            .map(|(task_desc, task_status)| {
                (task_desc, task_status.status.unwrap_or(TaskStatus::Removed))
            })
    else {
        bot.edit_message_text(chat_id, msg_id, MsgTaskNotFound { gid })
            .await?;
        return Ok(());
    };
    let keyboard = make_single_task_keyboard(gid, task_status);
    bot.edit_message_text(chat_id, msg_id, task_desc)
        .reply_markup(keyboard)
        .await?;
    server
        .tasks_cache
        .write()
        .add_task_subscriber(gid.into(), chat_id, msg_id);
    Ok(())
}

/// Download a file from Telegram servers.
async fn get_telegram_file(
    bot: &Bot,
    file_id: &str,
    http_client: &reqwest::Client,
) -> anyhow::Result<Bytes> {
    let file = bot.get_file(file_id.to_owned().into()).await?;
    let url = bot
        .api_url()
        .join(&format!("file/bot{}/{}", bot.token(), file.path))
        .expect("failed to format file url");
    let data = http_client
        .get(url)
        .send()
        .await?
        .error_for_status()?
        .bytes()
        .await?;
    Ok(data)
}

/// Show server selection or unauthorized message.
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
