//! Reproducible query result packages, including reviewed template custody.
use super::*;
use flate2::{Compression, GzBuilder};

/// A deterministic tar+gzip: sorted members, fixed owner, mode and time, so equal files give equal bytes.
pub(super) fn package(
    task: &ClaimedTask,
    query: &TaskQuery,
    csv: &[u8],
    rows: usize,
    columns: &Value,
    server_version: &str,
    source_version: Option<&Value>,
) -> Result<Vec<u8>> {
    let parameters = json!({
        "method": "guarded read-only database query",
        "source": query.title,
        "release": query.release,
        "row_limit": query.row_limit,
        "guard": query.guard,
    });
    let sql = format!("{}\n", query.admitted_sql);
    let table_sha = hex::encode(Sha256::digest(csv));
    let mut receipt = json!({
        "schema": "enrichedlab.query-receipt.v1",
        "task_id": task.id,
        "source": query.source,
        "title": query.title,
        "release": query.release,
        "snapshot": query.snapshot,
        "guard": query.guard,
        "query_sha256": query.sql_sha256,
        "proposed_sql": query.proposed_sql,
        "row_limit": query.row_limit,
        "row_count": rows,
        "columns": columns,
        "table_sha256": table_sha,
        "server_version": server_version,
        "transaction": "repeatable read, read only",
        "null_in_csv": "an empty unquoted field; an empty text value is written as two quotes",
    });
    if let Some(version) = source_version {
        receipt["source_version"] = version.clone();
    }
    let readme = readme(query, rows, source_version.is_some());
    let mut members: Vec<(String, Vec<u8>)> = vec![
        ("README.md".into(), readme.into_bytes()),
        ("method/query.sql".into(), sql.into_bytes()),
        ("receipt.json".into(), serde_json::to_vec_pretty(&receipt)?),
        (TABLE_PATH.into(), csv.to_vec()),
        (
            "run_parameters.json".into(),
            serde_json::to_vec_pretty(&parameters)?,
        ),
    ];
    if let Some(record) = &query.reviewed_query {
        members.push((
            "method/template.sql".into(),
            record.template.as_bytes().to_vec(),
        ));
        members.push((
            "method/parameters.json".into(),
            serde_json::to_vec_pretty(&json!({"tool": record.tool, "version": record.version,
                "template_sha256": record.template_sha256, "schema_sha256": record.schema_sha256,
                "parameters": record.parameters, "requested": record.requested, "normalized": record.normalized,
                "row_cap": record.row_cap}))?,
        ));
    }
    members.sort_by(|a, b| a.0.cmp(&b.0));
    let mut sums = String::new();
    for (path, bytes) in &members {
        sums.push_str(&format!(
            "{}  {}\n",
            hex::encode(Sha256::digest(bytes)),
            path
        ));
    }
    members.push(("SHA256SUMS".into(), sums.into_bytes()));
    let mut gz = GzBuilder::new()
        .mtime(0)
        .write(Vec::new(), Compression::default());
    {
        let mut tar = tar::Builder::new(&mut gz);
        tar.mode(tar::HeaderMode::Deterministic);
        for (path, bytes) in &members {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_uid(0);
            header.set_gid(0);
            header.set_mtime(0);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            tar.append_data(&mut header, path, bytes.as_slice())?;
        }
        tar.finish()?;
    }
    Ok(gz.finish()?)
}

fn readme(query: &TaskQuery, rows: usize, live: bool) -> String {
    let rerun = if live {
        "This query read a live lab database, whose imports keep arriving. `receipt.json` names the version the database \
         stated in the same transaction (`source_version`). Run `method/query.sql` against the lab database with the \
         source's schema first on the search path: while no import since that version changed the rows it reads, the \
         table it returns has the same SHA-256 as the one here."
    } else {
        "Load the same snapshot (its files and checksums are named in `receipt.json`) into PostgreSQL, then run \
         `method/query.sql` with the snapshot's schema first on the search path. The table it returns has the same \
         SHA-256 as the one here when the snapshot matches."
    };
    let reviewed = if query.reviewed_query.is_some() {
        "\n- `method/template.sql` is the reviewed template.\n- `method/parameters.json` keeps its version, schema, digests and requested and normalized parameters.\nThe result still needs a person's validation.\n"
    } else {
        ""
    };
    format!(
        "# Database query\n\nThis package holds one read-only query of {title} ({release}) and its result: {rows} rows in \
         `{TABLE_PATH}`.\n\n- `method/query.sql` is the exact query that ran, in PostgreSQL's dialect.\n\
         - `receipt.json` names the snapshot or version it read, the check that admitted the query, the server version, \
         the columns with their types and the table's SHA-256.\n- `SHA256SUMS` lists every file's SHA-256.\n\n\
         {reviewed}The table is PostgreSQL's own CSV output: a header row, then one row per result row. A missing value (NULL) is \
         `NA`, unquoted; a text value that is `NA` is quoted, and in a one-column table every other value is quoted, so \
         no row is a blank line.\n\n## Check the files\n\n\
         macOS: `shasum -a 256 -c SHA256SUMS`  \nLinux: `sha256sum -c SHA256SUMS`\n\n## Rerun the query\n\n\
         {rerun}\n\n## Read the table in R\n\n\
         `read.csv(\"{TABLE_PATH}\")` reads every missing value as NA. It also reads the text `NA` as NA, quoted or not, \
         and it skips a line holding only `\"\"`, so an empty text value in a one-column table is not read back.\n",
        title = query.title,
        release = query.release,
    )
}
