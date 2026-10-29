//! Reading window, call custody and methods review checks.
use super::*;

fn figure(n: usize, chars: usize, next: Option<&str>) -> Built {
    Built {
        part: json!({"ref": format!("library://{PAPER}/figure/{n}"), "part": "figure", "text": "x".repeat(chars)}),
        withheld: vec![],
        refs: next.map(|r| vec![r.to_owned()]).unwrap_or_default(),
    }
}

#[test]
fn a_part_dropped_in_its_own_round_is_recorded_as_dropped_and_unlocks_nothing() {
    // Three parts of about 7 KB in one round of 16,384 bytes. The first leaves before any
    // call holds it, so it is dropped, not shown, and the ref it would unlock stays unreadable.
    let (mut reading, _) = opened(16_384);
    let unlock = |n: usize| format!("library://{PAPER}/section/aaaaaaaaaaaaaaaa-1@{n}");
    let round = round_of(
        &mut reading,
        vec![
            figure(1, 7000, Some(&unlock(1))),
            figure(2, 7000, Some(&unlock(2))),
            figure(3, 7000, Some(&unlock(3))),
        ],
    );
    assert_eq!(
        refs(&round, "dropped"),
        [format!("library://{PAPER}/figure/1")]
    );
    assert_eq!(
        refs(&round, "shown"),
        [
            format!("library://{PAPER}/figure/2"),
            format!("library://{PAPER}/figure/3")
        ]
    );
    assert!(refs(&round, "evicted").is_empty());
    assert_eq!(reading.resolve(&unlock(1)), Err("read_ref_not_shown"));
    assert!(reading.resolve(&unlock(2)).is_ok() && reading.resolve(&unlock(3)).is_ok());
}

#[test]
fn the_oldest_parts_leave_when_new_ones_need_the_room() {
    let (mut reading, mut context) = opened(16_384);
    round_of(
        &mut reading,
        vec![figure(1, 7000, None), figure(2, 7000, None)],
    );
    // A later round: the third evicts the first, which a call held; the second read again replaces its copy; a part
    // larger than the reading is refused.
    let round = round_of(
        &mut reading,
        vec![
            figure(3, 7000, None),
            figure(2, 100, None),
            figure(4, 20_000, None),
        ],
    );
    assert_eq!(
        refs(&round, "evicted"),
        [format!("library://{PAPER}/figure/1")]
    );
    assert_eq!(round["refused"][0]["code"], "read_part_too_large");
    assert_eq!(
        refs(&round, "shown"),
        [
            format!("library://{PAPER}/figure/3"),
            format!("library://{PAPER}/figure/2")
        ]
    );
    assert_eq!(reading.parts.len(), 2);
    reading.place(&mut context);
    assert_eq!(context["reading"]["parts"].as_array().unwrap().len(), 2);
    assert_eq!(
        context["reading"]["evicted"],
        json!([format!("library://{PAPER}/figure/1")])
    );
    assert_eq!(context["reading_offer"]["rounds_left"], ROUNDS_MAX - 2);
    let cited = context_literature(&context);
    assert!(cited.contains(&format!("library://{PAPER}/figure/2")));
    assert!(!cited.contains(&format!("library://{PAPER}/figure/1")));
    for _ in 2..ROUNDS_MAX {
        reading
            .rounds
            .push(json!({"refused": [], "evicted": [], "dropped": []}));
    }
    reading.place(&mut context);
    assert_eq!(context["reading_offer"]["how"], SPENT);
    assert_eq!(reading.rounds_left(), 0);
    let record = reading.record(100_000);
    assert_eq!(record["rounds"].as_array().unwrap().len(), ROUNDS_MAX);
    assert_eq!(record["held"].as_array().unwrap().len(), 2);
}

#[test]
fn the_methods_review_reads_only_cited_parts_within_its_room() {
    let reading = json!({"parts": [
        {"ref": "library://p/section/a", "part": "section", "paper": "A", "heading": "Methods", "text": "y".repeat(5000)},
        {"ref": "library://p/figure/1", "part": "figure", "paper": "A", "label": "Figure 1", "caption": "Cap."}]});
    let proposal: Proposal = serde_json::from_value(json!({"action": "delegate", "summary": "Run it.",
        "actions": [{"action_key": "a1", "title": "T", "profile": "p", "objective": "O", "rationale": "R",
            "literature_refs": ["library://p/figure/1", "library://p/section/a"]}]}))
    .unwrap();
    let both = cited(&reading, &proposal, 10_000).unwrap();
    assert_eq!(both.as_array().unwrap().len(), 2);
    assert_eq!(
        both[0]["text"].as_str().unwrap().chars().count(),
        REVIEW_PART_CHARS
    );
    assert_eq!(both[1]["heading"], "Figure 1");
    assert_eq!(both[1]["text"], "Cap.");
    // A part that does not fit is passed over; a smaller cited part still goes.
    let small = cited(&reading, &proposal, 600).unwrap();
    assert_eq!(small.as_array().unwrap().len(), 1);
    assert_eq!(small[0]["part"], "figure");
    assert!(cited(&reading, &proposal, 10).is_none());
    let none: Proposal =
        serde_json::from_value(json!({"action": "paused", "summary": "Wait."})).unwrap();
    assert!(cited(&reading, &none, 10_000).is_none());
}

#[test]
fn reading_bytes_must_fit_the_model_beside_its_ceiling() {
    let config = |input: usize, output: usize, window: usize| -> ModelConfig {
        serde_json::from_value(json!({"schema_version": 1, "endpoint": "http://127.0.0.1:1/v1/chat/completions",
            "model": "m", "max_input_bytes": input, "max_output_tokens": output, "context_window_tokens": window}))
        .unwrap()
    };
    assert!(allowance_valid(&config(65536, 32768, 262_144), 98_304));
    assert!(!allowance_valid(
        &config(65536, 32768, 262_144),
        READING_BYTES_MIN - 1
    ));
    assert!(!allowance_valid(
        &config(65536, 32768, 262_144),
        READING_BYTES_MAX + 1
    ));
    assert!(!allowance_valid(&config(65536, 32768, 180_000), 98_304));
    assert!(!allowance_valid(&config(196_608, 1024, 600_000), 65_536));
}

#[test]
fn each_read_call_keeps_its_context_reply_and_reasoning_once() {
    let call = |n: u8, reasoning: &str| Call {
        value: json!({"action": "read"}),
        receipt: json!({"context_sha256": format!("sha256:c{n}"), "content_sha256": format!("sha256:r{n}"),
            "reasoning_sha256": format!("sha256:z{n}")}),
        context: format!("context {n}"),
        content: format!("reply {n}"),
        reasoning: reasoning.into(),
    };
    let calls = vec![call(1, "thinking"), call(2, ""), call(1, "thinking")];
    let (texts, expected) = call_texts(&calls);
    assert_eq!(texts.len(), 5);
    assert_eq!(
        expected
            .iter()
            .map(|v| v.as_str().unwrap())
            .collect::<Vec<_>>(),
        vec![
            "sha256:c1",
            "sha256:r1",
            "sha256:z1",
            "sha256:c2",
            "sha256:r2"
        ]
    );
}
