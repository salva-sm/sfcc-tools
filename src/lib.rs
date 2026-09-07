use serde::Deserialize;
use zed_extension_api::{
    self as zed, DebugAdapterBinary, DebugConfig, DebugRequest, DebugScenario, DebugTaskDefinition,
    Result, StartDebuggingRequestArguments, StartDebuggingRequestArgumentsRequest, Worktree,
};

const BINARY: &str = "b2c";
const CARTRIDGE_CANDIDATES: [&str; 2] = ["source/cartridges", "cartridges"];

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct B2cConfig {
    cartridge_path: Option<String>,
    config: Option<String>,
    instance: Option<String>,
    client_id: Option<String>,
}

struct B2cDebugExtension;

impl zed::Extension for B2cDebugExtension {
    fn new() -> Self {
        B2cDebugExtension
    }

    fn get_dap_binary(
        &mut self,
        _adapter_name: String,
        definition: DebugTaskDefinition,
        user_installed_path: Option<String>,
        worktree: &Worktree,
    ) -> Result<DebugAdapterBinary> {
        let settings: B2cConfig = serde_json::from_str(&definition.config)
            .map_err(|error| format!("cannot read the debug configuration: {error}"))?;

        let command = user_installed_path
            .or_else(|| worktree.which(BINARY))
            .ok_or_else(|| {
                format!(
                    "{BINARY} is not on PATH - install it with `npm i -g @salesforce/b2c-cli` \
                     or set the adapter path in your Zed settings"
                )
            })?;

        let root = worktree.root_path();
        let cartridges = cartridge_path(&settings, worktree)?;
        let config = match &settings.config {
            Some(configured) => absolute(configured, &root),
            None => format!("{}/dw.json", parent_of(&cartridges)),
        };

        let mut arguments = vec![
            "debug".to_string(),
            "--cartridge-path".to_string(),
            cartridges.clone(),
            "--config".to_string(),
            config,
        ];
        if let Some(instance) = settings.instance {
            arguments.push("--instance".to_string());
            arguments.push(instance);
        }
        if let Some(client_id) = settings.client_id {
            arguments.push("--client-id".to_string());
            arguments.push(client_id);
        }

        Ok(DebugAdapterBinary {
            command: Some(command),
            arguments,
            envs: worktree.shell_env(),
            cwd: Some(parent_of(&cartridges)),
            connection: None,
            request_args: StartDebuggingRequestArguments {
                configuration: definition.config,
                request: StartDebuggingRequestArgumentsRequest::Attach,
            },
        })
    }

    fn dap_request_kind(
        &mut self,
        _adapter_name: String,
        _config: serde_json::Value,
    ) -> Result<StartDebuggingRequestArgumentsRequest> {
        Ok(StartDebuggingRequestArgumentsRequest::Attach)
    }

    fn dap_config_to_scenario(&mut self, config: DebugConfig) -> Result<DebugScenario> {
        if let DebugRequest::Launch(_) = config.request {
            return Err(
                "the B2C script debugger attaches to a running instance; it cannot launch one"
                    .to_string(),
            );
        }

        Ok(DebugScenario {
            label: config.label,
            adapter: config.adapter,
            build: None,
            config: "{}".to_string(),
            tcp_connection: None,
        })
    }
}

fn cartridge_path(settings: &B2cConfig, worktree: &Worktree) -> Result<String> {
    let root = worktree.root_path();

    if let Some(configured) = &settings.cartridge_path {
        return Ok(absolute(configured, &root));
    }

    for candidate in CARTRIDGE_CANDIDATES {
        let marker = format!("{candidate}/modules/server/route.js");
        if worktree.read_text_file(&marker).is_ok() {
            return Ok(absolute(candidate, &root));
        }
    }

    Err(format!(
        "no cartridges directory found under {root} - set \"cartridge_path\" in the debug configuration"
    ))
}

fn parent_of(path: &str) -> String {
    match path.trim_end_matches('/').rsplit_once('/') {
        Some((parent, _)) => parent.to_string(),
        None => ".".to_string(),
    }
}

fn absolute(path: &str, root: &str) -> String {
    let looks_absolute = path.starts_with('/') || path.chars().nth(1) == Some(':');
    match looks_absolute {
        true => path.to_string(),
        false => format!("{root}/{path}"),
    }
}

zed::register_extension!(B2cDebugExtension);
