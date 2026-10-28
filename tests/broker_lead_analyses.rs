//! The research lead's template analyses (migration 060) against an isolated PostgreSQL and a
//! stand-in render service: a run may list the lab's template-analysis method while figure and dataset methods stay
//! request-only, the coordinator reads the method and the newest catalog under the run's lease, the turn commit runs a
//! person's request checks on the analysis the lead chose and refuses every other shape, the committed step is a
//! template analysis of the finished step's table with its origin recorded, the worker runs it, and a lab dataset's
//! tasks no longer hold a run's places.
use axum::{
    Json, Router,
    http::StatusCode,
    routing::{get, post},
};
use base64::Engine;
use enrichedlab_agent_sandbox::broker::{
    analysis_run,
    config::WorkerConfig,
    coordinator::{
        CheckedLead, CompiledContext, LeadCheck, Redirect, Refusal, ResultIndex, StartRun,
        check_lead_replies, check_lead_reply, compile_context, delegated_action, reconcile,
        refusal, rejection_reason,
    },
    lead_analysis::{jsonb_text_bytes, with_analysis_catalog},
    model::SubmitTaskRequest,
    model::{PrincipalClass, VerifiedIdentity},
    project::{CheckpointRequest, CreateProject, ResearchState, Visibility},
    query_run,
    sql_guard::{self, Allowed},
    step_checks,
    store::Store,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeSet, HashMap, HashSet},
    env, fs,
    path::PathBuf,
    time::Duration,
};
use tokio_postgres::{Config, NoTls, config::Host};
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

const MANIFEST: &str = "5f1c0d9a3a4e2b7c8d6e5f4a3b2c1d0e9f8a7b6c5d4e3f2a1b0c9d8e7f6a5b4c";
const IMAGE: &str = "sha256:7f1b2c3d4e5f60718293a4b5c6d7e8f90112233445566778899aabbccddeeff0";
const TABLE: &str = "results/query_result.csv";

fn state(steps: Value) -> Value {
    json!({"schema_version":1,"goal":"Which example foods carry the most of the example nutrient?",
        "constraints":["Read only the reference data."], "plan": steps,
        "lessons":[],"next_action":"Query the reference data.",
        "workspace":{"schema_version":1,"profile":"reference-sql","image_digest":format!("sha256:{}","a".repeat(64)),
        "source_digest":format!("sha256:{}","b".repeat(64)),"input_refs":["input://qualification/example"],
        "tool_refs":["tool://reference-sql"],"preparation_receipt":null}})
}

fn step(key: &str, status: &str, task: Option<Uuid>) -> Value {
    json!({"action_key": key, "title": key, "status": status, "task_id": task, "evidence_refs": [],
        "rationale": "Answer the question."})
}

fn query_action(key: &str) -> Value {
    let sql = "SELECT f.description, n.amount FROM food f JOIN food_nutrient n USING (fdc_id) ORDER BY n.amount DESC NULLS LAST, f.description";
    let relations: BTreeSet<String> = ["food", "food_nutrient"]
        .iter()
        .map(|s| s.to_string())
        .collect();
    let functions: BTreeSet<String> = ["count"].iter().map(|s| s.to_string()).collect();
    let a = sql_guard::admit(
        sql,
        &Allowed {
            schema: "reference_qual",
            relations: &relations,
            functions: &functions,
            row_cap: 100,
        },
    )
    .unwrap();
    json!({"action_key": key, "profile": "reference-sql", "objective": "Rank the foods.", "request_sha256": "c".repeat(64),
        "query": {"source": "example-reference", "proposed_sql": sql, "admitted_sql": a.sql,
            "sha256": format!("sha256:{}", hex::encode(Sha256::digest(a.sql.as_bytes()))), "guard": sql_guard::GUARD,
            "relations": a.relations, "row_limit": a.limit}})
}

/// The step the lead chose, as the coordinator sends it to the turn commit.
fn analysis_action(key: &str, analysis: Value) -> Value {
    json!({"action_key": key, "profile": "template-analysis", "objective": "Compare the foods.",
        "request_sha256": "d".repeat(64), "template_analysis": analysis})
}

/// The step's analysis as the commit receives it: the result the table was shown in and the catalog it was chosen from.
fn analysis(source: Uuid, artifact: Uuid, catalog_sha256: &str) -> Value {
    json!({"source_task": source, "artifact": artifact, "table_path": TABLE, "feature": "group_compare",
        "settings": {"value": "amount", "group": "description", "test": "t"}, "title": "Compare foods",
        "catalog_sha256": catalog_sha256})
}

/// The newest catalog as `research_run_analysis_catalog` returns it for a run that lists the method.
fn catalog_read() -> Value {
    json!({"profile": "template-analysis", "catalog": {"catalog_sha256": "a".repeat(64), "service_version": "0.6.0",
        "image_digest": IMAGE, "features": [
            {"id": "group_compare", "label": "Two-group comparison", "description": "A t or Wilcoxon test. Welch by default.",
             "params": {"value": {"type": "column", "required": true}, "group": {"type": "column", "required": true},
               "test": {"type": "enum", "choices": ["auto", "t", "wilcoxon"]},
               "min_n": {"type": "number", "min": 3, "max": 487}}}]}})
}

/// A lead turn after the ranking step finished: the coordinator's context for it and the checks it runs on a reply.
struct Turn {
    research: ResearchState,
    profiles: Value,
    methods: Value,
    compiled: CompiledContext,
    names: Vec<String>,
}

impl Turn {
    /// The turn after the ranking step `task` kept its table in the result `artifact`, with the catalog `read`.
    fn after_ranking(task: Uuid, artifact: Uuid, read: &Value) -> Self {
        Self::after_ranking_within(task, artifact, read, 32768)
    }

    /// The same turn with a lead context of `budget` bytes.
    fn after_ranking_within(task: Uuid, artifact: Uuid, read: &Value, budget: usize) -> Self {
        Self::after_ranking_expecting(task, artifact, read, budget, None)
    }

    /// The same turn where the ranking step stated `expect` and its result was checked against it.
    fn after_ranking_expecting(
        task: Uuid,
        artifact: Uuid,
        read: &Value,
        budget: usize,
        expect: Option<Value>,
    ) -> Self {
        let reference = format!("artifact://tasks/{task}/{artifact}");
        let mut research: ResearchState = serde_json::from_value(state(json!([
            step("rank-foods", "pending", Some(task)),
            step("compare", "pending", None)
        ])))
        .unwrap();
        let facts = json!([{"task_id": task, "status": "completed", "result_class": "success", "error_code": null,
            "evidence_refs": [reference]}]);
        reconcile(&mut research, &facts).unwrap();
        let digest = json!({"schema": "enrichedlab.result-digest.v1", "status": "summarized",
            "package": {"members": 4, "checksums_listed": true, "all_matched": true},
            "tables": [{"path": TABLE, "sha256": "b".repeat(64), "bytes": 64, "rows": 2, "columns": ["description", "amount"],
                "cells": [["Example food A", "48.2"], ["Example food B, raw", "31.7"]]},
                {"path": "results/wide.csv", "sha256": "c".repeat(64), "bytes": 9000, "rows": 2, "columns": ["description"],
                 "columns_omitted": 70, "cells_omitted": "too_large"}],
            "tables_omitted": 0, "figures": [], "parameters": {}});
        let mut results = json!({"results": [{"task_id": task, "ref": reference, "label": "Query result", "status": "summarized",
            "digest_sha256": "d".repeat(64), "digest": digest}], "earlier_findings": null});
        if let Some(expect) = expect {
            research.plan[0].expect = Some(serde_json::from_value(expect).unwrap());
            results["step_checks"] =
                json!(step_checks::checks_for(&research.plan, &results["results"]));
        }
        let profiles = json!([{"key": "reference-sql", "name": "Query the example reference data", "description": "SQL."},
            {"key": "template-analysis", "name": "Run a template analysis", "description": "Reviewed R analyses."}]);
        let methods = with_analysis_catalog(&profiles, read);
        let compiled = compile_context(
            &research.context(3, 8192).unwrap(),
            &HashMap::new(),
            &methods,
            &facts,
            &results,
            2,
            budget,
        );
        Turn {
            research,
            profiles,
            methods,
            compiled,
            names: vec!["reference-sql".to_owned(), "template-analysis".to_owned()],
        }
    }

    fn check_with(&self, methods: &Value, reply: &Value) -> anyhow::Result<CheckedLead> {
        check_lead_reply(
            &LeadCheck {
                state: &self.research,
                profiles: &self.names,
                parallel: 2,
                methods,
                sources: &HashMap::new(),
                own_queries: &json!([]),
                context: &self.compiled.value,
                index: &self.compiled.index,
                rejected: &self.compiled.rejected,
                archived: &HashSet::new(),
                input_ceiling: usize::MAX,
            },
            reply,
        )
    }

    fn check(&self, reply: &Value) -> anyhow::Result<CheckedLead> {
        self.check_with(&self.methods, reply)
    }

    /// The step as the coordinator sends it to the turn commit.
    fn action(&self, project: Uuid, reply: &Value) -> Value {
        let checked = self.check(reply).unwrap();
        let a = &checked.proposal.actions[0];
        let req = SubmitTaskRequest {
            group: "pilot-lab".into(),
            profile: a.profile.clone(),
            objective: a.objective.clone(),
            project_id: Some(project),
            action_key: Some(a.action_key.clone()),
            parent_task_id: None,
        };
        delegated_action(
            &req,
            a,
            &checked.admitted,
            &checked.analyses,
            &checked.queries,
        )
    }
}

fn lead_reply(table: &str, settings: Value) -> Value {
    json!({"action": "delegate", "answer": "not_yet", "summary": "Compare the two foods.", "findings": [],
        "actions": [{"action_key": "compare", "title": "Compare foods", "profile": "template-analysis",
            "objective": "Compare the amounts between the foods.", "rationale": "The ranked table is ready.",
            "analysis": {"source": "rank-foods", "table": table, "feature": "group_compare", "settings": settings},
            "expect": {"rows": {"min": 1}}}],
        "lessons": []})
}

fn welch() -> Value {
    json!({"value": "amount", "group": "description", "test": "t"})
}

#[test]
fn a_result_that_failed_its_step_check_is_no_analysis_source() {
    // A result that failed its step's check leaves the tables a template analysis may read, so the
    // lead cannot choose it, while a result that passed stays a source.
    let (task, artifact) = (Uuid::new_v4(), Uuid::new_v4());
    let sources = |turn: &Turn| -> Vec<String> {
        turn.compiled.value["analysis_source_tables"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| t["table"].as_str().map(str::to_owned))
            .collect()
    };
    let passed = Turn::after_ranking_expecting(
        task,
        artifact,
        &catalog_read(),
        32768,
        Some(json!({"table": TABLE, "rows": {"min": 1}})),
    );
    assert_eq!(
        passed.compiled.value["analyses"][0]["check"]["state"],
        "passed"
    );
    assert!(sources(&passed).contains(&TABLE.to_owned()));
    assert!(passed.check(&lead_reply(TABLE, welch())).is_ok());
    let failed = Turn::after_ranking_expecting(
        task,
        artifact,
        &catalog_read(),
        32768,
        Some(json!({"table": TABLE, "rows": {"min": 3}})),
    );
    assert_eq!(
        failed.compiled.value["analyses"][0]["check"]["state"],
        "failed"
    );
    assert!(!sources(&failed).contains(&TABLE.to_owned()));
    let refused = failed.check(&lead_reply(TABLE, welch())).err().unwrap();
    assert_eq!(refusal(&refused).reason, "proposal_analysis_table_unknown");
    assert_eq!(refusal(&refused).names, ["compare", "rank-foods", TABLE]);
}

#[test]
fn the_lead_sees_the_analyses_and_its_choice_is_checked_as_the_coordinator_checks_it() {
    // Through the coordinator's own context: the method shows its analyses compactly and never the catalog, a finished
    // step's table can be chosen by the step's key, a table wider than its digest cannot, and the offline scorer gives
    // the same answer only when it reads the same catalog.
    let (task, artifact) = (Uuid::new_v4(), Uuid::new_v4());
    let turn = Turn::after_ranking(task, artifact, &catalog_read());
    let shown = &turn.compiled.value["methods"][1];
    assert_eq!(shown["key"], "template-analysis");
    assert!(shown.get("analysis_catalog").is_none());
    assert_eq!(
        shown["analyses"],
        json!([{"id": "group_compare", "label": "Two-group comparison", "about": "A t or Wilcoxon test.",
        "inputs": {
            "source": {"type": "finished_step_key", "required": true},
            "table": {"type": "exact_table_path", "required": true},
            "settings": [
                {"name": "group", "type": "column", "required": true, "column_names": true},
                {"name": "value", "type": "column", "required": true, "column_names": true},
                {"name": "min_n", "type": "number", "required": false, "min": 3, "max": 487},
                {"name": "test", "type": "enum", "required": false,
                 "choices": ["auto", "t", "wilcoxon"]}
            ]
        }}])
    );
    let checked = turn.check(&lead_reply(TABLE, welch())).unwrap();
    let chosen = &checked.analyses["compare"];
    assert_eq!(chosen.record(), analysis(task, artifact, &"a".repeat(64)));
    // A number the shown analyses' words hold may name a setting in a step's title or rationale, never in the summary,
    // a finding, a lesson or a part: there it may be a result value written without its binding.
    let mut said = lead_reply(TABLE, welch());
    said["actions"][0]["title"] = json!("Compare foods with at most 487 rows a group");
    said["actions"][0]["rationale"] = json!("A group may hold 3 to 487 rows.");
    assert_eq!(turn.check(&said).unwrap().summary_numbers_masked, 0);
    said["summary"] = json!("Compare the two foods; a group may hold up to 487 rows.");
    assert_eq!(turn.check(&said).unwrap().summary_numbers_masked, 1);
    // A number no shown analysis sets is masked in a step's words, so it never reaches the record; one
    // inside a name nothing shows is still refused.
    said["actions"][0]["title"] = json!("Compare foods with at most 488 rows a group");
    let masked = turn.check(&said).unwrap();
    assert_eq!(masked.step_numbers_masked, 1);
    assert_eq!(
        masked.proposal.actions[0].title,
        "Compare foods with at most (a number) rows a group"
    );
    said["actions"][0]["title"] = json!("Compare foods by Gene488 rows a group");
    let refused = turn.check(&said).err().unwrap();
    assert_eq!(refused.to_string(), "proposal_prose_number");
    assert_eq!(refusal(&refused).names, ["actions.title: 488"]);
    let mut said = lead_reply(TABLE, welch());
    said["lessons"] = json!([{"statement": "A group may hold 487 rows.", "kind": "hypothesis", "evidence_refs": []}]);
    let refused = turn.check(&said).err().unwrap();
    assert_eq!(refusal(&refused).names, ["lessons.statement: 487"]);
    // The offline scorer rebuilds the same index from the kept context, setting numbers included.
    let (offline, _) = ResultIndex::from_context(&turn.compiled.value, &turn.methods).unwrap();
    assert_eq!(offline, turn.compiled.index);
    let wide = turn
        .check(&lead_reply("results/wide.csv", welch()))
        .err()
        .unwrap();
    assert_eq!(wide.to_string(), "proposal_analysis_table_unsuitable");
    assert_eq!(refusal(&wide).names, ["compare", "results/wide.csv"]);

    // The step carries the analysis, and its request digest names it, so another analysis under the same step is
    // another request.
    let project = Uuid::new_v4();
    let action = turn.action(project, &lead_reply(TABLE, welch()));
    assert_eq!(action["template_analysis"], chosen.record());
    assert_eq!(action["profile"], "template-analysis");
    let mut wilcoxon = welch();
    wilcoxon["test"] = json!("wilcoxon");
    let other = turn.action(project, &lead_reply(TABLE, wilcoxon));
    assert_ne!(action["request_sha256"], other["request_sha256"]);
    assert_eq!(
        action["request_sha256"],
        turn.action(project, &lead_reply(TABLE, welch()))["request_sha256"]
    );

    // Every refusal of a chosen analysis reaches the log and the next call as itself; the lead's own mistakes narrow
    // the reply to one step after two refusals, a catalog no worker has read does not.
    for code in [
        "proposal_analysis_unexpected",
        "proposal_analysis_missing",
        "proposal_analysis_unavailable",
        "proposal_analysis_feature_unknown",
        "proposal_analysis_source_unknown",
        "proposal_analysis_source_not_finished",
        "proposal_analysis_source_rejected",
        "proposal_analysis_table_unknown",
        "proposal_analysis_table_unsuitable",
        "proposal_analysis_settings_invalid",
        "proposal_analysis_setting_unknown",
        "proposal_analysis_column_unknown",
        "proposal_analysis_setting_invalid",
        "proposal_analysis_setting_missing",
    ] {
        assert_eq!(rejection_reason(code), code);
        let redirect = Redirect::after(
            &Refusal {
                reason: rejection_reason(code),
                names: vec![],
            },
            2,
        );
        if code == "proposal_analysis_unavailable" {
            assert_eq!(redirect, None);
        } else {
            assert_eq!(redirect, Some(Redirect::OneStep));
        }
    }

    // The offline scorer accepts the reply only with the catalog that call read.
    let line = |catalog: Option<Value>| {
        let mut l = json!({"id": "b4", "context": turn.compiled.value,
            "reply": lead_reply(TABLE, welch()).to_string(), "finish_reason": "stop", "state": turn.research,
            "profiles": turn.names, "parallel": 2, "methods": turn.profiles, "sources": [], "own_queries": []});
        if let Some(catalog) = catalog {
            l["analysis_catalog"] = catalog;
        }
        l.to_string()
    };
    let mut out = Vec::new();
    let input = [line(Some(catalog_read())), line(None)].join("\n");
    assert_eq!(check_lead_replies(input.as_bytes(), &mut out).unwrap(), 2);
    let rows: Vec<Value> = String::from_utf8(out)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    assert_eq!(
        (&rows[0]["outcome"], &rows[0]["steps"]),
        (&json!("accepted"), &json!(1))
    );
    assert_eq!(
        (&rows[1]["outcome"], &rows[1]["code"]),
        (&json!("refused"), &json!("proposal_analysis_unexpected"))
    );
    // Without the method in the run's list, the same step is not the lead's to take.
    assert_eq!(
        turn.check_with(&turn.profiles, &lead_reply(TABLE, welch()))
            .err()
            .unwrap()
            .to_string(),
        "proposal_analysis_unexpected"
    );
}

#[test]
fn a_small_context_gives_the_catalog_a_quarter_of_its_budget() {
    // The catalog's view is held to a quarter of the lead's budget, at most 8 KiB, so a large
    // catalog cannot take the room a small context's results need. What does not fit is listed by id and label.
    let features: Vec<Value> = (0..200)
        .map(|n| json!({"id": format!("analysis_{n:03}"), "label": "x".repeat(60), "description": "y".repeat(200),
            "params": {"v": {"type": "column", "required": true}}}))
        .collect();
    let read = json!({"profile": "template-analysis", "catalog": {"catalog_sha256": "a".repeat(64),
        "service_version": "0.6.0", "image_digest": IMAGE, "features": features}});
    for budget in [8192, 16384, 32768, 65536] {
        let turn = Turn::after_ranking_within(Uuid::new_v4(), Uuid::new_v4(), &read, budget);
        let shown = &turn.compiled.value["methods"][1];
        let room = (budget / 4).min(8192);
        let size = |v: &Value| serde_json::to_vec(v).unwrap().len();
        // Contracts and the analyses listed by id and label share the room, with the note that explains the
        // listing; contracts keep at least two thirds of it, within one contract.
        let contracts = size(&shown["analyses"]);
        let one = size(&shown["analyses"][0]) + 1;
        let whole =
            contracts + size(&shown["other_analyses"]) + size(&shown["other_analyses_note"]);
        assert!(whole <= room, "{budget}: {whole} of {room}");
        assert!(
            contracts + one > room - room / 3,
            "{budget}: contracts {contracts} of {room}"
        );
        let count = |field: &str| shown[field].as_array().map_or(0, Vec::len);
        assert_eq!(
            count("analyses")
                + count("other_analyses")
                + shown["analyses_omitted"].as_u64().unwrap_or(0) as usize,
            200
        );
        assert!(count("other_analyses") > 0);
    }
}

/// A stand-in for the reviewed render service: one figure and one analysis, and an analysis run that completes.
async fn render_service() -> String {
    async fn features() -> Json<Value> {
        Json(
            json!({"service_version": "0.6.0", "image_digest": IMAGE, "features": [
            {"id": "scatter", "label": "Scatter plot", "kind": "figure", "description": "One point per row.",
             "params": {"x": {"type": "column", "required": true}, "y": {"type": "column", "required": true}}},
            {"id": "group_compare", "label": "Two-group comparison", "kind": "analysis", "description": "A t or Wilcoxon test.",
             "params": {"value": {"type": "column", "required": true}, "group": {"type": "column", "required": true},
               "test": {"type": "enum", "choices": ["auto", "t", "wilcoxon"], "required": false}}}]}),
        )
    }
    async fn render(Json(body): Json<Value>) -> (StatusCode, Json<Value>) {
        let b64 = |bytes: &[u8]| base64::engine::general_purpose::STANDARD.encode(bytes);
        let exact = json!({"columns": ["group", "n", "mean"],
            "rows": [{"group": "Example food A", "n": 1, "mean": 48.2}, {"group": "Example food B, raw", "n": 1, "mean": 31.7}],
            "method": "Welch two-sample t-test", "notes": "", "stats": {"p_value": 0.04}, "n": 2});
        (
            StatusCode::OK,
            Json(json!({"kind": "analysis",
            "results": {"columns": ["group", "n", "mean"], "rows": [], "method": "Welch two-sample t-test", "notes": null,
                "stats": {"p_value": 0.04}, "n": 2},
            "results_exact": exact, "png": b64(b"\x89PNG\r\n\x1a\nstand-in"), "svg": "<svg xmlns='http://www.w3.org/2000/svg'/>",
            "code": "source(file.path(dirname(.tf), \"_common.R\"))\n", "common_code": "WORK <- commandArgs(TRUE)[1]\n",
            "service_version": "0.6.0", "image_digest": IMAGE,
            "spec": {"params": body["params"], "title": body["title"], "feature_id": body["feature_id"]}, "notes": []})),
        )
    }
    let app = Router::new()
        .route("/features", get(features))
        .route("/render", post(render));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

#[tokio::test]
#[ignore = "isolated PostgreSQL through qualify_remote_broker.sh lead-analyses"]
async fn the_lead_runs_a_template_analysis_on_a_finished_step() {
    let dsn_file = env::var_os("BROKER_QUALIFICATION_MIGRATION_DSN_FILE").unwrap();
    let dsn = fs::read_to_string(&dsn_file).unwrap();
    let config: Config = dsn.parse().unwrap();
    assert_eq!(config.get_dbname(), Some("broker_qual"));
    assert!(matches!(config.get_hosts(),[Host::Tcp(h)] if h=="127.0.0.1"));
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
    // Settings are measured as PostgreSQL writes them as jsonb text, the measure the commit's check applies, for every
    // kind of value serde_json sends.
    let long: serde_json::Map<String, Value> = (0..40)
        .map(|n| (format!("s{n:02}"), json!("x".repeat(195))))
        .collect();
    for value in [
        Value::Object(long),
        json!({"a": 1e3, "b": 1.5e-7, "c": -0.0, "d": 1e300, "e": -12, "f": 0.001, "g": 1.2345678901234568e20,
            "h": [true, null, "q\"\n\u{1}\u{7f}é/\\"], "i": 5e-324, "j": 1.7976931348623157e308, "k": u64::MAX,
            "l": i64::MIN, "m": 0.1, "n": 100, "o": 2.5e15, "p": {}, "q": [], "r": {"é": ""}}),
    ] {
        let stored: i32 = owner
            .query_one("SELECT octet_length($1::jsonb::text)", &[&value])
            .await
            .unwrap()
            .get(0);
        assert_eq!(jsonb_text_bytes(&value), Some(stored as usize), "{value}");
    }
    let api = store("broker_api_qual", 3);
    let coordinator = store("broker_coordinator_qual", 2);
    let worker = store("broker_worker_qual", 2);
    // The coordinator is ready only with migration 060 and EXECUTE on the run's catalog read.
    coordinator.ready_for_research_results().await.unwrap();
    let catalog_fn =
        "enrichedlab_agent.broker_research_run_analysis_catalog(text,uuid,text,bigint)";
    owner
        .batch_execute(&format!(
            "REVOKE EXECUTE ON FUNCTION {catalog_fn} FROM broker_coordinator_qual"
        ))
        .await
        .unwrap();
    assert_eq!(
        coordinator
            .ready_for_research_results()
            .await
            .unwrap_err()
            .code,
        "broker_results_schema_not_ready"
    );
    owner
        .batch_execute(&format!(
            "GRANT EXECUTE ON FUNCTION {catalog_fn} TO broker_coordinator_qual"
        ))
        .await
        .unwrap();
    coordinator.ready_for_research_results().await.unwrap();
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

    owner.batch_execute(&format!("
        CREATE SCHEMA reference_qual;
        CREATE TABLE reference_qual.food (fdc_id integer PRIMARY KEY, description text NOT NULL);
        CREATE TABLE reference_qual.food_nutrient (fdc_id integer NOT NULL, amount double precision);
        CREATE TABLE reference_qual.snapshot_manifest (manifest_sha256 text NOT NULL);
        INSERT INTO reference_qual.food VALUES (1,'Example food A'),(2,'Example food B, raw');
        INSERT INTO reference_qual.food_nutrient VALUES (1,48.2),(2,31.7);
        INSERT INTO reference_qual.snapshot_manifest VALUES ('{MANIFEST}');
        CREATE ROLE reference_reader_qual LOGIN;
        GRANT USAGE ON SCHEMA reference_qual TO reference_reader_qual;
        GRANT SELECT ON ALL TABLES IN SCHEMA reference_qual TO reference_reader_qual;
        INSERT INTO enrichedlab_agent.broker_agent_profiles(tenant_id,profile_key,name,description,harness,thinking,route,
          controller_profile,gateway_profile,image_ref,cpu_limit,memory_limit,state_profile,role,tool_bundle,max_run_seconds,
          max_task_attempts,max_provider_attempts)
        SELECT tenant_id,k,n,description,harness,thinking,r,controller_profile,gateway_profile,image_ref,cpu_limit,
          memory_limit,state_profile,role,tool_bundle,max_run_seconds,1,max_provider_attempts
        FROM enrichedlab_agent.broker_agent_profiles,
          (VALUES ('reference-sql','Query the example reference data','server-query'),
                  ('figure-render','Draw a figure from a result table','server-render'),
                  ('lab-dataset','Read a lab dataset','server-dataset'),
                  ('template-analysis','Run a template analysis on a result table','server-analysis')) v(k,n,r)
        WHERE tenant_id='research-site' AND profile_key='general-research';
        INSERT INTO enrichedlab_agent.broker_group_profile_grants(tenant_id,group_key,profile_key)
          SELECT 'research-site','pilot-lab',k FROM unnest(ARRAY['reference-sql','figure-render','lab-dataset','template-analysis']) k;")).await.unwrap();
    owner
        .query_one(
            "SELECT enrichedlab_agent.broker_register_sql_source('research-site',$1)",
            &[&json!({"source": "example-reference", "profile": "reference-sql", "title": "Example reference data",
                "description": "Invented foods and one invented nutrient's amount per 100 g.", "release": "Qualification release",
                "schema": "reference_qual",
                "relations": [{"name": "food", "row_count": 2, "columns": [{"name": "fdc_id", "type": "integer"}, {"name": "description", "type": "text"}]},
                              {"name": "food_nutrient", "row_count": 2, "columns": [{"name": "fdc_id", "type": "integer"}, {"name": "amount", "type": "double precision"}]}],
                "functions": ["count"], "row_cap": 100, "statement_timeout_ms": 5000,
                "connection_ref": "reference-qual", "snapshot": {"manifest_sha256": MANIFEST}})],
        )
        .await
        .unwrap();
    let new_project = |title: &'static str| {
        let api = api.clone();
        async move {
            let project = Uuid::new_v4();
            api.create_project(
                a,
                &CreateProject {
                    project_id: project,
                    group: "pilot-lab".into(),
                    title: title.into(),
                    visibility: Visibility::Private,
                },
            )
            .await
            .unwrap();
            let research: ResearchState = serde_json::from_value(state(json!([
                step("rank-foods", "pending", None),
                step("compare", "pending", None)
            ])))
            .unwrap();
            api.checkpoint_project(
                a,
                project,
                &CheckpointRequest {
                    expected_revision: 0,
                    request_id: Uuid::new_v4(),
                    state: research,
                },
            )
            .await
            .unwrap();
            project
        }
    };
    let start = |project: Uuid, profiles: &[&str], parallel: i32| {
        let api = api.clone();
        let profiles: Vec<String> = profiles.iter().map(|p| p.to_string()).collect();
        async move {
            api.start_research_run(
                a,
                project,
                &StartRun {
                    run_id: Uuid::new_v4(),
                    expected_revision: 1,
                    allowed_profiles: profiles,
                    max_turns: 6,
                    max_parallel: parallel,
                },
            )
            .await
        }
    };
    let revision = |project: Uuid| {
        let owner = &owner;
        async move {
            owner
                .query_one(
                    "SELECT revision FROM enrichedlab_agent.broker_projects WHERE project_id=$1",
                    &[&project],
                )
                .await
                .unwrap()
                .get::<_, i64>(0)
        }
    };

    // A lab dataset's tasks are not analyses: a queued intake holds none of a run's places.
    let datasets = new_project("Dataset places").await;
    let intake: Uuid = owner
        .query_one(
            "SELECT enrichedlab_agent.broker_submit_project_task('research-site',$1,$2,'dataset-intake',NULL,'pilot-lab',
              'lab-dataset','Read a lab dataset','dataset-intake',repeat('e',64))",
            &[&a, &datasets],
        )
        .await
        .unwrap()
        .get(0);
    let dataset = Uuid::new_v4();
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_datasets(tenant_id,dataset_id,project_id,request_id,file_name,media_type,
               size_bytes,sha256,guard_intake,guard_outcome,guard_findings,guard_policy,grid_sha256,grid_bytes,intake_task_id,
               principal_id)
             VALUES('research-site',$1,$2,$3,'weights.csv','text/csv',10,repeat('f',64),$4,'allowed','[]','policy',
               repeat('f',64),10,$5,$6)",
            &[&dataset, &datasets, &Uuid::new_v4(), &Uuid::new_v4(), &intake, &a],
        )
        .await
        .unwrap();
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_dataset_requests(tenant_id,task_id,project_id,dataset_id,operation,title,
               request,request_sha256,principal_id)
             VALUES('research-site',$1,$2,$3,'intake','weights.csv','{}',repeat('f',64),$4)",
            &[&intake, &datasets, &dataset, &a],
        )
        .await
        .unwrap();
    let one_place = start(datasets, &["reference-sql"], 1).await.unwrap();
    let claim = coordinator
        .claim_research_run("lead-a", 90)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim["run"]["run_id"], json!(one_place));
    let generation = claim["run"]["generation"].as_i64().unwrap();
    // A run that does not list the lab's analysis method reads no method.
    assert_eq!(
        coordinator
            .research_run_analysis_catalog(one_place, "lead-a", generation)
            .await
            .unwrap(),
        json!({"profile": null, "catalog": null})
    );
    coordinator
        .commit_research_turn(
            one_place,
            "lead-a",
            generation,
            revision(datasets).await,
            &state(json!([
                step("rank-foods", "pending", None),
                step("compare", "pending", None)
            ])),
            &json!([query_action("rank-foods")]),
            &json!({"schema_version": 1, "test": "bounded metadata"}),
            "delegate",
        )
        .await
        .unwrap();
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_research_runs SET available_at=clock_timestamp()+interval '1 day' WHERE run_id=$1",
            &[&one_place],
        )
        .await
        .unwrap();
    // The intake has done its part; no worker here serves datasets.
    assert_eq!(api.cancel_task(a, intake).await.unwrap(), "cancelled");

    // A run may list the template-analysis method; figure and dataset methods stay request-only.
    let project = new_project("Lead analyses").await;
    for refused in [
        vec!["reference-sql", "figure-render"],
        vec!["reference-sql", "lab-dataset"],
    ] {
        assert_eq!(
            start(project, &refused, 2).await.unwrap_err().code,
            "profile_forbidden"
        );
    }
    let run = start(project, &["reference-sql", "template-analysis"], 2)
        .await
        .unwrap();
    let claim = coordinator
        .claim_research_run("lead-a", 90)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim["run"]["run_id"], json!(run));
    let generation = claim["run"]["generation"].as_i64().unwrap();
    let committed = coordinator
        .commit_research_turn(
            run,
            "lead-a",
            generation,
            revision(project).await,
            &state(json!([
                step("rank-foods", "pending", None),
                step("compare", "pending", None)
            ])),
            &json!([query_action("rank-foods")]),
            &json!({"schema_version": 1, "test": "bounded metadata"}),
            "delegate",
        )
        .await
        .unwrap();
    let source: Uuid = serde_json::from_value(committed["tasks"][0]["task_id"].clone()).unwrap();

    let root = std::env::temp_dir().join(format!("lead_analyses_codetestfile_{}", Uuid::new_v4()));
    let connections = root.join("connections");
    fs::create_dir_all(root.join("artifacts")).unwrap();
    fs::create_dir_all(&connections).unwrap();
    fs::write(
        connections.join("reference-qual.dsn"),
        dsn.replace("user=postgres", "user=reference_reader_qual"),
    )
    .unwrap();
    let config = WorkerConfig {
        database_dsn: dsn.replace("user=postgres", "user=broker_worker_qual"),
        tenant: "research-site".into(),
        artifact_root: root.join("artifacts"),
        controller_root: root.clone(),
        controller_state: root.clone(),
        controller_config: root.clone(),
        controller_bin: PathBuf::from("/bin/false"),
        openshell_bin: PathBuf::from("/bin/false"),
        docker_bin: PathBuf::from("/bin/false"),
        worker_id: "analysis-worker".into(),
        direct_dispatch: None,
        query_connection_dir: Some(connections),
        render_url: Some(render_service().await),
        dataset_fetcher_config: None,
        live_output: false,
        max_parallel: 1,
        lease_seconds: 120,
        poll_interval: Duration::from_millis(200),
        database_pool: 2,
        max_workspace_files: 10,
        max_workspace_bytes: 1024,
    };
    // The first queued task is the one-place run's query, then this run's.
    for _ in 0..2 {
        let claimed = worker
            .claim_task("analysis-worker", 120)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.route, query_run::ROUTE);
        query_run::execute(worker.clone(), &config, claimed, CancellationToken::new())
            .await
            .unwrap();
    }

    // The lead's next turn: the run's method under its lease, with no catalog until a worker reads one.
    owner
        .execute(
            "UPDATE enrichedlab_agent.broker_research_runs SET available_at=clock_timestamp() WHERE run_id=$1",
            &[&run],
        )
        .await
        .unwrap();
    let claim = coordinator
        .claim_research_run("lead-a", 90)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(claim["run"]["run_id"], json!(run));
    let generation = claim["run"]["generation"].as_i64().unwrap();
    let read = coordinator
        .research_run_analysis_catalog(run, "lead-a", generation)
        .await
        .unwrap();
    assert_eq!(
        read,
        json!({"profile": "template-analysis", "catalog": null})
    );
    assert_eq!(
        coordinator
            .research_run_analysis_catalog(run, "lead-b", generation)
            .await
            .unwrap_err()
            .code,
        "lease_fence_lost"
    );
    let turn_state = state(json!([
        step("rank-foods", "completed", Some(source)),
        step("compare", "pending", None)
    ]));
    let commit = |actions: Value| {
        let coordinator = coordinator.clone();
        let turn_state = turn_state.clone();
        async move {
            let expected = revision(project).await;
            coordinator
                .commit_research_turn(
                    run,
                    "lead-a",
                    generation,
                    expected,
                    &turn_state,
                    &actions,
                    &json!({"schema_version": 1, "test": "bounded metadata"}),
                    "delegate",
                )
                .await
        }
    };
    // The result the ranking step kept its table in.
    let artifact: Uuid = owner
        .query_one(
            "SELECT artifact_id FROM enrichedlab_agent.broker_artifacts WHERE task_id=$1 AND role='result'",
            &[&source],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(
        commit(json!([analysis_action(
            "compare",
            analysis(source, artifact, &"a".repeat(64))
        )]))
        .await
        .unwrap_err()
        .code,
        "analysis_catalog_unavailable"
    );
    analysis_run::refresh_catalog(&worker, &config)
        .await
        .unwrap();
    let read = coordinator
        .research_run_analysis_catalog(run, "lead-a", generation)
        .await
        .unwrap();
    assert_eq!(read["profile"], "template-analysis");
    assert_eq!(read["catalog"]["service_version"], "0.6.0");
    assert_eq!(read["catalog"]["features"][0]["id"], "group_compare");
    let catalog = read["catalog"]["catalog_sha256"]
        .as_str()
        .unwrap()
        .to_owned();
    let analysis = |source: Uuid| analysis(source, artifact, &catalog);

    // Every other shape is refused, and a person's request checks run on the analysis the lead chose.
    let with = |key: &str, value: Value| {
        let mut x = analysis(source);
        x[key] = value;
        x
    };
    let mut extra = analysis(source);
    extra["code"] = json!("analyze <- function(data) data");
    let mut on_query = query_action("compare");
    on_query["template_analysis"] = analysis(source);
    let mut missing = analysis_action("compare", Value::Null);
    missing.as_object_mut().unwrap().remove("template_analysis");
    let mut unpinned = analysis(source);
    unpinned.as_object_mut().unwrap().remove("artifact");
    let mut any_catalog = analysis(source);
    any_catalog
        .as_object_mut()
        .unwrap()
        .remove("catalog_sha256");
    for (actions, code) in [
        (json!([missing]), "template_analysis_invalid"),
        (
            json!([analysis_action("compare", unpinned)]),
            "template_analysis_invalid",
        ),
        (
            json!([analysis_action("compare", any_catalog)]),
            "template_analysis_invalid",
        ),
        (
            json!([analysis_action(
                "compare",
                with("artifact", json!("not-an-id"))
            )]),
            "template_analysis_invalid",
        ),
        (
            json!([analysis_action(
                "compare",
                with("catalog_sha256", json!("A".repeat(64)))
            )]),
            "template_analysis_invalid",
        ),
        // A result the lead was not shown the table in, or a catalog newer than the one it chose from.
        (
            json!([analysis_action(
                "compare",
                with("artifact", json!(Uuid::new_v4()))
            )]),
            "analysis_table_unknown",
        ),
        (
            json!([analysis_action(
                "compare",
                with("catalog_sha256", json!("f".repeat(64)))
            )]),
            "analysis_catalog_changed",
        ),
        (
            json!([analysis_action("compare", json!([1]))]),
            "template_analysis_invalid",
        ),
        (
            json!([analysis_action("compare", extra)]),
            "template_analysis_invalid",
        ),
        (
            json!([analysis_action("compare", with("title", json!(1)))]),
            "template_analysis_invalid",
        ),
        (
            json!([analysis_action(
                "compare",
                with("source_task", json!("not-an-id"))
            )]),
            "template_analysis_invalid",
        ),
        (json!([on_query]), "template_analysis_invalid"),
        (
            json!([analysis_action(
                "compare",
                with("source_task", json!(Uuid::new_v4()))
            )]),
            "task_not_found",
        ),
        (
            json!([analysis_action(
                "compare",
                with("table_path", json!("results/other.csv"))
            )]),
            "analysis_table_unknown",
        ),
        (
            json!([analysis_action(
                "compare",
                with("feature", json!("scatter"))
            )]),
            "analysis_feature_unknown",
        ),
        (
            json!([analysis_action(
                "compare",
                with("settings", json!({"value": "kcal", "group": "description"}))
            )]),
            "analysis_settings_invalid",
        ),
        (
            json!([analysis_action(
                "compare",
                with("settings", json!({"value": "amount"}))
            )]),
            "analysis_settings_invalid",
        ),
    ] {
        assert_eq!(commit(actions).await.unwrap_err().code, code);
    }

    // The analysis the lead chose, as the coordinator builds the step from its reply (the shapes refused above are this
    // one changed): the task's parent is the source, its request records where it came from.
    let chosen =
        Turn::after_ranking(source, artifact, &read).action(project, &lead_reply(TABLE, welch()));
    assert_eq!(chosen["template_analysis"], analysis(source));
    let committed = commit(json!([chosen])).await.unwrap();
    let task: Uuid = serde_json::from_value(committed["tasks"][0]["task_id"].clone()).unwrap();
    let row = owner
        .query_one(
            "SELECT r.source_task_id, r.table_path, r.feature, r.settings, r.title, r.request, r.principal_id,
               pt.parent_task_id, pt.action_key, t.profile_key
             FROM enrichedlab_agent.broker_analysis_requests r
             JOIN enrichedlab_agent.broker_project_tasks pt USING (tenant_id, task_id)
             JOIN enrichedlab_agent.broker_tasks t USING (tenant_id, task_id)
             WHERE r.task_id=$1",
            &[&task],
        )
        .await
        .unwrap();
    assert_eq!(row.get::<_, Uuid>(0), source);
    assert_eq!(row.get::<_, String>(1), TABLE);
    assert_eq!(row.get::<_, String>(2), "group_compare");
    assert_eq!(
        row.get::<_, Value>(3),
        json!({"value": "amount", "group": "description", "test": "t"})
    );
    assert_eq!(row.get::<_, String>(4), "Compare foods");
    let request = row.get::<_, Value>(5);
    assert_eq!(
        request["origin"],
        json!({"run": run, "turn": 2, "action_key": "compare"})
    );
    assert_eq!(
        request["schema"],
        "enrichedlab.template-analysis-request.v1"
    );
    assert_eq!(row.get::<_, Uuid>(6), a);
    assert_eq!(row.get::<_, Option<Uuid>>(7), Some(source));
    assert_eq!(row.get::<_, String>(8), "compare");
    assert_eq!(row.get::<_, String>(9), "template-analysis");

    // The worker runs it as any template analysis.
    let claimed = worker
        .claim_task("analysis-worker", 120)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (claimed.id, claimed.route.as_str()),
        (task, analysis_run::ROUTE)
    );
    analysis_run::execute(worker.clone(), &config, claimed, CancellationToken::new())
        .await
        .unwrap();
    let detail = api.task_analysis_detail(a, task).await.unwrap();
    assert_eq!(detail["source_task"], json!(source));
    assert_eq!(detail["receipts"][0]["status"], "completed");

    // The lead reads a result's digest without the cells of a table that holds the lab's data, by the
    // recorded sources, whenever the digest was written. With nothing marking its source as the lab's, the analysis's
    // table shows its cells to the lead; once the ranked query's task is a lab dataset's task, neither the query's
    // table nor the analysis of it does, though neither path names the lab, and the headline statistics keep theirs.
    let lead_results = || async {
        // The lead's next turn, as when its lease from the last one has run out.
        owner
            .execute(
                "UPDATE enrichedlab_agent.broker_research_runs SET available_at=clock_timestamp(),
                   lease_until=least(lease_until, clock_timestamp()) WHERE run_id=$1",
                &[&run],
            )
            .await
            .unwrap();
        let status: String = owner
            .query_one(
                "SELECT status FROM enrichedlab_agent.broker_research_runs WHERE run_id=$1",
                &[&run],
            )
            .await
            .unwrap()
            .get(0);
        let claim = coordinator
            .claim_research_run("lead-a", 90)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("run not claimable in status {status}"));
        assert_eq!(claim["run"]["run_id"], json!(run));
        let generation = claim["run"]["generation"].as_i64().unwrap();
        coordinator
            .research_run_results(run, "lead-a", generation)
            .await
            .unwrap()
    };
    let tables_of = |results: &Value, of: Uuid| -> Vec<Value> {
        results["results"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["task_id"] == json!(of))
            .flat_map(|r| {
                r["digest"]["tables"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default()
            })
            .collect()
    };
    let shows_cells = |tables: &[Value], path: &str| {
        tables
            .iter()
            .any(|t| t["path"] == path && t.get("cells").is_some())
    };
    let before = lead_results().await;
    assert!(shows_cells(
        &tables_of(&before, task),
        "results/results.csv"
    ));
    assert!(shows_cells(&tables_of(&before, source), TABLE));
    owner
        .execute(
            "INSERT INTO enrichedlab_agent.broker_dataset_requests(tenant_id,task_id,project_id,dataset_id,operation,title,
               request,request_sha256,principal_id)
             VALUES('research-site',$1,$2,$3,'intake','weights.csv','{}',repeat('f',64),$4)",
            &[&source, &project, &dataset, &a],
        )
        .await
        .unwrap();
    let after = lead_results().await;
    for (of, path, omitted) in [
        (source, TABLE, "lab_derived"),
        (task, "results/results.csv", "lab_derived"),
    ] {
        let table = tables_of(&after, of)
            .into_iter()
            .find(|t| t["path"] == path)
            .unwrap();
        assert!(table.get("cells").is_none(), "{path}");
        assert_eq!(table["cells_omitted"], omitted, "{path}");
        assert!(
            table["columns"].is_array() && table["rows"].is_number(),
            "{path}"
        );
    }
    assert!(shows_cells(
        &tables_of(&after, task),
        "results/statistics.csv"
    ));

    fs::remove_dir_all(&root).unwrap();
    drop(owner);
    connection.abort();
}
