use std::{future::Future, time::Duration};

use anyhow::Result;
use aria2_rs::{
    call::{TellActiveCall, TellStoppedCall, TellWaitingCall},
    status::Status,
    BatchClient, ConnectionMeta,
};
use smol_str::SmolStr;

use crate::config::{Aria2Config, Param};

pub struct AddUrisResult {
    pub gids: Vec<SmolStr>,
    pub error: Option<anyhow::Error>,
}

const MAX_RETRIES: u32 = 3;
const RETRY_DELAY: Duration = Duration::from_millis(100);

async fn retry_call<T, F, Fut>(op_name: &str, f: F) -> Result<T>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Result<T, aria2_rs::Error>>,
{
    let mut last_err = None;
    for attempt in 0..MAX_RETRIES {
        match f().await {
            Ok(result) => return Ok(result),
            Err(e) => {
                tracing::warn!("{} attempt {}/{} failed: {}", op_name, attempt + 1, MAX_RETRIES, e);
                last_err = Some(e);
                if attempt + 1 < MAX_RETRIES {
                    tokio::time::sleep(RETRY_DELAY).await;
                }
            }
        }
    }
    Err(last_err.expect("MAX_RETRIES must be > 0").into())
}

#[derive(Clone)]
pub struct Aria2Client {
    cli: BatchClient,
}

impl Aria2Client {
    const DEFAULT_CHANNEL_BUFFER_SIZE: usize = 100;
    const DEFAULT_INTERVAL: Duration = Duration::from_secs(1);

    pub async fn connect<C: Param<Aria2Config>>(cfg: &C) -> Result<Self> {
        let aria_config = cfg.param();

        let conn_meta = ConnectionMeta {
            url: aria_config.rpc_url,
            token: Some(aria_config.token),
        };
        let cli = BatchClient::connect(
            conn_meta,
            aria_config
                .channel_buffer_size
                .unwrap_or(Self::DEFAULT_CHANNEL_BUFFER_SIZE),
            aria_config
                .interval_secs
                .map(Duration::from_secs)
                .unwrap_or(Self::DEFAULT_INTERVAL),
        )
        .await?;
        Ok(Self { cli })
    }

    pub async fn get_tasks(&self) -> Result<Vec<Status>> {
        let (mut active, waiting, stopped) = tokio::try_join!(
            self.cli.call(TellActiveCall::default()),
            self.cli.call(TellWaitingCall {
                offset: 0,
                num: 1000,
                keys: Default::default(),
            }),
            self.cli.call(TellStoppedCall {
                offset: 0,
                num: 1000,
                keys: Default::default(),
            })
        )?;

        active.extend(waiting.into_iter());
        active.extend(stopped.into_iter());

        Ok(active)
    }

    pub async fn pause(&self, gid: &str) -> Result<()> {
        self.cli
            .call_instantly(&aria2_rs::call::PauseCall { gid: gid.into() })
            .await?;
        Ok(())
    }

    pub async fn resume(&self, gid: &str) -> Result<()> {
        self.cli
            .call_instantly(&aria2_rs::call::UnpauseCall { gid: gid.into() })
            .await?;
        Ok(())
    }

    pub async fn remove(&self, gid: &str) -> Result<()> {
        self.cli
            .call_instantly(&aria2_rs::call::RemoveCall { gid: gid.into() })
            .await?;
        Ok(())
    }

    pub async fn add_uris(&self, links: &[String], dir: Option<SmolStr>) -> AddUrisResult {
        let options = dir.map(|dir| aria2_rs::options::TaskOptions {
            dir: Some(dir),
            ..Default::default()
        });
        let mut gids = Vec::with_capacity(links.len());
        for link in links.iter() {
            let link_vec: aria2_rs::SmallVec<String> = [link.to_string()].as_slice().into();
            let call = aria2_rs::call::AddUriCall {
                uris: link_vec,
                options: options.clone(),
            };
            match retry_call("add_uris", || self.cli.call_instantly(&call)).await {
                Ok(gid) => gids.push(gid.0),
                Err(e) => return AddUrisResult { gids, error: Some(e) },
            }
        }
        AddUrisResult { gids, error: None }
    }

    pub async fn add_torrent(&self, torrent_data: &[u8], dir: Option<SmolStr>) -> Result<SmolStr> {
        let options = dir.map(|dir| aria2_rs::options::TaskOptions {
            dir: Some(dir),
            ..Default::default()
        });
        let call = aria2_rs::call::AddTorrentCall {
            torrent: torrent_data.into(),
            uris: Default::default(),
            options,
        };
        let gid = retry_call("add_torrent", || self.cli.call_instantly(&call)).await?;
        Ok(gid.0)
    }

    pub async fn purge_downloaded(&self) -> Result<()> {
        self.cli
            .call_instantly(&aria2_rs::call::PurgeDownloadResultCall)
            .await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    #[ignore] // requires real aria2 server
    async fn it_works() {
        use crate::aria2::Aria2Client;
        use crate::config::Aria2Config;

        let cfg = Aria2Config {
            rpc_url: "wss://x.ihc.im:4430/jsonrpc".to_string(),
            token: "token:ARIA2@MARESERENITATIS".to_string(),
            channel_buffer_size: None,
            interval_secs: None,
            admins_override: None,
            download_override: None,
        };
        let cli = Aria2Client::connect(&cfg).await.unwrap();
        let tasks = cli.get_tasks().await.unwrap();
        dbg!(tasks);
    }
}
