//! Owner-only registration. Runtime admission remains the control even for rows written by an owner another way.
//! `render` runs the same checks against a source declaration file and prints the body `register` would send, for a
//! site whose migration owner is reached only from inside the database cell (the operator passes the body to
//! `broker_register_reviewed_query` as that owner).
use super::*;
use anyhow::{Result, anyhow, bail};
use std::{
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, clap::Args)]
pub struct Args {
    #[command(subcommand)]
    pub command: Command,
}
#[derive(Clone, Debug, clap::Subcommand)]
pub enum Command {
    Register(Register),
    Render(Render),
}
#[derive(Clone, Debug, clap::Args)]
pub struct Register {
    #[arg(long)]
    pub tenant: String,
    #[arg(long)]
    pub file: PathBuf,
    #[arg(long)]
    pub template: PathBuf,
    #[arg(long)]
    pub migration_dsn_file: Option<PathBuf>,
    #[arg(long, default_value = "AGENT_SANDBOX_MIGRATION_DSN")]
    pub migration_dsn_env: String,
}
#[derive(Clone, Debug, clap::Args)]
pub struct Render {
    #[arg(long)]
    pub file: PathBuf,
    #[arg(long)]
    pub template: PathBuf,
    /// The source's declaration as stored: `schema_name`, `relations` (objects with a `name`), `functions` and
    /// `row_cap`, the columns `register` reads from `broker_sql_sources`.
    #[arg(long)]
    pub source_file: PathBuf,
}
fn read(path: &Path, max: usize) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(max as u64 + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() > max {
        bail!("reviewed_query_definition_invalid");
    }
    Ok(bytes)
}

fn load(file: &Path, template: &Path) -> Result<Tool> {
    let mut tool: Tool = serde_json::from_slice(&read(file, 65536)?)
        .map_err(|_| anyhow!("reviewed_query_definition_invalid"))?;
    tool.template = String::from_utf8(read(template, sql_guard::MAX_SQL_BYTES)?)
        .map_err(|_| anyhow!("reviewed_query_definition_invalid"))?;
    tool.template_sha256 = digest(tool.template.as_bytes());
    Ok(tool)
}

fn relation_names(relations: &Value) -> BTreeSet<String> {
    relations
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|v| v["name"].as_str().map(str::to_owned))
        .collect()
}

/// The body `register` sends, checked against a declaration file instead of the database row.
pub fn render(args: &Render) -> Result<Value> {
    let tool = load(&args.file, &args.template)?;
    let source: Value = serde_json::from_slice(&read(&args.source_file, 4 << 20)?)
        .map_err(|_| anyhow!("reviewed_query_source_unavailable"))?;
    let schema = source["schema_name"]
        .as_str()
        .ok_or_else(|| anyhow!("reviewed_query_source_unavailable"))?;
    let functions = source["functions"]
        .as_array()
        .ok_or_else(|| anyhow!("reviewed_query_source_unavailable"))?
        .iter()
        .map(|v| v.as_str().map(str::to_owned))
        .collect::<Option<BTreeSet<_>>>()
        .ok_or_else(|| anyhow!("reviewed_query_source_unavailable"))?;
    let row_cap = source["row_cap"]
        .as_u64()
        .ok_or_else(|| anyhow!("reviewed_query_source_unavailable"))?;
    tool.registration_check(&Allowed {
        schema,
        relations: &relation_names(&source["relations"]),
        functions: &functions,
        row_cap,
    })?;
    Ok(serde_json::to_value(&tool)?)
}

pub async fn run(args: Args) -> Result<()> {
    let args = match args.command {
        Command::Register(args) => args,
        Command::Render(args) => {
            println!("{}", render(&args)?);
            return Ok(());
        }
    };
    crate::broker::config::validate_tenant(&args.tenant)?;
    let tool = load(&args.file, &args.template)?;
    let dsn =
        crate::broker::config::migration_dsn(&args.migration_dsn_file, &args.migration_dsn_env)?;
    let pool = crate::broker::store::build_pool(&dsn, 1)?;
    let client = pool
        .get()
        .await
        .map_err(|_| anyhow!("reviewed_query_database_unavailable"))?;
    // The runtime identities have neither SELECT on the registry nor EXECUTE on registration.
    let row = client.query_opt("SELECT schema_name,relations,functions,row_cap FROM enrichedlab_agent.broker_sql_sources WHERE tenant_id=$1 AND source_key=$2", &[&args.tenant, &tool.source])
        .await.map_err(|_| anyhow!("reviewed_query_source_unavailable"))?
        .ok_or_else(|| anyhow!("reviewed_query_source_unavailable"))?;
    let relations = relation_names(&row.get::<_, Value>(1));
    let functions = row.get::<_, Vec<String>>(2).into_iter().collect();
    tool.registration_check(&Allowed {
        schema: &row.get::<_, String>(0),
        relations: &relations,
        functions: &functions,
        row_cap: row.get::<_, i32>(3) as u64,
    })?;
    let body = serde_json::to_value(&tool)?;
    client
        .query_one(
            "SELECT enrichedlab_agent.broker_register_reviewed_query($1,$2)",
            &[&args.tenant, &body],
        )
        .await
        .map_err(|error| {
            let code = error.as_db_error().map(|e| e.message());
            anyhow!(if code == Some("broker:idempotency_conflict") {
                "idempotency_conflict"
            } else {
                "reviewed_query_registration_refused"
            })
        })?;
    println!(
        "{}",
        json!({"tool":tool.key,"version":tool.version,"template_sha256":tool.template_sha256})
    );
    Ok(())
}
