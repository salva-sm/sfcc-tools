use crate::logging;
use anyhow::{Result, bail};
pub use sfcc_core::webdav::*;
use std::time::{Duration, Instant};

pub trait Ready {
    /// Probe until the sandbox answers, creating the code version when it is
    /// missing. Gives up after `max_wait`, or never when it is `None`.
    async fn wait_until_ready(&self, max_wait: Option<Duration>) -> Result<()>;
}

impl Ready for Dav {
    async fn wait_until_ready(&self, max_wait: Option<Duration>) -> Result<()> {
        let started = Instant::now();
        let mut announced = false;
        loop {
            match self.availability().await {
                Availability::Ready => {
                    if announced {
                        logging::ok("sandbox is back online");
                    }
                    return Ok(());
                }
                Availability::MissingCodeVersion => {
                    self.mkcol(self.base_url()).await?;
                    return Ok(());
                }
                Availability::Unauthorized => {
                    bail!("the sandbox rejected the credentials from dw.json (HTTP 401/403)")
                }
                Availability::Unavailable(reason) => {
                    if let Some(limit) = max_wait
                        && started.elapsed() >= limit
                    {
                        bail!("sandbox unreachable after {}s: {reason}", limit.as_secs());
                    }
                    if !announced {
                        logging::warn(format!("sandbox unavailable ({reason}) - waiting"));
                        announced = true;
                    }
                    tokio::time::sleep(Duration::from_secs(5)).await;
                }
            }
        }
    }
}
