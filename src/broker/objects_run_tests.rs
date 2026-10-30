use super::*;

fn task() -> ClaimedTask {
    ClaimedTask {
        id: uuid::Uuid::nil(),
        principal_id: uuid::Uuid::nil(),
        group_key: "lab".into(),
        profile_key: "lab-objects-example".into(),
        objective: "o".into(),
        logical_agent: "a".into(),
        lease_generation: 1,
        attempt: 1,
        max_task_attempts: 1,
        max_provider_attempts: 1,
        harness: "pi".into(),
        thinking: None,
        route: ROUTE.into(),
        controller_profile: "disabled-legacy".into(),
        gateway_profile: "disabled-legacy".into(),
        image_ref: "worker@sha256:x".into(),
        cpu_limit: "1".into(),
        memory_limit: "1Gi".into(),
        state_profile: "s".into(),
        role: "r".into(),
        max_run_seconds: 300,
    }
}

fn row(rel: &str, bytes: &[u8]) -> String {
    json!({"key": format!("afri/counts/processed/{rel}"), "version_id": "v1", "size": bytes.len(),
        "sha256": hex::encode(Sha256::digest(bytes)), "sha1": "b".repeat(40), "box_id": "1", "study": "afri",
        "kind": "counts", "stage": "processed", "rel_path": rel, "census": "box_repository_20261005"})
    .to_string()
}

fn request(files: &[&str]) -> TaskObjects {
    TaskObjects {
        profile: "lab-objects-example".into(),
        collection: uuid::Uuid::nil(),
        bucket: "example-lab-objects".into(),
        prefix: "afri/counts/processed/".into(),
        generation: "g20261030-afri-2".into(),
        manifest_sha256: "c".repeat(64),
        files: files.iter().map(|f| (*f).to_owned()).collect(),
        columns: Vec::new(),
        request_sha256: "d".repeat(64),
    }
}

#[test]
fn rows_are_found_by_the_collection_prefix_and_anything_else_is_refused() {
    let manifest = format!("{}\n{}\n", row("a b (1).csv", b"x"), row("dir/c.tsv", b"y"));
    let rows = rows_for(
        manifest.as_bytes(),
        "afri/counts/processed/",
        &["dir/c.tsv".into(), "a b (1).csv".into()],
    )
    .unwrap();
    assert_eq!(rows[0].rel_path, "dir/c.tsv");
    assert_eq!(rows[1].rel_path, "a b (1).csv");
    let code = |files: &[&str], manifest: &str| {
        rows_for(
            manifest.as_bytes(),
            "afri/counts/processed/",
            &files.iter().map(|f| (*f).to_owned()).collect::<Vec<_>>(),
        )
        .unwrap_err()
        .code
    };
    assert_eq!(
        code(&["missing.csv"], &manifest),
        "objects_file_not_in_manifest"
    );
    assert_eq!(
        code(&["dir/c.tsv", "dir/c.tsv"], &manifest),
        "objects_request_invalid"
    );
    assert_eq!(code(&["../x"], &manifest), "objects_request_invalid");
    // A row whose key is not its own study, kind, stage and path, or a repeated row, refuses the manifest.
    let forged =
        row("a.csv", b"x").replace("afri/counts/processed/a.csv", "afri/counts/processed/b.csv");
    assert_eq!(code(&["b.csv"], &forged), "objects_manifest_invalid");
    let twice = format!("{}\n{}\n", row("a.csv", b"x"), row("a.csv", b"x"));
    assert_eq!(code(&["a.csv"], &twice), "objects_manifest_invalid");
    assert_eq!(code(&["a.csv"], "{"), "objects_manifest_invalid");
}

#[test]
fn delimited_files_become_tables_within_their_bounds() {
    assert_eq!(delimiter("x/a.CSV", b"a,b"), Some(b','));
    assert_eq!(delimiter("a.tsv", b"a\tb"), Some(b'\t'));
    assert_eq!(delimiter("a.txt", b"a\tb\n1\t2"), Some(b'\t'));
    assert_eq!(delimiter("a.txt", b"plain words\n"), None);
    assert_eq!(delimiter("a.npy", b"\x93NUMPY"), None);
    let (csv, rows, columns) = table(b"gene\tcount\nA,1\t5\nB\t\"7\"\n", b'\t', &[]).unwrap();
    assert_eq!((rows, columns), (2, 2));
    assert_eq!(
        String::from_utf8(csv).unwrap(),
        "gene,count\n\"A,1\",5\nB,7\n"
    );
    assert_eq!(table(b"a,b\n1\n", b',', &[]).unwrap_err(), "ragged");
    assert_eq!(table(b"a,b\n", b',', &[]).unwrap_err(), "no_rows");
    let mut many = b"n\n".to_vec();
    for i in 0..=MAX_TABLE_ROWS {
        many.extend_from_slice(format!("{i}\n").as_bytes());
    }
    assert_eq!(table(&many, b',', &[]).unwrap_err(), "too_many_rows");
}

#[test]
fn a_package_keeps_every_file_and_its_tables_list_without_cells() {
    let counts = b"gene,count\nA,5\nB,7\n";
    let image = b"\x89PNG\r\n";
    let manifest = format!(
        "{}\n{}\n",
        row("counts.csv", counts),
        row("plot.png", image)
    );
    let rows = rows_for(
        manifest.as_bytes(),
        "afri/counts/processed/",
        &["counts.csv".into(), "plot.png".into()],
    )
    .unwrap();
    let (kept, tables) = package(
        &task(),
        &request(&["counts.csv", "plot.png"]),
        &rows,
        vec![counts.to_vec(), image.to_vec()],
    )
    .unwrap();
    assert_eq!(tables.as_array().unwrap().len(), 1);
    assert_eq!(tables[0]["path"], "results/lab_data/afri/counts.csv");
    // The same inputs give the same bytes.
    let (again, _) = package(
        &task(),
        &request(&["counts.csv", "plot.png"]),
        &rows,
        vec![counts.to_vec(), image.to_vec()],
    )
    .unwrap();
    assert_eq!(kept, again);
    // The result digest lists the table with its columns and counts and never its cells, and never the inputs.
    let dir = tempfile::tempdir().unwrap();
    let path = "tasks/00000000-0000-0000-0000-000000000000/attempts/1-1/exports/payload";
    std::fs::create_dir_all(dir.path().join(path).parent().unwrap()).unwrap();
    std::fs::write(dir.path().join(path), &kept).unwrap();
    let built = digest::build(
        dir.path(),
        uuid::Uuid::nil(),
        path,
        kept.len() as u64,
        &hex::encode(Sha256::digest(&kept)),
    );
    let listed = built.value["tables"].as_array().unwrap();
    assert_eq!(listed.len(), 1, "{}", built.value);
    assert_eq!(listed[0]["path"], "results/lab_data/afri/counts.csv");
    assert_eq!(listed[0]["columns"], json!(["gene", "count"]));
    assert_eq!(listed[0]["rows"], 2);
    assert_eq!(listed[0]["cells_omitted"], "lab_data");
    assert!(listed[0].get("cells").is_none());
}

#[test]
fn two_files_with_one_name_get_two_table_paths() {
    let a = b"x\n1\n";
    let manifest = format!("{}\n{}\n", row("one/t.csv", a), row("two/t.csv", a));
    let rows = rows_for(
        manifest.as_bytes(),
        "afri/counts/processed/",
        &["one/t.csv".into(), "two/t.csv".into()],
    )
    .unwrap();
    let (_, tables) = package(
        &task(),
        &request(&["one/t.csv", "two/t.csv"]),
        &rows,
        vec![a.to_vec(), a.to_vec()],
    )
    .unwrap();
    assert_eq!(tables[0]["path"], "results/lab_data/afri/t.csv");
    assert_eq!(tables[1]["path"], "results/lab_data/afri/t_2.csv");
}

#[test]
fn the_read_budget_counts_the_manifest() {
    assert!(within_budget(0, MAX_STEP_BYTES).is_ok());
    assert!(within_budget(1, MAX_STEP_BYTES).is_err());
    assert!(within_budget(MAX_MANIFEST_BYTES, MAX_STEP_BYTES - MAX_MANIFEST_BYTES).is_ok());
    assert!(within_budget(u64::MAX, 1).is_err());
}

#[test]
fn a_step_keeps_only_the_columns_it_names_in_their_order() {
    let names = |v: &[&str]| v.iter().map(|s| (*s).to_owned()).collect::<Vec<_>>();
    let src = b"id,a,b,c\n1,x,2,y\n3,z,4,w\n";
    let (csv, rows, columns) = table(src, b',', &names(&["c", "id"])).unwrap();
    assert_eq!((rows, columns), (2, 2));
    assert_eq!(csv, b"c,id\ny,1\nw,3\n");
    // No names keeps every column, as before.
    let (csv, _, columns) = table(src, b',', &[]).unwrap();
    assert_eq!((columns, csv.as_slice()), (4, &src[..]));
    // A named column the header lacks, or holds twice, keeps the file as a file only.
    assert_eq!(
        table(src, b',', &names(&["id", "missing"])).unwrap_err(),
        "columns_missing"
    );
    assert_eq!(
        table(b"a,a,b\n1,2,3\n", b',', &names(&["a"])).unwrap_err(),
        "columns_ambiguous"
    );
    // A ragged file is refused whatever it keeps.
    assert_eq!(
        table(b"a,b\n1,2\n3\n", b',', &names(&["a"])).unwrap_err(),
        "ragged"
    );
}
