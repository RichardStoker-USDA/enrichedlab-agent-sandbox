//! `GET /api/v1/tasks/{task}/log/stream`: a task's live output pieces as Server-Sent Events,
//! resumable with `Last-Event-ID`, ending with `closed` once the task has ended.
use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex as StdMutex, PoisonError};

use serde_json::Value;

use super::*;

#[derive(Debug, Deserialize)]
pub(super) struct OutputQuery {
    after: Option<i64>,
}

/// Live output streams open at once, in all and per person. Each stream polls the database while it
/// waits, so the number open is bounded rather than left to the connection pool.
const OUTPUT_STREAMS: usize = 64;
const OUTPUT_STREAMS_PER_PERSON: usize = 4;

/// The live output streams open now: in all, and per principal.
#[derive(Default)]
pub struct OutputStreams {
    open: StdMutex<(usize, HashMap<Uuid, usize>)>,
}

impl OutputStreams {
    /// A place for one more stream of `principal`'s, or none when either limit is reached. The place is given back
    /// when it is dropped, when its stream ends or its reader goes away.
    fn admit(self: &Arc<Self>, principal: Uuid) -> Option<OutputStreamPlace> {
        let mut open = self.open.lock().unwrap_or_else(PoisonError::into_inner);
        let (all, people) = &mut *open;
        let mine = people.entry(principal).or_default();
        if *all >= OUTPUT_STREAMS || *mine >= OUTPUT_STREAMS_PER_PERSON {
            if *mine == 0 {
                people.remove(&principal);
            }
            return None;
        }
        *all += 1;
        *mine += 1;
        Some(OutputStreamPlace {
            streams: Arc::clone(self),
            principal,
        })
    }
}

struct OutputStreamPlace {
    streams: Arc<OutputStreams>,
    principal: Uuid,
}

impl Drop for OutputStreamPlace {
    fn drop(&mut self) {
        let mut open = self
            .streams
            .open
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let (all, people) = &mut *open;
        *all -= 1;
        if let Some(mine) = people.get_mut(&self.principal) {
            *mine -= 1;
            if *mine == 0 {
                people.remove(&self.principal);
            }
        }
    }
}

/// Where a reader resumes: the standard `Last-Event-ID` header, else `?after=`, else the start.
fn resume_after(headers: &HeaderMap, query: &OutputQuery) -> Result<i64, ApiError> {
    let header = headers
        .get("last-event-id")
        .map(|value| {
            value
                .to_str()
                .ok()
                .and_then(|text| text.trim().parse::<i64>().ok())
                .filter(|after| *after >= 0)
                .ok_or_else(|| {
                    ApiError::invalid(
                        "output_cursor_invalid",
                        "Last-Event-ID must be a piece number",
                    )
                })
        })
        .transpose()?;
    match header.or(query.after) {
        Some(after) if after < 0 => Err(ApiError::invalid(
            "output_cursor_invalid",
            "after must be a piece number",
        )),
        after => Ok(after.unwrap_or(0)),
    }
}

/// The `closed` event's data: the current attempt's closure and the task's saved output files. Without a closure, as
/// when a worker stopped before closing its attempt, `recorded` is false and the count, size, digest and dropped bytes
/// are null: what was stored is not checked, which is not the same as an attempt that stored nothing.
pub(super) fn closed(read: &Value) -> Value {
    let artifacts = read["artifacts"].clone();
    match read.get("closure").filter(|closure| closure.is_object()) {
        Some(closure) => json!({
            "attempt": closure["attempt"],
            "recorded": true,
            "reason": null,
            "pieces": closure["pieces"],
            "size": closure["size"],
            "sha256": closure["sha256"],
            "dropped": closure["dropped"],
            "artifacts": artifacts,
        }),
        None => json!({
            "attempt": read["attempt"],
            "recorded": false,
            "reason": "no_closure",
            "pieces": null,
            "size": null,
            "sha256": null,
            "dropped": null,
            "artifacts": artifacts,
        }),
    }
}

pub(super) async fn output_stream(
    State(state): State<ApiState>,
    headers: HeaderMap,
    AxumPath(task): AxumPath<Uuid>,
    Query(query): Query<OutputQuery>,
) -> Result<impl IntoResponse, ApiError> {
    let after = resume_after(&headers, &query)?;
    let principal = authenticate(&state, &headers).await?;
    // Refuse a task the reader may not read before the stream starts.
    state
        .store
        .task_output_after(principal, task, after, 1)
        .await
        .map_err(ApiError::from)?;
    let place = state.output_streams.admit(principal).ok_or(ApiError {
        status: StatusCode::TOO_MANY_REQUESTS,
        code: "output_streams_busy",
        message: "Too many live output streams are open; close one and try again",
    })?;
    struct Cursor {
        /// Held while the stream is open.
        _place: OutputStreamPlace,
        state: ApiState,
        principal: Uuid,
        task: Uuid,
        after: i64,
        pending: VecDeque<Value>,
        ending: Option<Value>,
        done: bool,
    }
    let cursor = Cursor {
        _place: place,
        state,
        principal,
        task,
        after,
        pending: VecDeque::new(),
        ending: None,
        done: false,
    };
    let events = stream::unfold(cursor, |mut cursor| async move {
        loop {
            if cursor.done {
                return None;
            }
            if let Some(piece) = cursor.pending.pop_front() {
                let seq = piece["seq"].as_i64().unwrap_or(cursor.after);
                cursor.after = seq;
                let data = serde_json::to_string(&piece).unwrap_or_else(|_| "{}".into());
                return Some((
                    Ok::<Event, Infallible>(
                        Event::default()
                            .id(seq.to_string())
                            .event("piece")
                            .data(data),
                    ),
                    cursor,
                ));
            }
            if let Some(ending) = cursor.ending.take() {
                cursor.done = true;
                return Some((
                    Ok(Event::default().event("closed").data(ending.to_string())),
                    cursor,
                ));
            }
            match cursor
                .state
                .store
                .task_output_after(cursor.principal, cursor.task, cursor.after, 200)
                .await
            {
                Ok(read) => {
                    let pieces = read["pieces"].as_array().cloned().unwrap_or_default();
                    let ended = read["status"].as_str().is_some_and(is_terminal_status);
                    if !pieces.is_empty() {
                        cursor.pending.extend(pieces);
                    } else if ended {
                        // The task ended after this read's pieces were taken, so nothing more can arrive.
                        cursor.ending = Some(closed(&read));
                    } else {
                        sleep(Duration::from_secs(1)).await;
                    }
                }
                Err(_) => {
                    cursor.done = true;
                    return Some((
                        Ok(Event::default()
                            .event("stream_closed")
                            .data("{\"code\":\"output_stream_unavailable\"}")),
                        cursor,
                    ));
                }
            }
        }
    });
    Ok(Sse::new(events).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keepalive"),
    ))
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{api, person, serve, sha};
    use super::*;
    use crate::broker::store::{OutputPiece, OutputTotals};
    use futures_util::StreamExt;

    #[test]
    fn streams_are_limited_in_all_and_per_person() {
        let streams = Arc::new(OutputStreams::default());
        let (me, other) = (Uuid::new_v4(), Uuid::new_v4());
        let mine: Vec<_> = (0..OUTPUT_STREAMS_PER_PERSON)
            .map(|_| streams.admit(me).unwrap())
            .collect();
        assert!(streams.admit(me).is_none(), "a fifth stream for one person");
        let others: Vec<_> = (0..OUTPUT_STREAMS - OUTPUT_STREAMS_PER_PERSON)
            .map(|_| streams.admit(Uuid::new_v4()).unwrap())
            .collect();
        assert!(streams.admit(other).is_none(), "past the limit in all");
        drop(others);
        assert!(streams.admit(me).is_none());
        let another = streams.admit(other);
        assert!(another.is_some());
        drop(mine);
        drop(another);
        let open = streams.open.lock().unwrap();
        assert_eq!((open.0, open.1.len()), (0, 0), "every place is given back");
    }

    #[test]
    fn readers_resume_from_last_event_id_or_after() {
        let mut headers = HeaderMap::new();
        assert_eq!(
            resume_after(&headers, &OutputQuery { after: None }).unwrap(),
            0
        );
        assert_eq!(
            resume_after(&headers, &OutputQuery { after: Some(4) }).unwrap(),
            4
        );
        headers.insert("last-event-id", HeaderValue::from_static(" 7 "));
        assert_eq!(
            resume_after(&headers, &OutputQuery { after: Some(4) }).unwrap(),
            7
        );
        for bad in ["-1", "x", ""] {
            headers.insert("last-event-id", HeaderValue::from_str(bad).unwrap());
            assert!(
                resume_after(&headers, &OutputQuery { after: None }).is_err(),
                "{bad}"
            );
        }
        assert!(resume_after(&HeaderMap::new(), &OutputQuery { after: Some(-2) }).is_err());
        // No closure is not an empty one: nothing is claimed about what was stored.
        assert_eq!(
            closed(&json!({"attempt": 2, "closure": null, "artifacts": []})),
            json!({"attempt": 2, "recorded": false, "reason": "no_closure", "pieces": null, "size": null,
                "sha256": null, "dropped": null, "artifacts": []})
        );
    }

    /// Reads Server-Sent Events from one response as (event, id, data) triples.
    struct Events {
        body: std::pin::Pin<
            Box<dyn futures_util::Stream<Item = reqwest::Result<axum::body::Bytes>> + Send>,
        >,
        text: String,
        seen: Vec<(String, String, Value)>,
    }

    impl Events {
        fn new(response: reqwest::Response) -> Self {
            Self {
                body: Box::pin(response.bytes_stream()),
                text: String::new(),
                seen: Vec::new(),
            }
        }

        /// Reads until `enough` holds for everything seen so far; fails after ten seconds.
        async fn until(
            &mut self,
            enough: impl Fn(&[(String, String, Value)]) -> bool,
        ) -> &[(String, String, Value)] {
            let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
            while !enough(&self.seen) {
                let chunk = tokio::time::timeout_at(deadline, self.body.next())
                    .await
                    .expect("events arrive in time");
                let Some(chunk) = chunk else { break };
                self.text
                    .push_str(&String::from_utf8_lossy(&chunk.unwrap()));
                while let Some(end) = self.text.find("\n\n") {
                    let block: String = self.text.drain(..end + 2).collect();
                    let (mut event, mut id, mut data) =
                        (String::new(), String::new(), String::new());
                    for line in block.lines() {
                        if let Some(value) = line.strip_prefix("event:") {
                            event = value.trim().to_owned();
                        } else if let Some(value) = line.strip_prefix("id:") {
                            id = value.trim().to_owned();
                        } else if let Some(value) = line.strip_prefix("data:") {
                            data.push_str(value.strip_prefix(' ').unwrap_or(value));
                        }
                    }
                    if !event.is_empty() {
                        let data = serde_json::from_str(&data).unwrap_or(Value::Null);
                        self.seen.push((event, id, data));
                    }
                }
            }
            &self.seen
        }
    }

    fn piece(text: &str) -> OutputPiece {
        OutputPiece {
            stream: "combined",
            text: text.into(),
            bytes: text.len() as u32,
            dropped: 0,
            at: "2026-10-12T08:00:00Z".into(),
        }
    }

    #[tokio::test]
    #[ignore = "isolated PostgreSQL through qualify_remote_broker.sh live-output"]
    async fn the_lease_holder_records_output_and_readers_watch_it_to_a_checked_close() {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let dsn = std::fs::read_to_string(
            std::env::var_os("BROKER_QUALIFICATION_MIGRATION_DSN_FILE").unwrap(),
        )
        .unwrap();
        let config: tokio_postgres::Config = dsn.parse().unwrap();
        let (owner, connection) = config.connect(tokio_postgres::NoTls).await.unwrap();
        tokio::spawn(connection);
        owner
            .batch_execute(
                "INSERT INTO enrichedlab_agent.broker_principal_invitations(tenant_id,email,group_key,role) \
                 VALUES ('research-site','output-outsider@example.invalid','pilot-lab','researcher');",
            )
            .await
            .unwrap();
        let api_dsn = dsn.replace("user=postgres", "user=broker_api_qual");
        let worker_dsn = dsn.replace("user=postgres", "user=broker_worker_qual");
        let root = tempfile::tempdir().unwrap();
        let me = person("output-reader", "researcher@example.invalid");
        let state = api(&api_dsn, me.clone(), root.path());
        let actor = state.store.resolve_principal(&me).await.unwrap();
        let outsider = person("output-outsider", "output-outsider@example.invalid");
        let stranger = api(&api_dsn, outsider.clone(), root.path());
        stranger.store.resolve_principal(&outsider).await.unwrap();
        let worker = Store::connect(&worker_dsn, "research-site".into(), 2).unwrap();
        worker.ready_for_live_output(false).await.unwrap();
        state.store.ready_for_live_output(true).await.unwrap();
        assert!(
            state.store.ready_for_live_output(false).await.is_err(),
            "the API role may not record output"
        );
        let task = state
            .store
            .submit_task(
                actor,
                "pilot-lab",
                "general-research",
                "Print progress while it runs.",
                "live-output-1",
                &"d".repeat(64),
            )
            .await
            .unwrap();
        let claimed = worker
            .claim_task("output-worker", 300)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.id, task);
        let g = claimed.lease_generation;

        // Pieces under the lease; the same batch again is not stored twice; gaps, other leases and unsanitized text
        // are refused; the API role cannot record.
        let first = [
            piece("loading data\n"),
            piece("\u{1b}[32mstep 1 done\u{1b}[0m\n"),
        ];
        assert_eq!(
            worker
                .record_task_output(task, "output-worker", g, 0, &first)
                .await
                .unwrap(),
            2
        );
        assert_eq!(
            worker
                .record_task_output(task, "output-worker", g, 0, &first)
                .await
                .unwrap(),
            2,
            "a replayed batch is accepted once"
        );
        for (who, generation, at, pieces, code) in [
            (
                "output-worker",
                g,
                5,
                vec![piece("x\n")],
                "task_output_sequence",
            ),
            (
                "output-worker",
                g,
                0,
                vec![piece("changed\n")],
                "task_output_sequence",
            ),
            (
                "output-worker",
                g + 1,
                2,
                vec![piece("x\n")],
                "lease_fence_lost",
            ),
            ("other-worker", g, 2, vec![piece("x\n")], "lease_fence_lost"),
            (
                "output-worker",
                g,
                2,
                vec![piece("bell\u{7}\n")],
                "task_output_invalid",
            ),
            (
                "output-worker",
                g,
                2,
                vec![piece("\u{1b}[2Kerase\n")],
                "task_output_invalid",
            ),
            (
                "output-worker",
                g,
                2,
                vec![piece("")],
                "task_output_invalid",
            ),
        ] {
            let error = worker
                .record_task_output(task, who, generation, at, &pieces)
                .await
                .unwrap_err();
            assert_eq!(error.code, code, "{who} {generation} {at} {pieces:?}");
        }
        assert!(
            state
                .store
                .record_task_output(task, "output-worker", g, 2, &[piece("x\n")])
                .await
                .is_err()
        );

        // A reader watches the pieces live; a piece recorded after it connects arrives on the same stream, and
        // nothing closes it while the task runs.
        let origin = serve(state.clone()).await;
        let elsewhere = serve(stranger).await;
        let client = reqwest::Client::new();
        let response = client
            .get(format!("{origin}/api/v1/tasks/{task}/log/stream"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        let mut live = Events::new(response);
        let seen = live.until(|seen| seen.len() >= 2).await.to_vec();
        assert_eq!(
            seen.iter()
                .map(|(event, id, _)| (event.as_str(), id.as_str()))
                .collect::<Vec<_>>(),
            [("piece", "1"), ("piece", "2")]
        );
        assert_eq!(seen[1].2["text"], "\u{1b}[32mstep 1 done\u{1b}[0m\n");
        assert_eq!(
            (
                &seen[0].2["stream"],
                &seen[0].2["attempt"],
                &seen[0].2["bytes"],
                &seen[0].2["dropped"]
            ),
            (
                &json!("combined"),
                &json!(claimed.attempt),
                &json!(13),
                &json!(0)
            )
        );
        worker
            .record_task_output(task, "output-worker", g, 2, &[piece("finished\n")])
            .await
            .unwrap();
        let seen = live.until(|seen| seen.len() >= 3).await.to_vec();
        assert_eq!(
            (seen[2].0.as_str(), seen[2].1.as_str(), &seen[2].2["text"]),
            ("piece", "3", &json!("finished\n")),
            "a later piece arrives while the task runs, not a close"
        );
        drop(live);

        // A closure must match what the database stores; once closed, the attempt takes no more pieces.
        let text = "loading data\n\u{1b}[32mstep 1 done\u{1b}[0m\nfinished\n";
        let totals = OutputTotals {
            pieces: 3,
            size: text.len() as i64,
            sha256: hex::encode(Sha256::digest(text.as_bytes())),
            dropped: 0,
        };
        let wrong = OutputTotals {
            sha256: "0".repeat(64),
            ..totals.clone()
        };
        assert_eq!(
            worker
                .close_task_output(task, "output-worker", g, &wrong)
                .await
                .unwrap_err()
                .code,
            "task_output_digest_mismatch"
        );
        assert_eq!(
            worker
                .close_task_output(task, "output-worker", g, &totals)
                .await
                .unwrap(),
            totals.sha256
        );
        assert!(
            worker
                .close_task_output(task, "output-worker", g, &totals)
                .await
                .is_ok()
        );
        assert_eq!(
            worker
                .record_task_output(task, "output-worker", g, 3, &[piece("late\n")])
                .await
                .unwrap_err()
                .code,
            "task_output_closed"
        );
        assert!(
            owner
                .execute(
                    "UPDATE enrichedlab_agent.broker_task_output_pieces SET text = 'edited' WHERE task_id = $1",
                    &[&task],
                )
                .await
                .is_err(),
            "pieces are immutable"
        );

        // The task ends with a saved output file; a reader resuming after piece 1 gets the rest and a checked close.
        let saved = b"loading data\nstep 1 done\nfinished\n";
        let relative = format!("tasks/{task}/stdout.log");
        owner
            .execute(
                "INSERT INTO enrichedlab_agent.broker_artifacts(tenant_id,task_id,role,label,relative_path,media_type,size_bytes,sha256) \
                 VALUES ('research-site',$1,'stdout','Output',$2,'text/plain',$3,$4)",
                &[&task, &relative, &(saved.len() as i64), &sha(saved)],
            )
            .await
            .unwrap();
        assert!(
            worker
                .finish_task(task, "output-worker", g, true, "completed", "")
                .await
                .unwrap()
        );
        let response = client
            .get(format!("{origin}/api/v1/tasks/{task}/log/stream"))
            .header("last-event-id", "1")
            .send()
            .await
            .unwrap();
        let mut resumed = Events::new(response);
        let seen = resumed
            .until(|seen| seen.iter().any(|(event, _, _)| event == "closed"))
            .await
            .to_vec();
        assert_eq!(
            seen.iter()
                .map(|(event, id, _)| (event.as_str(), id.as_str()))
                .collect::<Vec<_>>(),
            [("piece", "2"), ("piece", "3"), ("closed", "")]
        );
        let ending = &seen[2].2;
        assert_eq!(
            (
                &ending["recorded"],
                &ending["pieces"],
                &ending["sha256"],
                &ending["attempt"]
            ),
            (
                &json!(true),
                &json!(3),
                &json!(totals.sha256),
                &json!(claimed.attempt)
            )
        );
        assert_eq!(ending["artifacts"][0]["role"], "stdout");
        assert_eq!(ending["artifacts"][0]["sha256"], sha(saved));
        let whole: String = [&seen[0].2, &seen[1].2]
            .iter()
            .map(|piece| piece["text"].as_str().unwrap())
            .collect();
        assert_eq!(whole, "\u{1b}[32mstep 1 done\u{1b}[0m\nfinished\n");

        // A second task, still running. A batch whose reply was lost is sent again with newer pieces: the stored ones
        // must equal it in every field and are not stored twice; the rest are added.
        let second = state
            .store
            .submit_task(
                actor,
                "pilot-lab",
                "general-research",
                "Print more progress.",
                "live-output-2",
                &"e".repeat(64),
            )
            .await
            .unwrap();
        let claimed = worker
            .claim_task("output-worker", 300)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.id, second);
        let g = claimed.lease_generation;
        let record = |first: i32, pieces: Vec<OutputPiece>| {
            let worker = worker.clone();
            async move {
                worker
                    .record_task_output(second, "output-worker", g, first, &pieces)
                    .await
            }
        };
        let (p1, p2, p3, p4, p5) = (
            piece("1\n"),
            piece("2\n"),
            piece("3\n"),
            piece("4\n"),
            piece("5\n"),
        );
        assert_eq!(record(0, vec![p1, p2]).await.unwrap(), 2);
        assert_eq!(record(2, vec![p3.clone(), p4.clone()]).await.unwrap(), 4);
        assert_eq!(
            record(2, vec![p3.clone(), p4.clone(), p5.clone()])
                .await
                .unwrap(),
            5,
            "the stored prefix is checked and the new piece added"
        );
        assert_eq!(record(4, vec![p5.clone()]).await.unwrap(), 5);
        let other_bytes = OutputPiece {
            bytes: 9,
            ..p4.clone()
        };
        let other_time = OutputPiece {
            at: "2026-10-12T08:00:01Z".into(),
            ..p4.clone()
        };
        for (first, pieces) in [
            (3, vec![other_bytes, p5.clone()]),
            (3, vec![other_time, p5.clone()]),
            (6, vec![piece("6\n")]),
            (-1, vec![piece("6\n")]),
        ] {
            assert_eq!(
                record(first, pieces).await.unwrap_err().code,
                "task_output_sequence",
                "{first}"
            );
        }
        // Direction controls never reach a reader: the database refuses them, and what the sanitizer and the piece
        // builder make from raw output, color codes at every cut included, is accepted.
        assert_eq!(
            record(5, vec![piece("file\u{202e}txt.exe\n")])
                .await
                .unwrap_err()
                .code,
            "task_output_invalid"
        );
        let mut raw = Vec::new();
        for at in 0..3 {
            raw.extend(std::iter::repeat_n(b'a', 4090 - at));
            raw.extend_from_slice(
                b"\x1b[1;31m\xe2\x80\xaered\xe2\x81\xa6\x1b[0m\xc2\x85\x1b]0;title\x07",
            );
        }
        let mut made = Vec::new();
        let mut pieces = crate::broker::live_output::Pieces::default();
        pieces.push(
            &crate::broker::live_output::Line {
                at: None,
                part: 0,
                content: raw,
                newline: true,
            },
            tokio::time::Instant::now(),
            &mut made,
        );
        pieces.finish(&mut made);
        assert!(made.len() >= 3);
        let count = made.len() as i64;
        assert_eq!(record(5, made).await.unwrap(), 5 + count);

        // Four streams per person: a fifth waits for one to close.
        let mut open = Vec::new();
        for _ in 0..4 {
            let response = client
                .get(format!("{origin}/api/v1/tasks/{second}/log/stream"))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), 200);
            open.push(response);
        }
        let response = client
            .get(format!("{origin}/api/v1/tasks/{second}/log/stream"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 429);
        assert_eq!(
            response.json::<Value>().await.unwrap()["error"]["code"],
            "output_streams_busy"
        );
        drop(open);

        // A task that ends without a closure, as when its worker stopped first: the close says nothing was checked.
        assert!(
            worker
                .finish_task(second, "output-worker", g, true, "completed", "")
                .await
                .unwrap()
        );
        let mut ended = None;
        for _ in 0..50 {
            let response = client
                .get(format!("{origin}/api/v1/tasks/{second}/log/stream"))
                .header("last-event-id", (5 + count).to_string())
                .send()
                .await
                .unwrap();
            if response.status() == 200 {
                ended = Some(Events::new(response));
                break;
            }
            // The four streams above give their places back once the server sees them go.
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
        let mut ended = ended.expect("a stream once the others closed");
        let seen = ended
            .until(|seen| seen.iter().any(|(event, _, _)| event == "closed"))
            .await
            .to_vec();
        assert_eq!(
            seen.last().unwrap().2,
            json!({"attempt": claimed.attempt, "recorded": false, "reason": "no_closure", "pieces": null,
                "size": null, "sha256": null, "dropped": null, "artifacts": []})
        );

        // A lease past its expiry records and closes nothing, though no one has claimed the task again yet.
        let third = state
            .store
            .submit_task(
                actor,
                "pilot-lab",
                "general-research",
                "Print progress after a pause.",
                "live-output-3",
                &"f".repeat(64),
            )
            .await
            .unwrap();
        let claimed = worker
            .claim_task("output-worker", 300)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(claimed.id, third);
        let g = claimed.lease_generation;
        owner
            .execute(
                "UPDATE enrichedlab_agent.broker_tasks SET lease_expires_at = clock_timestamp() - interval '1 second' \
                 WHERE task_id = $1",
                &[&third],
            )
            .await
            .unwrap();
        assert_eq!(
            worker
                .record_task_output(third, "output-worker", g, 0, &[piece("late\n")])
                .await
                .unwrap_err()
                .code,
            "lease_fence_lost"
        );
        let nothing = OutputTotals {
            pieces: 0,
            size: 0,
            sha256: hex::encode(Sha256::digest(b"")),
            dropped: 0,
        };
        assert_eq!(
            worker
                .close_task_output(third, "output-worker", g, &nothing)
                .await
                .unwrap_err()
                .code,
            "lease_fence_lost"
        );

        // Nothing for a person who may not read the task, and a bad cursor is refused.
        assert_eq!(
            client
                .get(format!("{elsewhere}/api/v1/tasks/{task}/log/stream"))
                .send()
                .await
                .unwrap()
                .status(),
            404
        );
        assert_eq!(
            client
                .get(format!("{origin}/api/v1/tasks/{task}/log/stream?after=-1"))
                .send()
                .await
                .unwrap()
                .status(),
            400
        );
    }
}
