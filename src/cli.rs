use std::io;
use std::path::PathBuf;

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use clap_complete::{Shell, generate};

use crate::engine::{Engine, ProviderFilter};
use crate::model::{API_VERSION, IndexEnvelope, ListEnvelope, ResultRecord, SearchEnvelope};
use crate::normalize::now;
use crate::paths::AppPaths;

#[derive(Debug, Parser)]
#[command(name = "dredge", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Incrementally update the local search index.
    Index {
        /// Reparse every discovered transcript.
        #[arg(long)]
        force: bool,
        /// Emit the stable versioned JSON object.
        #[arg(long)]
        json: bool,
    },
    /// Search session titles, prompts, and conversation text.
    Search {
        /// Full-text query. Quote multiple words in your shell.
        query: String,
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long, value_enum)]
        provider: Option<ProviderArg>,
        #[arg(long)]
        within: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = RefreshArg::Auto)]
        refresh: RefreshArg,
        #[arg(long)]
        json: bool,
    },
    /// List indexed sessions, newest first.
    List {
        #[arg(long, default_value_t = 20)]
        limit: usize,
        #[arg(long, value_enum)]
        provider: Option<ProviderArg>,
        #[arg(long)]
        within: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = RefreshArg::Auto)]
        refresh: RefreshArg,
        #[arg(long)]
        json: bool,
    },
    /// Show local index statistics.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Generate shell completion definitions.
    Completions { shell: Shell },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum ProviderArg {
    Claude,
    Codex,
}

impl From<ProviderArg> for ProviderFilter {
    fn from(provider: ProviderArg) -> Self {
        match provider {
            ProviderArg::Claude => Self::Claude,
            ProviderArg::Codex => Self::Codex,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, ValueEnum)]
enum RefreshArg {
    #[default]
    Auto,
    Always,
    Never,
}

pub fn run() -> Result<()> {
    let cli = Cli::parse();
    if let Command::Completions { shell } = cli.command {
        generate(shell, &mut Cli::command(), "dredge", &mut io::stdout());
        return Ok(());
    }

    let engine = Engine::open(AppPaths::discover()?)?;
    match cli.command {
        Command::Index { force, json } => {
            let outcome = engine.refresh(force)?;
            let envelope = IndexEnvelope {
                api_version: API_VERSION,
                generated_at: now(),
                index: outcome.stats,
            };
            if json {
                print_json(&envelope)?;
            } else {
                println!(
                    "Indexed {} sessions ({} changed, {} removed, {} failed)\n{}",
                    envelope.index.sessions,
                    envelope.index.changed,
                    envelope.index.removed,
                    envelope.index.failed,
                    envelope.index.path
                );
            }
        }
        Command::Search {
            query,
            limit,
            provider,
            within,
            refresh,
            json,
        } => {
            let refreshed = refresh_index(&engine, refresh)?;
            let results = engine.search(
                &query,
                limit,
                provider.map(ProviderFilter::from),
                within.as_deref(),
            )?;
            let envelope = SearchEnvelope {
                api_version: API_VERSION,
                generated_at: now(),
                query,
                index_refreshed: refreshed,
                results,
            };
            if json {
                print_json(&envelope)?;
            } else {
                print_human_results(&envelope.results, true);
            }
        }
        Command::List {
            limit,
            provider,
            within,
            refresh,
            json,
        } => {
            let refreshed = refresh_index(&engine, refresh)?;
            let results =
                engine.list(limit, provider.map(ProviderFilter::from), within.as_deref())?;
            let envelope = ListEnvelope {
                api_version: API_VERSION,
                generated_at: now(),
                index_refreshed: refreshed,
                results,
            };
            if json {
                print_json(&envelope)?;
            } else {
                print_human_results(&envelope.results, false);
            }
        }
        Command::Status { json } => {
            let envelope = IndexEnvelope {
                api_version: API_VERSION,
                generated_at: now(),
                index: engine.status()?,
            };
            if json {
                print_json(&envelope)?;
            } else {
                println!(
                    "{} sessions, {} transcript bytes\n{}",
                    envelope.index.sessions, envelope.index.transcript_bytes, envelope.index.path
                );
            }
        }
        Command::Completions { .. } => unreachable!(),
    }
    Ok(())
}

fn refresh_index(engine: &Engine, refresh: RefreshArg) -> Result<bool> {
    match refresh {
        RefreshArg::Never => Ok(false),
        RefreshArg::Auto => Ok(engine.refresh(false)?.refreshed),
        RefreshArg::Always => Ok(engine.refresh(true)?.refreshed),
    }
}

fn print_json(value: &impl serde::Serialize) -> Result<()> {
    serde_json::to_writer(io::stdout().lock(), value)?;
    println!();
    Ok(())
}

fn print_human_results(results: &[ResultRecord], scores: bool) {
    for result in results {
        let heading = result
            .title
            .as_deref()
            .or(result.first_prompt.as_deref())
            .unwrap_or("(untitled)");
        if scores {
            println!(
                "{:.4}  {}  {}  {}",
                result.score.unwrap_or(0.0),
                result.provider,
                result.session_id,
                heading
            );
            if let Some(snippet) = &result.snippet {
                println!("        {snippet}");
            }
        } else {
            println!(
                "{}  {}  {}  {}",
                result.updated_at, result.provider, result.session_id, heading
            );
        }
    }
}
