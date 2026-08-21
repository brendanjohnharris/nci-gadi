use clap::Parser;
use monitor::pbs;

/// One-shot per-job usage panel: the monitor TUI's "Job Usage" tab as a plain
/// command (summary, running table with CPU/EFF/MEM/JOBFS/WALLTIME, queued
/// diagnosis, node states, queue pressure).
#[derive(Parser)]
#[command(name = "qusage", about = "Per-job resource usage for your PBS jobs on Gadi")]
struct Cli {
    /// User to report on (default: $USER)
    #[arg(short = 'u', long)]
    username: Option<String>,
    /// Also show the SU + storage report (runs nci_account; adds a few seconds)
    #[arg(short = 'a', long)]
    account: bool,
    /// Also show recently finished jobs with exit status
    #[arg(short = 'r', long)]
    recent: bool,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let user = cli
        .username
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_default();

    if cli.account {
        let project = std::env::var("PROJECT").unwrap_or_default();
        match pbs::fetch_account(&project) {
            Ok(Some(text)) => println!("{text}"),
            Ok(None) => {}
            Err(e) => eprintln!("nci_account: {e}"),
        }
    }

    let jobs = pbs::fetch_user_jobs(&user)?;
    print!("{}", pbs::fetch_usage(&jobs)?);

    if cli.recent {
        if let Some(text) = pbs::fetch_recent(&user)? {
            println!("\n\x1b[1mRECENT JOBS\x1b[0m\n{text}");
        }
    }
    Ok(())
}
