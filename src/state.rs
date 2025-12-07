use crate::{
    aria2::Aria2Client,
    config::{Aria2ConfigGroup, DownloadConfig, Param, TelegramConfig},
    format::{
        make_single_task_keyboard, make_tasks_keyboard, MessageFmtBrief, MessageFmtDetailed,
        TaskExt,
    },
    utils::{ExpiredDeque, SingleMultiMap},
};
use aria2_rs::{
    status::{Status, TaskStatus},
    SmallVec,
};
use hashlink::LruCache;
use parking_lot::{Mutex, RwLock};
use smol_str::SmolStr;
use std::{collections::HashMap, sync::Arc};
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

/// A wrapper around `HashMap<SmolStr, Arc<Status>>` that implements `PartialEq`
/// by comparing only the fields relevant for UI display updates.
///
/// This avoids expensive deep equality checks on the full `Status` struct,
/// and only compares fields that affect what users see:
/// - status: task state (active/paused/complete/etc)
/// - completed_length/total_length: progress
/// - download_speed/upload_speed: transfer speeds
/// - connections/num_seeders: peer info
#[derive(Clone, Default)]
pub struct TasksMap(HashMap<SmolStr, Arc<Status>>);

impl TasksMap {
    pub fn new() -> Self {
        Self(HashMap::new())
    }

    pub fn get(&self, gid: &str) -> Option<&Arc<Status>> {
        self.0.get(gid)
    }

    pub fn values(&self) -> impl Iterator<Item = &Arc<Status>> {
        self.0.values()
    }
}

impl FromIterator<(SmolStr, Arc<Status>)> for TasksMap {
    fn from_iter<I: IntoIterator<Item = (SmolStr, Arc<Status>)>>(iter: I) -> Self {
        Self(iter.into_iter().collect())
    }
}

impl PartialEq for TasksMap {
    fn eq(&self, other: &Self) -> bool {
        if self.0.len() != other.0.len() {
            return false;
        }
        for (gid, old_task) in &self.0 {
            let Some(new_task) = other.0.get(gid) else {
                return false;
            };
            if old_task.status != new_task.status
                || old_task.completed_length != new_task.completed_length
                || old_task.total_length != new_task.total_length
                || old_task.download_speed != new_task.download_speed
                || old_task.upload_speed != new_task.upload_speed
                || old_task.connections != new_task.connections
                || old_task.num_seeders != new_task.num_seeders
            {
                return false;
            }
        }
        true
    }
}

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
    tasks: TasksMap,
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
            tasks: TasksMap::new(),
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

        if !self.subscribers.list_subscribers.is_empty() {
            let tasks = self.fmt_tasks();
            let keyboard = make_tasks_keyboard(tasks);
            for &list_sub in self.subscribers.list_subscribers.iter() {
                let bot = self.bot.clone();
                let keyboard = keyboard.clone();
                tokio::spawn(async move {
                    let mut rep =
                        bot.edit_message_reply_markup(list_sub.chat_id, list_sub.message_id);
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

        for (gid, subscribers) in self.subscribers.task_subscribers.iter() {
            if let Some((task_desc, task_status)) = self.fmt_task(gid) {
                let keyboard = make_single_task_keyboard(
                    gid,
                    task_status.status.unwrap_or(TaskStatus::Removed),
                );
                for &task_sub in subscribers.iter() {
                    let bot = self.bot.clone();
                    let text = task_desc.clone();
                    let keyboard = keyboard.clone();
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

    pub async fn refresh(this: &Arc<RwLock<Self>>, selected_client: &Aria2Client) {
        if !this.read().expired() {
            return;
        }
        if let Ok(Ok(tasks)) =
            tokio::time::timeout(REFRESH_TIMEOUT, selected_client.get_tasks()).await
        {
            let mut tasks_cache = this.write();
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

/// Single server state
pub struct ServerState {
    pub name: String,
    pub client: Aria2Client,
    pub tasks_cache: Arc<RwLock<TasksCache>>,
    pub download_config: DownloadConfig,
    _drop: tokio::sync::oneshot::Receiver<()>,
}

impl ServerState {
    pub async fn new(
        name: String,
        client: Aria2Client,
        tasks_cache: Arc<RwLock<TasksCache>>,
        download_config: DownloadConfig,
    ) -> anyhow::Result<Self> {
        let (mut drop_tx, _drop) = tokio::sync::oneshot::channel();
        let server_state = Self {
            name,
            client,
            tasks_cache,
            download_config,
            _drop,
        };

        // spawn background refresh loop
        {
            let client = server_state.client.clone();
            let tasks_cache = server_state.tasks_cache.clone();
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

        Ok(server_state)
    }
}

pub struct State {
    // user id -> {server name -> ServerState{Aria2Client, TasksCache, DownloadConfig}}
    pub server_group: HashMap<i64, SingleMultiMap<Arc<ServerState>>>,
    // user id -> ServerState{Aria2Client, TasksCache, DownloadConfig}
    pub server_selected: RwLock<HashMap<i64, Arc<ServerState>>>,

    // telearia2 internal cache
    pub uri_cache: Arc<Mutex<LruCache<String, SmallVec<String>>>>,
    pub file_cache: Arc<Mutex<LruCache<SmolStr, String>>>,
}

impl State {
    pub async fn new<C: Param<Aria2ConfigGroup> + Param<TelegramConfig> + Param<DownloadConfig>>(
        cfg: &C,
        bot: Bot,
    ) -> anyhow::Result<Self> {
        let telegram_config: TelegramConfig = cfg.param();
        let client_config_group: Aria2ConfigGroup = cfg.param();
        let default_download_config: DownloadConfig = cfg.param();

        let mut server_group_builder: HashMap<i64, HashMap<String, Arc<ServerState>>> =
            HashMap::new();
        for (name, client_config) in client_config_group.into_iter() {
            let client = Aria2Client::connect(&client_config).await?;
            let tasks_cache = Arc::new(RwLock::new(TasksCache::new(
                telegram_config
                    .subscribe_expire_secs
                    .map(std::time::Duration::from_secs)
                    .unwrap_or(DEFAULT_SUBSCRIBER_EXPIRE),
                bot.clone(),
            )));
            let download_config = client_config
                .download_override
                .clone()
                .unwrap_or_else(|| default_download_config.clone());
            let server_state = Arc::new(
                ServerState::new(
                    name.clone(),
                    client.clone(),
                    tasks_cache.clone(),
                    download_config,
                )
                .await?,
            );

            let admins = client_config
                .admins_override
                .as_ref()
                .unwrap_or(&telegram_config.admins);
            for admin in admins.iter() {
                server_group_builder
                    .entry(*admin)
                    .or_default()
                    .insert(name.clone(), server_state.clone());
            }
        }

        let server_group: HashMap<i64, SingleMultiMap<Arc<ServerState>>> = server_group_builder
            .into_iter()
            .filter_map(|(k, v)| SingleMultiMap::try_from(v).ok().map(|smm| (k, smm)))
            .collect();

        let mut server_selected = HashMap::new();
        for (&user, servers) in server_group.iter() {
            // If user only has one server, select it automatically
            if let Some(server) = servers.unwrap_single_ref() {
                server_selected.insert(user, server.clone());
            }
        }

        Ok(Self {
            server_group,
            server_selected: RwLock::new(server_selected),
            uri_cache: Arc::new(Mutex::new(LruCache::new(URI_LRU_SIZE))),
            file_cache: Arc::new(Mutex::new(LruCache::new(URI_LRU_SIZE))),
        })
    }

    #[inline]
    pub fn authorized(&self, user_id: i64) -> Option<impl Iterator<Item = &str>> {
        self.server_group
            .get(&user_id)
            .map(|servers| servers.iter().map(|(name, _)| name))
    }

    #[inline]
    pub fn selected(&self, user_id: i64) -> Option<Arc<ServerState>> {
        self.server_selected.read().get(&user_id).cloned()
    }

    #[inline]
    pub fn try_select(&self, user_id: i64, server: &str) -> SelectResult {
        if let Some(servers) = self.server_group.get(&user_id) {
            if servers.unwrap_single_ref().is_some() {
                // If user only has one server, no need to select
                return SelectResult::NoNeed;
            }
            if let Some(server) = servers.get(server) {
                self.server_selected.write().insert(user_id, server.clone());
                return SelectResult::Success;
            }
        }
        SelectResult::Failure
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectResult {
    Success,
    NoNeed,
    Failure,
}
