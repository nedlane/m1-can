//! `m1-can` CLI: export a repo's MoTeC `.m1dbc` CAN databases as standard
//! Vector `.dbc` files, verify committed exports, and inspect the bus topology
//! created by `DBC.<Name>.Init(<bus>)` calls.
use clap::{Parser, Subcommand};
use std::path::PathBuf;
use std::process::ExitCode;

#[derive(Parser)]
#[command(
    name = "m1-can",
    about = "Inspect MoTeC M1 CAN topology and export .m1dbc databases as Vector .dbc files",
    version
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Inspect DBC module bus bindings, messages, and repeated CAN identifiers.
    Inspect {
        /// Path to the project's `Project.m1prj`.
        #[arg(long)]
        project: PathBuf,
        /// Return only messages whose symbol path contains this text.
        #[arg(long)]
        filter: Option<String>,
        /// Maximum messages returned; zero means no limit.
        #[arg(long, default_value_t = 200)]
        limit: usize,
    },
    /// Export the repo's MoTeC `.m1dbc` CAN databases as standard Vector `.dbc`
    /// files.
    ///
    /// Takes no path: it is driven by the `[dbc]` section of the
    /// `m1-tools.toml` that governs the working directory, found by walking up,
    /// so it can be run from anywhere in the repo (and from a hook, which knows
    /// nothing about paths). That section names `src_dir` (where the `.m1dbc`
    /// sources live), `out_dir` (where the `.dbc` exports go), and one
    /// `[dbc.exports]` line per pair, `"source stem" = "output stem"` — see the
    /// README for the full block.
    ///
    /// The `.m1dbc` stays the source of truth; each `.dbc` is a generated export,
    /// committed so tools that cannot read MoTeC XML (Vector, SavvyCAN,
    /// python-can) see the same database. Output is deterministic — same source,
    /// same bytes — so a regenerated export is either identical or a real change.
    /// Every run verifies its own work: the messages and signals written must
    /// equal the convertible count in the source, and re-reading the written file
    /// with an independent DBC parser must reproduce those same counts.
    ///
    /// Exit codes: 0 = written, or with `--check` everything in sync (and also a
    /// repo with no `[dbc]` section — nothing to export is not a failure);
    /// 1 = a count or round-trip mismatch, or a committed export that is out of
    /// date; 2 = the config, a source, or a generated file could not be read or
    /// parsed. A failing file does not stop the run: every file is reported and
    /// the worst code is returned.
    Export {
        /// Verify the committed `.dbc` exports match their `.m1dbc` sources
        /// without writing anything: generate into a temporary directory and
        /// compare. Both sides are compared as decoded text, so line endings, a
        /// leading UTF-8 BOM and a committed export saved as Windows-1252 are
        /// not drift. Exit 1 if any export is out of date. This is what a
        /// pre-commit hook or CI job runs.
        #[arg(long)]
        check: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(&cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: &Cli) -> Result<ExitCode, Box<dyn std::error::Error>> {
    match &cli.command {
        Command::Inspect {
            project,
            filter,
            limit,
        } => {
            let outcome = m1_can::inspect(project, filter.as_deref(), *limit)?;
            println!("{}", serde_json::to_string_pretty(&outcome)?);
            Ok(ExitCode::SUCCESS)
        }
        Command::Export { check } => {
            // The working directory is the input: the `[dbc]` section is found by
            // walking up from it, so the verb works from anywhere in the repo.
            let cwd = std::env::current_dir()
                .map_err(|e| format!("cannot determine the working directory: {e}"))?;
            // 2 (a config/IO/parse failure) has to be returned, not raised: main's
            // catch-all maps every Err to 1, which is the "stale export" code.
            Ok(ExitCode::from(m1_can::export(&cwd, *check).code()))
        }
    }
}
