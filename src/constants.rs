//! Centralized constants for the telearia2 bot.
//!
//! This module contains all configurable constants that control
//! various aspects of the bot's behavior.

use std::time::Duration;

// ============================================================================
// Aria2 Operation Settings
// ============================================================================

/// Timeout for aria2 operations (add uri, add torrent, etc.)
pub const ARIA2_OP_TIMEOUT: Duration = Duration::from_secs(10);

/// Maximum number of retries for aria2 operations
pub const ARIA2_MAX_RETRIES: u32 = 3;

/// Delay between aria2 operation retries
pub const ARIA2_RETRY_DELAY: Duration = Duration::from_millis(100);

// ============================================================================
// Task Cache Settings
// ============================================================================

/// Default expiration time for task subscribers
pub const DEFAULT_SUBSCRIBER_EXPIRE: Duration = Duration::from_secs(3 * 60);

/// Timeout for refreshing task cache from aria2
pub const REFRESH_TIMEOUT: Duration = Duration::from_secs(10);

/// Interval between background task cache refreshes
pub const REFRESH_INTERVAL: Duration = Duration::from_secs(1);

/// Expiration time for cached task data
pub const CACHE_EXPIRE: Duration = Duration::from_secs(3);

/// Size of the LRU cache for URI and file mappings
pub const URI_LRU_SIZE: usize = 4096;

// ============================================================================
// Telegram/Download Settings
// ============================================================================

/// Maximum torrent file size (1 MiB)
pub const MAX_TORRENT_SIZE: u32 = 1024 * 1024;

/// Maximum length for brief task names in UI
pub const MAX_BRIEF_NAME_LEN: usize = 40;
