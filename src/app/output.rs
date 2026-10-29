//! Printing records and actions, and small naming helpers.
use super::*;

pub(super) fn random_name() -> String {
    let mut bytes = [0_u8; 6];
    getrandom::fill(&mut bytes).expect("operating system randomness unavailable");
    format!("agent-{}", hex::encode(bytes))
}

pub(super) fn find_live_status<'a>(live: &'a Value, name: &str) -> Option<&'a str> {
    live.as_array()?
        .iter()
        .find(|item| item.get("name").and_then(Value::as_str) == Some(name))
        .and_then(|item| {
            item.get("status")
                .or_else(|| item.get("state"))
                .or_else(|| item.get("phase"))
                .and_then(Value::as_str)
        })
}

pub(super) fn print_records(output: OutputFormat, records: &[SandboxRecord]) {
    match output {
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(records).expect("record serialization")
        ),
        OutputFormat::Human => {
            for record in records {
                println!(
                    "Created {} [{}] -> {}",
                    record.name,
                    record.route,
                    record.host_workspace.display()
                );
            }
        }
    }
}

pub(super) fn print_action(
    output: OutputFormat,
    action: &str,
    record: &SandboxRecord,
) -> Result<()> {
    match output {
        OutputFormat::Human => println!("{}: {}", capitalize(action), record.name),
        OutputFormat::Json => print_json(&json!({"action": action, "name": record.name}))?,
    }
    Ok(())
}

pub(super) fn print_policy_action(
    output: OutputFormat,
    action: &str,
    record: &SandboxRecord,
    host: &str,
    write: bool,
) -> Result<()> {
    match output {
        OutputFormat::Human => println!(
            "{} {} for {} ({})",
            capitalize(action),
            host,
            record.name,
            if write { "read-write" } else { "read-only" }
        ),
        OutputFormat::Json => print_json(
            &json!({"action": action, "name": record.name, "host": host, "write": write}),
        )?,
    }
    Ok(())
}

pub(super) fn print_value(output: OutputFormat, value: &impl Serialize, label: &str) -> Result<()> {
    match output {
        OutputFormat::Json => print_json(value),
        OutputFormat::Human => {
            println!("{label}");
            println!("{}", serde_json::to_string_pretty(value)?);
            Ok(())
        }
    }
}

pub(super) fn print_text(output: OutputFormat, value: &str, label: &str) -> Result<()> {
    match output {
        OutputFormat::Human => {
            print!("{value}");
            Ok(())
        }
        OutputFormat::Json => print_json(&json!({"result": label, "output": value})),
    }
}

pub(super) fn print_json(value: &impl Serialize) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

pub(super) fn capitalize(value: &str) -> String {
    let mut chars = value.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}
