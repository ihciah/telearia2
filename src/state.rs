use crate::{
    aria2::Aria2Client,
    config::{Aria2Config, DownloadConfig, Param, TelegramConfig},
    format::{
        make_single_task_keyboard, make_tasks_keyboard, MessageFmtBrief, MessageFmtDetailed,
        TaskExt,
    },
    utils::ExpiredDeque,
};
use aria2_rs::{
    status::{Status, TaskStatus},
    SmallVec,
};
use hashlink::LruCache;
use parking_lot::{Mutex, RwLock};
use smol_str::SmolStr;
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use teloxide::{
    requests::Requester,
    types::{ChatId, MessageId},
    Bot,
};

const DEFAULT_SUBSCRIBER_EXPIRE: std::time::Duration = std::time::Duration::from_secs(3 * 60);
const REFRESH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
const REFRESH_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);
const CACHE_EXPIRE: std::time::Duration = std::time::Duration::from_secs(3);
const URI_LRU_SIZE: usize = 1024;

#[derive(Debug, Clone)]
pub struct Subscribers {
    list_subscribers: ExpiredDeque<Subscriber>,
    task_subscribers: HashMap<SmolStr, ExpiredDeque<Subscriber>>,
}

impl Subscribers {
    pub fn new(expire: std::time::Duration) -> Self {
        Self {
            list_subscribers: ExpiredDeque::new(expire),
            task_subscribers: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Subscriber {
    chat_id: ChatId,
    message_id: MessageId,
}

pub struct TasksCache {
    // GID -> Status
    tasks: HashMap<SmolStr, Arc<aria2_rs::status::Status>>,
    // Last refresh time
    last_refresh: std::time::Instant,
    // subscribers
    subscribers: Subscribers,
    // telegram bot
    bot: Bot,
}

impl TasksCache {
    pub fn new(expire: std::time::Duration, bot: Bot) -> Self {
        Self {
            tasks: HashMap::new(),
            last_refresh: std::time::Instant::now(),
            subscribers: Subscribers::new(expire),
            bot,
        }
    }

    pub fn fmt_tasks(&self) -> Vec<(String, String)> {
        let mut tasks: Vec<_> = self
            .tasks
            .values()
            .cloned()
            .map(|t| ((t.progress() * 10000.0) as u16, t))
            .collect();
        tasks.sort_unstable_by(|(x_prog, x), (y_prog, y)| {
            // Order by status
            if let (Some(xs), Some(ys)) = (&x.status, &y.status) {
                if xs != ys {
                    return xs.cmp(ys);
                }
            }
            // Order by progress
            if x_prog != y_prog {
                return x_prog.cmp(y_prog);
            }
            // Order by name
            x.name().cmp(y.name())
        });

        tasks
            .into_iter()
            .map(|(_, t)| {
                (
                    format!("{}", MessageFmtBrief(&t)),
                    t.gid.as_deref().unwrap_or("unknown-gid").to_string(),
                )
            })
            .collect()
    }

    pub fn fmt_task(&self, gid: &str) -> Option<(String, &Arc<Status>)> {
        self.tasks
            .get(gid)
            .map(|t| (format!("{}", MessageFmtDetailed(t)), t))
    }

    pub fn expired(&self) -> bool {
        self.last_refresh.elapsed() > CACHE_EXPIRE
    }

    pub fn add_list_subscriber(&mut self, chat_id: ChatId, message_id: MessageId) {
        self.subscribers.list_subscribers.push_back(Subscriber {
            chat_id,
            message_id,
        });
    }

    pub fn add_task_subscriber(&mut self, gid: SmolStr, chat_id: ChatId, message_id: MessageId) {
        let subscribers = self
            .subscribers
            .task_subscribers
            .entry(gid)
            .or_insert_with(|| ExpiredDeque::new(DEFAULT_SUBSCRIBER_EXPIRE));
        subscribers.push_back(Subscriber {
            chat_id,
            message_id,
        });
    }

    pub fn has_subscriber(&self) -> bool {
        !self.subscribers.list_subscribers.is_empty()
            || !self.subscribers.task_subscribers.is_empty()
    }

    pub fn notify_subscribers(&mut self) {
        self.subscribers.list_subscribers.clean();
        self.subscribers.task_subscribers.retain(|_, v| {
            v.clean();
            !v.is_empty()
        });

        for &list_sub in self.subscribers.list_subscribers.iter() {
            let tasks = self.fmt_tasks();
            let keyboard = make_tasks_keyboard(tasks);
            let bot = self.bot.clone();
            tokio::spawn(async move {
                let mut rep = bot.edit_message_reply_markup(list_sub.chat_id, list_sub.message_id);
                rep.reply_markup = Some(keyboard);
                if let Err(e) = rep.await {
                    if !matches!(
                        e,
                        teloxide::RequestError::Api(teloxide::ApiError::MessageNotModified)
                    ) {
                        tracing::warn!("Failed to edit message: {e}");
                    }
                }
            });
        }

        for (gid, subscribers) in self.subscribers.task_subscribers.iter() {
            let task_desc = self.fmt_task(gid);
            if let Some((task_desc, task_status)) = task_desc {
                for &task_sub in subscribers.iter() {
                    let bot = self.bot.clone();
                    let text = task_desc.clone();
                    let keyboard = make_single_task_keyboard(
                        gid,
                        task_status.status.unwrap_or(TaskStatus::Removed),
                    );
                    tokio::spawn(async move {
                        let mut rep =
                            bot.edit_message_text(task_sub.chat_id, task_sub.message_id, text);
                        rep.reply_markup = Some(keyboard);
                        if let Err(e) = rep.await {
                            if !matches!(
                                e,
                                teloxide::RequestError::Api(teloxide::ApiError::MessageNotModified)
                            ) {
                                tracing::warn!("Failed to edit message: {e}");
                            }
                        }
                    });
                }
            }
        }
    }
}

pub struct State {
    admins: HashSet<i64>,
    pub tasks_cache: Arc<RwLock<TasksCache>>,
    pub uri_cache: Arc<Mutex<LruCache<String, SmallVec<String>>>>,
    pub file_cache: Arc<Mutex<LruCache<SmolStr, String>>>,
    pub client: Aria2Client,
    pub download_config: DownloadConfig,
    _drop: tokio::sync::oneshot::Receiver<()>,
}

impl State {
    pub async fn new<C: Param<Aria2Config> + Param<TelegramConfig> + Param<DownloadConfig>>(
        cfg: &C,
        bot: Bot,
    ) -> anyhow::Result<Self> {
        let telegram_cfg: TelegramConfig = cfg.param();
        let client = Aria2Client::connect(cfg).await?;
        let tasks_cache = Arc::new(RwLock::new(TasksCache::new(
            telegram_cfg
                .subscribe_expire_secs
                .map(std::time::Duration::from_secs)
                .unwrap_or(DEFAULT_SUBSCRIBER_EXPIRE),
            bot,
        )));

        // Spawn background refresh loop
        let (mut drop_tx, _drop) = tokio::sync::oneshot::channel();
        {
            let client = client.clone();
            let tasks_cache = tasks_cache.clone();
            tokio::spawn(async move {
                tokio::pin! {
                    let drop = drop_tx.closed();
                }
                loop {
                    tokio::select! {
                        _ = &mut drop => {
                            break;
                        }
                        _ = tokio::time::sleep(REFRESH_INTERVAL) => {
                            // Skip refresh when no subscriber
                            if !tasks_cache.read().has_subscriber() {
                                continue;
                            }

                            if let Ok(Ok(tasks)) = tokio::time::timeout(REFRESH_TIMEOUT, client.get_tasks()).await {
                                let tasks = tasks.into_iter().filter_map(|t| {
                                    if let Some(gid) = &t.gid {
                                        Some((gid.clone(), Arc::new(t)))
                                    } else {
                                        None
                                    }
                                }).collect();

                                let mut tasks_cache = tasks_cache.write();
                                // Skip notify when nothing changes
                                if tasks_cache.tasks == tasks {
                                    tasks_cache.last_refresh = std::time::Instant::now();
                                    continue;
                                }
                                tasks_cache.tasks = tasks;
                                tasks_cache.last_refresh = std::time::Instant::now();
                                tasks_cache.notify_subscribers();
                            }
                        }
                    }
                }
            });
        }

        Ok(Self {
            admins: telegram_cfg.admins.iter().cloned().collect(),
            tasks_cache,
            uri_cache: Arc::new(Mutex::new(LruCache::new(URI_LRU_SIZE))),
            file_cache: Arc::new(Mutex::new(LruCache::new(URI_LRU_SIZE))),
            client,
            download_config: cfg.param(),
            _drop,
        })
    }

    #[inline]
    pub fn auth(&self, user_id: i64) -> bool {
        self.admins.contains(&user_id)
    }

    pub async fn refresh(&self) {
        if !self.tasks_cache.read().expired() {
            return;
        }
        if let Ok(Ok(tasks)) = tokio::time::timeout(REFRESH_TIMEOUT, self.client.get_tasks()).await
        {
            let mut tasks_cache = self.tasks_cache.write();
            tasks_cache.tasks = tasks
                .into_iter()
                .filter_map(|t| {
                    if let Some(gid) = &t.gid {
                        Some((gid.clone(), Arc::new(t)))
                    } else {
                        None
                    }
                })
                .collect();
            tasks_cache.last_refresh = std::time::Instant::now();
        }
    }
}
