//! Throwaway: what the instance answers, before deciding anything.
use anyhow::{Context, Result, bail};
use reqwest::Method;
use sfcc_core::{config::Config, ocapi::Ocapi};

const USAGE: &str = "sites | dav <path> | data <path> | shop <site> <path>";
const DEFAULT_BODY_LIMIT: usize = 6000;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let config = Config::load(args.first().map(Into::into), None)?;
    println!("host {} ({:?})", config.hostname, config.instance());

    let mode = args.get(1).map(String::as_str).unwrap_or("sites");
    let positional = |index: usize| args.get(index).cloned().context(USAGE);

    if mode == "dav" {
        let dav = sfcc_core::webdav::Dav::new(&config)?;
        let url = format!(
            "https://{}/on/demandware.servlet/webdav/Sites/{}",
            config.hostname,
            positional(2)?
        );
        for entry in dav.list(&url).await? {
            let slash = if entry.is_dir { "/" } else { "" };
            println!("{}{slash}\t{}\t{}", entry.name, entry.size, entry.modified);
        }
        return Ok(());
    }

    let api = Ocapi::new(&config)?;
    let response = match mode {
        "sites" => {
            api.data(Method::GET, "sites?select=(**)&count=200")
                .await?
                .send()
                .await?
        }
        "data" => api.data(Method::GET, &positional(2)?).await?.send().await?,
        "shop" => api.shop(&positional(2)?, &positional(3)?).send().await?,
        _ => bail!(USAGE),
    };

    println!("HTTP {}", response.status());
    let body = response.text().await?;
    let limit = std::env::var("LIMIT")
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(DEFAULT_BODY_LIMIT);
    println!("{}", &body[..body.len().min(limit)]);
    Ok(())
}
