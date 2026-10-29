//! Upstream task lineage (migration 065) in isolated PostgreSQL: one read follows only recorded
//! inputs from a figure through a template analysis and guarded query, reaches dataset and sealed-volume sources,
//! applies every producing task's reader rule, and stops at its fixed depth without changing version 2.
use enrichedlab_agent_sandbox::broker::{
    model::{PrincipalClass, SubmitTaskRequest, VerifiedIdentity},
    project::{CreateProject, Visibility},
    store::{Store, StoreErrorKind},
};
use serde_json::{Value, json};
use std::{env, fs};
use tokio_postgres::{Client, Config, NoTls, config::Host};
use uuid::Uuid;

const TENANT: &str = "research-site";
const PROFILE: &str = "general-research";

#[test]
fn migration_065_is_valid_postgresql_syntax() {
    let migration = include_str!("../migrations/postgres/065_task_lineage_upstream.sql");
    pg_query::parse(migration).unwrap();
    pg_query::parse_plpgsql(migration).unwrap();
}

fn digest(c: char) -> String {
    c.to_string().repeat(64)
}

fn image(c: char) -> String {
    format!("sha256:{}", digest(c))
}

fn person(subject: &str, email: &str) -> VerifiedIdentity {
    VerifiedIdentity {
        issuer: "https://identity.example.invalid".into(),
        class: PrincipalClass::Person,
        subject: subject.into(),
        email: email.into(),
        display_name: None,
    }
}

async fn submit(api: &Store, actor: Uuid, project: Uuid, key: &str, objective: &str) -> Uuid {
    api.submit_project_task(
        actor,
        &SubmitTaskRequest {
            group: "pilot-lab".into(),
            profile: PROFILE.into(),
            objective: objective.into(),
            project_id: Some(project),
            action_key: Some(key.into()),
            parent_task_id: None,
        },
        key,
        &digest('d'),
    )
    .await
    .unwrap()
}

async fn artifact(owner: &Client, task: Uuid, path: &str, sha256: &str) -> Uuid {
    owner
        .query_one(
            "INSERT INTO enrichedlab_agent.broker_artifacts
             (tenant_id,task_id,visibility,role,label,relative_path,media_type,size_bytes,sha256)
             VALUES($1,$2,'researcher','result','Result',$3,'application/gzip',100,$4)
             RETURNING artifact_id",
            &[&TENANT, &task, &path, &sha256],
        )
        .await
        .unwrap()
        .get(0)
}

async fn completed_attempt(owner: &Client, task: Uuid, artifact: Uuid) {
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_tasks
             SET status='completed',attempt=1,started_at=clock_timestamp(),finished_at=clock_timestamp()
             WHERE tenant_id=$1 AND task_id=$2",
            &[&TENANT, &task],
        )
        .await
        .unwrap();
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_attempt_executions
             (tenant_id,task_id,attempt,lease_generation,image_digest,tool_ref,parameters_sha256)
             VALUES($1,$2,1,1,$3,'qualification/v1',$4)",
            &[&TENANT, &task, &image('e'), &image('f')],
        )
        .await
        .unwrap();
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_attempt_outputs
             (tenant_id,task_id,attempt,lease_generation,relative_path,sha256,artifact_id,role,size_bytes)
             SELECT tenant_id,task_id,1,1,relative_path,sha256,artifact_id,role,size_bytes
             FROM enrichedlab_agent.broker_artifacts
             WHERE tenant_id=$1 AND artifact_id=$2",
            &[&TENANT, &artifact],
        )
        .await
        .unwrap();
}

async fn sealed_output_input(
    owner: &Client,
    consumer: Uuid,
    source_artifact: Uuid,
    reference: &str,
) {
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_tasks SET attempt=1
             WHERE tenant_id=$1 AND task_id=$2",
            &[&TENANT, &consumer],
        )
        .await
        .unwrap();
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_attempt_inputs
             (tenant_id,task_id,attempt,lease_generation,input_ref,input_class,sealed_sha256,size_bytes,
              custody_transaction,source)
             VALUES($1,$2,1,1,$3,'prior_output',$4,100,'lineage-qualification',
                    jsonb_build_object('artifact_id',$5::uuid::text))",
            &[
                &TENANT,
                &consumer,
                &reference,
                &digest('8'),
                &source_artifact,
            ],
        )
        .await
        .unwrap();
}

fn ids(items: &Value) -> Vec<Uuid> {
    items
        .as_array()
        .unwrap()
        .iter()
        .map(|item| serde_json::from_value(item["task_id"].clone()).unwrap())
        .collect()
}

#[tokio::test]
#[ignore = "isolated PostgreSQL through qualify_remote_broker.sh lineage-upstream"]
async fn one_read_drills_down_to_sources_without_disclosing_unreadable_tasks() {
    let dsn = fs::read_to_string(env::var_os("BROKER_QUALIFICATION_MIGRATION_DSN_FILE").unwrap())
        .unwrap();
    let config: Config = dsn.parse().unwrap();
    assert_eq!(config.get_dbname(), Some("broker_qual"));
    assert!(matches!(config.get_hosts(),[Host::Tcp(h)] if h=="127.0.0.1"));
    let (owner, connection) = config.connect(NoTls).await.unwrap();
    let connection = tokio::spawn(connection);
    let store = |user: &str, size| {
        Store::connect(
            &dsn.replace("user=postgres", &format!("user={user}")),
            TENANT.into(),
            size,
        )
        .unwrap()
    };
    let api = store("broker_api_qual", 3);
    api.ready_for_review_api().await.unwrap();

    owner
        .batch_execute(
            "INSERT INTO enrichedlab_agent.broker_principal_invitations
             (tenant_id,email,group_key,role)
             VALUES('research-site','lineage-outsider@example.invalid','pilot-lab','researcher');
             UPDATE enrichedlab_agent.broker_research_groups SET max_queued_tasks=1000
             WHERE tenant_id='research-site' AND group_key='pilot-lab';",
        )
        .await
        .unwrap();
    let reader = api
        .resolve_principal(&person("lineage-reader", "researcher@example.invalid"))
        .await
        .unwrap();
    let outsider = api
        .resolve_principal(&person(
            "lineage-outsider",
            "lineage-outsider@example.invalid",
        ))
        .await
        .unwrap();
    let project = Uuid::new_v4();
    api.create_project(
        reader,
        &CreateProject {
            project_id: project,
            group: "pilot-lab".into(),
            title: "Lineage qualification".into(),
            visibility: Visibility::Private,
        },
    )
    .await
    .unwrap();

    // A guarded query, a reviewed template analysis of its table and a figure of the analysis.
    let query = submit(
        &api,
        reader,
        project,
        "lineage-query",
        "Run the registered query.",
    )
    .await;
    let analysis = submit(
        &api,
        reader,
        project,
        "lineage-analysis",
        "Apply the reviewed analysis.",
    )
    .await;
    let figure = submit(
        &api,
        reader,
        project,
        "lineage-figure",
        "Render the reviewed figure.",
    )
    .await;
    let query_package = artifact(&owner, query, "outputs/query.tar.gz", &digest('a')).await;
    let analysis_package =
        artifact(&owner, analysis, "outputs/analysis.tar.gz", &digest('b')).await;
    let figure_package = artifact(&owner, figure, "outputs/figure.tar.gz", &digest('c')).await;
    for (task, output) in [
        (query, query_package),
        (analysis, analysis_package),
        (figure, figure_package),
    ] {
        completed_attempt(&owner, task, output).await;
    }

    let snapshot = json!({"manifest_sha256": digest('1')});
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_sql_sources
             (tenant_id,source_key,profile_key,title,description,release,schema_name,relations,functions,row_cap,
              statement_timeout_ms,connection_ref,snapshot,data_class)
             VALUES($1,'lineage-source',$2,'Registered source','Qualification source','snapshot-1','lineage_schema',
                    '[{\"name\":\"measurements\",\"columns\":[\"estimate\"]}]'::jsonb,ARRAY['count'],100,1000,
                    'lineage-source',$3,'public_reference')",
            &[&TENANT, &PROFILE, &snapshot],
        )
        .await
        .unwrap();
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_task_queries
             (tenant_id,task_id,source_key,profile_key,proposed_sql,admitted_sql,sql_sha256,guard,relations,row_limit)
             VALUES($1,$2,'lineage-source',$3,'SELECT count(*) FROM measurements',
                    'SELECT count(*) FROM lineage_schema.measurements LIMIT 100',$4,'guarded-sql/v1',
                    ARRAY['measurements'],100)",
            &[
                &TENANT,
                &query,
                &PROFILE,
                &format!("sha256:{}", digest('2')),
            ],
        )
        .await
        .unwrap();
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_query_receipts
             (tenant_id,task_id,attempt,lease_generation,status,row_count,columns,package_sha256,server_version,
              started_at,finished_at)
             VALUES($1,$2,1,1,'completed',1,'[{\"name\":\"count\"}]'::jsonb,$3,'qualification',
                    clock_timestamp(),clock_timestamp())",
            &[&TENANT, &query, &digest('a')],
        )
        .await
        .unwrap();

    let analysis_catalog = digest('3');
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_analysis_catalogs
             (tenant_id,catalog_sha256,service_version,image_digest,features)
             VALUES($1,$2,'1.0.0',$3,'[{\"id\":\"summary_stats\",\"label\":\"Summary\",\"params\":{}}]'::jsonb)",
            &[&TENANT, &analysis_catalog, &image('3')],
        )
        .await
        .unwrap();
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_analysis_requests
             (tenant_id,task_id,project_id,source_task_id,source_artifact_id,source_package_sha256,table_path,
              table_sha256,table_rows,table_columns,feature,settings,title,catalog_sha256,request,request_sha256,principal_id)
             VALUES($1,$2,$3,$4,$5,$6,'results/query.csv',$7,1,'[\"count\"]'::jsonb,
                    'summary_stats','{}'::jsonb,'Reviewed summary',$8,'{}'::jsonb,$9,$10)",
            &[
                &TENANT,
                &analysis,
                &project,
                &query,
                &query_package,
                &digest('a'),
                &digest('4'),
                &analysis_catalog,
                &digest('5'),
                &reader,
            ],
        )
        .await
        .unwrap();
    let analysis_files = json!([{"path":"results/analysis.csv","sha256":digest('6')}]);
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_analysis_receipts
             (tenant_id,task_id,attempt,lease_generation,status,service_version,image_digest,method,files,notes,
              package_sha256,started_at,finished_at)
             VALUES($1,$2,1,1,'completed','1.0.0',$3,'Reviewed summary method',$4,'[]'::jsonb,$5,
                    clock_timestamp(),clock_timestamp())",
            &[
                &TENANT,
                &analysis,
                &image('3'),
                &analysis_files,
                &digest('b'),
            ],
        )
        .await
        .unwrap();

    let render_catalog = digest('7');
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_render_catalogs
             (tenant_id,catalog_sha256,service_version,image_digest,features)
             VALUES($1,$2,'1.0.0',$3,'[{\"id\":\"scatter\",\"label\":\"Scatter\",\"params\":{}}]'::jsonb)",
            &[&TENANT, &render_catalog, &image('4')],
        )
        .await
        .unwrap();
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_figure_requests
             (tenant_id,task_id,project_id,source_task_id,source_artifact_id,source_package_sha256,table_path,
              table_sha256,table_rows,table_columns,feature,controls,title,catalog_sha256,request,request_sha256,principal_id)
             VALUES($1,$2,$3,$4,$5,$6,'results/analysis.csv',$7,1,'[\"estimate\"]'::jsonb,
                    'scatter','{}'::jsonb,'Reviewed figure',$8,'{}'::jsonb,$9,$10)",
            &[
                &TENANT,
                &figure,
                &project,
                &analysis,
                &analysis_package,
                &digest('b'),
                &digest('6'),
                &render_catalog,
                &digest('8'),
                &reader,
            ],
        )
        .await
        .unwrap();
    let figure_files = json!([{"path":"figures/figure.png","sha256":digest('9')}]);
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_figure_receipts
             (tenant_id,task_id,attempt,lease_generation,status,service_version,image_digest,files,notes,
              package_sha256,started_at,finished_at)
             VALUES($1,$2,1,1,'completed','1.0.0',$3,$4,'[]'::jsonb,$5,
                    clock_timestamp(),clock_timestamp())",
            &[
                &TENANT,
                &figure,
                &image('4'),
                &figure_files,
                &digest('c'),
            ],
        )
        .await
        .unwrap();

    let v2 = api.task_lineage_v2(reader, figure).await.unwrap();
    let v3 = api.task_lineage_v3(reader, figure).await.unwrap();
    assert_eq!(v3["schema"], "enrichedlab.task-lineage.v3");
    assert_eq!(ids(&v3["upstream"]["nodes"]), [figure, analysis, query]);
    assert_eq!(v3["upstream"]["edges"].as_array().unwrap().len(), 2);
    assert_eq!(
        v3["upstream"]["edges"][0],
        json!({"from":analysis,"to":figure,"output":"results/analysis.csv",
            "digest":digest('6'),"recorded_by":"render_request"})
    );
    assert_eq!(
        v3["upstream"]["edges"][1],
        json!({"from":query,"to":analysis,"output":"results/query.csv",
            "digest":digest('4'),"recorded_by":"analysis_request"})
    );
    assert_eq!(v3["upstream"]["sources"].as_array().unwrap().len(), 1);
    assert_eq!(v3["upstream"]["sources"][0]["kind"], "guarded_query");
    assert_eq!(v3["upstream"]["sources"][0]["node"], json!(query));
    assert_eq!(v3["upstream"]["sources"][0]["source"], "lineage-source");
    assert_eq!(
        v3["upstream"]["sources"][0]["snapshot"]["manifest_sha256"],
        digest('1')
    );
    assert_eq!(v3["upstream"]["withheld"], 0);
    assert_eq!(v3["upstream"]["limits"]["truncated"], false);
    assert_eq!(
        v3["upstream"]["nodes"][1]["method"]["analysis"]["service_version"],
        "1.0.0"
    );
    assert_eq!(
        v3["upstream"]["nodes"][0]["attempts"][0]["execution"]["image_digest"],
        image('e')
    );
    // Apart from its schema tag and new upstream member, version 3 is exactly a version 2 read.
    let mut projected = v3.clone();
    projected.as_object_mut().unwrap().remove("upstream");
    projected["schema"] = json!("enrichedlab.task-lineage.v2");
    assert_eq!(projected, v2);
    assert_eq!(
        api.task_lineage_v3(outsider, figure)
            .await
            .unwrap_err()
            .kind,
        StoreErrorKind::NotFound
    );

    // A preparation follows its recorded intake package, then terminates at the registered file and guard decision.
    let intake = submit(
        &api,
        reader,
        project,
        "lineage-dataset-intake",
        "Profile the guarded file.",
    )
    .await;
    let preparation = submit(
        &api,
        reader,
        project,
        "lineage-dataset-preparation",
        "Apply the reviewed preparation.",
    )
    .await;
    let intake_sha = digest('a');
    let intake_package =
        artifact(&owner, intake, "outputs/dataset-intake.tar.gz", &intake_sha).await;
    completed_attempt(&owner, intake, intake_package).await;
    let dataset = Uuid::new_v4();
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_datasets
             (tenant_id,dataset_id,project_id,request_id,file_name,media_type,size_bytes,sha256,guard_intake,
              guard_outcome,guard_findings,guard_policy,grid_sha256,grid_bytes,intake_task_id,principal_id)
             VALUES($1,$2,$3,$4,'observations.csv','text/csv',240,$5,$6,'allowed','[]'::jsonb,
                    'upload-policy-v1',$7,400,$8,$9)",
            &[
                &TENANT,
                &dataset,
                &project,
                &Uuid::new_v4(),
                &digest('b'),
                &Uuid::new_v4(),
                &digest('c'),
                &intake,
                &reader,
            ],
        )
        .await
        .unwrap();
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_dataset_requests
             (tenant_id,task_id,project_id,dataset_id,operation,title,request,request_sha256,principal_id)
             VALUES($1,$2,$3,$4,'intake','Dataset intake','{}'::jsonb,$5,$6)",
            &[&TENANT, &intake, &project, &dataset, &digest('d'), &reader],
        )
        .await
        .unwrap();
    let spec = json!({"schema":"enrichedlab.dataset-preparation.v1","table_name":"prepared"});
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_dataset_requests
             (tenant_id,task_id,project_id,dataset_id,operation,spec,spec_sha256,title,request,request_sha256,principal_id)
             VALUES($1,$2,$3,$4,'preparation',$5,$6,'Prepared dataset','{}'::jsonb,$7,$8)",
            &[
                &TENANT,
                &preparation,
                &project,
                &dataset,
                &spec,
                &digest('e'),
                &digest('f'),
                &reader,
            ],
        )
        .await
        .unwrap();
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_dataset_receipts
             (tenant_id,task_id,attempt,lease_generation,status,profile,grid_sha256,package_sha256,executor,
              executor_version,started_at,finished_at)
             VALUES($1,$2,1,1,'profiled','{}'::jsonb,$3,$4,'reviewed-dataset',1,
                    clock_timestamp(),clock_timestamp())",
            &[&TENANT, &intake, &digest('c'), &intake_sha],
        )
        .await
        .unwrap();
    let dataset_lineage = api.task_lineage_v3(reader, preparation).await.unwrap();
    assert_eq!(
        ids(&dataset_lineage["upstream"]["nodes"]),
        [preparation, intake]
    );
    assert_eq!(
        dataset_lineage["upstream"]["edges"][0],
        json!({"from":intake,"to":preparation,"output":"outputs/dataset-intake.tar.gz",
            "digest":intake_sha,"recorded_by":"dataset_request"})
    );
    assert_eq!(
        dataset_lineage["upstream"]["sources"][0]["kind"],
        "lab_dataset"
    );
    assert_eq!(
        dataset_lineage["upstream"]["sources"][0]["node"],
        json!(intake)
    );
    assert_eq!(
        dataset_lineage["upstream"]["sources"][0]["guard"],
        json!({"decision":"allowed","policy":"upload-policy-v1"})
    );

    // An explicit artifact input whose producing project is private is counted without exposing that task.
    let hidden_project = Uuid::new_v4();
    api.create_project(
        outsider,
        &CreateProject {
            project_id: hidden_project,
            group: "pilot-lab".into(),
            title: "Unreadable lineage fixture".into(),
            visibility: Visibility::Private,
        },
    )
    .await
    .unwrap();
    let hidden = submit(
        &api,
        outsider,
        hidden_project,
        "lineage-hidden",
        "Unreadable producer marker.",
    )
    .await;
    let hidden_artifact = artifact(&owner, hidden, "outputs/private.tar.gz", &digest('1')).await;
    let consumer = submit(
        &api,
        reader,
        project,
        "lineage-withheld",
        "Read one sealed prior output.",
    )
    .await;
    sealed_output_input(
        &owner,
        consumer,
        hidden_artifact,
        "input://sealed/private-output",
    )
    .await;
    let withheld = api.task_lineage_v3(reader, consumer).await.unwrap();
    assert_eq!(ids(&withheld["upstream"]["nodes"]), [consumer]);
    assert_eq!(withheld["upstream"]["edges"], json!([]));
    assert_eq!(withheld["upstream"]["sources"], json!([]));
    assert_eq!(withheld["upstream"]["withheld"], 1);
    let encoded = serde_json::to_string(&withheld).unwrap();
    assert!(!encoded.contains(&hidden.to_string()), "{encoded}");
    assert!(!encoded.contains("Unreadable producer marker"), "{encoded}");

    // Root depth is zero: an eighteen-node chain includes depths zero through sixteen and reports the cutoff.
    let mut chain = Vec::new();
    for n in 0..18 {
        chain.push(
            submit(
                &api,
                reader,
                project,
                &format!("lineage-depth-{n}"),
                "Follow the recorded prior output.",
            )
            .await,
        );
    }
    for n in 0..17 {
        let output = artifact(
            &owner,
            chain[n],
            &format!("outputs/depth-{n}.tar.gz"),
            &digest('2'),
        )
        .await;
        sealed_output_input(
            &owner,
            chain[n + 1],
            output,
            &format!("input://lineage/depth-{n}"),
        )
        .await;
    }
    let limited = api.task_lineage_v3(reader, chain[17]).await.unwrap();
    assert_eq!(limited["upstream"]["nodes"].as_array().unwrap().len(), 17);
    assert_eq!(limited["upstream"]["edges"].as_array().unwrap().len(), 16);
    assert_eq!(limited["upstream"]["limits"]["truncated"], true);
    assert_eq!(limited["upstream"]["limits"]["reason"], "depth");
    assert_eq!(limited["upstream"]["limits"]["max_depth"], 16);
    assert_eq!(limited["upstream"]["limits"]["max_nodes"], 200);

    // Only the API role gets the new read, and the migration is visible through the standard schema marker.
    let schema_ready: bool = owner
        .query_one("SELECT enrichedlab_agent.broker_schema_ready(65)", &[])
        .await
        .unwrap()
        .get(0);
    assert!(schema_ready);
    for (role, granted) in [
        ("broker_api_qual", true),
        ("broker_worker_qual", false),
        ("broker_custodian_qual", false),
        ("public", false),
    ] {
        let actual: bool = owner
            .query_one(
                "SELECT has_function_privilege($1,
                 'enrichedlab_agent.broker_task_lineage_v3(text,uuid,uuid)','EXECUTE')",
                &[&role],
            )
            .await
            .unwrap()
            .get(0);
        assert_eq!(actual, granted, "{role}");
    }

    drop(owner);
    connection.await.unwrap().unwrap();
}
