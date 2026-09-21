use anyhow::{Context, Result};
use futures::{SinkExt, StreamExt};
use std::time::Duration;
use tokio_tungstenite::connect_async;
use tokio_tungstenite::tungstenite::Message;

const RELOAD_COMMAND: &str = r#"{"id":1,"method":"Page.reload","params":{"ignoreCache":false}}"#;
const STEP_TIMEOUT: Duration = Duration::from_secs(5);
const BROWSER_EXTENSIONS: [&str; 3] = [".isml", ".css", ".js"];

pub struct Browser {
    client: reqwest::Client,
    port: u16,
    host: String,
}

impl Browser {
    pub fn new(port: u16, host: String) -> Result<Browser> {
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(5))
            .build()
            .context("cannot build the DevTools client")?;
        Ok(Browser { client, port, host })
    }

    pub async fn reload_storefront(&self) -> Result<usize> {
        let mut reloaded = 0;
        for socket in self.storefront_tabs().await? {
            match self.send_reload(&socket).await {
                Ok(()) => reloaded += 1,
                Err(error) => crate::logging::warn(format!("could not reload a tab: {error:#}")),
            }
        }
        Ok(reloaded)
    }

    async fn storefront_tabs(&self) -> Result<Vec<String>> {
        let url = format!("http://127.0.0.1:{}/json/list", self.port);
        let body = self
            .client
            .get(&url)
            .send()
            .await
            .with_context(|| {
                format!(
                    "no DevTools endpoint on port {} - start Chrome with --remote-debugging-port={}",
                    self.port, self.port
                )
            })?
            .text()
            .await
            .context("cannot read the DevTools tab list")?;

        let targets: serde_json::Value =
            serde_json::from_str(&body).context("malformed DevTools tab list")?;

        Ok(targets
            .as_array()
            .map(|tabs| {
                tabs.iter()
                    .filter(|tab| tab["type"] == "page")
                    .filter(|tab| tab["url"].as_str().is_some_and(|url| url.contains(&self.host)))
                    .filter_map(|tab| tab["webSocketDebuggerUrl"].as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default())
    }

    async fn send_reload(&self, socket: &str) -> Result<()> {
        let (mut stream, _) = tokio::time::timeout(STEP_TIMEOUT, connect_async(socket))
            .await
            .context("timed out opening the DevTools socket")??;

        stream
            .send(Message::Text(RELOAD_COMMAND.into()))
            .await
            .context("cannot send Page.reload")?;
        let _ = tokio::time::timeout(STEP_TIMEOUT, stream.next()).await;
        let _ = stream.close(None).await;
        Ok(())
    }
}

pub fn worth_reloading(paths: &[String]) -> bool {
    paths.iter().any(|path| {
        let lowered = path.to_lowercase();
        BROWSER_EXTENSIONS.iter().any(|extension| lowered.ends_with(extension))
    })
}
