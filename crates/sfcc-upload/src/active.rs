use crate::logging;
use crate::ocapi::Ocapi;
use sfcc_core::config::Config;

pub enum Serving {
    Ours,
    Other(String),
    Nothing,
    /// No API client, or one without the Data API permissions: nothing to act on.
    Unknown(String),
}

pub async fn check(config: &Config) -> Serving {
    if config.api_client.is_none() {
        return Serving::Unknown("dw.json has no API client".to_string());
    }
    let active = match Ocapi::new(config) {
        Ok(ocapi) => ocapi.active_code_version().await,
        Err(error) => Err(error),
    };
    match active {
        Ok(Some(id)) if id == config.code_version => Serving::Ours,
        Ok(Some(id)) => Serving::Other(id),
        Ok(None) => Serving::Nothing,
        Err(error) => Serving::Unknown(format!("{error:#}")),
    }
}

pub fn describe(serving: &Serving) -> String {
    match serving {
        Serving::Ours => "yes".to_string(),
        Serving::Other(id) => format!("no - {id} is"),
        Serving::Nothing => "no - none is".to_string(),
        Serving::Unknown(reason) => format!("unknown: {reason}"),
    }
}

/// With "ensure-active" in dw.json, the synced code version is activated when it is not.
/// Never an error: without the permissions to check or to activate, it says so and goes on.
pub async fn ensure(config: &Config) {
    if !config.ensure_active {
        return;
    }
    let previous = match check(config).await {
        Serving::Ours => return,
        Serving::Unknown(reason) => {
            logging::info(format!(
                "ensure-active: which code version is active was not checked - {reason}"
            ));
            return;
        }
        Serving::Other(id) => id,
        Serving::Nothing => "none".to_string(),
    };

    let activated = match Ocapi::new(config) {
        Ok(ocapi) => ocapi.activate(&config.code_version).await,
        Err(error) => Err(error),
    };
    match activated {
        Ok(()) => logging::ok(format!(
            "{} activated - {previous} was the active code version",
            config.code_version
        )),
        Err(error) => logging::info(format!(
            "ensure-active: {} is not the active code version ({previous} is), and was not \
             activated - {error:#}",
            config.code_version
        )),
    }
}
