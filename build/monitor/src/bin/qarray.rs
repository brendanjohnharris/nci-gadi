use clap::Parser;
use monitor::pbs;

/// One-shot array-job progress: a bar per array job with completed/running
/// subjob counts and an ETA from the mean completed-subjob walltime.
#[derive(Parser)]
#[command(name = "qarray", about = "Progress bars for your PBS array jobs on Gadi")]
struct Cli {
    /// User whose array jobs to show (default: $USER)
    #[arg(short = 'u', long)]
    username: Option<String>,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let user = cli
        .username
        .or_else(|| std::env::var("USER").ok())
        .unwrap_or_default();
    let out = pbs::fetch_array_progress(&user)?;
    if out.trim().is_empty() {
        println!("No array jobs for {user}.");
    } else {
        println!("{out}");
    }
    Ok(())
}
