//! An answer document's package: the file, the exact request body the broker's writer read and the writer's receipt,
//! each answer table as CSV, a spec naming every value's cell and every source's digests, a README and `SHA256SUMS`, as
//! one deterministic gzip-compressed tar like a figure version's. The cited tables and figures are named by digest, not
//! copied: they are in their own packages.
use flate2::{Compression, GzBuilder};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::{Composed, TaskDocument};
use crate::broker::model::ClaimedTask;

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub(super) fn package(
    task: &ClaimedTask,
    request: &TaskDocument,
    composed: &Composed,
    file: &[u8],
    receipt: &Value,
) -> anyhow::Result<Vec<u8>> {
    let version = receipt["writer"]["version"].as_str().unwrap_or("");
    let document = format!("document.{}", request.kind);
    let spec = json!({
        "schema": "enrichedlab.document-spec.v1",
        "task_id": task.id,
        "kind": request.kind,
        "project": request.project,
        "run": request.run,
        "turn": request.turn,
        "decision_record_sha256": request.record_sha256,
        "request_sha256": request.request_sha256,
        "request": {"path": "request.json", "sha256": sha(&composed.body)},
        "file": {"path": document, "sha256": sha(file), "size_bytes": file.len()},
        "writer": receipt["writer"],
        "counts": composed.counts,
        "sources": request.sources.iter().map(|s| json!({"ref": s.reference, "table_path": s.table_path,
            "table_sha256": s.table_sha256, "rows": s.rows, "package_sha256": s.package_sha256})).collect::<Vec<_>>(),
        "figures": request.figures.iter().map(|f| json!({"ref": f.reference, "path": f.path, "sha256": f.sha256,
            "package_sha256": f.package_sha256})).collect::<Vec<_>>(),
        "findings": composed.findings,
        "values": composed.values,
        "notes": composed.notes,
        "acceptance": "not accepted by a person",
    });
    let mut members: Vec<(String, Vec<u8>)> = vec![
        (document.clone(), file.to_vec()),
        ("request.json".into(), composed.body.clone()),
        ("receipt.json".into(), serde_json::to_vec_pretty(receipt)?),
        ("spec.json".into(), serde_json::to_vec_pretty(&spec)?),
        (
            "README.md".into(),
            readme(request, &document, version).into_bytes(),
        ),
    ];
    members.extend(composed.csv.iter().cloned());
    members.sort_by(|a, b| a.0.cmp(&b.0));
    let mut sums = String::new();
    for (path, bytes) in &members {
        sums.push_str(&format!("{}  {path}\n", sha(bytes)));
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

fn readme(request: &TaskDocument, document: &str, version: &str) -> String {
    let kind = match request.kind.as_str() {
        "xlsx" => "an Excel workbook",
        "docx" => "a Word document",
        _ => "PowerPoint slides",
    };
    format!(
        "# Answer document\n\nThis package holds {kind} (`{document}`) of the answer research run `{run}` reached at \
         turn {turn}. Code wrote it from the stored decision and the result tables it cites; no model wrote any of it, \
         and the EnrichedLab broker's document writer ({version}) made the file from `request.json` alone, with no \
         template and no other program. Each figure is the cited PNG, byte for byte. No person has accepted this \
         answer yet.\n\n\
         - `{document}` is the document.\n- `request.json` is the exact request body the writer read, and \
         `receipt.json` the writer's receipt with the request's and the file's SHA-256.\n- `tables/` holds each answer \
         table with every row, as CSV; a cell that begins with =, +, -, @ or a tab and is not a number starts with an \
         apostrophe there so a spreadsheet opens it as text, and the cited result package holds the stored text \
         (`spec.json` notes say how many cells were marked and which tables are too large to copy).\n- `spec.json` \
         names every value a finding states with the table, row and column it was copied from, each finding whole, \
         and every cited result by SHA-256.\n- \
         `SHA256SUMS` lists every file's SHA-256.\n\n## Check the files\n\nmacOS: `shasum -a 256 -c SHA256SUMS`  \n\
         Linux: `sha256sum -c SHA256SUMS`\n\n## Make it again\n\nThe writer is deterministic: the same broker \
         release writes the same bytes. With that release's image, \
         `agent-sandbox-broker write-document < request.json > {document}` writes a file whose SHA-256 is the one in \
         `receipt.json`.\n",
        run = request.run,
        turn = request.turn,
    )
}
