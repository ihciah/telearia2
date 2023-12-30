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

            let dir = self.dir.as_deref().unwrap_or("Unknown");
            writeln!(f, "Dir: {dir}")?;

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
        if let Some(total_length) = self.total_length {
            if total_length == 0 {
                1.0
            } else {
                let completed_length = self.completed_length.unwrap_or(0);
                completed_length as f64 / total_length as f64
            }
        } else {
            0.0
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
        write!(f, "{:.2} {}", size, unit)
    }
}

pub fn make_tasks_keyboard(tasks: Vec<(String, String)>) -> InlineKeyboardMarkup {
    let keyboard: Vec<_> = tasks
        .into_iter()
        .map(|(desc, id)| vec![InlineKeyboardButton::callback(desc, format!("task|{id}"))])
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
    formatter: F,
) -> InlineKeyboardMarkup
where
    F: Fn(&str) -> String,
{
    let mut keyboard: Vec<Vec<InlineKeyboardButton>> = vec![];
    for dir_cfg in mapping.chunks(3) {
        let row = dir_cfg
            .iter()
            .map(|dc| InlineKeyboardButton::callback(&dc.name, formatter(dc.path.as_str())))
            .collect();

        keyboard.push(row);
    }
    keyboard.push(vec![InlineKeyboardButton::callback(
        "Default",
        formatter(default_dir),
    )]);
    InlineKeyboardMarkup::new(keyboard)
}
