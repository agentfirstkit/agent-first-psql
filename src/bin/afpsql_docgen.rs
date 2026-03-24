use agent_first_psql::cli::AfdCli;
use clap::Parser;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

#[derive(Parser)]
#[command(name = "afpsql-docgen")]
#[command(about = "Generate Markdown reference docs from afpsql's clap definitions")]
struct Args {
    /// Preview whether the generated output matches the file on disk
    #[arg(long)]
    dry_run: bool,

    /// Output path, relative to the crate root by default
    #[arg(long, default_value = "docs/cli.md")]
    output: PathBuf,
}

fn main() -> Result<ExitCode, Box<dyn Error>> {
    let args = Args::parse();
    let output_path = resolve_output_path(&args.output);
    let markdown = render_reference_markdown();

    if args.dry_run {
        let existing = fs::read_to_string(&output_path).map_err(|e| {
            format!(
                "failed to read {} for --dry-run: {}",
                output_path.display(),
                e
            )
        })?;

        if existing == markdown {
            return Ok(ExitCode::SUCCESS);
        }

        return Err(format!(
            "{} is out of date; regenerate with `./scripts/generate-cli-doc.sh`",
            output_path.display()
        )
        .into());
    }

    if let Some(parent) = output_path.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(&output_path, markdown)?;
    Ok(ExitCode::SUCCESS)
}

fn resolve_output_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        return path.to_path_buf();
    }

    Path::new(env!("CARGO_MANIFEST_DIR")).join(path)
}

fn render_reference_markdown() -> String {
    let raw = clap_markdown::help_markdown::<AfdCli>();
    let body = strip_first_heading(&raw);

    format!(
        concat!(
            "<!-- Generated from src/cli.rs. Do not edit by hand. -->\n\n",
            "# afpsql CLI Reference\n\n",
            "> Generated from `src/cli.rs`.\n",
            "> Regenerate with `./scripts/generate-cli-doc.sh`.\n",
            "> See [reference.md](reference.md) for protocol fields and examples.\n\n",
            "{}\n"
        ),
        body.trim_start()
    )
}

fn strip_first_heading(raw: &str) -> String {
    let mut dropped = false;
    let mut body = Vec::new();

    for line in raw.lines() {
        if !dropped && line.trim_start().starts_with('#') {
            dropped = true;
            continue;
        }

        if !dropped && line.trim().is_empty() {
            continue;
        }

        body.push(line);
    }

    body.join("\n")
}
