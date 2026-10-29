//! Synthetic reviewed-query coverage within the reference-sql PostgreSQL drill.
use super::*;
use enrichedlab_agent_sandbox::broker::{
    coordinator::{CodeRefused, Proposal, query_sources, with_query_sources},
    reviewed_query::{Tool, digest},
};

fn reply(tool: &str, parameters: Value) -> Proposal {
    serde_json::from_value(
        json!({"action":"delegate","summary":"Rank the synthetic foods.","actions":[{
        "action_key":"rank-foods","title":"Rank synthetic foods","profile":"reference-sql",
        "objective":"Rank the foods.","rationale":"Answer the synthetic question.",
        "reviewed_query":{"tool":tool,"parameters":parameters}}]}),
    )
    .unwrap()
}

pub async fn prepare(
    owner: &tokio_postgres::Client,
    coordinator: &Store,
    run: Uuid,
    generation: i64,
) -> Value {
    let template = "SELECT f.description, n.amount FROM food f JOIN food_nutrient n USING (fdc_id) WHERE $1 = '' OR f.description = $1 ORDER BY n.amount DESC NULLS LAST, f.description LIMIT 100";
    let mut body = json!({"key":"rank-synthetic","version":1,"source":"example-reference","title":"Rank synthetic foods",
        "purpose":"Rank the synthetic source with a typed name filter.","parameters":[
            {"name":"name","type":"text","max_length":64,"required":false,"default":"","description":"Optional exact name"}],
        "output_columns":[{"name":"description","type":"text","description":"Synthetic food name"},
            {"name":"amount","type":"double precision","description":"Synthetic amount"}],
        "row_cap":100,"state":"enabled"});
    // Exercise the owner CLI, which calls the common lowering and guard before SQL registration.
    let folder = env::temp_dir().join(format!("reviewed_query_codetestfile_{}", Uuid::new_v4()));
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("tool.json"), serde_json::to_vec(&body).unwrap()).unwrap();
    fs::write(folder.join("template.sql"), template).unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_agent-sandbox-broker"))
        .args([
            "reviewed-query",
            "register",
            "--tenant",
            "research-site",
            "--migration-dsn-file",
        ])
        .arg(env::var_os("BROKER_QUALIFICATION_MIGRATION_DSN_FILE").unwrap())
        .arg("--file")
        .arg(folder.join("tool.json"))
        .arg("--template")
        .arg(folder.join("template.sql"))
        .output()
        .unwrap();
    fs::remove_dir_all(folder).unwrap();
    assert!(output.status.success(), "owner registration command failed");
    body["template"] = json!(template);
    body["template_sha256"] = json!(digest(template.as_bytes()));
    let register = "SELECT enrichedlab_agent.broker_register_reviewed_query('research-site',$1)";
    let first: Value = owner.query_one(register, &[&body]).await.unwrap().get(0);
    assert_eq!(first["key"], "rank-synthetic");
    let mut changed = body.clone();
    changed["title"] = json!("Different title");
    let error = owner.query_one(register, &[&changed]).await.unwrap_err();
    assert_eq!(
        error.as_db_error().unwrap().message(),
        "broker:idempotency_conflict"
    );
    for (field, value) in [
        ("row_cap", json!(101)),
        ("parameters", json!([{"name":"broken"}])),
        ("template_sha256", json!("0".repeat(64))),
    ] {
        let mut invalid = body.clone();
        invalid["key"] = json!("invalid-tool");
        invalid[field] = value;
        assert!(owner.query_one(register, &[&invalid]).await.is_err());
    }
    size_boundary(owner, &body).await;
    let roles=owner.query("SELECT has_function_privilege(r,'enrichedlab_agent.broker_register_reviewed_query(text,jsonb)','EXECUTE') FROM unnest(ARRAY['broker_api_qual','broker_worker_qual','broker_coordinator_qual']) r",&[]).await.unwrap();
    assert!(roles.iter().all(|r| !r.get::<_, bool>(0)));
    assert!(owner.execute("UPDATE enrichedlab_agent.broker_reviewed_queries SET definition=jsonb_set(definition,'{title}','\"Changed\"')",&[]).await.is_err());
    assert!(
        owner
            .execute("DELETE FROM enrichedlab_agent.broker_reviewed_queries", &[])
            .await
            .is_err()
    );

    let mut newer = body.clone();
    newer["version"] = json!(2);
    owner.query_one(register, &[&newer]).await.unwrap();
    owner.execute("UPDATE enrichedlab_agent.broker_reviewed_queries SET definition=jsonb_set(definition,'{state}','\"retired\"') WHERE tool_key='rank-synthetic' AND version=2",&[]).await.unwrap();
    let retired: Value = owner.query_one(register, &[&newer]).await.unwrap().get(0);
    assert_eq!(retired["state"], "retired");
    assert!(owner.execute("UPDATE enrichedlab_agent.broker_reviewed_queries SET definition=jsonb_set(definition,'{state}','\"enabled\"') WHERE tool_key='rank-synthetic' AND version=2",&[]).await.is_err());

    // Bypassing convenience registration cannot bypass proposal admission.
    let mut bad = body.clone();
    bad["key"] = json!("owner-bad-tool");
    bad["template"] = json!(
        "SELECT f.description FROM undeclared_relation f WHERE f.description=$1 ORDER BY 1 LIMIT 100"
    );
    bad["template_sha256"] = json!(digest(bad["template"].as_str().unwrap().as_bytes()));
    let bad_tool: Tool = serde_json::from_value(bad.clone()).unwrap();
    let allowed = Allowed {
        schema: "reference_qual",
        relations: &BTreeSet::from(["food".into(), "food_nutrient".into()]),
        functions: &BTreeSet::from(["count".into(), "max".into(), "round".into()]),
        row_cap: 100,
    };
    assert_eq!(
        bad_tool.registration_check(&allowed).unwrap_err().code,
        "sql_refused_relation"
    );
    owner.execute("INSERT INTO enrichedlab_agent.broker_reviewed_queries(tenant_id,tool_key,version,source_key,definition) VALUES ('research-site','owner-bad-tool',1,'example-reference',$1)",&[&bad]).await.unwrap();
    let sources = query_sources(
        &coordinator
            .research_run_sql_sources(run, "lead-a", generation)
            .await
            .unwrap(),
    )
    .unwrap();
    let methods = with_query_sources(&json!([{"key":"reference-sql"}]), &sources);
    assert!(methods[0]["query"]["reviewed_queries"].is_array());
    assert!(!methods.to_string().contains("SELECT"));
    let refused = reply("owner-bad-tool", json!({"name":"synthetic"}))
        .admit_queries(&sources)
        .unwrap_err();
    assert_eq!(refused.to_string(), "proposal_reviewed_query_invalid");
    assert!(
        refused.downcast_ref::<CodeRefused>().unwrap().names[0].contains("sql_refused_relation")
    );
    let refused = reply("rank-synthetic", json!({"name":42}))
        .admit_queries(&sources)
        .unwrap_err();
    assert_eq!(
        refused.downcast_ref::<CodeRefused>().unwrap().names,
        ["step rank-foods: name: reviewed_query_parameter_type"]
    );
    let count: i64 = owner
        .query_one(
            "SELECT count(*) FROM enrichedlab_agent.broker_task_queries WHERE run_id=$1",
            &[&run],
        )
        .await
        .unwrap()
        .get(0);
    assert_eq!(count, 0, "refused proposals never reach a worker");
    let proposal = reply("rank-synthetic", json!({}));
    let admitted = proposal.admit_queries(&sources).unwrap();
    let request = enrichedlab_agent_sandbox::broker::model::SubmitTaskRequest {
        group: "pilot-lab".into(),
        profile: "reference-sql".into(),
        objective: "Rank the foods.".into(),
        project_id: None,
        action_key: Some("rank-foods".into()),
        parent_task_id: None,
    };
    let committed = enrichedlab_agent_sandbox::broker::coordinator::delegated_action(
        &request,
        &proposal.actions[0],
        &Default::default(),
        &Default::default(),
        &admitted,
    );
    committed["query"].clone()
}

/// `jsonb_text_bytes` is the length PostgreSQL gives jsonb text, and the Rust check and the registry agree at the
/// 65,536-byte limit.
async fn size_boundary(owner: &tokio_postgres::Client, body: &Value) {
    use enrichedlab_agent_sandbox::broker::reviewed_query::{
        MAX_DEFINITION_BYTES, jsonb_text_bytes,
    };
    for value in [
        json!(0.25),
        json!(100.0),
        json!(1e-7),
        json!(1.7976931348623157e308),
        json!(-5),
        json!(0),
        json!("q\"\\\n\u{7f}\u{e9}"),
        json!([]),
        json!({}),
        json!({"a": [1, {"b": null, "c": true}], "d": "x"}),
        body.clone(),
    ] {
        let printed: i32 = owner
            .query_one("SELECT octet_length($1::jsonb::text)", &[&value])
            .await
            .unwrap()
            .get(0);
        assert_eq!(printed as usize, jsonb_text_bytes(&value), "{value}");
    }
    let template = "SELECT f.description FROM food f WHERE f.description IN ($1, $2, $3, $4, $5, $6, $7, $8) \
                    ORDER BY f.description LIMIT 10";
    let definition = |key: &str, target: usize| {
        let mut tool = json!({"key": key, "version": 1, "source": "example-reference", "title": "Synthetic size boundary",
            "purpose": "Select synthetic foods by eight closed choices.", "parameters": (1..=8).map(|n| json!({
                "name": format!("p{n}"), "description": "Synthetic choice", "type": "enum", "required": true,
                "choices": []})).collect::<Vec<_>>(),
            "output_columns": [{"name": "description", "type": "text", "description": "Synthetic food name"}],
            "row_cap": 10, "state": "enabled", "template": template, "template_sha256": digest(template.as_bytes())});
        // 200-character choices round-robin while more than 260 bytes remain (each adds 204 once its list is not
        // empty), then one last choice of the remainder less its quotes and separator, which is 53 to 256 characters.
        let mut n = 0;
        while target - jsonb_text_bytes(&tool) > 260 {
            let choices = tool["parameters"][n % 8]["choices"].as_array_mut().unwrap();
            choices.push(json!(format!("{n:05}{}", "c".repeat(195))));
            n += 1;
        }
        let length = target - jsonb_text_bytes(&tool) - 4;
        assert!((6..=256).contains(&length), "{length}");
        let choices = tool["parameters"][n % 8]["choices"].as_array_mut().unwrap();
        assert!(!choices.is_empty() && choices.len() < 64);
        choices.push(json!(format!("{n:05}{}", "c".repeat(length - 5))));
        assert_eq!(jsonb_text_bytes(&tool), target);
        tool
    };
    let allowed = Allowed {
        schema: "reference_qual",
        relations: &BTreeSet::from(["food".into(), "food_nutrient".into()]),
        functions: &BTreeSet::from(["count".into(), "max".into(), "round".into()]),
        row_cap: 100,
    };
    let register = "SELECT enrichedlab_agent.broker_register_reviewed_query('research-site',$1)";
    let at_limit = definition("size-at-limit", MAX_DEFINITION_BYTES);
    let tool: Tool = serde_json::from_value(at_limit.clone()).unwrap();
    tool.registration_check(&allowed).unwrap();
    owner.query_one(register, &[&at_limit]).await.unwrap();
    let over = definition("size-over-limit", MAX_DEFINITION_BYTES + 1);
    let tool: Tool = serde_json::from_value(over.clone()).unwrap();
    assert_eq!(
        tool.registration_check(&allowed).unwrap_err().code,
        "reviewed_query_definition_too_large"
    );
    let error = owner.query_one(register, &[&over]).await.unwrap_err();
    assert_eq!(
        error.as_db_error().unwrap().message(),
        "broker:reviewed_query_invalid"
    );
}

pub fn check_package(bytes: &[u8], query: &Value) {
    use std::io::Read;
    let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    let mut members = std::collections::BTreeMap::new();
    for entry in tar.entries().unwrap() {
        let mut entry = entry.unwrap();
        let name = entry.path().unwrap().to_string_lossy().into_owned();
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).unwrap();
        members.insert(name, bytes);
    }
    assert_eq!(
        members["method/template.sql"],
        query["reviewed_query"]["template"]
            .as_str()
            .unwrap()
            .as_bytes()
    );
    let params: Value = serde_json::from_slice(&members["method/parameters.json"]).unwrap();
    assert_eq!(params["requested"], json!({}));
    assert_eq!(params["normalized"], json!({"name":""}));
    assert_eq!(
        params["schema_sha256"],
        query["reviewed_query"]["schema_sha256"]
    );
    assert_eq!(
        members["method/query.sql"],
        format!("{}\n", query["admitted_sql"].as_str().unwrap()).as_bytes()
    );
}

pub async fn lab_query(
    owner: &tokio_postgres::Client,
    coordinator: &Store,
    run: Uuid,
    generation: i64,
) -> Value {
    let template = "SELECT sample_id, weight_g FROM sample WHERE weight_g >= $1 ORDER BY weight_g DESC, sample_id LIMIT 3";
    let body = json!({"key":"rank-lab-samples","version":1,"source":"lab-weights","title":"Rank synthetic samples",
        "purpose":"Select synthetic sample weights above a lower bound.","parameters":[
            {"name":"minimum","type":"number","minimum":-100.0,"maximum":100.0,"required":false,"default":0.0,"description":"Minimum weight"}],
        "output_columns":[{"name":"sample_id","type":"text","description":"Synthetic identifier"},
            {"name":"weight_g","type":"double precision","description":"Synthetic weight"}],
        "row_cap":3,"state":"enabled","template":template,"template_sha256":digest(template.as_bytes())});
    let tool: Tool = serde_json::from_value(body.clone()).unwrap();
    tool.registration_check(&Allowed {
        schema: "lab_qual",
        relations: &BTreeSet::from(["sample".into()]),
        functions: &BTreeSet::new(),
        row_cap: 100,
    })
    .unwrap();
    owner
        .query_one(
            "SELECT enrichedlab_agent.broker_register_reviewed_query('research-site',$1)",
            &[&body],
        )
        .await
        .unwrap();
    let sources = query_sources(
        &coordinator
            .research_run_sql_sources(run, "lead-a", generation)
            .await
            .unwrap(),
    )
    .unwrap();
    let mut proposal = reply("rank-lab-samples", json!({}));
    proposal.actions[0].profile = "lab-sql".into();
    proposal.actions[0].action_key = "rank-samples".into();
    let queries = proposal.admit_queries(&sources).unwrap();
    let q = &queries["rank-samples"];
    assert_eq!(q.admitted.limit, 3);
    json!({"source":q.source,"proposed_sql":q.proposed,"admitted_sql":q.admitted.sql,
        "sha256":format!("sha256:{}",digest(q.admitted.sql.as_bytes())),"guard":sql_guard::GUARD,
        "relations":q.admitted.relations,"row_limit":q.admitted.limit,"reviewed_query":q.reviewed})
}
