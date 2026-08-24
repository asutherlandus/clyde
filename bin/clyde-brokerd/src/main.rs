//! `clyde-brokerd`: the Clyde credential broker.

use std::path::PathBuf;
use std::sync::Arc;

use clap::Parser;
use clyde_brokerd::{Broker, resolve_credential, service};
use clyde_policy::config::Config;
use clyde_store::{SqliteStore, Store};

#[derive(Debug, Parser)]
#[command(
    name = "clyde-brokerd",
    about = "Clyde credential broker",
    version,
    disable_help_subcommand = true
)]
struct Options {
    /// Socket to listen on. Defaults to the state directory's `run/brokerd.sock`.
    #[arg(long)]
    socket: Option<PathBuf>,

    /// State directory, for reading approval records.
    #[arg(long)]
    state_dir: Option<PathBuf>,

    /// Host configuration file.
    #[arg(long, env = "CLYDE_HOST_CONFIG")]
    config: Option<PathBuf>,

    #[arg(long, env = "CLYDE_LOG", default_value = "info")]
    log: String,

    /// Report capabilities and exit.
    #[arg(long)]
    check: bool,
}

fn main() -> std::process::ExitCode {
    let options = Options::parse();
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(&options.log))
        .with_target(false)
        .init();

    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("clyde-brokerd: could not start the async runtime: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(options)) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("clyde-brokerd: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}

async fn run(options: Options) -> Result<(), Box<dyn std::error::Error>> {
    let state_root = options.state_dir.clone().unwrap_or_else(default_state_root);
    let config = load_config(options.config.as_deref())?;
    let socket = options
        .socket
        .clone()
        .or_else(|| config.broker.socket.clone())
        .unwrap_or_else(|| state_root.join("run/brokerd.sock"));

    let store: Arc<dyn Store> = Arc::new(SqliteStore::open(state_root.join("db.sqlite"))?);
    let credential = resolve_credential(&config);
    let broker = Arc::new(Broker {
        config,
        credential,
        git: clyde_git::GitRunner::discover()?,
        store,
        scratch: state_root.join("broker-scratch"),
    });

    if options.check {
        println!("{}", serde_json::to_string_pretty(&broker.capabilities())?);
        return Ok(());
    }

    // Stated at start: a broker with no credential answers capability queries
    // and refuses pushes, which is the honest behaviour rather than failing at
    // the moment someone tries to publish.
    if !broker.credential.is_present() {
        tracing::warn!(
            "no credential is available; pushes will be refused. Set broker.ssh_key, or enable broker.allow_ssh_agent with SSH_AUTH_SOCK set"
        );
    } else {
        tracing::info!(
            kind = broker.credential.kind().unwrap_or("unknown"),
            "credential loaded; it is held in this process only and is never written or logged"
        );
    }

    let listener = service::bind(&socket).await?;
    tracing::info!(socket = %socket.display(), "clyde-brokerd is listening");

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let Ok((stream, _)) = accepted else { continue };
                let broker = Arc::clone(&broker);
                tokio::spawn(async move { service::serve(broker, stream).await });
            }
            _ = tokio::signal::ctrl_c() => {
                tracing::info!("clyde-brokerd is shutting down");
                let _ = std::fs::remove_file(&socket);
                return Ok(());
            }
        }
    }
}

fn default_state_root() -> PathBuf {
    std::env::var_os("CLYDE_STATE_DIR")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("HOME").map(|home| {
                PathBuf::from(home)
                    .join(".local")
                    .join("share")
                    .join("clyde")
            })
        })
        .unwrap_or_else(|| PathBuf::from("/var/lib/clyde"))
}

/// Loads host and user configuration.
///
/// The broker never reads repository configuration: `broker.*` is host or user
/// configuration only, and a repository that could set it would be choosing what
/// the credential-holding process does.
fn load_config(host: Option<&std::path::Path>) -> Result<Config, Box<dyn std::error::Error>> {
    let mut config = Config::defaults();
    let candidates = [host
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("/etc/clyde/config.toml"))];
    for path in candidates {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let (next, _) = clyde_policy::config::apply_layer(
            config,
            &text,
            clyde_policy::config::ConfigSource::Host,
        )?;
        config = next;
    }
    if let Some(home) = std::env::var_os("HOME") {
        let user = PathBuf::from(home).join(".config/clyde/config.toml");
        if let Ok(text) = std::fs::read_to_string(&user) {
            let (next, _) = clyde_policy::config::apply_layer(
                config,
                &text,
                clyde_policy::config::ConfigSource::User,
            )?;
            config = next;
        }
    }
    Ok(config)
}
