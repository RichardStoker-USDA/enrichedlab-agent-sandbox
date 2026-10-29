use anyhow::Result;
use clap::Parser;
use enrichedlab_agent_sandbox::broker::plan_publisher::{
    CustodyPlanKind, PlanPublicationConfig, publish,
};
use std::{path::PathBuf, str::FromStr};
use uuid::Uuid;

#[derive(Parser)]
#[command(about = "Publish one operator-approved same-name custodian plan")]
struct Args {
    #[arg(long)]
    source_root: PathBuf,
    #[arg(long)]
    source: String,
    #[arg(long)]
    source_writer_uid: u32,
    #[arg(long)]
    plan_root: PathBuf,
    #[arg(long)]
    plan_writer_uid: u32,
    #[arg(long)]
    kind: String,
    #[arg(long)]
    task_id: Uuid,
    #[arg(long)]
    attempt: i32,
    #[arg(long)]
    lease_generation: i64,
    #[arg(long)]
    expected_sha256: String,
}

fn main() {
    if let Err(error) = run(Args::parse()) {
        let code = error.to_string();
        if code.starts_with("plan_publisher_")
            && code
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
        {
            eprintln!("{code}");
        } else {
            eprintln!("plan_publisher_failed");
        }
        std::process::exit(1);
    }
}

fn run(args: Args) -> Result<()> {
    let receipt = publish(&PlanPublicationConfig {
        source_root: args.source_root,
        source_name: args.source,
        source_writer_uid: args.source_writer_uid,
        plan_root: args.plan_root,
        plan_writer_uid: args.plan_writer_uid,
        kind: CustodyPlanKind::from_str(&args.kind)?,
        task_id: args.task_id,
        attempt: args.attempt,
        lease_generation: args.lease_generation,
        expected_sha256: args.expected_sha256,
    })?;
    println!("{}", serde_json::to_string(&receipt)?);
    Ok(())
}
