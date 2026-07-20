use clap::Parser;
use monitor::pbs;

/// One-shot per-job usage panel: the monitor TUI's "Job Usage" tab as a plain
/// command (summary line, then CPU/EFF/MEM/WALLTIME per running job).
#[derive(Parser)]
#[command(name = "qusage", about = "Per-job resource usage for your PBS jobs on Gadi")]
struct Cli {
    /// User to report on (default: $USER)
    #[arg(short = 'u', long)]
    username: Option<String>,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let user = cli
        .username
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_default();
    let jobs = pbs::fetch_user_jobs(&user)?;
    print!("{}", pbs::fetch_usage(&jobs)?);
    Ok(())
}
