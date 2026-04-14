use crate::types::TraceEntry;
use anyhow::Result;
use std::io::Write;

/// Export trace entries as JSON.
pub fn export_json(steps: &[TraceEntry], mut writer: impl Write) -> Result<()> {
    let json = serde_json::to_string_pretty(steps)?;
    writer.write_all(json.as_bytes())?;
    Ok(())
}

/// Export trace entries as CSV.
pub fn export_csv(steps: &[TraceEntry], mut writer: impl Write) -> Result<()> {
    writeln!(
        writer,
        "step_id,session_id,step_number,action_type,description,verification,result,tokens,cost_usd,duration_ms,timestamp"
    )?;

    for step in steps {
        let action_type = step.action.action_type.as_str();
        let verification = match &step.verification.decision {
            crate::types::VerificationDecision::Pass => "pass",
            crate::types::VerificationDecision::Warn { .. } => "warn",
            crate::types::VerificationDecision::Fail { .. } => "fail",
        };
        let result = match &step.result {
            crate::types::StepResult::Success => "success",
            crate::types::StepResult::Failure { .. } => "failure",
            crate::types::StepResult::RolledBack { .. } => "rolled_back",
            crate::types::StepResult::Skipped { .. } => "skipped",
        };

        writeln!(
            writer,
            "{},{},{},{},{},{},{},{},{:.4},{},{}",
            csv_field(&step.step_id),
            csv_field(&step.session_id),
            step.step_number,
            csv_field(action_type),
            csv_field(&step.action.description),
            csv_field(verification),
            csv_field(result),
            step.tokens_used,
            step.cost_usd,
            step.duration_ms,
            csv_field(&step.timestamp.to_rfc3339()),
        )?;
    }

    Ok(())
}

fn csv_field(value: &str) -> String {
    let escaped = value.replace('"', "\"\"");
    format!("\"{escaped}\"")
}
