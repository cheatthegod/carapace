use anyhow::{Context, Result, bail};
use carapace_core::tracer::export::{export_csv, export_json};
use carapace_core::{
    ActionType, Anomaly, CarapaceConfig, CompositeVerifier, ExecutionContext, StepAction,
    StepResult, StepSummary, Storage, TraceEntry, Tracer, VerificationDecision,
    VerificationOutcome, Verifier, default_config_path, default_db_path, load_config,
    new_id, write_default_config,
};
use carapace_mcp::McpServer;
use chrono::Utc;
use clap::{Parser, Subcommand, ValueEnum};
use serde_json::json;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Instant;
use tokio::process::Command;

#[derive(Debug, Parser)]
#[command(name = "carapace", version, about = "Execution safety and audit wrapper for AI agents")]
struct Cli {
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    #[arg(long, global = true)]
    db_path: Option<PathBuf>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Subcommand)]
enum Commands {
    /// Write a default config file.
    Init {
        #[arg(long)]
        path: Option<PathBuf>,

        #[arg(long)]
        force: bool,
    },

    /// Wrap an agent command with verification and trace logging.
    Wrap {
        #[arg(long)]
        agent_name: Option<String>,

        #[arg(long)]
        cwd: Option<PathBuf>,

        #[arg(long)]
        plan: Option<String>,

        #[arg(long = "file")]
        target_files: Vec<PathBuf>,

        #[arg(required = true, num_args = 1.., trailing_var_arg = true)]
        command: Vec<String>,
    },

    /// Verify an action without executing it.
    Verify {
        action_type: CliActionType,
        description: String,

        #[arg(long)]
        session_id: Option<String>,

        #[arg(long)]
        step_number: Option<u32>,

        #[arg(long)]
        working_dir: Option<PathBuf>,

        #[arg(long)]
        agent_name: Option<String>,

        #[arg(long)]
        plan: Option<String>,

        #[arg(long)]
        tool_name: Option<String>,

        #[arg(long = "file")]
        target_files: Vec<PathBuf>,

        #[arg(long, default_value = "{}")]
        args_json: String,
    },

    /// Print an aggregate session summary.
    Summary {
        session_id: String,

        #[arg(long)]
        json: bool,
    },

    /// Export a recorded session trace.
    Trace {
        session_id: String,

        #[arg(long, value_enum, default_value_t = OutputFormat::Json)]
        format: OutputFormat,

        #[arg(long)]
        output: Option<PathBuf>,
    },

    /// Inspect or run the MCP endpoint.
    Mcp {
        #[command(subcommand)]
        command: McpCommands,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum CliActionType {
    Read,
    Write,
    Delete,
    Execute,
    ApiCall,
    Search,
}

impl From<CliActionType> for ActionType {
    fn from(value: CliActionType) -> Self {
        match value {
            CliActionType::Read => ActionType::Read,
            CliActionType::Write => ActionType::Write,
            CliActionType::Delete => ActionType::Delete,
            CliActionType::Execute => ActionType::Execute,
            CliActionType::ApiCall => ActionType::ApiCall,
            CliActionType::Search => ActionType::Search,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutputFormat {
    Json,
    Csv,
}

#[derive(Debug, Subcommand)]
enum McpCommands {
    /// Print the current MCP manifest.
    Manifest,

    /// Start the placeholder stdio MCP server.
    Serve,
}

struct Runtime {
    config: CarapaceConfig,
    config_path: PathBuf,
    db_path: PathBuf,
}

#[tokio::main]
async fn main() -> Result<()> {
    init_tracing();

    let cli = Cli::parse();

    match &cli.command {
        Commands::Init { path, force } => {
            let target = match path {
                Some(path) => path.clone(),
                None => default_config_path()?,
            };

            write_config_file(&target, *force)?;
        }
        Commands::Wrap {
            agent_name,
            cwd,
            plan,
            target_files,
            command,
        } => {
            let code = run_wrap(
                &cli,
                agent_name.clone(),
                cwd.clone(),
                plan.clone(),
                target_files.clone(),
                normalize_command(command)?,
            )
            .await?;
            std::process::exit(code);
        }
        Commands::Verify {
            action_type,
            description,
            session_id,
            step_number,
            working_dir,
            agent_name,
            plan,
            tool_name,
            target_files,
            args_json,
        } => {
            run_verify(
                &cli,
                (*action_type).into(),
                description.clone(),
                session_id.clone(),
                *step_number,
                working_dir.clone(),
                agent_name.clone(),
                plan.clone(),
                tool_name.clone(),
                target_files.clone(),
                args_json.clone(),
            )
            .await?;
        }
        Commands::Summary { session_id, json } => {
            run_summary(&cli, session_id, *json).await?;
        }
        Commands::Trace {
            session_id,
            format,
            output,
        } => {
            run_trace_export(&cli, session_id, *format, output.clone()).await?;
        }
        Commands::Mcp { command } => {
            run_mcp(&cli, command).await?;
        }
    }

    Ok(())
}

fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info".into());

    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .try_init();
}

fn write_config_file(path: &Path, force: bool) -> Result<()> {
    if path.exists() && !force {
        bail!(
            "Config already exists at {} (use --force to overwrite)",
            path.display()
        );
    }

    write_default_config(path)?;
    println!("Wrote default config to {}", path.display());
    Ok(())
}

fn load_runtime(cli: &Cli) -> Result<Runtime> {
    let config = load_config(cli.config.as_deref())?;
    let config_path = match &cli.config {
        Some(path) => path.clone(),
        None => default_config_path()?,
    };
    let db_path = match &cli.db_path {
        Some(path) => path.clone(),
        None => default_db_path()?,
    };

    Ok(Runtime {
        config,
        config_path,
        db_path,
    })
}

async fn open_storage(path: &Path) -> Result<Storage> {
    let db_path = path.to_string_lossy().into_owned();
    Storage::new(&db_path).await
}

fn normalize_command(command: &[String]) -> Result<String> {
    let parts = command
        .iter()
        .map(|part| part.trim())
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();

    if parts.is_empty() {
        bail!("No wrapped command provided");
    }

    if parts.len() == 1 {
        return Ok(parts[0].to_string());
    }

    Ok(parts
        .into_iter()
        .map(shell_escape)
        .collect::<Vec<_>>()
        .join(" "))
}

async fn run_wrap(
    cli: &Cli,
    agent_name: Option<String>,
    cwd: Option<PathBuf>,
    plan: Option<String>,
    target_files: Vec<PathBuf>,
    command_line: String,
) -> Result<i32> {
    let runtime = load_runtime(cli)?;
    let storage = open_storage(&runtime.db_path).await?;
    let tracer = Tracer::new(storage.clone(), runtime.config.trace.clone());

    let session_id = new_id();
    let working_dir = resolve_working_dir(cwd)?;
    let working_dir_display = working_dir.display().to_string();
    let agent_name = agent_name.or_else(|| first_token(&command_line));

    storage
        .create_session(&session_id, agent_name.as_deref(), &working_dir_display)
        .await?;

    let action = StepAction {
        action_type: ActionType::Execute,
        tool_name: Some("wrap".into()),
        arguments: json!({
            "command": command_line,
            "shell": "sh -lc",
            "config_path": runtime.config_path,
        }),
        target_files: target_files
            .iter()
            .map(|path| normalize_path(path))
            .collect(),
        description: format!("Execute wrapped command: {command_line}"),
    };

    let context = ExecutionContext {
        session_id: session_id.clone(),
        step_number: 1,
        working_dir: working_dir_display.clone(),
        agent_name: agent_name.clone(),
        plan,
        previous_steps: vec![],
    };

    let verification = verify_action(&runtime.config, &action, &context);
    print_verification(&verification);

    if verification.decision.is_fail() {
        let entry = TraceEntry {
            step_id: new_id(),
            session_id: session_id.clone(),
            step_number: 1,
            action,
            reason: Some("Verification blocked wrapped command".into()),
            verification,
            checkpoint_id: None,
            result: StepResult::Skipped {
                reason: "Blocked before execution".into(),
            },
            tokens_used: 0,
            cost_usd: 0.0,
            duration_ms: 0,
            timestamp: Utc::now(),
        };

        let anomalies = record_trace(&runtime.config, &storage, &tracer, entry).await?;
        storage
            .update_session_status(&session_id, "blocked")
            .await
            .ok();
        print_anomalies(&anomalies);
        println!("Session: {session_id}");
        return Ok(2);
    }

    let started = Instant::now();
    let status_result = Command::new("sh")
        .arg("-lc")
        .arg(&command_line)
        .current_dir(&working_dir)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await;

    let duration_ms = started.elapsed().as_millis() as u64;

    let result = match &status_result {
        Ok(status) if status.success() => StepResult::Success,
        Ok(status) => StepResult::Failure {
            error: format!("Command exited with status {}", render_exit_status(status.code())),
        },
        Err(err) => StepResult::Failure {
            error: format!("Failed to spawn wrapped command: {err}"),
        },
    };

    let entry = TraceEntry {
        step_id: new_id(),
        session_id: session_id.clone(),
        step_number: 1,
        action,
        reason: Some("Executed via carapace wrap".into()),
        verification,
        checkpoint_id: None,
        result,
        tokens_used: 0,
        cost_usd: 0.0,
        duration_ms,
        timestamp: Utc::now(),
    };

    let anomalies = record_trace(&runtime.config, &storage, &tracer, entry).await?;
    print_anomalies(&anomalies);

    match status_result {
        Ok(status) => {
            let final_status = if status.success() { "completed" } else { "failed" };
            storage.update_session_status(&session_id, final_status).await.ok();
            println!("Session: {session_id}");
            Ok(status.code().unwrap_or(1))
        }
        Err(err) => {
            storage.update_session_status(&session_id, "failed").await.ok();
            println!("Session: {session_id}");
            Err(err).context("Wrapped command failed to start")
        }
    }
}

async fn run_verify(
    cli: &Cli,
    action_type: ActionType,
    description: String,
    session_id: Option<String>,
    step_number: Option<u32>,
    working_dir: Option<PathBuf>,
    agent_name: Option<String>,
    plan: Option<String>,
    tool_name: Option<String>,
    target_files: Vec<PathBuf>,
    args_json: String,
) -> Result<()> {
    let runtime = load_runtime(cli)?;
    let storage = open_storage(&runtime.db_path).await?;

    let session_id = session_id.unwrap_or_else(new_id);
    let previous_steps = storage.get_previous_summaries(&session_id, 25).await?;

    let step_number = step_number.unwrap_or(previous_steps.len() as u32 + 1);
    let working_dir = match working_dir {
        Some(path) => path,
        None => std::env::current_dir().context("Failed to determine current working directory")?,
    };

    let action = StepAction {
        action_type,
        tool_name,
        arguments: parse_args_json(&args_json)?,
        target_files: target_files
            .iter()
            .map(|path| normalize_path(path))
            .collect(),
        description,
    };

    let context = ExecutionContext {
        session_id,
        step_number,
        working_dir: working_dir.display().to_string(),
        agent_name,
        plan,
        previous_steps,
    };

    let verification = verify_action(&runtime.config, &action, &context);
    print_verification(&verification);
    Ok(())
}

async fn run_summary(cli: &Cli, session_id: &str, as_json: bool) -> Result<()> {
    let runtime = load_runtime(cli)?;
    let storage = open_storage(&runtime.db_path).await?;
    let summary = storage.get_session_summary(session_id).await?;

    if as_json {
        println!("{}", serde_json::to_string_pretty(&summary)?);
        return Ok(());
    }

    println!("Session: {}", summary.session_id);
    println!("Steps: {}", summary.total_steps);
    println!("Successful: {}", summary.successful_steps);
    println!("Failed: {}", summary.failed_steps);
    println!("Rollbacks: {}", summary.rollbacks);
    println!("Verifier interceptions: {}", summary.verifier_interceptions);
    println!("Anomalies: {}", summary.anomalies_detected);
    println!("Tokens: {}", summary.total_tokens);
    println!("Cost (USD): {:.4}", summary.total_cost_usd);
    println!("Duration (ms): {}", summary.total_duration_ms);
    Ok(())
}

async fn run_trace_export(
    cli: &Cli,
    session_id: &str,
    format: OutputFormat,
    output: Option<PathBuf>,
) -> Result<()> {
    let runtime = load_runtime(cli)?;
    let storage = open_storage(&runtime.db_path).await?;
    let trace = storage.get_session_steps(session_id).await?;

    match output {
        Some(path) => {
            let mut file = std::fs::File::create(&path)
                .with_context(|| format!("Failed to create {}", path.display()))?;
            write_trace(&trace, format, &mut file)?;
            println!("Exported {} trace entries to {}", trace.len(), path.display());
        }
        None => {
            let stdout = io::stdout();
            let mut handle = stdout.lock();
            write_trace(&trace, format, &mut handle)?;
            handle.flush()?;
        }
    }

    Ok(())
}

async fn run_mcp(cli: &Cli, command: &McpCommands) -> Result<()> {
    let runtime = load_runtime(cli)?;
    let server = McpServer::new(runtime.config, &runtime.db_path).await?;

    match command {
        McpCommands::Manifest => {
            println!("{}", server.manifest_json()?);
        }
        McpCommands::Serve => {
            server.serve_stdio().await?;
        }
    }

    Ok(())
}

fn write_trace(
    trace: &[TraceEntry],
    format: OutputFormat,
    writer: &mut dyn Write,
) -> Result<()> {
    match format {
        OutputFormat::Json => export_json(trace, writer),
        OutputFormat::Csv => export_csv(trace, writer),
    }
}

fn verify_action(
    config: &CarapaceConfig,
    action: &StepAction,
    context: &ExecutionContext,
) -> VerificationOutcome {
    if !config.verification.enabled {
        return VerificationOutcome {
            decision: VerificationDecision::Pass,
            checks_performed: vec![],
            duration_ms: 0,
        };
    }

    let verifier = CompositeVerifier::new(config.verification.clone());
    verifier.verify(action, context)
}

async fn record_trace(
    config: &CarapaceConfig,
    storage: &Storage,
    tracer: &Tracer,
    entry: TraceEntry,
) -> Result<Vec<Anomaly>> {
    if config.trace.enabled {
        return tracer.record_step(entry).await;
    }

    storage.insert_step(&entry).await?;
    Ok(vec![])
}

fn print_verification(outcome: &VerificationOutcome) {
    match &outcome.decision {
        VerificationDecision::Pass => println!("Verification: pass"),
        VerificationDecision::Warn { reasons } => {
            println!("Verification: warn");
            for reason in reasons {
                println!("- {reason}");
            }
        }
        VerificationDecision::Fail { reasons, suggestions } => {
            println!("Verification: fail");
            for reason in reasons {
                println!("- {reason}");
            }
            for suggestion in suggestions {
                println!("  suggestion: {suggestion}");
            }
        }
    }
}

fn print_anomalies(anomalies: &[Anomaly]) {
    if anomalies.is_empty() {
        return;
    }

    println!("Anomalies:");
    for anomaly in anomalies {
        println!(
            "- {:?} ({:?}): {}",
            anomaly.anomaly_type, anomaly.severity, anomaly.detail
        );
    }
}

fn parse_args_json(input: &str) -> Result<serde_json::Value> {
    serde_json::from_str(input).with_context(|| "Expected --args-json to be valid JSON")
}

fn resolve_working_dir(path: Option<PathBuf>) -> Result<PathBuf> {
    match path {
        Some(path) => Ok(path),
        None => std::env::current_dir().context("Failed to determine current working directory"),
    }
}

fn first_token(command: &str) -> Option<String> {
    command
        .split_whitespace()
        .next()
        .map(std::string::ToString::to_string)
}

fn normalize_path(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn render_exit_status(code: Option<i32>) -> String {
    match code {
        Some(code) => code.to_string(),
        None => "terminated by signal".into(),
    }
}

fn shell_escape(value: &str) -> String {
    if value.is_empty() {
        return "''".into();
    }

    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.' | '/' | ':'))
    {
        return value.to_string();
    }

    let escaped = value.replace('\'', "'\"'\"'");
    format!("'{escaped}'")
}

#[allow(dead_code)]
fn summarize_steps(trace: &[TraceEntry]) -> Vec<StepSummary> {
    trace.iter().map(StepSummary::from).collect()
}
