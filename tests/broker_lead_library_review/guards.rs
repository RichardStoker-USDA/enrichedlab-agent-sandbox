//! Malformed legacy outcomes cannot break packages; the leased writer refuses new malformed rounds.
use super::*;

pub(super) async fn qualify(
    api: &Store,
    owner: &tokio_postgres::Client,
    reader: Uuid,
    run: Uuid,
    project: Uuid,
    artifact: &Value,
) {
    let malformed = [
        Value::Null,
        json!({}),
        json!("wrong"),
        json!([null]),
        json!([{}]),
        json!([{"shown": null}]),
        json!([{"shown": {}}]),
    ];
    owner.batch_execute("BEGIN").await.unwrap();
    let generation: i64 = owner.query_one("UPDATE enrichedlab_agent.broker_research_runs SET status='running', lease_owner='review-guard-test', lease_until=clock_timestamp()+interval '1 minute' WHERE tenant_id='research-site' AND run_id=$1 RETURNING generation", &[&run]).await.unwrap().get(0);
    let digest = artifact["inputs_sha256"].as_str().unwrap();
    for rounds in &malformed {
        let mut record = artifact.clone();
        record["reading"]["rounds"] = rounds.clone();
        owner.batch_execute("SAVEPOINT bad_rounds").await.unwrap();
        let error = owner.query_one("SELECT enrichedlab_agent.broker_literature_review_for_run('research-site',$1,'review-guard-test',$2,$3,$4)", &[&run, &generation, &digest, &record]).await.unwrap_err();
        assert_eq!(
            error.as_db_error().unwrap().message(),
            "broker:context_invalid"
        );
        owner.batch_execute("ROLLBACK TO bad_rounds").await.unwrap();
    }
    // An empty set of rounds is valid for a refusal before any successful read.
    let mut record = artifact.clone();
    record["reading"]["rounds"] = json!([]);
    owner.query_one("SELECT enrichedlab_agent.broker_literature_review_for_run('research-site',$1,'review-guard-test',$2,$3,$4)", &[&run, &generation, &digest, &record]).await.unwrap();
    owner.batch_execute("ROLLBACK").await.unwrap();
    for (i, rounds) in malformed.into_iter().enumerate() {
        let digest = format!("sha256:{:064x}", i + 1);
        let record = json!({"status": "refused", "faults": ["shape"], "specialist": [], "reading": {"rounds": rounds}});
        owner.execute("INSERT INTO enrichedlab_agent.broker_literature_reviews(tenant_id,run_id,inputs_sha256,record) VALUES('research-site',$1,$2,$3)", &[&run,&digest,&record]).await.unwrap();
    }
    let package = api.project_package_snapshot(reader, project).await.unwrap();
    assert_eq!(package["literature_reviews"].as_array().unwrap().len(), 8);
}
