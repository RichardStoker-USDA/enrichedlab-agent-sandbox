use super::*;

const PAPER_A: &str = "library://5b1a1c7e-9d0f-4c3a-8e2b-6f4d3c2b1a00";
const PAPER_B: &str = "library://0c9d8e7f-6a5b-4c3d-9e2f-1a0b9c8d7e6f";

fn context(reading: bool) -> Value {
    let section = format!("{PAPER_A}/section/66214248e9528f7b-1");
    let mut value = json!({
        "reading": {"parts": [
            {"ref": PAPER_A, "part": "outline",
                "sections": [{"ref": section}, {"ref": format!("{PAPER_A}/section/66214248e9528f7b-2")}],
                "figures": [{"ref": format!("{PAPER_A}/figure/1")}], "tables": [{"ref": format!("{PAPER_A}/table/1")}]},
            {"ref": section, "part": "section", "text": "t", "next": format!("{section}@3")}
        ]},
        "literature": [{"ref": format!("{PAPER_B}/7c3e9a21-1d4b-4f6a-9e8c-2a1b3c4d5e6f"), "paper_ref": PAPER_B},
            {"ref": "literature://6f5e4d3c-2b1a-4098-8f7e-6d5c4b3a2f1e/0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d", "origin": "commons"}],
        "literature_claims": [{"ref": format!("{PAPER_B}/1f2e3d4c-5b6a-4987-8a6b-5c4d3e2f1a0b")}]
    });
    if reading {
        value["reading_offer"] = json!({"how": "…", "rounds_left": 2, "bytes": 24576});
    }
    value
}

#[test]
fn listed_parts_passages_and_unopened_papers_each_get_their_fix() {
    let unread = UnreadRefs::of(&context(true));
    let section = format!("{PAPER_A}/section/66214248e9528f7b-1");
    // What the lead read is citable, so it has no fix: the outline itself and the section it read.
    assert_eq!(unread.fix(PAPER_A, NOT_EVIDENCE), NOT_EVIDENCE);
    assert_eq!(unread.fix(&section, NOT_EVIDENCE), NOT_EVIDENCE);
    for listed in [
        format!("{PAPER_A}/section/66214248e9528f7b-2"),
        format!("{PAPER_A}/figure/1"),
        format!("{PAPER_A}/table/1"),
        format!("{section}@3"),
    ] {
        assert_eq!(unread.fix(&listed, NOT_EVIDENCE), LISTED, "{listed}");
    }
    assert_eq!(
        unread.fix(
            &format!("{PAPER_B}/7c3e9a21-1d4b-4f6a-9e8c-2a1b3c4d5e6f"),
            NOT_EVIDENCE
        ),
        PASSAGE
    );
    // A passage the reading offer gave no paper_ref (a commons passage, or a claim's) cannot be read: no read advice.
    assert_eq!(
        unread.fix(
            &format!("{PAPER_B}/1f2e3d4c-5b6a-4987-8a6b-5c4d3e2f1a0b"),
            NOT_EVIDENCE
        ),
        PASSAGE_UNREADABLE
    );
    assert_eq!(
        unread.fix("literature://6f5e4d3c-2b1a-4098-8f7e-6d5c4b3a2f1e/0a1b2c3d-4e5f-4a6b-8c7d-9e0f1a2b3c4d", NOT_EVIDENCE),
        PASSAGE_UNREADABLE
    );
    assert_eq!(unread.fix(PAPER_B, NOT_EVIDENCE), PAPER);
    assert_eq!(
        unread.fix("artifact://tasks/x/y", NOT_EVIDENCE),
        NOT_EVIDENCE
    );
}

#[test]
fn without_a_reading_offer_no_fix_says_to_read() {
    let unread = UnreadRefs::of(&context(false));
    assert_eq!(
        unread.fix(&format!("{PAPER_A}/figure/1"), NOT_EVIDENCE),
        LISTED_UNREADABLE
    );
    assert_eq!(
        unread.fix(
            &format!("{PAPER_B}/7c3e9a21-1d4b-4f6a-9e8c-2a1b3c4d5e6f"),
            NOT_EVIDENCE
        ),
        PASSAGE_UNREADABLE
    );
    assert_eq!(unread.fix(PAPER_B, NOT_EVIDENCE), PAPER_UNREADABLE);
    for fix in [LISTED_UNREADABLE, PASSAGE_UNREADABLE, PAPER_UNREADABLE] {
        assert!(!fix.contains(": read") && !fix.contains("; read"), "{fix}");
    }
}

fn lesson(refs: &[String]) -> Lesson {
    serde_json::from_value(json!({"statement": "s", "kind": "observation", "evidence_refs": refs}))
        .unwrap()
}

#[test]
fn lesson_names_start_at_the_first_faulty_lesson_and_are_bounded() {
    let unread = UnreadRefs::of(&context(true));
    let evidence: HashSet<String> = [format!("{PAPER_A}/section/66214248e9528f7b-1")].into();
    let listed = format!("{PAPER_A}/figure/1");
    let lessons = vec![
        lesson(&["artifact://skipped".to_owned()]),
        lesson(&[
            format!("{PAPER_A}/section/66214248e9528f7b-1"),
            listed.clone(),
        ]),
        lesson(&["artifact://tasks/x/y".to_owned()]),
    ];
    assert_eq!(
        unread.lesson_names(&lessons, 1, &evidence),
        vec![
            format!("lesson 2: {listed} {LISTED}"),
            format!("lesson 3: artifact://tasks/x/y {NOT_EVIDENCE}"),
        ]
    );
    let many: Vec<String> = (0..20).map(|n| format!("artifact://tasks/x/{n}")).collect();
    let names = unread.lesson_names(&[lesson(&many)], 0, &evidence);
    assert_eq!(names.len(), NAMES_MAX);
    assert_eq!(
        names[0],
        format!("lesson 1: artifact://tasks/x/0 {NOT_EVIDENCE}")
    );
}

#[test]
fn every_name_keeps_its_whole_fix_through_the_note_the_next_call_reads() {
    // The ref and the action key are the reply's own text and may be long or multibyte; they give way, never the fix,
    // both here and when with_prior_attempt cuts each name to PRIOR_NAME_CHARS.
    let unread = UnreadRefs::of(&context(true));
    let listed = format!("{PAPER_A}/figure/1");
    let long_refs = [
        format!("artifact://{}", "x".repeat(300)),
        format!("artifact://{}", "é".repeat(200)),
        listed.clone(),
    ];
    let lessons: Vec<Lesson> = long_refs
        .iter()
        .map(|r| lesson(std::slice::from_ref(r)))
        .collect();
    let mut names = unread.lesson_names(&lessons, 0, &HashSet::new());
    let action: ProposedAction = serde_json::from_value(
        json!({"action_key": "k".repeat(100), "title": "t", "profile": "p",
        "objective": "o", "rationale": "r", "literature_refs": long_refs}),
    )
    .unwrap();
    names.extend(unread.literature_names(std::slice::from_ref(&action), 0, &HashSet::new()));
    let fixes = [
        NOT_EVIDENCE,
        NOT_EVIDENCE,
        LISTED,
        NOT_LITERATURE,
        NOT_LITERATURE,
        LISTED,
    ];
    assert_eq!(names.len(), fixes.len());
    for (name, fix) in names.iter().zip(fixes) {
        assert!(
            name.chars().count() <= PRIOR_NAME_CHARS && name.len() <= NAME_BYTES,
            "{name}"
        );
        assert!(name.ends_with(fix), "{name}");
    }
    let mut note = json!({});
    with_prior_attempt(
        &mut note,
        Some(&Refusal {
            reason: "proposal_evidence_unknown",
            names: names.clone(),
            focus: Vec::new(),
        }),
        1,
    );
    let kept: Vec<&str> = note["prior_attempt"]["names"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert!(!kept.is_empty());
    for (kept, fix) in kept.iter().zip(fixes) {
        assert!(kept.ends_with(fix), "{kept}");
    }
}

#[test]
fn action_names_return_at_most_twelve_starting_with_the_first_faults() {
    // What returns is bounded here; that building stops at the bound is the loop's break, read in review.
    let unread = UnreadRefs::default();
    let refs: Vec<String> = (0..10_000)
        .map(|n| format!("artifact://tasks/x/{n}"))
        .collect();
    let action: ProposedAction =
        serde_json::from_value(json!({"action_key": "a", "title": "t", "profile": "p",
        "objective": "o", "rationale": "r", "literature_refs": refs}))
        .unwrap();
    let names = unread.literature_names(&[action], 0, &HashSet::new());
    assert_eq!(names.len(), NAMES_MAX);
    assert_eq!(
        names[0],
        format!("action a: more than {LITERATURE_REFS_MAX} literature_refs")
    );
    assert_eq!(
        names[1],
        format!("action a: artifact://tasks/x/0 {NOT_LITERATURE}")
    );
}

#[test]
fn the_paper_fix_wording_names_both_repairs() {
    // The wording only: that a reply following either repair is accepted is checked in tests/broker_research_runs.rs
    // (a_refusal_names_each_unread_ref_a_reply_cites_with_its_fix for the outline, the lesson test for a section).
    assert!(
        PAPER.contains("outline") && PAPER.contains("section ref"),
        "{PAPER}"
    );
}

#[test]
fn trimming_keeps_read_and_shown_evidence_drops_bare_observations_and_records_only_context_ids() {
    let unread = UnreadRefs::of(&context(true));
    let read = format!("{PAPER_A}/section/66214248e9528f7b-1");
    let evidence: HashSet<String> = ["artifact://tasks/a/b".to_owned(), read.clone()].into();
    let passage = format!("{PAPER_B}/7c3e9a21-1d4b-4f6a-9e8c-2a1b3c4d5e6f");
    // A ref the reply wrote may be any text; it is left out and counted, never recorded.
    let prose = "Tooth length rose 40 percent with orange juice.".to_owned();
    let mut lessons = vec![
        lesson(&[read.clone(), passage.clone()]),
        lesson(std::slice::from_ref(&passage)),
        serde_json::from_value(
            json!({"statement": "s", "kind": "hypothesis", "evidence_refs": [passage]}),
        )
        .unwrap(),
        lesson(&["artifact://tasks/a/b".to_owned()]),
        lesson(&[read.clone(), prose]),
    ];
    let trim = trim_lessons(&mut lessons, &evidence, &unread);
    // The first keeps its read part, the bare observation goes, the hypothesis stays without evidence, the result
    // stays, and the last keeps its read part.
    assert_eq!(lessons.len(), 4);
    assert_eq!(lessons[0].evidence_refs, vec![read.clone()]);
    assert_eq!(lessons[1].kind, LessonKind::Hypothesis);
    assert!(lessons[1].evidence_refs.is_empty());
    assert_eq!(
        lessons[2].evidence_refs,
        vec!["artifact://tasks/a/b".to_owned()]
    );
    assert_eq!(lessons[3].evidence_refs, vec![read]);
    assert_eq!(trim.context_refs, vec![passage]);
    assert_eq!((trim.refs_total, trim.lessons), (4, 1));
    let many: Vec<String> = (0..30).map(|n| format!("artifact://tasks/x/{n}")).collect();
    let mut one = vec![lesson(&many)];
    let trim = trim_lessons(&mut one, &evidence, &unread);
    assert_eq!(
        (trim.refs_total, trim.context_refs.len(), trim.lessons),
        (30, 0, 1)
    );
    // Context ids are recorded at most NAMES_MAX times.
    let listed: Vec<String> = (0..20)
        .map(|n| format!("{PAPER_A}/section/66214248e9528f7b-{}", n + 2))
        .collect();
    let mut wide = json!({"reading": {"parts": [{"ref": PAPER_A, "part": "outline",
        "sections": listed.iter().map(|r| json!({"ref": r})).collect::<Vec<_>>()}]}});
    wide["reading_offer"] = json!({"how": "…"});
    let mut one = vec![lesson(&listed)];
    let trim = trim_lessons(&mut one, &evidence, &UnreadRefs::of(&wide));
    assert_eq!((trim.refs_total, trim.context_refs.len()), (20, NAMES_MAX));
}
