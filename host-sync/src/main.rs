use anyhow::{Context, Result};
use chrono::Utc;
use clap::{Parser, Subcommand};
use frame_host_sync::{session, watch_calendar, Config, WatchControl};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};

#[derive(Parser)]
#[command(version, about = "Sync this computer's data to Family Frame")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// List serial ports. Close any console/deploy tools before syncing.
    Ports,
    /// Print the current machine's lock state without sending data.
    HostState,
    /// Configure and sync calendar sources. Other host capabilities can be added alongside this.
    Calendar {
        #[command(subcommand)]
        command: CalendarCommand,
    },
}

#[derive(Subcommand)]
enum CalendarCommand {
    /// Fetch and send one complete calendar replacement.
    Sync {
        #[arg(long)]
        config: PathBuf,
    },
    /// Keep syncing. Failed cycles retain the last successful frame snapshot.
    Watch {
        #[arg(long)]
        config: PathBuf,
    },
    /// Fetch and write wire JSON to stdout. Does not open a serial port.
    Preview {
        #[arg(long)]
        config: PathBuf,
    },
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::HostState => println!("{}", serde_json::to_string(&session::current())?),
        Command::Ports => {
            for port in serialport::available_ports()? {
                println!("{}\t{:?}", port.port_name, port.port_type);
            }
        }
        Command::Calendar { command } => {
            let (path, watch, preview) = match command {
                CalendarCommand::Sync { config } => (config, false, false),
                CalendarCommand::Watch { config } => (config, true, false),
                CalendarCommand::Preview { config } => (config, false, true),
            };
            let (control, commands) = mpsc::channel();
            let stopping = Arc::new(AtomicBool::new(false));
            let requested = Arc::clone(&stopping);
            ctrlc::set_handler(move || {
                if !requested.swap(true, Ordering::Relaxed) {
                    let _ = control.send(WatchControl::Stop);
                    eprintln!("Stopping; waiting for in-flight work and subprocess cleanup.");
                }
            })
            .context("could not install termination handler")?;
            let config = Config::load(&path)?;
            if watch {
                return watch_calendar(
                    config,
                    path.parent().unwrap_or(Path::new(".")).to_owned(),
                    commands,
                    |_| {},
                );
            }
            let connection = if preview {
                None
            } else {
                Some(
                    config
                        .connection
                        .as_ref()
                        .context("sync requires a connection in config")?,
                )
            };
            let base = path.parent().unwrap_or(Path::new("."));
            let snapshot = config.calendar.fetch(base, Utc::now());
            if stopping.load(Ordering::Relaxed) {
                return Ok(());
            }
            let snapshot = snapshot?;
            if let Some(connection) = connection {
                connection.send(&snapshot)?;
                let count: usize = snapshot.sources.iter().map(|s| s.events.len()).sum();
                eprintln!(
                    "{} Calendar stored on frame. {count} source events.",
                    Utc::now().to_rfc3339()
                );
            } else {
                let stdout = std::io::stdout();
                let mut output = stdout.lock();
                output.write_all(&snapshot.encode()?)?;
                writeln!(output)?;
            }
        }
    }
    Ok(())
}
