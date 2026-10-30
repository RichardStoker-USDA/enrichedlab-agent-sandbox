use super::*;

const MARKER: &str = "SYNTHETIC_PRIVATE_REQUEST_é";

#[tokio::test]
async fn invalid_request_text_stays_in_private_context_not_the_ledger() {
    let store = Store::connect("host=localhost dbname=unused", "test".into(), 1).unwrap();
    for reference in [
        MARKER.to_owned(),
        format!("library://{PAPER}/section/0123456789abcdef-1"),
        "library://00000000-0000-4000-8000-000000000099".into(),
    ] {
        let (mut reading, mut context) = opened(65536);
        let value = json!({"action": "read", "summary": MARKER, "reads": [reference]});
        let call = Call {
            content: value.to_string(),
            value,
            receipt: json!({}),
            context: String::new(),
            reasoning: String::new(),
            system: String::new(),
        };
        reading
            .read(
                &store,
                None,
                None,
                Uuid::nil(),
                "test",
                1,
                "pilot-lab",
                &call,
            )
            .await
            .unwrap();
        reading.place(&mut context);
        assert_eq!(context["reading"]["refused"][0]["ref"], reference);
        let record = reading.record(65536);
        assert_eq!(record["schema_version"], 2);
        assert!(!record.to_string().contains(&reference));
        assert!(!record.to_string().contains(MARKER));
        for key in ["requested", "refused"] {
            let entry = &record["rounds"][0][key][0];
            assert_eq!(entry["ref_sha256"], sha(reference.as_bytes()));
            assert_eq!(entry["ref_bytes"], reference.len());
            assert!(entry.get("ref").is_none());
        }
        assert!(
            record["rounds"][0]["refused"][0]["code"]
                .as_str()
                .unwrap()
                .starts_with("read_ref_")
        );
    }
}

#[tokio::test]
async fn malformed_read_requests_record_only_the_reply_digest_and_bytes() {
    let store = Store::connect("host=localhost dbname=unused", "test".into(), 1).unwrap();
    let (mut reading, _) = opened(65536);
    let value = json!({"action": "read", "reads": [MARKER]});
    let call = Call {
        content: value.to_string(),
        value,
        receipt: json!({}),
        context: String::new(),
        reasoning: String::new(),
        system: String::new(),
    };
    reading
        .read(
            &store,
            None,
            None,
            Uuid::nil(),
            "test",
            1,
            "pilot-lab",
            &call,
        )
        .await
        .unwrap();
    let record = reading.record(65536);
    assert_eq!(
        record["rounds"][0]["refused_request"],
        "read_request_invalid"
    );
    assert_eq!(
        record["rounds"][0]["request_sha256"],
        sha(call.content.as_bytes())
    );
    assert_eq!(record["rounds"][0]["request_bytes"], call.content.len());
    assert!(!record.to_string().contains(MARKER));
}

#[test]
fn every_ledger_ref_requires_broker_provenance() {
    let (mut reading, _) = opened(65536);
    let known = format!("library://{PAPER}");
    let unshown = format!("library://{PAPER}/section/0123456789abcdef-1");
    let mut round = json!({"requested": [known, MARKER], "refused": [], "shown": [],
        "dropped": [], "evicted": [MARKER, known, unshown], "withheld": []});
    for key in ["refused", "shown", "dropped", "withheld"] {
        round[key] = json!([
            {"ref": MARKER, "bytes": 42, "unexpected": MARKER},
            {"ref": unshown, "bytes": 42},
            {"ref": known, "bytes": 42}
        ]);
    }
    reading.rounds.push(round);
    let record = reading.record(65536);
    assert!(!record.to_string().contains(MARKER));
    assert!(!record.to_string().contains(&unshown));
    for key in ["refused", "shown", "dropped", "withheld"] {
        let entries = &record["rounds"][0][key];
        assert!(entries[0].get("ref").is_none());
        assert!(entries[1].get("ref").is_none());
        assert_eq!(entries[2]["ref"], known);
        assert_eq!(entries[0]["bytes"], 42);
    }
    // A section becomes safe only when the broker has shown its ref or its part.
    reading.shown.insert(unshown.clone());
    assert_eq!(
        reading.record(65536)["rounds"][0]["dropped"][1]["ref"],
        unshown
    );
}
