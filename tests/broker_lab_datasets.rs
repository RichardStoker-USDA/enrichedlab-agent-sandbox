//! Lab datasets (migration 059) against an isolated PostgreSQL and a stand-in upload guard: readiness by
//! role, the dataset method offered only through a request, the guard's decision recorded whatever it is, the intake
//! that checks and profiles the guard's rebuilt grid, a preparation a person approves, the prepared table with its
//! receipt, a refused preparation that names the rows to decide about, a grid that does not match its digest, the lease
//! fence, readers, lineage, the project's contents, a result digest that lists the prepared table without its cells, and
//! a template analysis of the prepared table. Every value is synthetic.
use axum::{
    Json, Router,
    extract::{Path as AxumPath, State},
    http::{HeaderMap, StatusCode},
    routing::get,
};
use enrichedlab_agent_sandbox::broker::{
    analysis_run,
    config::WorkerConfig,
    dataset_run,
    model::{PrincipalClass, SubmitTaskRequest, VerifiedIdentity},
    project::{Capability, CreateProject, ProjectGrant, Visibility},
    store::{
        AnalysisRequest, DatasetReceipt, DatasetRecord, PreparationRequest, Store, StoreErrorKind,
    },
};
use flate2::{Compression, write::GzEncoder};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    env, fs,
    io::{Read, Write},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};
use time::OffsetDateTime;
use tokio_postgres::{Config, NoTls, config::Host};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const POLICY: &str = "guard-policy-2026-10-12.1";
const TOKEN: &str = "fetcher-token";
const IMAGE: &str = "registry.example.invalid/broker@sha256:5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e5e";
const TABLE: &str = "results/lab_data/gtt_cohort3.csv";

fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// A glucose tolerance sheet as labs keep one: a title, a heading over the time points, units, censored insulin,
/// spellings of sex, a repeated mouse, a mean row and a note.
fn tolerance_rows() -> Vec<Value> {
    vec![
        json!(["Glucose tolerance test, cohort 3"]),
        json!([]),
        json!([
            "Mouse ID",
            "Sex",
            "Diet",
            "Body weight (g)",
            "Glucose (mg/dL)",
            "",
            "",
            "",
            "",
            "Insulin"
        ]),
        json!(["", "", "", "", "0", "15", "30", "60", "120", ""]),
        json!(["", "", "", "", "", "", "", "", "", "ng/mL"]),
        json!([
            "M-0417", "Male", "HFD", "31,2", "142", "310", "295", "240", "160", "1.2"
        ]),
        json!([
            "M-0418", "male ", "HFD", "29,8", "150", "330", "NA", "251", "171", "<0.2"
        ]),
        json!([
            "M-0419", "Female", "Chow", "22,4", "120", "250", "230", "180", "130", "ND"
        ]),
        json!([
            "M-0419", "female", "Chow", "23,0", "118", "", "228", "175", "-", "0.9"
        ]),
        json!([
            "M-0421", "MALE", "HFD", "30,1", "145", "300", "280", "230", "155", "1.4"
        ]),
        json!([
            "Mean", "", "", "27,3", "135", "297", "258", "215", "154", ""
        ]),
        json!([]),
        json!(["Mouse M-0420 died on day 3 and is not listed"]),
    ]
}

/// The guard's rebuilt copy of a delimited file: `enrichedlab.cell-grid.v1`.
fn grid(rows: &[Value], file_sha: &str) -> Vec<u8> {
    let columns = rows
        .iter()
        .map(|r| r.as_array().map_or(0, Vec::len))
        .max()
        .unwrap_or(0);
    let mut text = json!({"schema": "enrichedlab.cell-grid.v1", "source": {"type": "delimited", "sha256": file_sha,
        "bytes": 612, "encoding": null, "delimiter": null, "sheets": [{"name": null, "rows": rows.len(),
        "columns": columns, "hidden": false, "formulas": 0, "formulas_without_values": 0, "merged_count": 0,
        "merged": []}]}})
    .to_string();
    text.push('\n');
    text.push_str("{\"sheet\":0}\n");
    for row in rows {
        text.push_str(&row.to_string());
        text.push('\n');
    }
    let mut gz = GzEncoder::new(Vec::new(), Compression::default());
    gz.write_all(text.as_bytes()).unwrap();
    gz.finish().unwrap()
}

/// The preparation a person approves for the tolerance sheet: one row per mouse and time point.
fn tolerance_spec() -> Value {
    json!({
        "schema": "enrichedlab.dataset-preparation.v1",
        "table_name": "gtt_cohort3",
        "sheet": 0,
        "header_rows": [3, 4],
        "units_row": 5,
        "rows": {"first": 6, "last": 10},
        "drop_rows": [[9, 9]],
        "missing": ["NA", "-"],
        "columns": [
            {"from": "A", "name": "mouse_id", "type": "text"},
            {"from": "B", "name": "sex", "type": "category",
                "recode": {"male": "Male", "MALE": "Male", "female": "Female"}},
            {"from": "C", "name": "diet", "type": "category"},
            {"from": "D", "name": "body_weight", "type": "number", "unit": "g", "decimal": "comma"},
            {"from": "J", "name": "insulin", "type": "number", "unit": "ng/mL",
                "censored": {"rule": "missing", "limit": 0.2}}
        ],
        "long": {"columns": ["E", "F", "G", "H", "I"], "name": "time_min", "values": [0, 15, 30, 60, 120],
            "value": {"name": "glucose", "type": "integer", "unit": "mg/dL"}},
        "key": ["mouse_id", "time_min"]
    })
}

fn expected_table() -> String {
    let mut table = String::from(
        "mouse_id,sex,diet,body_weight,insulin,insulin_censored,insulin_limit,time_min,glucose\n",
    );
    for (mouse, sex, diet, weight, insulin, glucose) in [
        (
            "M-0417",
            "Male",
            "HFD",
            "31.2",
            "1.2,NA,NA",
            ["142", "310", "295", "240", "160"],
        ),
        (
            "M-0418",
            "Male",
            "HFD",
            "29.8",
            "NA,below,0.2",
            ["150", "330", "NA", "251", "171"],
        ),
        (
            "M-0419",
            "Female",
            "Chow",
            "22.4",
            "NA,not_detected,0.2",
            ["120", "250", "230", "180", "130"],
        ),
        (
            "M-0421",
            "Male",
            "HFD",
            "30.1",
            "1.4,NA,NA",
            ["145", "300", "280", "230", "155"],
        ),
    ] {
        for (time, value) in ["0", "15", "30", "60", "120"].iter().zip(glucose) {
            table.push_str(&format!(
                "{mouse},{sex},{diet},{weight},{insulin},{time},{value}\n"
            ));
        }
    }
    table
}

/// A stand-in for the upload guard's fetch: the rebuilt copy of each known intake, for the fetcher's token only.
#[derive(Clone, Default)]
struct Guard {
    copies: Arc<Mutex<HashMap<Uuid, Vec<u8>>>>,
    fetched: Arc<Mutex<Vec<Uuid>>>,
}

async fn guard_service(guard: Guard) -> String {
    async fn disarmed(
        State(guard): State<Guard>,
        AxumPath(intake): AxumPath<Uuid>,
        headers: HeaderMap,
    ) -> Result<Vec<u8>, StatusCode> {
        if headers.get("authorization").and_then(|v| v.to_str().ok())
            != Some(&format!("Bearer {TOKEN}"))
        {
            return Err(StatusCode::UNAUTHORIZED);
        }
        guard.fetched.lock().unwrap().push(intake);
        guard
            .copies
            .lock()
            .unwrap()
            .get(&intake)
            .cloned()
            .ok_or(StatusCode::NOT_FOUND)
    }
    let app = Router::new()
        .route("/v1/intake/{intake}/disarmed", get(disarmed))
        .with_state(guard);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

/// A stand-in render service that declares one analysis template, so a person may ask for one.
async fn render_service() -> String {
    async fn features() -> Json<Value> {
        Json(
            json!({"service_version": "0.6.0", "image_digest": format!("sha256:{}", "7".repeat(64)), "features": [
                {"id": "summary_stats", "label": "Summary statistics", "kind": "analysis",
                 "params": {"value": {"type": "column", "required": true}, "group": {"type": "column", "required": false}}}
            ]}),
        )
    }
    let app = Router::new().route("/features", get(features));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

fn private_file(path: &Path, text: &str) {
    fs::write(path, text).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
}

/// The members of a package in their order.
fn members(bytes: &[u8]) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for entry in tar::Archive::new(flate2::read::GzDecoder::new(bytes))
        .entries()
        .unwrap()
    {
        let mut entry = entry.unwrap();
        let name = entry.path().unwrap().to_string_lossy().into_owned();
        let mut body = Vec::new();
        entry.read_to_end(&mut body).unwrap();
        out.push((name, body));
    }
    out
}

/// Every member but the list is listed in `SHA256SUMS` with its digest.
fn sums_match(members: &[(String, Vec<u8>)]) {
    let (_, sums) = members.iter().find(|(n, _)| n == "SHA256SUMS").unwrap();
    let sums = String::from_utf8(sums.clone()).unwrap();
    let listed: Vec<&str> = sums.lines().collect();
    assert_eq!(listed.len(), members.len() - 1);
    for (name, body) in members.iter().filter(|(n, _)| n != "SHA256SUMS") {
        assert!(
            listed.contains(&format!("{}  {name}", sha(body)).as_str()),
            "{name}"
        );
    }
}

#[tokio::test]
#[ignore = "isolated PostgreSQL through qualify_remote_broker.sh lab-datasets"]
async fn a_lab_file_becomes_a_prepared_table_through_the_guard_and_a_persons_preparation() {
    let dsn_file = env::var_os("BROKER_QUALIFICATION_MIGRATION_DSN_FILE").unwrap();
    let dsn = fs::read_to_string(&dsn_file).unwrap();
    let config: Config = dsn.parse().unwrap();
    assert_eq!(config.get_dbname(), Some("broker_qual"));
    assert!(matches!(config.get_hosts(), [Host::Tcp(h)] if h == "127.0.0.1"));
    let (owner, connection) = config.connect(NoTls).await.unwrap();
    let connection = tokio::spawn(connection);
    let store = |user: &str, size| {
        Store::connect(
            &dsn.replace("user=postgres", &format!("user={user}")),
            "research-site".into(),
            size,
        )
        .unwrap()
    };
    let api = store("broker_api_qual", 3);
    let worker = store("broker_worker_qual", 2);

    // Each role is ready only with migration 059 and its own grants.
    api.ready_for_lab_datasets(true).await.unwrap();
    worker.ready_for_lab_datasets(false).await.unwrap();
    assert_eq!(
        api.ready_for_lab_datasets(false).await.unwrap_err().code,
        "broker_schema_not_ready"
    );
    assert!(worker.ready_for_lab_datasets(true).await.is_err());
    for (function, role, api_side) in [
        (
            "enrichedlab_agent.broker_record_dataset(text,uuid,uuid,uuid,text,text,bigint,text,uuid,text,jsonb,text,text,bigint)",
            "broker_api_qual",
            true,
        ),
        (
            "enrichedlab_agent.broker_record_dataset_receipt(text,uuid,text,integer,bigint,text,text,jsonb,jsonb,text,text,text,bigint,jsonb,text,text,integer,timestamptz,timestamptz)",
            "broker_worker_qual",
            false,
        ),
    ] {
        let role_store = if api_side { &api } else { &worker };
        owner
            .batch_execute(&format!(
                "REVOKE EXECUTE ON FUNCTION {function} FROM {role}"
            ))
            .await
            .unwrap();
        assert!(role_store.ready_for_lab_datasets(api_side).await.is_err());
        owner
            .batch_execute(&format!("GRANT EXECUTE ON FUNCTION {function} TO {role}"))
            .await
            .unwrap();
        role_store.ready_for_lab_datasets(api_side).await.unwrap();
    }

    owner
        .batch_execute(&format!(
            "INSERT INTO enrichedlab_agent.broker_principal_invitations(tenant_id,email,group_key,role) VALUES
               ('research-site','outsider@example.invalid','pilot-lab','researcher'),
               ('research-site','workload@example.invalid','pilot-lab','researcher');
             INSERT INTO enrichedlab_agent.broker_agent_profiles(tenant_id,profile_key,name,description,harness,thinking,
               route,controller_profile,gateway_profile,image_ref,cpu_limit,memory_limit,state_profile,role,tool_bundle,
               max_run_seconds,max_task_attempts,max_provider_attempts)
             SELECT tenant_id,k,n,description,harness,thinking,r,controller_profile,gateway_profile,'{IMAGE}',cpu_limit,
               memory_limit,state_profile,role,tool_bundle,max_run_seconds,1,max_provider_attempts
             FROM enrichedlab_agent.broker_agent_profiles,
               (VALUES ('lab-dataset','Read a lab dataset','server-dataset'),
                       ('template-analysis','Run a template analysis on a result table','server-analysis')) v(k,n,r)
             WHERE tenant_id='research-site' AND profile_key='general-research';"
        ))
        .await
        .unwrap();
    let person = |subject: &str, email: &str| VerifiedIdentity {
        issuer: "https://identity.example.invalid".into(),
        class: PrincipalClass::Person,
        subject: subject.into(),
        email: email.into(),
        display_name: None,
    };
    let a = api
        .resolve_principal(&person("owner", "researcher@example.invalid"))
        .await
        .unwrap();
    let outsider = api
        .resolve_principal(&person("outsider", "outsider@example.invalid"))
        .await
        .unwrap();
    let workload = api
        .resolve_principal(&VerifiedIdentity {
            issuer: "kubernetes://serviceaccount".into(),
            class: PrincipalClass::ServiceAccount,
            subject: "system:serviceaccount:qual:caller".into(),
            email: "workload@example.invalid".into(),
            display_name: Some("Qualification caller".into()),
        })
        .await
        .unwrap();
    let project = Uuid::new_v4();
    api.create_project(
        a,
        &CreateProject {
            project_id: project,
            group: "pilot-lab".into(),
            title: "Glucose tolerance".into(),
            visibility: Visibility::Private,
        },
    )
    .await
    .unwrap();
    let workload_project = Uuid::new_v4();
    api.create_project(
        workload,
        &CreateProject {
            project_id: workload_project,
            group: "pilot-lab".into(),
            title: "A workload's project".into(),
            visibility: Visibility::Private,
        },
    )
    .await
    .unwrap();

    // Without a dataset method for the lab nothing is sent to the guard.
    let refusal = api
        .dataset_upload_check(a, project, Uuid::new_v4())
        .await
        .unwrap_err();
    assert_eq!(
        (refusal.kind, refusal.code),
        (StoreErrorKind::Forbidden, "dataset_method_unavailable")
    );
    owner
        .batch_execute(
            "INSERT INTO enrichedlab_agent.broker_group_profile_grants(tenant_id,group_key,profile_key)
               VALUES('research-site','pilot-lab','lab-dataset');",
        )
        .await
        .unwrap();
    // The method runs only from a person's upload or preparation: it is not offered, a person cannot run it directly
    // and a research run cannot be given it.
    let me = api.me(a).await.unwrap();
    assert_eq!(
        me.profiles
            .iter()
            .find(|p| p.key == "lab-dataset")
            .map(|p| p.offered),
        Some(false)
    );
    let digest = "0".repeat(64);
    assert_eq!(
        api.submit_task(
            a,
            "pilot-lab",
            "lab-dataset",
            "Read a file",
            "direct-dataset",
            &digest
        )
        .await
        .unwrap_err()
        .code,
        "profile_forbidden"
    );
    assert_eq!(
        api.submit_project_task(
            a,
            &SubmitTaskRequest {
                group: "pilot-lab".into(),
                profile: "lab-dataset".into(),
                objective: "Read a file".into(),
                project_id: Some(project),
                action_key: Some("direct-dataset".into()),
                parent_task_id: None,
            },
            "direct-dataset-project",
            &digest,
        )
        .await
        .unwrap_err()
        .code,
        "profile_forbidden"
    );
    for (profiles, allowed) in [
        (vec!["lab-dataset"], false),
        (vec!["general-research"], true),
    ] {
        let row = owner
            .query_one(
                "SELECT enrichedlab_agent.broker_research_authorized('research-site',$1,$2,$3)",
                &[&a, &project, &profiles],
            )
            .await
            .unwrap();
        assert_eq!(row.get::<_, bool>(0), allowed, "{profiles:?}");
    }

    // Before the guard sees a file: who may send one, and to which project.
    let first = Uuid::new_v4();
    assert_eq!(
        api.dataset_upload_check(a, project, first).await.unwrap(),
        json!({"group": "pilot-lab", "profile": "lab-dataset"})
    );
    for (actor, place, kind, code) in [
        (
            outsider,
            project,
            StoreErrorKind::NotFound,
            "project_not_found",
        ),
        (
            workload,
            workload_project,
            StoreErrorKind::Forbidden,
            "person_required",
        ),
    ] {
        let refusal = api
            .dataset_upload_check(actor, place, first)
            .await
            .unwrap_err();
        assert_eq!((refusal.kind, refusal.code), (kind, code));
    }

    // The guard allowed the file and rebuilt it as a grid: the dataset and its intake are recorded together, and the
    // same request again answers the same dataset.
    let file_sha = sha(b"synthetic tolerance sheet as a person sent it");
    let rows = tolerance_rows();
    let copy = grid(&rows, &file_sha);
    let copy_sha = sha(&copy);
    let intake = Uuid::new_v4();
    let guard = Guard::default();
    guard.copies.lock().unwrap().insert(intake, copy.clone());
    let none = json!([]);
    let allowed = DatasetRecord {
        project,
        request_id: first,
        file_name: "GTT cohort 3.csv",
        media_type: "text/csv",
        size_bytes: 612,
        sha256: &file_sha,
        guard_intake: intake,
        outcome: "allowed",
        findings: &none,
        policy: POLICY,
        grid: Some((&copy_sha, copy.len() as i64)),
    };
    let dataset = api.record_dataset(a, &allowed).await.unwrap();
    let dataset_id: Uuid = serde_json::from_value(dataset["dataset_id"].clone()).unwrap();
    let intake_task: Uuid = serde_json::from_value(dataset["intake_task"].clone()).unwrap();
    assert_eq!(dataset["guard"]["outcome"], "allowed");
    assert_eq!(dataset["grid_sha256"], copy_sha);
    assert_eq!(api.record_dataset(a, &allowed).await.unwrap(), dataset);
    assert_eq!(
        api.dataset_upload_check(a, project, first).await.unwrap(),
        json!({ "existing": dataset })
    );
    let other_sha = sha(b"another file");
    let conflict = api
        .record_dataset(
            a,
            &DatasetRecord {
                sha256: &other_sha,
                ..allowed.clone()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        (conflict.kind, conflict.code),
        (StoreErrorKind::Conflict, "idempotency_conflict")
    );
    // A file the guard holds is recorded with its findings and has no intake; what the record cannot hold is refused.
    let held_findings = json!(["yara_medium"]);
    let held = api
        .record_dataset(
            a,
            &DatasetRecord {
                request_id: Uuid::new_v4(),
                file_name: "plate reader.xlsm",
                media_type: "application/vnd.ms-excel.sheet.macroenabled.12",
                sha256: &other_sha,
                guard_intake: Uuid::new_v4(),
                outcome: "quarantined",
                findings: &held_findings,
                grid: None,
                ..allowed.clone()
            },
        )
        .await
        .unwrap();
    assert_eq!(
        (&held["intake_task"], &held["intake_waiting"]),
        (&Value::Null, &json!(false))
    );
    assert_eq!(held["guard"]["findings"], held_findings);

    // A database that ran the draft of migration 059 that tied an intake task to every grid keeps that
    // check, which refuses an allowed file whose intake waits. Migrating again replaces it and keeps the rows; the
    // allowed file below is then recorded.
    owner
        .batch_execute(
            "ALTER TABLE enrichedlab_agent.broker_datasets DROP CONSTRAINT broker_datasets_task_needs_grid;
             ALTER TABLE enrichedlab_agent.broker_datasets ADD CHECK ((grid_sha256 IS NULL) = (grid_bytes IS NULL)
               AND (grid_sha256 IS NULL) = (intake_task_id IS NULL))",
        )
        .await
        .unwrap();
    let draft_sha = sha(b"a file recorded under the draft's check");
    let under_draft = api
        .record_dataset(
            a,
            &DatasetRecord {
                request_id: Uuid::new_v4(),
                file_name: "under draft.csv",
                sha256: &draft_sha,
                guard_intake: Uuid::new_v4(),
                ..allowed.clone()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        (under_draft.kind, under_draft.code),
        (StoreErrorKind::Invalid, "dataset_invalid")
    );
    let count = "SELECT count(*) FROM enrichedlab_agent.broker_datasets";
    let rows_before: i64 = owner.query_one(count, &[]).await.unwrap().get(0);
    enrichedlab_agent_sandbox::broker::store::migrate(
        enrichedlab_agent_sandbox::broker::config::MigrationArgs {
            migration_dsn_env: "AGENT_SANDBOX_BROKER_MIGRATION_DSN".into(),
            migration_dsn_file: Some(PathBuf::from(&dsn_file)),
        },
    )
    .await
    .unwrap();
    let rows_after: i64 = owner.query_one(count, &[]).await.unwrap().get(0);
    assert_eq!((rows_before, rows_after), (2, 2));
    let checks: Vec<String> = owner
        .query(
            "SELECT conname FROM pg_constraint
              WHERE conrelid = 'enrichedlab_agent.broker_datasets'::regclass AND contype = 'c'
                AND position('intake_task_id' in pg_get_constraintdef(oid)) > 0",
            &[],
        )
        .await
        .unwrap()
        .iter()
        .map(|row| row.get(0))
        .collect();
    assert_eq!(checks, ["broker_datasets_task_needs_grid"]);
    for bad in [
        DatasetRecord {
            request_id: Uuid::new_v4(),
            grid: None,
            ..allowed.clone()
        },
        DatasetRecord {
            request_id: Uuid::new_v4(),
            file_name: "../escape.csv",
            ..allowed.clone()
        },
        DatasetRecord {
            request_id: Uuid::new_v4(),
            outcome: "blocked",
            ..allowed.clone()
        },
    ] {
        let refusal = api.record_dataset(a, &bad).await.unwrap_err();
        assert_eq!(
            (refusal.kind, refusal.code),
            (StoreErrorKind::Invalid, "dataset_invalid")
        );
    }

    // A dataset whose guard copy does not match the digest the decision named: same size, other bytes.
    let mut tampered = copy.clone();
    let last = tampered.len() - 1;
    tampered[last] ^= 0xff;
    let lost_intake = Uuid::new_v4();
    guard.copies.lock().unwrap().insert(lost_intake, tampered);
    let lost_sha = sha(b"a file whose copy changed");
    let lost = api
        .record_dataset(
            a,
            &DatasetRecord {
                request_id: Uuid::new_v4(),
                file_name: "changed.csv",
                sha256: &lost_sha,
                guard_intake: lost_intake,
                ..allowed.clone()
            },
        )
        .await
        .unwrap();
    let lost_id: Uuid = serde_json::from_value(lost["dataset_id"].clone()).unwrap();

    // A preparation waits for the profile.
    let spec = tolerance_spec();
    let prepare = |spec: &Value, title: &str, previous: Option<Uuid>, request_id: Uuid| {
        let api = api.clone();
        let spec = spec.clone();
        let title = title.to_owned();
        async move {
            api.request_dataset_preparation(
                a,
                &PreparationRequest {
                    project,
                    dataset: dataset_id,
                    spec: &spec,
                    title: &title,
                    previous,
                    request_id,
                },
            )
            .await
        }
    };
    let refusal = prepare(&spec, "", None, Uuid::new_v4()).await.unwrap_err();
    assert_eq!(
        (refusal.kind, refusal.code),
        (StoreErrorKind::Conflict, "dataset_not_profiled")
    );

    let root = tempfile::tempdir().unwrap();
    fs::create_dir_all(root.path().join("artifacts")).unwrap();
    let guard_url = guard_service(guard.clone()).await;
    private_file(&root.path().join("fetcher.token"), &format!("{TOKEN}\n"));
    private_file(
        &root.path().join("fetcher.json"),
        &json!({"schema": "enrichedlab.guard-fetcher.v1", "service_url": guard_url,
            "token_file": root.path().join("fetcher.token")})
        .to_string(),
    );
    let mut config = WorkerConfig {
        database_dsn: dsn.replace("user=postgres", "user=broker_worker_qual"),
        tenant: "research-site".into(),
        artifact_root: root.path().join("artifacts"),
        controller_root: root.path().to_path_buf(),
        controller_state: root.path().to_path_buf(),
        controller_config: root.path().to_path_buf(),
        controller_bin: PathBuf::from("/bin/false"),
        openshell_bin: PathBuf::from("/bin/false"),
        docker_bin: PathBuf::from("/bin/false"),
        worker_id: "dataset-worker".into(),
        direct_dispatch: None,
        query_connection_dir: None,
        render_url: None,
        dataset_fetcher_config: Some(root.path().join("fetcher.json")),
        live_output: false,
        max_parallel: 1,
        lease_seconds: 120,
        poll_interval: Duration::from_millis(200),
        database_pool: 2,
        max_workspace_files: 10,
        max_workspace_bytes: 1024,
    };
    let package = |task: Uuid| {
        let owner = &owner;
        let root = root.path().join("artifacts");
        async move {
            let row = owner
                .query_one(
                    "SELECT relative_path, sha256 FROM enrichedlab_agent.broker_artifacts
                     WHERE tenant_id='research-site' AND task_id=$1 AND role='result'",
                    &[&task],
                )
                .await
                .unwrap();
            let bytes = fs::read(root.join(row.get::<_, String>(0))).unwrap();
            assert_eq!(sha(&bytes), row.get::<_, String>(1));
            (row.get::<_, String>(1), members(&bytes))
        }
    };

    // The intake: only its own lease reads the request or records a receipt, and a receipt must fit the operation.
    let claimed = worker
        .claim_task("dataset-worker", 120)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (claimed.id, claimed.route.as_str()),
        (intake_task, dataset_run::ROUTE)
    );
    assert!(
        worker
            .task_dataset(
                claimed.id,
                "another-worker",
                claimed.attempt,
                claimed.lease_generation
            )
            .await
            .unwrap()
            .is_none()
    );
    let request = worker
        .task_dataset(
            claimed.id,
            "dataset-worker",
            claimed.attempt,
            claimed.lease_generation,
        )
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            request.operation.as_str(),
            request.grid_sha256.as_str(),
            request.guard_intake
        ),
        ("intake", copy_sha.as_str(), intake)
    );
    assert!(request.intake_package.is_none() && request.spec_text.is_none());
    let now = OffsetDateTime::now_utc();
    let receipt = |status: &'static str, table: bool| DatasetReceipt {
        status,
        error_code: (status == "failed").then(|| "dataset_guard_unavailable".to_owned()),
        profile: None,
        summary: None,
        grid_sha256: None,
        table: table.then(|| (TABLE.to_owned(), "a".repeat(64), 1, json!(["mouse_id"]))),
        package_sha256: table.then(|| "b".repeat(64)),
        executor: "enrichedlab.dataset-prepare",
        executor_version: 1,
        started_at: now,
        finished_at: now,
    };
    assert_eq!(
        worker
            .record_dataset_receipt(
                claimed.id,
                "dataset-worker",
                claimed.attempt,
                claimed.lease_generation + 1,
                &receipt("failed", false),
            )
            .await
            .unwrap_err()
            .code,
        "lease_fence_lost"
    );
    let refusal = worker
        .record_dataset_receipt(
            claimed.id,
            "dataset-worker",
            claimed.attempt,
            claimed.lease_generation,
            &receipt("prepared", true),
        )
        .await
        .unwrap_err();
    assert_eq!(
        (refusal.kind, refusal.code),
        (StoreErrorKind::Invalid, "dataset_receipt_invalid")
    );
    dataset_run::execute(worker.clone(), &config, claimed, CancellationToken::new())
        .await
        .unwrap();
    let detail = api.dataset_detail(a, project, dataset_id).await.unwrap();
    assert_eq!(detail["intake"]["status"], "completed");
    let receipts = detail["intake"]["receipts"].as_array().unwrap();
    assert_eq!(receipts.len(), 1);
    assert_eq!(
        (&receipts[0]["status"], &receipts[0]["grid_sha256"]),
        (&json!("profiled"), &json!(copy_sha))
    );
    let profile = &receipts[0]["profile"];
    assert_eq!(profile["schema"], "enrichedlab.dataset-profile.v1");
    let found = &profile["sheets"][0]["table"];
    assert_eq!(
        (
            &found["header_rows"],
            &found["units_row"],
            &found["data_rows"]
        ),
        (&json!([3, 4]), &json!(5), &json!(5))
    );
    // The profile names columns and counts; it holds no cell of the data rows.
    let text = profile.to_string();
    for cell in ["M-0417", "M-0419", "31,2", "<0.2", "M-0420 died"] {
        assert!(!text.contains(cell), "{cell}");
    }
    let (intake_sha, kept) = package(intake_task).await;
    assert_eq!(receipts[0]["package_sha256"], intake_sha);
    let names: Vec<&str> = kept.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        [
            "README.md",
            "profile.json",
            "profile_detail.json",
            "source/grid.ndjson.gz",
            "spec.json",
            "SHA256SUMS"
        ]
    );
    assert_eq!(kept[3].1, copy);
    sums_match(&kept);
    assert_eq!(guard.fetched.lock().unwrap().as_slice(), [intake]);

    // A copy that does not match the guard's decision is never profiled: the intake fails and names why.
    let claimed = worker
        .claim_task("dataset-worker", 120)
        .await
        .unwrap()
        .unwrap();
    dataset_run::execute(worker.clone(), &config, claimed, CancellationToken::new())
        .await
        .unwrap();
    let changed = api.dataset_detail(a, project, lost_id).await.unwrap();
    assert_eq!(
        (
            &changed["intake"]["status"],
            &changed["intake"]["receipts"][0]["status"],
            &changed["intake"]["receipts"][0]["error_code"]
        ),
        (
            &json!("failed"),
            &json!("failed"),
            &json!("dataset_guard_unavailable")
        )
    );

    // What the broker can check about a preparation, it refuses before a task exists.
    for (actor, dataset, spec, previous, kind, code) in [
        (
            outsider,
            dataset_id,
            spec.clone(),
            None,
            StoreErrorKind::NotFound,
            "project_not_found",
        ),
        (
            a,
            Uuid::new_v4(),
            spec.clone(),
            None,
            StoreErrorKind::NotFound,
            "dataset_not_found",
        ),
        (
            a,
            lost_id,
            spec.clone(),
            None,
            StoreErrorKind::Conflict,
            "dataset_not_profiled",
        ),
        (
            a,
            serde_json::from_value(held["dataset_id"].clone()).unwrap(),
            spec.clone(),
            None,
            StoreErrorKind::Conflict,
            "dataset_not_profiled",
        ),
        (
            a,
            dataset_id,
            json!({"schema": "another.schema"}),
            None,
            StoreErrorKind::Invalid,
            "dataset_preparation_invalid",
        ),
        (
            a,
            dataset_id,
            spec.clone(),
            Some(intake_task),
            StoreErrorKind::Invalid,
            "dataset_previous_invalid",
        ),
    ] {
        let refusal = api
            .request_dataset_preparation(
                actor,
                &PreparationRequest {
                    project,
                    dataset,
                    spec: &spec,
                    title: "",
                    previous,
                    request_id: Uuid::new_v4(),
                },
            )
            .await
            .unwrap_err();
        assert_eq!((refusal.kind, refusal.code), (kind, code), "{code}");
    }
    let asked = Uuid::new_v4();
    let prepared_task = prepare(&spec, "Tolerance, one row per time point", None, asked)
        .await
        .unwrap();
    assert_eq!(
        prepare(&spec, "Tolerance, one row per time point", None, asked)
            .await
            .unwrap(),
        prepared_task
    );
    assert_eq!(
        prepare(&spec, "Another title", None, asked)
            .await
            .unwrap_err()
            .code,
        "idempotency_conflict"
    );
    // Without the decision about the repeated mouse, the preparation stops and names both rows.
    let mut repeated = spec.clone();
    repeated["drop_rows"] = json!([]);
    let refused_task = prepare(&repeated, "", Some(prepared_task), Uuid::new_v4())
        .await
        .unwrap();

    // The change feed (migration 061) names the dataset with each step's status, so its page follows the steps.
    let before_steps = api.read_changes(a, None, 2_000).await.unwrap().position;
    for expected in [prepared_task, refused_task] {
        let claimed = worker
            .claim_task("dataset-worker", 120)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.id, expected);
        let request = worker
            .task_dataset(
                claimed.id,
                "dataset-worker",
                claimed.attempt,
                claimed.lease_generation,
            )
            .await
            .unwrap()
            .unwrap();
        assert_eq!(request.operation, "preparation");
        assert_eq!(request.intake_package.unwrap().sha256, intake_sha);
        assert_eq!(
            Some(sha(request.spec_text.unwrap().as_bytes())),
            request.spec_sha256
        );
        dataset_run::execute(worker.clone(), &config, claimed, CancellationToken::new())
            .await
            .unwrap();
    }
    let steps = api
        .read_changes(a, Some(&before_steps), 2_000)
        .await
        .unwrap();
    for key in [
        format!("dataset:{dataset_id}"),
        format!("task:{prepared_task}"),
        format!("task:{refused_task}"),
    ] {
        assert!(steps.keys.contains(&key), "{key} in {:?}", steps.keys);
    }
    let detail = api.dataset_detail(a, project, dataset_id).await.unwrap();
    let preparations = detail["preparations"].as_array().unwrap();
    assert_eq!(
        preparations
            .iter()
            .map(|p| p["task_id"].clone())
            .collect::<Vec<_>>(),
        [json!(refused_task), json!(prepared_task)]
    );
    let prepared = &preparations[1];
    assert_eq!(
        (&prepared["status"], &prepared["spec"]),
        (&json!("completed"), &spec)
    );
    let made = &prepared["receipts"][0];
    assert_eq!(
        (
            &made["status"],
            &made["table_path"],
            &made["table_rows"],
            &made["grid_sha256"]
        ),
        (
            &json!("prepared"),
            &json!(TABLE),
            &json!(20),
            &json!(copy_sha)
        )
    );
    assert_eq!(
        made["table_columns"],
        json!([
            "mouse_id",
            "sex",
            "diet",
            "body_weight",
            "insulin",
            "insulin_censored",
            "insulin_limit",
            "time_min",
            "glucose"
        ])
    );
    assert_eq!(made["summary"]["spec_sha256"], prepared["spec_sha256"]);
    assert_eq!(made["summary"]["rows"]["written"], 20);
    assert_eq!(
        made["summary"]["columns"][1]["recoded"],
        json!({"MALE -> Male": 1, "male -> Male": 1})
    );
    let (prepared_sha, files) = package(prepared_task).await;
    assert_eq!(made["package_sha256"], prepared_sha);
    let names: Vec<&str> = files.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(
        names,
        [
            "README.md",
            "exports/gtt_cohort3.spreadsheet.csv",
            "preparation.json",
            "receipt.json",
            TABLE,
            "spec.json",
            "SHA256SUMS"
        ]
    );
    let member = |name: &str| &files.iter().find(|(n, _)| n == name).unwrap().1;
    assert_eq!(
        String::from_utf8(member(TABLE).clone()).unwrap(),
        expected_table()
    );
    assert_eq!(made["table_sha256"], sha(member(TABLE)));
    // The package keeps the specification as the broker stored it: its digest is the one the request recorded.
    assert_eq!(prepared["spec_sha256"], sha(member("preparation.json")));
    assert!(member("exports/gtt_cohort3.spreadsheet.csv").starts_with("\u{feff}".as_bytes()));
    sums_match(&files);

    let refused = &preparations[0];
    assert_eq!(
        (
            &refused["status"],
            &refused["error_code"],
            &refused["previous"]
        ),
        (
            &json!("failed"),
            &json!("dataset_key_repeated"),
            &json!(prepared_task)
        )
    );
    let stopped = &refused["receipts"][0];
    assert_eq!(
        (
            &stopped["status"],
            &stopped["error_code"],
            &stopped["table_path"]
        ),
        (
            &json!("refused"),
            &json!("dataset_key_repeated"),
            &Value::Null
        )
    );
    assert_eq!(stopped["summary"]["repeated_key_count"], 5);
    assert_eq!(
        stopped["summary"]["repeated_keys"][0],
        json!({"row": 9, "first_row": 8})
    );
    let kept: i64 = owner
        .query_one(
            "SELECT count(*) FROM enrichedlab_agent.broker_artifacts WHERE tenant_id='research-site' AND task_id=$1",
            &[&refused_task],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(kept, 0);

    // The result digest lists the prepared table with its columns and counts, never its cells; the copy for
    // spreadsheets is not a result table.
    let digest: Value = owner
        .query_one(
            "SELECT d.digest FROM enrichedlab_agent.broker_artifact_digests d
             JOIN enrichedlab_agent.broker_artifacts a ON a.tenant_id=d.tenant_id AND a.artifact_id=d.artifact_id
             WHERE a.tenant_id='research-site' AND a.task_id=$1",
            &[&prepared_task],
        )
        .await
        .unwrap()
        .get(0);
    let listed = digest["tables"].as_array().unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(
        (
            &listed[0]["path"],
            &listed[0]["rows"],
            &listed[0]["cells_omitted"]
        ),
        (&json!(TABLE), &json!(20), &json!("lab_data"))
    );
    assert!(listed[0].get("cells").is_none());
    for cell in ["M-0417", "31.2", "not_detected"] {
        assert!(!digest.to_string().contains(cell), "{cell}");
    }

    // Readers: the project's datasets, newest first, and nothing for a person outside the project.
    let listing = api.project_datasets(a, project).await.unwrap();
    let ids: Vec<Value> = listing
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["dataset_id"].clone())
        .collect();
    assert_eq!(
        ids,
        [
            json!(lost_id),
            held["dataset_id"].clone(),
            json!(dataset_id)
        ]
    );
    let shown = &listing[2];
    assert_eq!(
        (
            &shown["intake"]["receipt"]["status"],
            &shown["intake"]["receipt"]["sheets"]
        ),
        (&json!("profiled"), &json!(1))
    );
    assert_eq!(
        (
            &shown["preparations"][1]["table_name"],
            &shown["preparations"][1]["receipt"]["table_rows"]
        ),
        (&json!("gtt_cohort3"), &json!(20))
    );
    for refusal in [
        api.project_datasets(outsider, project).await.unwrap_err(),
        api.dataset_detail(outsider, project, dataset_id)
            .await
            .unwrap_err(),
    ] {
        assert_eq!(refusal.code, "project_not_found");
    }
    assert_eq!(
        api.dataset_detail(a, workload_project, dataset_id)
            .await
            .unwrap_err()
            .code,
        "project_not_found"
    );
    // A service account that may read and contribute to the project lists its datasets, but the lab's cells reach
    // people only: it reads neither a dataset's detail nor the files of its intake and preparations, though it may read
    // those tasks, and it cannot start an intake.
    api.grant_project(
        a,
        project,
        &ProjectGrant {
            principal_id: workload,
            capabilities: vec![Capability::Read, Capability::Contribute],
        },
    )
    .await
    .unwrap();
    assert!(
        api.project_datasets(workload, project)
            .await
            .unwrap()
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["dataset_id"] == json!(dataset_id))
    );
    for refusal in [
        api.dataset_detail(workload, project, dataset_id)
            .await
            .unwrap_err(),
        api.start_dataset_intake(workload, project, dataset_id)
            .await
            .unwrap_err(),
    ] {
        assert_eq!(refusal.code, "person_required");
    }
    for task in [intake_task, prepared_task] {
        let row = owner
            .query_one(
                "SELECT artifact_id, enrichedlab_agent.broker_task_can('research-site', $2, $1, 'read')
                 FROM enrichedlab_agent.broker_artifacts
                 WHERE tenant_id='research-site' AND task_id=$1 AND role='result'",
                &[&task, &workload],
            )
            .await
            .unwrap();
        let artifact: Uuid = row.get(0);
        assert!(
            row.get::<_, bool>(1),
            "the service account may read the task"
        );
        assert_eq!(
            api.get_artifact(workload, task, artifact)
                .await
                .unwrap_err()
                .code,
            "artifact_not_found"
        );
        assert!(api.get_artifact(a, task, artifact).await.is_ok());
    }
    // The cells are withheld when a digest is read, not only when it is written, so a digest that
    // holds a lab table's cells (as one written before lab tables were classified would) reaches a person whole and the
    // service account without them; the headline statistics keep theirs.
    let legacy_artifact = Uuid::new_v4();
    let legacy = json!({"schema": "enrichedlab.result-digest.v1", "status": "summarized", "figures": [],
    "tables": [
        {"path": TABLE, "rows": 2, "columns": ["mouse_id", "glucose"], "cells": [["M-9001", "101"], ["M-9002", "102"]]},
        {"path": "results/statistics.csv", "rows": 1, "columns": ["statistic", "value"], "cells": [["n", "2"]]}
    ]});
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_artifacts
             SELECT (jsonb_populate_record(NULL::enrichedlab_agent.broker_artifacts, to_jsonb(a)
               || jsonb_build_object('artifact_id', $1::uuid, 'sha256', repeat('9', 64),
                                     'relative_path', 'outputs/legacy-results.tar.gz'))).*
             FROM enrichedlab_agent.broker_artifacts a
             WHERE a.tenant_id='research-site' AND a.task_id=$2 AND a.role='result' LIMIT 1",
            &[&legacy_artifact, &intake_task],
        )
        .await
        .unwrap();
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_artifact_digests(tenant_id,artifact_id,artifact_sha256,task_id,attempt,
               lease_generation,schema_version,status,digest,digest_sha256)
             VALUES('research-site',$1,repeat('9',64),$2,1,1,'enrichedlab.result-digest.v1','summarized',$3,repeat('a',64))",
            &[&legacy_artifact, &intake_task, &legacy],
        )
        .await
        .unwrap();
    let as_person = api
        .artifact_digest(a, intake_task, legacy_artifact)
        .await
        .unwrap();
    assert_eq!(as_person["digest"], legacy);
    let as_workload = api
        .artifact_digest(workload, intake_task, legacy_artifact)
        .await
        .unwrap();
    let tables = as_workload["digest"]["tables"].as_array().unwrap();
    assert!(tables[0].get("cells").is_none());
    assert_eq!(
        (
            &tables[0]["cells_omitted"],
            &tables[0]["rows"],
            &tables[0]["columns"]
        ),
        (
            &json!("lab_data"),
            &json!(2),
            &json!(["mouse_id", "glucose"])
        )
    );
    assert_eq!(tables[1]["cells"], json!([["n", "2"]]));
    assert!(!as_workload.to_string().contains("M-9001"));
    // A dataset's record does not change, except that an allowed dataset's intake task is set once: not its name, not
    // a second task, not a task for a file without a grid, and it is never removed.
    let held_id = held["dataset_id"].as_str().unwrap();
    for (sql, reason) in [
        (
            format!(
                "UPDATE enrichedlab_agent.broker_datasets SET file_name='renamed.csv' WHERE dataset_id='{dataset_id}'"
            ),
            "append-only",
        ),
        (
            format!(
                "UPDATE enrichedlab_agent.broker_datasets SET intake_task_id='{prepared_task}'
                 WHERE dataset_id='{dataset_id}'"
            ),
            "append-only",
        ),
        (
            format!(
                "UPDATE enrichedlab_agent.broker_datasets SET intake_task_id='{prepared_task}'
                 WHERE dataset_id='{held_id}'"
            ),
            "check constraint",
        ),
        (
            format!(
                "DELETE FROM enrichedlab_agent.broker_datasets WHERE dataset_id='{dataset_id}'"
            ),
            "append-only",
        ),
    ] {
        let error = owner.batch_execute(&sql).await.unwrap_err();
        let message = error.as_db_error().unwrap().message().to_owned();
        assert!(message.contains(reason), "{sql}: {message}");
    }

    // Lineage: a person's request is the basis, the reviewed executor ran in the image the method names, an intake read
    // the person's file as the guard rebuilt it and a preparation read the intake's kept grid.
    for (task, operation, class) in [
        (intake_task, "intake", "object_data"),
        (prepared_task, "preparation", "prior_output"),
    ] {
        let lineage = api.task_lineage(a, task).await.unwrap();
        assert_eq!(lineage["method"]["tool_class"], "vetted_service");
        assert_eq!(lineage["inputs"]["classes"], json!([class]));
        assert_eq!(lineage["inputs"]["classes_recorded_by"], "dataset_request");
        assert_eq!(lineage["origin"]["origin"], "person");
        assert_eq!(lineage["dataset"]["operation"], operation);
        assert_eq!(lineage["dataset"]["file_sha256"], file_sha);
        assert_eq!(lineage["dataset"]["guard_policy"], POLICY);
        assert_eq!(lineage["review"]["state"], "not_reviewed");
        let gaps = lineage["gaps"].as_array().unwrap();
        for gap in [
            "plan_step_not_recorded",
            "inputs_not_recorded",
            "input_class_not_recorded",
            "no_outputs_recorded",
            "image_not_digest_pinned",
            "origin_not_recorded",
        ] {
            assert!(!gaps.iter().any(|g| g == gap), "{gap} in {lineage}");
        }
    }
    let lineage = api.task_lineage(a, prepared_task).await.unwrap();
    assert_eq!(
        (
            &lineage["dataset"]["spec_sha256"],
            &lineage["dataset"]["table_sha256"]
        ),
        (&prepared["spec_sha256"], &made["table_sha256"])
    );

    // The project's contents name each dataset task's operation and the preparation it changes.
    let read = api.read_project(a, project).await.unwrap();
    let entry = |task: Uuid| {
        read["tasks"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["task_id"] == json!(task))
            .unwrap()["dataset"]
            .clone()
    };
    assert_eq!(
        (
            entry(intake_task)["operation"].clone(),
            entry(intake_task)["dataset"].clone()
        ),
        (json!("intake"), json!(dataset_id))
    );
    assert_eq!(
        (
            entry(refused_task)["operation"].clone(),
            entry(refused_task)["previous"].clone()
        ),
        (json!("preparation"), json!(prepared_task))
    );

    // Dataset tasks ready the lab's data for analyses: they neither wait in the inbox for review nor count as analyses.
    let inbox = api.inbox(a).await.unwrap();
    for task in [intake_task, prepared_task, refused_task] {
        assert!(
            !inbox["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|i| i["task_id"] == json!(task)),
            "{inbox}"
        );
    }
    let counted = |projects: Vec<Value>| {
        projects
            .into_iter()
            .find(|p| p["project_id"] == json!(project))
            .unwrap()["task_counts"]["total"]
            .clone()
    };
    assert_eq!(counted(api.list_projects(a, 50).await.unwrap()), 0);

    // The prepared table is a result table like any other: a person may ask for a template analysis of it.
    owner
        .batch_execute(
            "INSERT INTO enrichedlab_agent.broker_group_profile_grants(tenant_id,group_key,profile_key)
               VALUES('research-site','pilot-lab','template-analysis');",
        )
        .await
        .unwrap();
    config.render_url = Some(render_service().await);
    analysis_run::refresh_catalog(&worker, &config)
        .await
        .unwrap();
    let settings = json!({"value": "glucose", "group": "diet"});
    let analysis = api
        .request_analysis(
            a,
            &AnalysisRequest {
                project,
                source_task: prepared_task,
                table_path: TABLE,
                feature: "summary_stats",
                settings: &settings,
                title: "",
                previous: None,
                request_id: Uuid::new_v4(),
            },
        )
        .await
        .unwrap();
    // The analysis made from the prepared table is one.
    assert_eq!(counted(api.list_projects(a, 50).await.unwrap()), 1);
    // The start dialog's methods: neither runs on its own, and a research run may hold the template-analysis method but
    // not the dataset method.
    let me = api.me(a).await.unwrap();
    let choice = |key: &str| {
        me.profiles
            .iter()
            .find(|p| p.key == key)
            .map(|p| (p.offered, p.in_runs))
    };
    assert_eq!(choice("lab-dataset"), Some((false, false)));
    assert_eq!(choice("template-analysis"), Some((false, true)));
    let lineage = api.task_lineage(a, analysis).await.unwrap();
    assert_eq!(
        (
            &lineage["analysis"]["table_path"],
            &lineage["analysis"]["table_sha256"]
        ),
        (&json!(TABLE), &made["table_sha256"])
    );

    // An analysis of an analysis holds the lab's data whatever its table is called, through any number
    // of steps; a task whose sources hold none does not.
    let descend = |source: Uuid, key: &'static str| {
        let owner = &owner;
        async move {
            let child: Uuid = owner
                .query_one(
                    "SELECT enrichedlab_agent.broker_submit_project_task('research-site',$1,$2,$3,NULL,'pilot-lab',
                       'template-analysis','A later analysis',$3,repeat('e',64))",
                    &[&a, &project, &key],
                )
                .await
                .unwrap()
                .get(0);
            owner
                .execute(
                    "INSERT INTO enrichedlab_agent.broker_analysis_requests
                     SELECT (jsonb_populate_record(NULL::enrichedlab_agent.broker_analysis_requests, to_jsonb(r)
                       || jsonb_build_object('task_id', $1::uuid, 'source_task_id', $2::uuid,
                                             'table_path', 'results/statistics.csv'))).*
                     FROM enrichedlab_agent.broker_analysis_requests r WHERE r.task_id=$3",
                    &[&child, &source, &analysis],
                )
                .await
                .unwrap();
            child
        }
    };
    let holds = |task: Uuid| {
        let owner = &owner;
        async move {
            owner
                .query_one(
                    "SELECT enrichedlab_agent.broker_task_holds_lab_data('research-site', $1)",
                    &[&task],
                )
                .await
                .unwrap()
                .get::<_, bool>(0)
        }
    };
    // A task that holds none of the lab's data keeps a digest as stored.
    let plain: Uuid = owner
        .query_one(
            "SELECT enrichedlab_agent.broker_submit_project_task('research-site',$1,$2,'plain-step',NULL,'pilot-lab',
               'template-analysis','A step on no lab table','plain-step',repeat('e',64))",
            &[&a, &project],
        )
        .await
        .unwrap()
        .get(0);
    let read_as_lead = |task: Uuid, digest: Value| {
        let owner = &owner;
        async move {
            owner
                .query_one(
                    "SELECT enrichedlab_agent.broker_digest_without_lab_cells('research-site', $1, $2)",
                    &[&task, &digest],
                )
                .await
                .unwrap()
                .get::<_, Value>(0)
        }
    };
    let ordinary = json!({"schema": "enrichedlab.result-digest.v1", "status": "summarized", "figures": [],
    "tables": [
        {"path": "results/results.csv", "rows": 1, "columns": ["group", "mean"], "cells": [["a", "4.5"]]},
        {"path": "results/statistics.csv", "rows": 1, "columns": ["statistic", "value"], "cells": [["n", "2"]]}
    ]});
    assert_eq!(read_as_lead(plain, ordinary.clone()).await, ordinary);
    // A table under a lab folder loses its cells whatever task's digest lists it.
    let by_path = read_as_lead(plain, legacy.clone()).await;
    assert_eq!(
        by_path["tables"][0],
        json!({"path": TABLE, "rows": 2, "columns": ["mouse_id", "glucose"], "cells_omitted": "lab_data"})
    );
    assert_eq!(by_path["tables"][1], legacy["tables"][1]);
    let second = descend(analysis, "descendant-2").await;
    let third = descend(second, "descendant-3").await;
    let unrelated = descend(plain, "descendant-of-plain").await;
    assert!(holds(analysis).await && holds(second).await && holds(third).await);
    assert!(!holds(plain).await && !holds(unrelated).await);
    // Migration 073: a column summary's smallest and largest values leave a table wherever its cells do, also when the
    // cells were already left out for size; its counts stay. A task that holds none of the lab's data keeps every value.
    let summarized = json!({"schema": "enrichedlab.result-digest.v1", "status": "summarized", "figures": [],
    "tables": [
        {"path": "results/results.csv", "rows": 1, "columns": ["group", "mean"], "cells": [["a", "4.5"]],
         "summary": {"group": {"numeric": false, "distinct": 1}, "mean": {"numeric": true, "min": "4.5", "max": "4.5", "distinct": 1}}},
        {"path": "results/wide.csv", "rows": 900, "columns": ["mean"], "cells_omitted": "too_large",
         "summary": {"mean": {"numeric": true, "min": "0.1", "max": "9.9", "distinct": 900}}},
        {"path": "results/statistics.csv", "rows": 1, "columns": ["statistic", "value"], "cells": [["n", "2"]],
         "summary": {"statistic": {"numeric": false, "distinct": 1}, "value": {"numeric": true, "min": "2", "max": "2", "distinct": 1}}}
    ]});
    let lab_read = read_as_lead(analysis, summarized.clone()).await;
    assert_eq!(
        lab_read["tables"][0],
        json!({"path": "results/results.csv", "rows": 1, "columns": ["group", "mean"], "cells_omitted": "lab_derived",
            "summary": {"group": {"numeric": false, "distinct": 1}, "mean": {"numeric": true, "distinct": 1}}})
    );
    assert_eq!(
        lab_read["tables"][1],
        json!({"path": "results/wide.csv", "rows": 900, "columns": ["mean"], "cells_omitted": "too_large",
            "summary": {"mean": {"numeric": true, "distinct": 900}}})
    );
    assert_eq!(lab_read["tables"][2], summarized["tables"][2]);
    assert_eq!(read_as_lead(plain, summarized.clone()).await, summarized);
    // Migration 073: a package carries every result's files and digest whole, so a reader that is not a person gets none
    // of a project whose tasks hold the lab's data, even with read access; a person does, and the same service account
    // still gets one of a project that holds none.
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_project_grants(tenant_id,project_id,principal_id,capabilities)
             VALUES('research-site',$1,$2,ARRAY['read']) ON CONFLICT DO NOTHING",
            &[&project, &workload],
        )
        .await
        .unwrap();
    let package_scope = |actor: Uuid, project: Uuid| {
        let owner = &owner;
        async move {
            owner
                .query_one(
                    "SELECT enrichedlab_agent.broker_project_can('research-site',$1,$2,'read'),
                       enrichedlab_agent.broker_project_package_scope('research-site',$1,$2)",
                    &[&actor, &project],
                )
                .await
                .map(|row| (row.get::<_, bool>(0), row.get::<_, Option<String>>(1)))
                .unwrap()
        }
    };
    assert_eq!(package_scope(workload, project).await, (true, None));
    assert!(package_scope(a, project).await.1.is_some());
    assert!(package_scope(workload, workload_project).await.1.is_some());

    // A project parent is a source, the walk has no depth limit and ends on a cycle, and a task given a
    // lab object or an earlier run's output, or one whose digest lists a table under a lab folder, holds the lab's data.
    let step = |parent: Option<Uuid>, key: String| {
        let owner = &owner;
        async move {
            owner
                .query_one(
                    "SELECT enrichedlab_agent.broker_submit_project_task('research-site',$1,$2,$3,$4,'pilot-lab',
                       'template-analysis','A later step',$3,repeat('e',64))",
                    &[&a, &project, &key, &parent],
                )
                .await
                .unwrap()
                .get::<_, Uuid>(0)
        }
    };
    // The chain's steps stay queued, so the lab's queue gets room for them.
    owner
        .batch_execute(
            "UPDATE enrichedlab_agent.broker_research_groups SET max_queued_tasks=1000
             WHERE tenant_id='research-site' AND group_key='pilot-lab';",
        )
        .await
        .unwrap();
    let mut chain = analysis;
    for n in 0..70 {
        chain = step(Some(chain), format!("chain-{n}")).await;
    }
    assert!(
        holds(chain).await,
        "a step 70 parents after the lab's data holds it"
    );
    let plain_child = step(Some(plain), "plain-child".into()).await;
    assert!(!holds(plain_child).await);
    for (key, class, source) in [
        (
            "object-input",
            "object_data",
            json!({"source_version": "v1", "source_sha256": "1".repeat(64)}),
        ),
        (
            "prior-input",
            "prior_output",
            json!({"source_pvc_uid": "pvc-1", "source_run_id": "run-1"}),
        ),
    ] {
        let given = step(None, key.into()).await;
        assert!(!holds(given).await, "{key} holds nothing before its input");
        owner
            .execute(
                "INSERT INTO enrichedlab_agent.broker_attempt_inputs(tenant_id,task_id,attempt,lease_generation,input_ref,
                   input_class,sealed_sha256,size_bytes,custody_transaction,source)
                 VALUES('research-site',$1,1,1,'input://volume/one',$2,repeat('2',64),10,'custody-1',$3)",
                &[&given, &class, &source],
            )
            .await
            .unwrap();
        assert!(holds(given).await, "{key}");
        assert!(
            holds(step(Some(given), format!("{key}-child")).await).await,
            "{key} child"
        );
    }
    let derived = step(None, "lab-folder-digest".into()).await;
    let derived_artifact = Uuid::new_v4();
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_artifacts
             SELECT (jsonb_populate_record(NULL::enrichedlab_agent.broker_artifacts, to_jsonb(a)
               || jsonb_build_object('artifact_id', $1::uuid, 'task_id', $2::uuid, 'sha256', repeat('8', 64),
                                     'relative_path', 'outputs/derived-results.tar.gz'))).*
             FROM enrichedlab_agent.broker_artifacts a
             WHERE a.tenant_id='research-site' AND a.task_id=$3 AND a.role='result' LIMIT 1",
            &[&derived_artifact, &derived, &intake_task],
        )
        .await
        .unwrap();
    let derived_digest = json!({"schema": "enrichedlab.result-digest.v1", "status": "summarized", "figures": [],
        "tables": [{"path": "results/lab_derived/results.csv", "rows": 1, "columns": ["n"], "cells_omitted": "lab_derived"}]});
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_artifact_digests(tenant_id,artifact_id,artifact_sha256,task_id,attempt,
               lease_generation,schema_version,status,digest,digest_sha256)
             VALUES('research-site',$1,repeat('8',64),$2,1,1,'enrichedlab.result-digest.v1','summarized',$3,repeat('b',64))",
            &[&derived_artifact, &derived, &derived_digest],
        )
        .await
        .unwrap();
    assert!(holds(derived).await);
    assert!(holds(step(Some(derived), "lab-folder-child".into()).await).await);
    // Two plain steps made parents of each other, a record the append-only table never lets the broker write: the walk
    // still ends and finds nothing. The check runs inside a transaction that puts the trigger back.
    let cycle: (bool, bool) = {
        let rows = owner
            .simple_query(&format!(
                "BEGIN;
                 ALTER TABLE enrichedlab_agent.broker_project_tasks DISABLE TRIGGER immutable_record;
                 UPDATE enrichedlab_agent.broker_project_tasks SET parent_task_id='{plain_child}'
                   WHERE tenant_id='research-site' AND task_id='{plain}';
                 SELECT enrichedlab_agent.broker_task_holds_lab_data('research-site', '{plain}'),
                        enrichedlab_agent.broker_task_holds_lab_data('research-site', '{plain_child}');
                 ROLLBACK;"
            ))
            .await
            .unwrap();
        rows.iter()
            .find_map(|m| match m {
                tokio_postgres::SimpleQueryMessage::Row(r) => {
                    Some((r.get(0) == Some("t"), r.get(1) == Some("t")))
                }
                _ => None,
            })
            .expect("the cycle check returned a row")
    };
    assert_eq!(cycle, (false, false));

    for table in [
        "broker_datasets",
        "broker_dataset_requests",
        "broker_dataset_receipts",
    ] {
        assert!(
            owner
                .execute(&format!("DELETE FROM enrichedlab_agent.{table}"), &[])
                .await
                .is_err(),
            "{table} is immutable"
        );
    }

    drop(owner);
    connection.abort();
}
