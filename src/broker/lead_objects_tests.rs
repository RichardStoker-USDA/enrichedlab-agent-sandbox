use super::*;
use crate::broker::coordinator::ProposedAction;
use crate::broker::knowledge::Hit;

const ID: &str = "0192d3b0-0000-7000-8000-000000000001";
const MANIFEST: &str = "aabe6e4575c2d36ad41d3dcda679858cddb246f1a3a7c2f4dfa208f295c0e170";

fn hit(kind: &str, generation: &str, source: &str, sha: Option<&str>) -> Hit {
    Hit {
        id: ID.parse().unwrap(),
        kind: kind.into(),
        scope: "example-lab".into(),
        data_class: "object_data".into(),
        title: "afri: counts files, processed".into(),
        summary: "65 files of study afri.".into(),
        source_uri: Some(source.into()),
        content_sha256: sha.map(str::to_owned),
        lexical_rank: Some(1),
        dense_rank: None,
        locator: Some(
            json!({"bucket": "example-lab-objects", "prefix": "afri/counts/processed/",
            "manifest": format!("manifests/{generation}.jsonl"), "generation": "g20261030-afri-2", "files": 65,
            "bytes": 41767329, "sample": ["a b (1).csv", "dir/c.tsv"]}),
        ),
        paper: None,
    }
}

fn found(hits: Vec<Hit>) -> Found {
    Found {
        query_sha256: "q".into(),
        dense_profile: None,
        hits,
    }
}

fn good() -> Hit {
    hit(
        "object_collection",
        "g20261030-afri-2",
        "s3://example-lab-objects/afri/counts/processed/",
        Some(MANIFEST),
    )
}

fn action(profile: &str, objects: Option<Value>, extra: Option<(&str, Value)>) -> ProposedAction {
    let mut a = json!({"action_key": "counts", "title": "Counts", "profile": profile, "objective": "o",
        "rationale": "r"});
    if let Some(objects) = objects {
        a["objects"] = objects;
    }
    if let Some((field, value)) = extra {
        a[field] = value;
    }
    serde_json::from_value(a).unwrap()
}

/// The run's methods with the objects method and its collections, and the context the lead was shown from them.
fn methods_and_context() -> (Value, Value) {
    let profiles = json!([{"key": "lab-objects-example", "name": "Lab files", "description": "d", "tool_bundle": "b"},
        {"key": "lab-sql-example-research", "name": "SQL", "description": "d", "tool_bundle": "b"}]);
    let methods = with_lab_objects(
        &profiles,
        &["lab-objects-example".into()],
        &collections(&found(vec![good()])),
    );
    let shown = crate::broker::coordinator::methods_view(&methods, 4096);
    (methods, json!({"methods": shown}))
}

#[test]
fn only_whole_collection_records_are_shown() {
    let shown = collections(&found(vec![
        good(),
        hit(
            "dataset",
            "g20261030-afri-2",
            "s3://example-lab-objects/afri/counts/processed/",
            Some(MANIFEST),
        ),
        hit(
            "object_collection",
            "g20261030-afri-2",
            "s3://other/afri/counts/processed/",
            Some(MANIFEST),
        ),
        hit(
            "object_collection",
            "G1",
            "s3://example-lab-objects/afri/counts/processed/",
            Some(MANIFEST),
        ),
        hit(
            "object_collection",
            "g20261030-afri-2",
            "s3://example-lab-objects/afri/counts/processed/",
            None,
        ),
    ]));
    let shown = shown.as_array().unwrap();
    assert_eq!(shown.len(), 1);
    assert_eq!(shown[0]["ref"], format!("knowledge://{ID}"));
    assert_eq!(shown[0]["manifest_sha256"], MANIFEST);
}

#[test]
fn the_lead_sees_names_and_counts_without_the_store_details() {
    let (_, context) = methods_and_context();
    let method = &context["methods"][0];
    assert_eq!(method["key"], "lab-objects-example");
    assert!(method.get(FOUND_FIELD).is_none());
    let shown = &method["objects"]["collections"][0];
    assert_eq!(shown["sample"], json!(["a b (1).csv", "dir/c.tsv"]));
    for private in ["bucket", "prefix", "generation", "manifest_sha256"] {
        assert!(shown.get(private).is_none(), "{private}");
    }
    assert_eq!(method["objects"]["note"], NOTE);
}

#[test]
fn a_step_names_a_shown_collection_and_files_from_its_sample() {
    let (methods, context) = methods_and_context();
    let collection = format!("knowledge://{ID}");
    let ok = action(
        "lab-objects-example",
        Some(json!({"collection": collection, "files": ["dir/c.tsv", "a b (1).csv"]})),
        None,
    );
    let admitted = admit(&[ok], &methods, &context).unwrap();
    let files = &admitted["counts"];
    assert_eq!(
        files.record(),
        json!({"collection": ID, "bucket": "example-lab-objects", "prefix": "afri/counts/processed/",
            "generation": "g20261030-afri-2", "manifest_sha256": MANIFEST, "files": ["dir/c.tsv", "a b (1).csv"]})
    );
    let code = |a: ProposedAction| admit(&[a], &methods, &context).unwrap_err().code;
    let step = |files: Value| {
        action(
            "lab-objects-example",
            Some(json!({"collection": collection, "files": files})),
            None,
        )
    };
    assert_eq!(
        code(step(json!(["missing.csv"]))),
        "proposal_objects_file_unknown"
    );
    assert_eq!(
        code(step(json!(["dir/c.tsv", "dir/c.tsv"]))),
        "proposal_objects_files_invalid"
    );
    assert_eq!(code(step(json!([]))), "proposal_objects_files_invalid");
    let unknown = action(
        "lab-objects-example",
        Some(
            json!({"collection": "knowledge://0192d3b0-0000-7000-8000-000000000009", "files": ["dir/c.tsv"]}),
        ),
        None,
    );
    assert_eq!(code(unknown), "proposal_objects_collection_unknown");
    assert_eq!(
        code(action("lab-objects-example", None, None)),
        "proposal_objects_missing"
    );
    assert_eq!(
        code(action(
            "lab-sql-example-research",
            Some(json!({"collection": collection, "files": ["dir/c.tsv"]})),
            None
        )),
        "proposal_objects_unexpected"
    );
    assert_eq!(
        code(action(
            "lab-objects-example",
            Some(json!({"collection": collection, "files": ["dir/c.tsv"]})),
            Some(("sql", json!("SELECT 1")))
        )),
        "proposal_objects_unexpected"
    );
    // A step for another method without objects is not this module's.
    assert!(
        admit(
            &[action("lab-sql-example-research", None, None)],
            &methods,
            &context
        )
        .unwrap()
        .is_empty()
    );
}

#[test]
fn a_digest_changes_with_the_files() {
    let (methods, context) = methods_and_context();
    let collection = format!("knowledge://{ID}");
    let one = admit(
        &[action(
            "lab-objects-example",
            Some(json!({"collection": collection, "files": ["dir/c.tsv"]})),
            None,
        )],
        &methods,
        &context,
    )
    .unwrap();
    let two = admit(
        &[action(
            "lab-objects-example",
            Some(json!({"collection": collection, "files": ["a b (1).csv"]})),
            None,
        )],
        &methods,
        &context,
    )
    .unwrap();
    assert_ne!(one["counts"].sha256(), two["counts"].sha256());
}

#[test]
fn a_step_may_keep_named_columns_and_records_them_only_when_named() {
    let (methods, context) = methods_and_context();
    let collection = format!("knowledge://{ID}");
    let step = |objects: Value| action("lab-objects-example", Some(objects), None);
    let admitted = admit(
        &[step(
            json!({"collection": collection, "files": ["dir/c.tsv"], "columns": ["gene", "count"]}),
        )],
        &methods,
        &context,
    )
    .unwrap();
    let kept = &admitted["counts"];
    assert_eq!(kept.record()["columns"], json!(["gene", "count"]));
    // Without columns the record and its digest are exactly r37's.
    let plain = admit(
        &[step(
            json!({"collection": collection, "files": ["dir/c.tsv"]}),
        )],
        &methods,
        &context,
    )
    .unwrap();
    assert!(plain["counts"].record().get("columns").is_none());
    assert_ne!(plain["counts"].sha256(), kept.sha256());
    let code = |objects: Value| admit(&[step(objects)], &methods, &context).unwrap_err();
    let too_many: Vec<String> = (0..=COLUMNS_MAX).map(|i| format!("c{i}")).collect();
    for (columns, name) in [
        (
            json!(too_many),
            format!("columns: {} of 1 to {COLUMNS_MAX}", COLUMNS_MAX + 1),
        ),
        // Named but empty is refused, never read as absent (r38 review): absent is the only way to keep every column.
        (json!([]), format!("columns: 0 of 1 to {COLUMNS_MAX}")),
        (json!(["a", "a"]), "column 2: named twice".to_owned()),
        (
            json!(["a", ""]),
            format!("column 2: 1 to {COLUMN_BYTES} bytes, no control character"),
        ),
        (
            json!(["a\nb"]),
            format!("column 1: 1 to {COLUMN_BYTES} bytes, no control character"),
        ),
        (
            json!(["x".repeat(COLUMN_BYTES + 1)]),
            format!("column 1: 1 to {COLUMN_BYTES} bytes, no control character"),
        ),
    ] {
        let refused =
            code(json!({"collection": collection, "files": ["dir/c.tsv"], "columns": columns}));
        assert_eq!(refused.code, "proposal_objects_columns_invalid");
        assert_eq!(refused.names.last().unwrap(), &name);
    }
    assert!(column_name(&"x".repeat(COLUMN_BYTES)));
    // A written null is not absent either.
    let null: Result<ObjectsChoice, _> = serde_json::from_value(
        json!({"collection": collection, "files": ["dir/c.tsv"], "columns": null}),
    );
    assert!(null.is_err());
}
