mod progress;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};
use plc2sysml::{input, input::physical::LoadedContext, output, output::sysml, resolve};
use progress::Progress;
use std::{fs, path::PathBuf, process::ExitCode};

#[derive(Parser)]
#[command(
    version,
    about = "Enrich SysML v2 structure with signal dependencies and control sequences from CODESYS"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    #[command(about = "Enrich a structural model with signal paths and operating sequences")]
    Generate(GenerateArgs),

    #[command(about = "Create a draft engineering-context file")]
    ContextTemplate(ContextArgs),
}

#[derive(Args)]
struct GenerateArgs {
    #[arg(short, long)]
    input: PathBuf,

    #[arg(
        short,
        long,
        help = "Folder for generated SysML files; replaces previous output"
    )]
    output: PathBuf,

    #[arg(long, help = "JSON linking source signals to the structural model")]
    context: PathBuf,
}

#[derive(Args)]
struct ContextArgs {
    #[arg(short, long)]
    input: PathBuf,

    #[arg(short, long)]
    output: PathBuf,

    #[arg(long)]
    physical_model: PathBuf,
}

pub fn run() -> ExitCode {
    match execute(Cli::parse()) {
        Ok(()) => ExitCode::SUCCESS,

        Err(error) => {
            eprintln!("error: {error:#}");
            ExitCode::FAILURE
        }
    }
}

fn execute(cli: Cli) -> Result<()> {
    match cli.command {
        Command::Generate(args) => generate(args),
        Command::ContextTemplate(args) => context_template(args),
    }
}

fn context_template(args: ContextArgs) -> Result<()> {
    let progress = Progress::new(2);
    progress.show(0, "Reading source");
    let model = resolve::program(input::read(&args.input)?)?;

    progress.show(1, "Writing context draft");
    let physical = args
        .physical_model
        .canonicalize()
        .context("Cannot open physical model folder")?;
    let draft = input::physical::template(&model, physical);

    if let Some(parent) = args.output.parent() {
        fs::create_dir_all(parent)?;
    }

    let mut file = fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&args.output)
        .context("Cannot create context draft; choose a path that does not already exist")?;
    serde_json::to_writer_pretty(&mut file, &draft)?;
    drop(progress);

    println!(
        "Wrote context draft to {}. Replace TODO associations before generation.",
        args.output.display()
    );

    Ok(())
}

fn generate(args: GenerateArgs) -> Result<()> {
    output::check_no_overlap(&args.input, &args.output)?;

    let progress = Progress::new(7);
    progress.show(0, "Reading source");
    let parsed = input::read(&args.input)?;

    progress.show(1, "Resolving declarations and references");
    let source = resolve::program(parsed)?;

    progress.show(2, "Mapping physical signals");
    let context = LoadedContext::load(&args.context, &source)?;
    output::check_no_overlap(&context.physical_model, &args.output)?;

    progress.show(3, "Analysing dependencies and behaviour");
    let analysis = plc2sysml::analysis::analyze(&source, &context.boundaries());
    let errors = plc2sysml::analysis::validate(&source, &analysis);

    if !errors.is_empty() {
        anyhow::bail!("Invalid analysis: {}", errors.join("; "));
    }

    progress.show(4, "Building system model");
    let system_model = sysml::build(&source, &analysis, &context);

    progress.show(5, "Rendering SysML");
    let files = sysml::render(&system_model)?;

    progress.show(6, "Writing model");
    output::write(&files, &args.output)?;
    drop(progress);

    println!("Wrote SysML to {}", args.output.display());

    Ok(())
}
