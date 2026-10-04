//! `ifami` — the reference client for [`ifami_core`].
//!
//! This binary exists to prove the engine is usable and testable from outside
//! its own crate, and to give CI something that exercises a real end-to-end run.
//! It contains no logic of its own: every decision belongs in `ifami-core`, and
//! anything here that grows beyond argument parsing and formatting is in the
//! wrong crate.
//!
//! ## Commands
//!
//! ```text
//! ifami get <URL>...     queue one or more links and wait for them
//! ifami info <URL>       list the formats found at a link, download nothing
//! ifami list             show the queue
//! ifami pause <ID>...    stop running transfers at a resumable offset
//! ifami resume <ID>...   continue paused or failed transfers
//! ifami rm <ID>...       forget a task; --purge also deletes its files
//! ifami doctor           print versions, paths, and the scope boundary
//! ```

mod format;

use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::{Parser, Subcommand};
use ifami_core::download::manager::{ManagerOptions, DEFAULT_CONCURRENCY};
use ifami_core::download::{Manager, Store, TaskState};
use ifami_core::error::Result;
use ifami_core::net::ReqwestClient;

/// Exit codes a caller can branch on. Documented because a shell script should
/// not have to guess.
mod exit {
    /// The command succeeded.
    pub const OK: u8 = 0;
    /// The command failed for an ordinary reason.
    pub const FAILURE: u8 = 1;
    /// The user asked for something contradictory (bad id, bad state).
    pub const USAGE: u8 = 2;
}

/// A local, private media fetcher.
///
/// Links you share, files you keep. No account, no telemetry, no advertising.
#[derive(Debug, Parser)]
#[command(name = "ifami", version, about, long_about = None)]
struct Cli {
    /// Queue file to use. Defaults to a per-user path.
    #[arg(long, short, global = true, value_name = "PATH")]
    queue: Option<PathBuf>,

    /// Where new files are written.
    #[arg(long, short, global = true, value_name = "DIR")]
    dest: Option<PathBuf>,

    /// Filename template, e.g. "{uploader} - {title} [{quality}].{container}".
    #[arg(long, short = 'T', global = true, value_name = "TEMPLATE")]
    template: Option<String>,

    /// Transfers to run at once.
    #[arg(long, short = 'j', global = true, value_name = "N")]
    concurrency: Option<usize>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Queue one or more links and wait for them to finish.
    Get {
        /// URLs to fetch. A bare `example.com/x.mp4` is accepted.
        #[arg(required = true, value_name = "URL")]
        urls: Vec<String>,
    },

    /// List the formats found at a link, without downloading anything.
    Info {
        /// URL to inspect.
        #[arg(value_name = "URL")]
        url: String,
    },

    /// Show the queue.
    List,

    /// Stop running transfers. Each stops at its next flush and stays resumable.
    Pause {
        /// Task ids to pause. Omit to pause everything.
        #[arg(value_name = "ID")]
        ids: Vec<String>,
    },

    /// Continue paused or failed transfers.
    Resume {
        /// Task ids to resume. Omit to resume everything resumable.
        #[arg(value_name = "ID")]
        ids: Vec<String>,
    },

    /// Forget a task.
    Rm {
        /// Task ids to remove.
        #[arg(required = true, value_name = "ID")]
        ids: Vec<String>,

        /// Also delete the partial and output files.
        #[arg(long)]
        purge: bool,
    },

    /// Print versions, paths, and the scope boundary.
    Doctor,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    match run(cli).await {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("ifami: {e}");
            ExitCode::from(exit::FAILURE)
        }
    }
}

async fn run(cli: Cli) -> Result<u8> {
    if let Command::Doctor = cli.command {
        return Ok(doctor(&cli));
    }

    let client = Arc::new(ReqwestClient::new()?);
    let store = Store::new(cli.queue.clone().unwrap_or_else(default_queue_path));
    let manager = Manager::with_options(store, client, options(&cli))?;

    if let Some(note) = manager.recovery_note() {
        // Said out loud, because a queue that silently lost tasks is the kind of
        // thing a person only notices after they have lost something.
        eprintln!("ifami: warning: {note}");
    }

    match cli.command {
        Command::Get { urls } => get(&manager, &urls).await,
        Command::Info { url } => info(&manager, &url).await,
        Command::List => Ok(list(&manager)),
        Command::Pause { ids } => pause(&manager, &ids),
        Command::Resume { ids } => resume(&manager, &ids),
        Command::Rm { ids, purge } => remove(&manager, &ids, purge).await,
        Command::Doctor => Ok(doctor(&cli)),
    }
}

async fn get(manager: &Manager, urls: &[String]) -> Result<u8> {
    let mut queued = Vec::new();
    let mut failures = 0usize;

    for url in urls {
        match manager.add_url(url).await {
            Ok(task) => {
                println!("queued  {}  {}", task.id, task.output.display());
                queued.push(task.id);
            }
            Err(e) => {
                // One bad link must not abandon the rest of a batch.
                eprintln!("ifami: {url}: {e}");
                failures += 1;
            }
        }
    }

    if queued.is_empty() {
        return Ok(exit::FAILURE);
    }

    print!("{}", format::spinner_note(&queued.len()));
    manager.wait_idle().await;

    let mut bad = failures;
    for id in &queued {
        match manager.get(id) {
            Some(task) if task.state == TaskState::Completed => {
                println!("done    {id}  {}", task.output.display());
            }
            Some(task) => {
                println!("stopped {id}  {} ({:?})", task.output.display(), task.state);
                if let Some(reason) = &task.last_error {
                    println!("        {reason}");
                }
                bad += 1;
            }
            None => bad += 1,
        }
    }

    println!();
    Ok(if bad == 0 { exit::OK } else { exit::FAILURE })
}

async fn info(manager: &Manager, url: &str) -> Result<u8> {
    let media = manager.probe_url(url).await?;

    println!("title    {}", media.title);
    println!("source   {}", media.source_url);
    if let Some(seconds) = media.duration_secs {
        println!("duration {seconds}s");
    }
    println!("formats  {}", media.formats.len());

    for format in media.formats_by_quality() {
        let quality = format
            .quality
            .and_then(|q| q.height)
            .map(|h| format!("{h}p"))
            .unwrap_or_else(|| "-".to_string());
        println!(
            "  [{}] {:<8} {:<8} {:<10} {}",
            format.id,
            format.container.extension(),
            format.kind.kind_name(),
            quality,
            format.url
        );
        if let Some(total) = format.total_bytes {
            println!("       size {}", format::bytes(total));
        }
        if format.requires_mux {
            println!(
                "       note: video-only; needs to be muxed with an audio track before it will play"
            );
        }
        if let Some(segments) = &format.segments {
            println!("       segments {}", segments.len());
        }
    }

    println!();
    println!(
        "ifami will queue: {}",
        media
            .best_standalone()
            .map(|f| f.id.as_str())
            .unwrap_or("(nothing self-contained — see SCOPE.md)")
    );

    Ok(exit::OK)
}

fn list(manager: &Manager) -> u8 {
    let tasks = manager.tasks();
    if tasks.is_empty() {
        println!("queue is empty");
        return exit::OK;
    }

    println!("{:<10} {:<12} {:>9}  OUTPUT", "ID", "STATE", "SIZE");
    for task in &tasks {
        let size = task
            .total_bytes
            .map(format::bytes)
            .unwrap_or_else(|| "-".to_string());
        println!(
            "{:<10} {:<12} {:>9}  {}",
            task.id,
            task.state.state_name(),
            size,
            task.output.display()
        );
        if let Some(error) = &task.last_error {
            println!("           {error}");
        }
    }
    exit::OK
}

fn pause(manager: &Manager, ids: &[String]) -> Result<u8> {
    if ids.is_empty() {
        let count = manager.pause_all();
        println!("pausing {count} running transfer(s)");
        return Ok(exit::OK);
    }

    let mut bad = 0usize;
    for id in ids {
        match manager.pause(id) {
            Ok(()) => println!("pausing {id}"),
            Err(e) => {
                eprintln!("ifami: {e}");
                bad += 1;
            }
        }
    }
    Ok(if bad == 0 { exit::OK } else { exit::USAGE })
}

fn resume(manager: &Manager, ids: &[String]) -> Result<u8> {
    let mut bad = 0usize;
    let count = if ids.is_empty() {
        manager.resume_all()?
    } else {
        let mut n = 0;
        for id in ids {
            match manager.resume(id) {
                Ok(()) => {
                    println!("resuming {id}");
                    n += 1;
                }
                Err(e) => {
                    eprintln!("ifami: {e}");
                    bad += 1;
                }
            }
        }
        n
    };

    if ids.is_empty() {
        println!("resumed {count} task(s)");
    }
    Ok(if bad == 0 { exit::OK } else { exit::USAGE })
}

async fn remove(manager: &Manager, ids: &[String], purge: bool) -> Result<u8> {
    let mut bad = 0usize;
    for id in ids {
        match manager.remove(id, purge).await {
            Ok(Some(task)) => println!("removed {id}  {}", task.output.display()),
            Ok(None) => {
                eprintln!("ifami: no task with id {id:?}");
                bad += 1;
            }
            Err(e) => {
                eprintln!("ifami: {e}");
                bad += 1;
            }
        }
    }
    Ok(if bad == 0 { exit::OK } else { exit::USAGE })
}

fn doctor(cli: &Cli) -> u8 {
    println!("ifami {}", ifami_core::VERSION);
    println!("user-agent        {}", ifami_core::USER_AGENT);
    println!(
        "queue             {}",
        cli.queue
            .clone()
            .unwrap_or_else(default_queue_path)
            .display()
    );
    println!("destination       {}", options(cli).destination.display());
    println!("template          {}", options(cli).template);
    println!("concurrency       {}", options(cli).concurrency);
    println!("transport         reqwest");
    println!();
    println!("ifami downloads media you have the right to download, from links you");
    println!("choose. It has no accounts, sends no telemetry, and shows no advertising.");
    println!();
    println!("It will not: bypass a login, a paywall or a geo-block; decrypt DRM;");
    println!("impersonate a browser's TLS or HTTP/2 fingerprint; run page JavaScript;");
    println!("or bundle a third-party downloader. See docs/SCOPE.md for the reasoning.");
    exit::OK
}

fn options(cli: &Cli) -> ManagerOptions {
    let defaults = ManagerOptions {
        concurrency: DEFAULT_CONCURRENCY,
        ..ManagerOptions::default()
    };
    ManagerOptions {
        destination: cli.dest.clone().unwrap_or(defaults.destination),
        template: cli.template.clone().unwrap_or(defaults.template),
        concurrency: cli.concurrency.unwrap_or(defaults.concurrency).max(1),
    }
}

/// Per-user queue path.
///
/// `%LOCALAPPDATA%\ifami\queue.json` on Windows, `$XDG_STATE_HOME/ifami` or
/// `~/.local/state/ifami` elsewhere. Deliberately not in the Downloads folder,
/// which users sync and clean.
fn default_queue_path() -> PathBuf {
    let root = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .or_else(|| {
            std::env::var_os("XDG_STATE_HOME")
                .map(PathBuf::from)
                .or_else(|| {
                    std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("state"))
                })
        })
        .unwrap_or_else(std::env::temp_dir);

    root.join("ifami").join("queue.json")
}
