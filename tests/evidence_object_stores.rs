//! Evidence object stores against two real S3 stores: `scripts/custody_stores_local.sh start` prints the variables
//! this test reads (a primary and a replica RustFS with TLS, object lock and replication). Without them it skips.
use std::{env, fs, path::PathBuf, time::Duration};

use enrichedlab_agent_sandbox::broker::evidence_objects::{
    EndpointConfig, Put, S3Store, StoreError, decode, encode, object_key, sha256_hex,
};

fn store(role: &str) -> Option<S3Store> {
    let upper = role.to_ascii_uppercase();
    let url = env::var(format!("CUSTODY_{upper}_URL")).ok()?;
    let config = EndpointConfig {
        url,
        region: env::var("CUSTODY_REGION").unwrap(),
        access_key_file: PathBuf::from(
            env::var(format!("CUSTODY_{upper}_ACCESS_KEY_FILE")).unwrap(),
        ),
        secret_key_file: PathBuf::from(
            env::var(format!("CUSTODY_{upper}_SECRET_KEY_FILE")).unwrap(),
        ),
    };
    let ca = fs::read(env::var("CUSTODY_CA").unwrap()).unwrap();
    Some(S3Store::connect(&config, &ca).unwrap())
}

#[tokio::test]
async fn a_new_object_is_written_once_replicated_and_read_back_exactly() {
    let (Some(primary), Some(replica)) = (store("primary"), store("replica")) else {
        eprintln!("skipped: no custody stores (scripts/custody_stores_local.sh start)");
        return;
    };
    let bucket = env::var("CUSTODY_BUCKET").unwrap();
    let bucket = bucket.as_str();
    let raw = format!("model text for custody {}", uuid::Uuid::new_v4())
        .repeat(50)
        .into_bytes();
    let digest = sha256_hex(&raw);
    let key = object_key(&digest).unwrap();
    let stored = encode(&raw).unwrap();

    let Put::Created { version_id } = primary.put_new(bucket, &key, stored.clone()).await.unwrap()
    else {
        panic!("a new key was not created");
    };
    // The same key again writes nothing.
    assert_eq!(
        primary.put_new(bucket, &key, stored.clone()).await.unwrap(),
        Put::Exists
    );

    let facts = primary.head(bucket, &key, Some(&version_id)).await.unwrap();
    assert_eq!(facts.stored_size, stored.len() as u64);
    assert_eq!(
        facts.retention_mode.as_deref(),
        Some("GOVERNANCE"),
        "the bucket's governance default applies"
    );
    assert!(facts.retain_until.is_some());
    let back = primary
        .get_version(bucket, &key, &version_id, stored.len() as u64)
        .await
        .unwrap();
    assert_eq!(sha256_hex(&decode(&back, raw.len()).unwrap()), digest);
    // A bound smaller than the object refuses it.
    assert!(matches!(
        primary
            .get_version(bucket, &key, &version_id, stored.len() as u64 - 1)
            .await,
        Err(StoreError::Malformed(_))
    ));

    // Replication keeps the version id; wait for it as custody does.
    let mut replica_facts = None;
    for _ in 0..60 {
        match replica.head(bucket, &key, Some(&version_id)).await {
            Ok(found) => {
                replica_facts = Some(found);
                break;
            }
            Err(StoreError::NotFound) => tokio::time::sleep(Duration::from_millis(500)).await,
            Err(other) => panic!("replica read failed: {other}"),
        }
    }
    let replica_facts = replica_facts.expect("the replica holds the version within 30 s");
    assert_eq!(replica_facts.stored_size, stored.len() as u64);
    let replica_back = replica
        .get_version(bucket, &key, &version_id, stored.len() as u64)
        .await
        .unwrap();
    assert_eq!(replica_back, back);

    // An unknown version and an unknown key are not found.
    assert_eq!(
        primary
            .head(bucket, &key, Some("00000000-0000-0000-0000-000000000000"))
            .await
            .unwrap_err(),
        StoreError::NotFound
    );
    // Retention extends on each copy separately (replication does not carry it) and reads back.
    let until = time::OffsetDateTime::now_utc() + time::Duration::days(400);
    for copy in [&primary, &replica] {
        copy.put_retention(bucket, &key, &version_id, until)
            .await
            .unwrap();
        let after = copy.head(bucket, &key, Some(&version_id)).await.unwrap();
        assert!(after.retain_until.unwrap() >= until - time::Duration::seconds(1));
        assert_eq!(after.retention_mode.as_deref(), Some("GOVERNANCE"));
    }
    // Governance only extends: an earlier date is refused.
    assert!(matches!(
        primary
            .put_retention(bucket, &key, &version_id, until - time::Duration::days(30))
            .await,
        Err(StoreError::Refused(_))
    ));
    let missing = object_key(&sha256_hex(b"never stored")).unwrap();
    assert_eq!(
        primary.head(bucket, &missing, None).await.unwrap_err(),
        StoreError::NotFound
    );
}
