//! `clyde-forward`: the in-sandbox egress forwarder.
//!
//! Trusted code running in an untrusted network namespace. It listens on
//! `127.0.0.1:<port>` inside the sandbox and bridges each accepted connection to
//! the bind-mounted Unix socket, which is the only route out.
//!
//! It holds no policy. Compromising it grants nothing beyond the allowlist the
//! host-side proxy already enforces, and it exits rather than starting if the
//! socket is absent — because a forwarder with nothing to forward to would make
//! a sandbox look connected when it is not.

use std::path::PathBuf;

use clap::Parser;
use clyde_egress::forwarder::{ForwarderConfig, run};

#[derive(Debug, Parser)]
#[command(
    name = "clyde-forward",
    about = "In-sandbox egress forwarder",
    version,
    disable_help_subcommand = true
)]
struct Options {
    /// Loopback port to listen on inside the sandbox.
    #[arg(long, default_value_t = 8118)]
    port: u16,

    /// The bind-mounted proxy socket.
    #[arg(long, default_value = "/run/clyde/egress.sock")]
    socket: PathBuf,
}

fn main() -> std::process::ExitCode {
    let options = Options::parse();
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("clyde-forward: could not start the async runtime: {error}");
            return std::process::ExitCode::FAILURE;
        }
    };
    match runtime.block_on(run(ForwarderConfig {
        port: options.port,
        socket: options.socket,
    })) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("clyde-forward: {error}");
            std::process::ExitCode::FAILURE
        }
    }
}
