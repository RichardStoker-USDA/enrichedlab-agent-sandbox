/// A lab reader's connection trusts its own authority and nothing else: against a PostgreSQL whose
/// certificate that authority signed, it connects; with the process's own trust, another authority, or a host name
/// the certificate does not hold, it refuses. `scripts/qualify_query_tls.sh` starts that PostgreSQL.
/// A reader of the servers scripts/qualify_query_tls.sh starts: `BROKER_TLS_PORT` holds TLS with a certificate
/// for `localhost` that the lab authority signed, `BROKER_PLAIN_PORT` plain text only.
fn qualification_reader(
    port: &str,
    host: &str,
    mode: &str,
    ca: Option<&[u8]>,
) -> super::ReaderConnection {
    let port = std::env::var(port).expect(port);
    super::ReaderConnection {
        dsn: format!("host={host} port={port} dbname=postgres user=postgres{mode}"),
        ca: ca.map(<[u8]>::to_vec),
    }
}

fn qualification_authority(name: &str) -> Vec<u8> {
    std::fs::read(std::env::var(name).expect(name)).unwrap()
}

#[test]
#[ignore = "needs the TLS PostgreSQL of scripts/qualify_query_tls.sh"]
fn a_lab_connection_trusts_its_own_authority_alone() {
    let ca = qualification_authority("BROKER_TLS_CA");
    let other = qualification_authority("BROKER_TLS_OTHER_CA");
    let tls = |host, ca| qualification_reader("BROKER_TLS_PORT", host, " sslmode=require", ca);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let client = runtime
        .block_on(super::connect(&tls("localhost", Some(&ca))))
        .expect("the lab's own authority is trusted");
    let one: i32 = runtime
        .block_on(client.query_one("SELECT 1", &[]))
        .unwrap()
        .get(0);
    assert_eq!(one, 1);
    assert!(
        runtime
            .block_on(super::connect(&tls("localhost", None)))
            .is_err(),
        "the process's own trust accepted the lab's server"
    );
    assert!(
        runtime
            .block_on(super::connect(&tls("localhost", Some(&other))))
            .is_err(),
        "another authority accepted the lab's server"
    );
    assert!(
        runtime
            .block_on(super::connect(&tls("127.0.0.1", Some(&ca))))
            .is_err(),
        "a host name the certificate does not hold was accepted"
    );
}

#[test]
#[ignore = "needs the TLS PostgreSQL of scripts/qualify_query_tls.sh and SSL_CERT_FILE naming its authority"]
fn a_lab_connection_never_falls_back_to_the_process_trust() {
    // Here the process trusts the lab's server, as the worker trusts the broker's own cell. A connection with
    // another authority must still refuse it: its authority replaces the process's trust, never adds to it.
    assert!(std::env::var_os("SSL_CERT_FILE").is_some());
    let ca = qualification_authority("BROKER_TLS_CA");
    let other = qualification_authority("BROKER_TLS_OTHER_CA");
    let tls = |ca| qualification_reader("BROKER_TLS_PORT", "localhost", " sslmode=require", ca);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime
        .block_on(super::connect(&tls(None)))
        .expect("the process trusts the lab's server");
    assert!(
        runtime
            .block_on(super::connect(&tls(Some(&other))))
            .is_err(),
        "the process's trust vouched for a server the connection's own authority did not sign"
    );
    runtime
        .block_on(super::connect(&tls(Some(&ca))))
        .expect("the lab's own authority is trusted");
}

#[test]
#[ignore = "needs the plain-text PostgreSQL of scripts/qualify_query_tls.sh"]
fn a_lab_connection_refuses_a_server_without_tls() {
    let ca = qualification_authority("BROKER_TLS_CA");
    let plain = |mode, ca| qualification_reader("BROKER_PLAIN_PORT", "localhost", mode, ca);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    // The server answers "no TLS": a connection without its own authority that only prefers TLS goes on in plain
    // text, so each refusal below comes from the authority's rule and not from the server.
    runtime
        .block_on(super::connect(&plain(" sslmode=prefer", None)))
        .expect("the server accepts plain text");
    for mode in [" sslmode=prefer", "", " sslmode=require"] {
        assert!(
            runtime
                .block_on(super::connect(&plain(mode, Some(&ca))))
                .is_err(),
            "a connection with its own authority went on without TLS ({mode:?})"
        );
    }
    let refused = runtime
        .block_on(super::connect(&plain(" sslmode=disable", Some(&ca))))
        .unwrap_err();
    assert!(refused.to_string().contains("must use TLS"));
}

#[test]
fn a_lab_connection_requires_tls_and_others_keep_their_mode() {
    let ca = b"-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n";
    let mode = |dsn_mode: &str, ca: Option<&[u8]>| {
        super::reader_config(&super::ReaderConnection {
            dsn: format!("host=127.0.0.1 port=1 dbname=x user=y{dsn_mode}"),
            ca: ca.map(<[u8]>::to_vec),
        })
        .map(|config| config.get_ssl_mode())
    };
    for written in [" sslmode=prefer", "", " sslmode=require"] {
        assert_eq!(
            mode(written, Some(ca)).unwrap(),
            SslMode::Require,
            "{written:?}"
        );
    }
    assert!(
        mode(" sslmode=disable", Some(ca))
            .unwrap_err()
            .to_string()
            .contains("must use TLS")
    );
    assert_eq!(mode(" sslmode=prefer", None).unwrap(), SslMode::Prefer);
    assert_eq!(mode("", None).unwrap(), SslMode::Prefer);
    assert_eq!(mode(" sslmode=require", None).unwrap(), SslMode::Require);
    assert_eq!(mode(" sslmode=disable", None).unwrap(), SslMode::Disable);
}

use super::*;

fn query() -> TaskQuery {
    TaskQuery {
        source: "fooddata-central-sr-foundation".into(),
        title: "FoodData Central".into(),
        release: "SR Legacy April 2018, Foundation Foods April 2026".into(),
        schema: "fdc".into(),
        relations: vec!["food".into()],
        functions: vec!["count".into()],
        row_cap: 5000,
        statement_timeout_ms: 30000,
        connection_ref: "fooddata-reader".into(),
        snapshot: json!({"manifest_sha256": "a".repeat(64)}),
        proposed_sql: "SELECT description FROM food ORDER BY 1".into(),
        admitted_sql: "SELECT description FROM food ORDER BY 1\nLIMIT 5000".into(),
        sql_sha256: String::new(),
        guard: sql_guard::GUARD.into(),
        row_limit: 5000,
        reviewed_query: None,
    }
}

#[test]
fn counts_rows_of_postgresql_csv_with_quoted_newlines() {
    let csv = b"description,n\n\"Apple, raw\",1\n\"two\nlines\",\n\"\",3\n";
    assert_eq!(count_rows(csv, 2), Some(3));
    assert_eq!(count_rows(csv, 3), None);
    assert_eq!(count_rows(b"description\n", 1), Some(0));
}

fn unpack(package: &[u8]) -> std::collections::BTreeMap<String, Vec<u8>> {
    use std::io::Read;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(package));
    archive
        .entries()
        .unwrap()
        .map(|entry| {
            let mut entry = entry.unwrap();
            let path = entry.path().unwrap().to_string_lossy().into_owned();
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).unwrap();
            (path, bytes)
        })
        .collect()
}

#[test]
fn expected_snapshots_are_a_manifest_or_a_live_version_relation() {
    assert!(matches!(
        expected(&json!({"manifest_sha256": "a".repeat(64)})),
        Ok(Expected::Manifest(_))
    ));
    assert!(matches!(
        expected(
            &json!({"kind": "live", "version_relation": "vw_database_version", "declaration_sha256": "a".repeat(64)})
        ),
        Ok(Expected::Live("vw_database_version"))
    ));
    for bad in [
        json!({}),
        json!({"manifest_sha256": "a".repeat(63)}),
        json!({"kind": "live"}),
        json!({"kind": "live", "version_relation": "vw\"; DROP TABLE x; --"}),
        json!({"kind": "live", "version_relation": "Version"}),
        json!({"kind": "pinned", "manifest_sha256": "a".repeat(64)}),
        // Only a pinned snapshot names a manifest, so an older worker never reads a live source.
        json!({"kind": "live", "version_relation": "vw_database_version", "manifest_sha256": "a".repeat(64)}),
    ] {
        assert!(
            matches!(expected(&bad), Err(Stop { code, .. }) if code == "snapshot_not_recorded"),
            "{bad}"
        );
    }
}

#[test]
fn packages_are_byte_identical_for_equal_results_and_digest_the_table() {
    let task = package_task();
    let csv = b"description,amount\nKiwifruit,92.7\n";
    let columns =
        json!([{"name": "description", "type": "text"}, {"name": "amount", "type": "float8"}]);
    let a = package(&task, &query(), csv, 1, &columns, "18.1", None).unwrap();
    let b = package(&task, &query(), csv, 1, &columns, "18.1", None).unwrap();
    assert_eq!(a, b);
    // A live lab source's version goes into the receipt, and the README says how to rerun against a live database.
    let version = json!({"schema_version": "0021", "newest_import_id": 7, "newest_import_at": "2026-10-06T13:31:28Z"});
    let live = package(&task, &query(), csv, 1, &columns, "18.1", Some(&version)).unwrap();
    assert_ne!(live, a);
    let members = unpack(&live);
    let receipt: Value = serde_json::from_slice(&members["receipt.json"]).unwrap();
    assert_eq!(receipt["source_version"], version);
    let readme = String::from_utf8(members["README.md"].clone()).unwrap();
    assert!(readme.contains("live lab database"), "{readme}");
    let pinned: Value = serde_json::from_slice(&unpack(&a)["receipt.json"]).unwrap();
    assert!(pinned.get("source_version").is_none());
    let dir = std::env::temp_dir().join(format!(
        "query_package_codetestfile_{}",
        uuid::Uuid::new_v4()
    ));
    let path = format!("tasks/{}/attempts/1-1/exports/payload", uuid::Uuid::nil());
    write_private(&dir.join(&path), &a).unwrap();
    let sha = hex::encode(Sha256::digest(&a));
    let built = digest::build(&dir, uuid::Uuid::nil(), &path, a.len() as u64, &sha);
    std::fs::remove_dir_all(&dir).unwrap();
    assert_eq!(built.status, "summarized", "{}", built.value);
    assert_eq!(built.value["package"]["all_matched"], true);
    let tables = built.value["tables"].as_array().unwrap();
    assert_eq!(tables[0]["path"], TABLE_PATH);
    assert_eq!(built.value["parameters"]["guard"], sql_guard::GUARD);
}

#[test]
fn reviewed_worker_admits_again_and_refuses_changed_provenance_before_connecting() {
    let q = reviewed_query();
    assert_eq!(admit_stored(&q).unwrap().limit, 2);
    let mut changed = q.clone();
    changed.proposed_sql = "DELETE FROM food".into();
    assert_eq!(
        admit_stored(&changed).unwrap_err().code,
        "sql_refused_not_select"
    );
    let mut changed = q.clone();
    changed.reviewed_query.as_mut().unwrap().normalized = json!({"name":"changed"});
    assert_eq!(
        admit_stored(&changed).unwrap_err().code,
        "reviewed_query_record_invalid"
    );
    let mut changed = q.clone();
    changed.reviewed_query.as_mut().unwrap().row_cap = 5001;
    assert_eq!(
        admit_stored(&changed).unwrap_err().code,
        "reviewed_query_record_invalid"
    );
    let mut changed = q;
    changed.relations = vec!["other".into()];
    assert_eq!(
        admit_stored(&changed).unwrap_err().code,
        "sql_refused_relation"
    );
}

fn package_task() -> ClaimedTask {
    ClaimedTask {
        id: uuid::Uuid::nil(),
        principal_id: uuid::Uuid::nil(),
        group_key: "lab".into(),
        profile_key: "reference-sql".into(),
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
        image_ref: "postgres@sha256:x".into(),
        cpu_limit: "1".into(),
        memory_limit: "1Gi".into(),
        state_profile: "s".into(),
        role: "r".into(),
        max_run_seconds: 300,
    }
}

fn reviewed_query() -> TaskQuery {
    use crate::broker::reviewed_query::{Tool, digest};
    let template = "SELECT description FROM food WHERE description=$1 ORDER BY description";
    let tool:Tool=serde_json::from_value(json!({"key":"synthetic-tool","version":1,"source":"synthetic-source",
        "title":"Synthetic selection","purpose":"Read synthetic names.","parameters":[
            {"name":"name","description":"Synthetic name","type":"text","required":true,"max_length":256}],
        "output_columns":[{"name":"description","type":"text","description":"Synthetic name"}],
        "row_cap":2,"state":"enabled","template":template,"template_sha256":digest(template.as_bytes())})).unwrap();
    let allowed = Allowed {
        schema: "fdc",
        relations: &BTreeSet::from(["food".into()]),
        functions: &BTreeSet::new(),
        row_cap: 5000,
    };
    let (admitted, record) = tool.lower(&json!({"name":"synthetic"}), &allowed).unwrap();
    let mut q = query();
    q.proposed_sql = admitted.sql.clone();
    q.admitted_sql = admitted.sql;
    q.row_limit = 2;
    q.reviewed_query = Some(record);
    q
}

#[test]
fn reviewed_package_keeps_template_parameters_lowered_sql_and_their_checksums() {
    let q = reviewed_query();
    let bytes = package(
        &package_task(),
        &q,
        b"description\nsynthetic\n",
        1,
        &json!([{"name":"description","type":"text"}]),
        "18.1",
        None,
    )
    .unwrap();
    let members = unpack(&bytes);
    let record = q.reviewed_query.as_ref().unwrap();
    assert_eq!(members["method/template.sql"], record.template.as_bytes());
    let parameters: Value = serde_json::from_slice(&members["method/parameters.json"]).unwrap();
    assert_eq!(parameters["requested"], record.requested);
    assert_eq!(parameters["normalized"], record.normalized);
    assert_eq!(parameters["schema_sha256"], record.schema_sha256);
    assert_eq!(
        members["method/query.sql"],
        format!("{}\n", q.admitted_sql).as_bytes()
    );
    let sums = std::str::from_utf8(&members["SHA256SUMS"]).unwrap();
    for name in [
        "method/template.sql",
        "method/parameters.json",
        "method/query.sql",
    ] {
        assert!(sums.contains(&format!(
            "{}  {name}\n",
            hex::encode(Sha256::digest(&members[name]))
        )));
    }
    assert!(
        std::str::from_utf8(&members["README.md"])
            .unwrap()
            .contains("person's validation")
    );
}
