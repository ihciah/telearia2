use std::{
    fmt::{Error, Formatter},
    sync::Arc,
};

use aria2_rs::status::{BittorrentStatus, Status, TaskStatus};
use teloxide::types::{InlineKeyboardButton, InlineKeyboardMarkup};

use crate::config::{DirConfig, MAX_BRIFE_NAME_LEN};

pub trait MessageFmt {
    fn fmt_message<const DETAILED: bool>(&self, f: &mut Formatter<'_>) -> Result<(), Error>;
}

impl<T: MessageFmt> MessageFmt for &T {
    fn fmt_message<const DETAILED: bool>(&self, f: &mut Formatter<'_>) -> Result<(), Error> {
        (**self).fmt_message::<DETAILED>(f)
    }
}

impl<T: MessageFmt> MessageFmt for Arc<T> {
    fn fmt_message<const DETAILED: bool>(&self, f: &mut Formatter<'_>) -> Result<(), Error> {
        (**self).fmt_message::<DETAILED>(f)
    }
}

impl<T: MessageFmt> MessageFmt for Box<T> {
    fn fmt_message<const DETAILED: bool>(&self, f: &mut Formatter<'_>) -> Result<(), Error> {
        (**self).fmt_message::<DETAILED>(f)
    }
}

pub struct MessageFmtDetailed<T>(pub T);
impl<T: MessageFmt> std::fmt::Display for MessageFmtDetailed<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), Error> {
        self.0.fmt_message::<true>(f)
    }
}

pub struct MessageFmtBrief<T>(pub T);
impl<T: MessageFmt> std::fmt::Display for MessageFmtBrief<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), Error> {
        self.0.fmt_message::<false>(f)
    }
}

impl MessageFmt for Status {
    fn fmt_message<const DETAILED: bool>(&self, f: &mut Formatter<'_>) -> Result<(), Error> {
        let name = self.name();
        let progress_size = self.progress_size();
        if DETAILED {
            // Name
            writeln!(f, "Task Name: {name}")?;

            // GID
            writeln!(f, "GID: {}", self.gid.as_deref().unwrap_or("Unknown"))?;

            // Status
            let status = match &self.status {
                Some(TaskStatus::Active) => "Active",
                Some(TaskStatus::Waiting) => "Waiting",
                Some(TaskStatus::Paused) => "Paused",
                Some(TaskStatus::Error) => "Error",
                Some(TaskStatus::Complete) => "Complete",
                Some(TaskStatus::Removed) => "Removed",
                None => "Unknown",
            };
            writeln!(f, "Status: {status}")?;

            // Dir
            let dir = self.dir.as_deref().unwrap_or("Unknown");
            writeln!(f, "Dir: {dir}")?;

            if self.status == Some(TaskStatus::Active) {
                // Seeder count
                if let (Some(seed_cnt), Some(conn_cnt)) = (self.num_seeders, self.connections) {
                    writeln!(f, "Conn/Seeder: {conn_cnt}/{seed_cnt}",)?;
                }

                // Speed
                if let (Some(ul_speed), Some(dl_speed)) = (self.upload_speed, self.download_speed) {
                    writeln!(
                        f,
                        "Speed: ⬆ {ul_speed}/s | ⬇ {dl_speed}/s",
                        ul_speed = SizeFormatter(ul_speed),
                        dl_speed = SizeFormatter(dl_speed),
                    )?;
                }
            }

            // Progress
            writeln!(
                f,
                "Progress: {:.3}% {}/{}",
                self.progress() * 100.,
                SizeFormatter(progress_size.0),
                SizeFormatter(progress_size.1)
            )?;
        } else {
            let status = match &self.status {
                Some(TaskStatus::Active) => "⏬",
                Some(TaskStatus::Waiting) => "🕒",
                Some(TaskStatus::Paused) => "⏸️",
                Some(TaskStatus::Error) => "❌",
                Some(TaskStatus::Complete) => "✅",
                Some(TaskStatus::Removed) => "❎",
                None => "❔",
            };
            if matches!(
                self.status,
                Some(TaskStatus::Active) | Some(TaskStatus::Waiting) | Some(TaskStatus::Paused)
            ) {
                write!(
                    f,
                    "{status}|{:.3}%|{}/{}|{}",
                    self.progress() * 100.,
                    SizeFormatter(progress_size.0),
                    SizeFormatter(progress_size.1),
                    name.trim_start_matches("https://")
                        .trim_start_matches("http://")
                        .chars()
                        .take(MAX_BRIFE_NAME_LEN)
                        .collect::<String>()
                )?;
            } else {
                write!(
                    f,
                    "{status}|{}|{}",
                    SizeFormatter(progress_size.1),
                    name.trim_start_matches("https://")
                        .trim_start_matches("http://")
                        .chars()
                        .take(MAX_BRIFE_NAME_LEN)
                        .collect::<String>()
                )?;
            }
        }

        Ok(())
    }
}

pub trait TaskExt {
    fn name(&self) -> &str;
    fn progress(&self) -> f64;
    fn progress_size(&self) -> (u64, u64);
}

impl TaskExt for Status {
    fn name(&self) -> &str {
        match &self {
            // Use torrent name as task name
            Status {
                bittorrent:
                    Some(BittorrentStatus {
                        info: Some(info), ..
                    }),
                ..
            } => info.name.as_str(),
            // Use first file uri or path as task name
            Status {
                files: Some(files), ..
            } => files
                .first()
                .map(|f| match f.uris.first() {
                    Some(uri) => uri.uri.as_str(),
                    None => f.path.as_str(),
                })
                .unwrap_or("Unknown Task Name"),
            _ => self.gid.as_deref().unwrap_or("Unknown Task Name"),
        }
    }

    fn progress(&self) -> f64 {
        match self.total_length {
            Some(total) if total > 0 => self.completed_length.unwrap_or(0) as f64 / total as f64,
            _ => 0.0,
        }
    }

    fn progress_size(&self) -> (u64, u64) {
        match (self.completed_length, self.total_length) {
            (Some(completed_length), Some(total_length)) => (completed_length, total_length),
            _ => (0, 0),
        }
    }
}

struct SizeFormatter(u64);
impl std::fmt::Display for SizeFormatter {
    fn fmt(&self, f: &mut Formatter<'_>) -> Result<(), Error> {
        macro_rules! clamp_size {
            ($size: expr, $unit_var: expr, $unit: literal) => {
                if $size > 1024.0 {
                    $size /= 1024.0;
                    $unit_var = $unit;
                }
            };
        }

        let mut size = self.0 as f64;
        let mut unit = "B";
        clamp_size!(size, unit, "KiB");
        clamp_size!(size, unit, "MiB");
        clamp_size!(size, unit, "GiB");
        clamp_size!(size, unit, "TiB");
        clamp_size!(size, unit, "PiB");
        write!(f, "{size:.2} {unit}")
    }
}

pub fn make_tasks_keyboard(tasks: Vec<(String, String)>) -> InlineKeyboardMarkup {
    let keyboard: Vec<_> = tasks
        .into_iter()
        .map(|(desc, id)| vec![InlineKeyboardButton::callback(desc, format!("task|{id}"))])
        .collect();
    InlineKeyboardMarkup::new(keyboard)
}

pub fn make_refresh_list_keyboard() -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
        "🔄 Refresh",
        "rlist",
    )]])
}

pub fn make_refresh_task_keyboard(gid: &str) -> InlineKeyboardMarkup {
    InlineKeyboardMarkup::new(vec![vec![InlineKeyboardButton::callback(
        "🔄 Refresh",
        format!("rtask|{gid}"),
    )]])
}

pub fn make_switch_server_keyboard<'a>(
    servers: impl Iterator<Item = &'a str>,
) -> InlineKeyboardMarkup {
    let keyboard: Vec<_> = servers
        .map(|server| {
            vec![InlineKeyboardButton::callback(
                server,
                format!("switch|{server}"),
            )]
        })
        .collect();
    InlineKeyboardMarkup::new(keyboard)
}

pub fn make_single_task_keyboard(gid: &str, status: TaskStatus) -> InlineKeyboardMarkup {
    const RESUME: &str = "▶️ Resume";
    const PAUSE: &str = "⏸ Pause";
    const REMOVE: &str = "⏹ Remove";

    let bs = match status {
        TaskStatus::Active | TaskStatus::Waiting => vec![
            InlineKeyboardButton::callback(PAUSE, format!("pause|{gid}")),
            InlineKeyboardButton::callback(REMOVE, format!("remove|{gid}")),
        ],
        TaskStatus::Paused => vec![
            InlineKeyboardButton::callback(RESUME, format!("resume|{gid}")),
            InlineKeyboardButton::callback(REMOVE, format!("remove|{gid}")),
        ],
        TaskStatus::Error | TaskStatus::Complete => vec![InlineKeyboardButton::callback(
            REMOVE,
            format!("remove|{gid}"),
        )],
        TaskStatus::Removed => vec![],
    };

    InlineKeyboardMarkup::new(vec![bs])
}

pub fn make_download_confirm_keyboard<F>(
    mapping: &[DirConfig],
    default_dir: &str,
    mut register: F,
) -> InlineKeyboardMarkup
where
    F: FnMut(&str) -> String,
{
    let mut keyboard: Vec<Vec<InlineKeyboardButton>> = vec![];
    for dir_cfg in mapping.chunks(3) {
        let row = dir_cfg
            .iter()
            .map(|dc| InlineKeyboardButton::callback(&dc.name, register(dc.path.as_str())))
            .collect();

        keyboard.push(row);
    }
    keyboard.push(vec![InlineKeyboardButton::callback(
        "Default",
        register(default_dir),
    )]);
    InlineKeyboardMarkup::new(keyboard)
}

pub mod msg {
    use std::fmt::Display;
    pub struct MsgStart;

    impl From<MsgStart> for String {
        fn from(_: MsgStart) -> Self {
            "Welcome to ihciah's aria2 bot!\nUse /help to get help.\nUse /task to get task list.\nTo download, send magnet link, torrent file or http(s) link to me!\n\nTelearia2 is an open source project(https://github.com/ihciah/telearia2).".into()
        }
    }

    pub struct MsgTask;

    impl From<MsgTask> for String {
        fn from(_: MsgTask) -> Self {
            "Tasks:\nThis page will be updated automatically within 3mins.\nUse /task to refresh again.".into()
        }
    }

    pub enum MsgSwitchResult<'a> {
        Success { server_name: &'a str },
        NoNeed,
        Failure,
    }

    impl From<MsgSwitchResult<'_>> for String {
        fn from(msg: MsgSwitchResult<'_>) -> Self {
            match msg {
                MsgSwitchResult::Success { server_name } => {
                    format!("Server switched to {server_name}.")
                }
                MsgSwitchResult::NoNeed => "Only one server is accessable.".into(),
                MsgSwitchResult::Failure => {
                    "Failed to switch server. The server may not exist, or you may not have permission to access it.".into()
                }
            }
        }
    }

    pub struct MsgSwitchPrompt<'a> {
        pub current_server_name: Option<&'a str>,
    }

    impl From<MsgSwitchPrompt<'_>> for String {
        fn from(prompt: MsgSwitchPrompt<'_>) -> Self {
            match prompt.current_server_name {
                Some(name) => format!("Current server: {name}. Please select server:"),
                None => "No server selected. Please select server:".into(),
            }
        }
    }

    pub struct MsgUnauthorized {
        pub user_id: i64,
    }

    impl From<MsgUnauthorized> for String {
        fn from(cmd: MsgUnauthorized) -> Self {
            format!(
                "User or group({}) are not authorized to use this command!",
                cmd.user_id
            )
        }
    }

    pub enum MsgCatchError<E> {
        InvalidCommand,
        Error { error: E },
    }

    impl<E: Display> From<MsgCatchError<E>> for String {
        fn from(res: MsgCatchError<E>) -> Self {
            match res {
                MsgCatchError::InvalidCommand => "Invalid command or format!".into(),
                MsgCatchError::Error { error } => format!("Error: {error}"),
            }
        }
    }

    pub struct MsgDownloadMagnetConfirm<'a, T> {
        pub magnets: &'a [T],
    }

    impl<'a, T: Display> From<MsgDownloadMagnetConfirm<'a, T>> for String {
        fn from(msg: MsgDownloadMagnetConfirm<'a, T>) -> Self {
            if msg.magnets.len() == 1 {
                format!("Confirm download {}?", msg.magnets[0])
            } else {
                format!("Confirm download {} magnets?", msg.magnets.len())
            }
        }
    }

    pub struct MsgDownloadLinkConfirm<'a, T> {
        pub links: &'a [T],
    }

    impl<'a, T: Display> From<MsgDownloadLinkConfirm<'a, T>> for String {
        fn from(msg: MsgDownloadLinkConfirm<'a, T>) -> Self {
            if msg.links.len() == 1 {
                format!("Confirm download {}?", msg.links[0])
            } else {
                format!("Confirm download {} links?", msg.links.len())
            }
        }
    }

    pub struct MsgDownloadTorrentConfirm<'a> {
        pub document: &'a teloxide::types::Document,
    }

    impl<'a> From<MsgDownloadTorrentConfirm<'a>> for String {
        fn from(msg: MsgDownloadTorrentConfirm<'a>) -> Self {
            match &msg.document.file_name {
                Some(name) => format!("Confirm download torrent file {name}?"),
                None => format!("Confirm download torrent file_{}?", msg.document.file.id),
            }
        }
    }

    pub struct MsgTaskNotFound<'a> {
        pub gid: &'a str,
    }

    impl<'a> From<MsgTaskNotFound<'a>> for String {
        fn from(msg: MsgTaskNotFound<'a>) -> Self {
            format!("Task {} not found!", msg.gid)
        }
    }

    pub enum MsgTaskActionResult<'a, E, T = ()> {
        Pause(&'a str, &'a Result<T, E>),
        Resume(&'a str, &'a Result<T, E>),
        Remove(&'a str, &'a Result<T, E>),
        Purge(&'a Result<T, E>),
    }

    impl<'a, E: Display> From<MsgTaskActionResult<'a, E>> for String {
        fn from(res: MsgTaskActionResult<'a, E>) -> Self {
            let (action, gid, result) = match res {
                MsgTaskActionResult::Purge(result) => {
                    return match result {
                        Ok(_) => "Purge downloaded results successfully!".into(),
                        Err(error) => format!("Purge downloaded results failed: {error}"),
                    }
                }
                MsgTaskActionResult::Pause(gid, result) => ("Pause", gid, result),
                MsgTaskActionResult::Resume(gid, result) => ("Resume", gid, result),
                MsgTaskActionResult::Remove(gid, result) => ("Remove", gid, result),
            };
            match result {
                Ok(_) => format!("{action} task {gid} successfully!"),
                Err(error) => format!("{action} task {gid} failed: {error}"),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_size_formatter_bytes() {
        assert_eq!(format!("{}", SizeFormatter(0)), "0.00 B");
        assert_eq!(format!("{}", SizeFormatter(512)), "512.00 B");
        assert_eq!(format!("{}", SizeFormatter(1023)), "1023.00 B");
    }

    #[test]
    fn test_size_formatter_kib() {
        assert_eq!(format!("{}", SizeFormatter(1025)), "1.00 KiB");
        assert_eq!(format!("{}", SizeFormatter(1536)), "1.50 KiB");
        assert_eq!(format!("{}", SizeFormatter(2048)), "2.00 KiB");
    }

    #[test]
    fn test_size_formatter_mib() {
        assert_eq!(format!("{}", SizeFormatter(1024 * 1025)), "1.00 MiB");
        assert_eq!(format!("{}", SizeFormatter(1024 * 1024 * 5)), "5.00 MiB");
    }

    #[test]
    fn test_size_formatter_gib() {
        assert_eq!(format!("{}", SizeFormatter(1024 * 1024 * 1025)), "1.00 GiB");
    }

    fn make_status(completed: Option<u64>, total: Option<u64>) -> Status {
        Status {
            gid: None,
            status: None,
            total_length: total,
            completed_length: completed,
            upload_length: None,
            bitfield: None,
            download_speed: None,
            upload_speed: None,
            info_hash: None,
            num_seeders: None,
            seeder: None,
            connections: None,
            error_code: None,
            error_message: None,
            followed_by: None,
            following: None,
            belongs_to: None,
            dir: None,
            files: None,
            bittorrent: None,
            num_pieces: None,
            piece_length: None,
        }
    }

    #[test]
    fn test_progress_zero_total() {
        let status = make_status(Some(100), Some(0));
        assert_eq!(status.progress(), 0.0);
    }

    #[test]
    fn test_progress_none_total() {
        let status = make_status(Some(100), None);
        assert_eq!(status.progress(), 0.0);
    }

    #[test]
    fn test_progress_normal() {
        let status = make_status(Some(500), Some(1000));
        assert_eq!(status.progress(), 0.5);
    }

    #[test]
    fn test_progress_complete() {
        let status = make_status(Some(1000), Some(1000));
        assert_eq!(status.progress(), 1.0);
    }

    #[test]
    fn test_progress_size() {
        let status = make_status(Some(500), Some(1000));
        assert_eq!(status.progress_size(), (500, 1000));
    }

    #[test]
    fn test_progress_size_none() {
        let status = make_status(None, None);
        assert_eq!(status.progress_size(), (0, 0));
    }
}
