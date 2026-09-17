use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "whyfault",
    version,
    about = "Explains why your program crashed, not just where."
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run a program under the tracer and write a .wft trace.
    Record {
        /// Output trace path.
        #[arg(short, long, default_value = "crash.wft")]
        output: String,
        /// Program and arguments.
        #[arg(required = true, trailing_var_arg = true)]
        argv: Vec<String>,
    },
    /// Explain a recorded crash.
    Explain {
        trace: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        verbose: bool,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.cmd {
        Cmd::Record { output, argv } => {
            anyhow::bail!("record not implemented yet (would trace {argv:?} into {output})")
        }
        Cmd::Explain { trace, .. } => {
            anyhow::bail!("explain not implemented yet (would read {trace})")
        }
    }
}
