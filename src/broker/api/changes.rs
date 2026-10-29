//! Each reader's change stream: `GET /api/v1/changes/stream`, server-sent events that name what to
//! read again. One listening connection per API replica wakes every open stream when a change commits; each stream then
//! reads the feed from its own position under its reader's access rule (migration 061). The commits of 250 ms make one
//! read per open stream, at most four reads run at once on a replica, and one person holds at most sixteen streams, so
//! the database's load grows with open streams times bursts of change and stays bounded. Replicas need no shared
//! memory: every replica hears every notification.
use super::*;
use crate::broker::store::{ChangeCursor, ChangeRead};
use std::collections::HashMap;
use tokio::sync::{Semaphore, broadcast};

/// One connection lasts at most fifteen minutes, as the project stream does, so a page never holds a stale sign-in.
const STREAM_LIFETIME: Duration = Duration::from_secs(15 * 60);
/// A stream reads the feed this often even with no notification: a notification lost with a dropped connection, or a
/// change held back behind a long transaction, still arrives.
const QUIET_READ: Duration = Duration::from_secs(30);
/// After a notification a stream waits this long so a burst of commits becomes one read.
const SETTLE: Duration = Duration::from_millis(250);
/// The most rows one read lists; more says `behind`, and the reader reads everything again.
const FEED_LIMIT: i32 = 2_000;
/// The most keys one `changed` event names. A read that names more is sent as a reset: the workspace refuses an event
/// of more than 500 keys and would reconnect from the same position again and again.
const MAX_EVENT_KEYS: usize = 256;
/// Feed reads at once across this replica's streams, so streams never take the whole connection pool.
const FEED_READS: usize = 4;
const PRUNE_EVERY: Duration = Duration::from_secs(60 * 60);
const PRUNE_FIRST: Duration = Duration::from_secs(60);
const CHECK_EVERY: Duration = Duration::from_secs(30);
/// The most streams one person holds open on a replica: one per open page, with room for many. A client that leaks
/// connections stops here instead of taking the replica's memory and read budget from everyone else.
const STREAMS_PER_PERSON: usize = 16;

pub(super) struct ChangeHub {
    wake: broadcast::Sender<()>,
    reads: Semaphore,
    open: std::sync::Mutex<HashMap<Uuid, usize>>,
}

/// One person's open stream, counted until the stream ends.
struct Slot {
    hub: Arc<ChangeHub>,
    actor: Uuid,
}

impl ChangeHub {
    fn take_slot(self: &Arc<Self>, actor: Uuid) -> Option<Slot> {
        let mut open = self.open.lock().unwrap_or_else(|e| e.into_inner());
        let count = open.entry(actor).or_default();
        if *count >= STREAMS_PER_PERSON {
            return None;
        }
        *count += 1;
        Some(Slot {
            hub: self.clone(),
            actor,
        })
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        let mut open = self.hub.open.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(count) = open.get_mut(&self.actor) {
            *count -= 1;
            if *count == 0 {
                open.remove(&self.actor);
            }
        }
    }
}

impl ChangeHub {
    /// Starts this replica's listener and the hourly prune.
    pub(super) fn start(store: Store) -> Arc<Self> {
        let (wake, _) = broadcast::channel(16);
        let hub = Arc::new(Self {
            wake,
            reads: Semaphore::new(FEED_READS),
            open: std::sync::Mutex::new(HashMap::new()),
        });
        tokio::spawn(listen(store.clone(), hub.wake.clone()));
        tokio::spawn(prune(store));
        hub
    }
}

/// Holds one listening connection, reconnecting with backoff. After each connect every stream reads the feed, so
/// changes committed while it was down are not lost: each stream reads from its own position.
async fn listen(store: Store, wake: broadcast::Sender<()>) {
    let mut backoff = Duration::from_secs(1);
    loop {
        match store.listen_for_changes().await {
            Ok(mut listener) => {
                backoff = Duration::from_secs(1);
                tracing::info!("change listener connected");
                let _ = wake.send(());
                let mut check = tokio::time::interval(CHECK_EVERY);
                check.tick().await;
                loop {
                    tokio::select! {
                        note = listener.notifications.recv() => match note {
                            Some(tenant) if tenant == store.tenant() => { let _ = wake.send(()); }
                            Some(_) => {}
                            None => break,
                        },
                        _ = check.tick() => {
                            let alive = tokio::time::timeout(Duration::from_secs(10),
                                listener.client.simple_query("SELECT 1")).await;
                            if !matches!(alive, Ok(Ok(_))) {
                                break;
                            }
                        }
                    }
                }
                tracing::warn!("change listener connection ended");
            }
            Err(_) => tracing::warn!("change listener could not connect"),
        }
        sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(30));
    }
}

async fn prune(store: Store) {
    // The first prune runs a minute after start, so a replica that restarts more often than hourly still prunes.
    let mut wait = PRUNE_FIRST;
    loop {
        sleep(wait).await;
        wait = PRUNE_EVERY;
        match store.prune_changes().await {
            Ok(removed) => tracing::info!(removed, "change feed pruned"),
            Err(error) => tracing::warn!(code = error.code, "change feed prune failed"),
        }
    }
}

#[derive(Deserialize)]
pub(super) struct StreamQuery {
    after: Option<String>,
}

/// The event a read calls for, if any: `reset` when the reader must read everything again, `changed` with the keys
/// and whether work still runs in each named project, nothing when no change concerns the reader.
pub(super) fn event_for(read: &ChangeRead, busy: Option<&serde_json::Value>) -> Option<Event> {
    let id = read.position.render();
    if read.behind || read.access_changed || read.keys.len() > MAX_EVENT_KEYS {
        let reason = if read.access_changed && !read.behind {
            "access_changed"
        } else {
            "behind"
        };
        return Some(
            Event::default()
                .id(id)
                .event("reset")
                .data(json!({ "reason": reason }).to_string()),
        );
    }
    if read.keys.is_empty() {
        return None;
    }
    let mut data = json!({ "keys": read.keys });
    if let Some(busy) = busy.filter(|b| b.as_object().is_some_and(|m| !m.is_empty())) {
        data["busy"] = busy.clone();
    }
    Some(
        Event::default()
            .id(id)
            .event("changed")
            .data(data.to_string()),
    )
}

struct Reader {
    state: ApiState,
    hub: Arc<ChangeHub>,
    actor: Uuid,
    position: ChangeCursor,
    wake: broadcast::Receiver<()>,
    opened: std::time::Instant,
    pending: VecDeque<Event>,
    done: bool,
    _slot: Slot,
}

impl Reader {
    /// Reads the feed once and queues what it calls for. A person who may no longer sign in ends the stream; any
    /// other failure waits for the next notification or quiet read.
    async fn read(&mut self) {
        let Ok(_permit) = self.hub.reads.acquire().await else {
            return;
        };
        match self
            .state
            .store
            .read_changes(self.actor, Some(&self.position), FEED_LIMIT)
            .await
        {
            Ok(read) => {
                let busy = self.busy(&read).await;
                self.position = read.position.clone();
                if let Some(event) = event_for(&read, busy.as_ref()) {
                    self.pending.push_back(event);
                }
            }
            Err(error) if error.kind == StoreErrorKind::Unauthorized => {
                self.pending.push_back(closed());
                self.done = true;
            }
            Err(error) => tracing::warn!(code = error.code, "change feed read failed"),
        }
    }

    async fn busy(&self, read: &ChangeRead) -> Option<serde_json::Value> {
        if read.behind
            || read.access_changed
            || read.projects.is_empty()
            || read.projects.len() > 64
        {
            return None;
        }
        self.state
            .store
            .projects_busy(self.actor, &read.projects)
            .await
            .ok()
    }
}

fn closed() -> Event {
    Event::default()
        .event("stream_closed")
        .data("{\"code\":\"principal_disabled\"}")
}

/// `GET /api/v1/changes/stream[?after=<position>]`. A first connection gets `ready` with the newest position. A
/// reconnection, by `after` or the browser's `Last-Event-ID`, first gets one `changed` with what it missed, or
/// `reset` when its position is unknown or older than the rows kept, then `ready`. Every event carries the position
/// as its id. `stream_complete` with reason `renew` ends a connection at its lifetime.
pub(super) async fn stream(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Query(query): Query<StreamQuery>,
) -> Result<Response, ApiError> {
    let actor = authenticate(&state, &headers).await?;
    let Some(hub) = state.changes.clone() else {
        return Err(ApiError::not_found("record_unavailable"));
    };
    let Some(slot) = hub.take_slot(actor) else {
        return Err(ApiError {
            status: StatusCode::TOO_MANY_REQUESTS,
            code: "change_streams_full",
            message: "Too many pages are following changes; close some and try again",
        });
    };
    let asked = query.after.or_else(|| {
        headers
            .get("last-event-id")
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    });
    // Subscribe before the first read, so a change that commits between the two still wakes this stream.
    let wake = hub.wake.subscribe();
    let mut reader = Reader {
        state,
        hub,
        actor,
        position: ChangeCursor { txid: 0, id: 0 },
        wake,
        opened: std::time::Instant::now(),
        pending: VecDeque::new(),
        done: false,
        _slot: slot,
    };
    let parsed = asked.as_deref().map(ChangeCursor::parse);
    if !matches!(parsed, Some(Some(_))) {
        let _permit = reader
            .hub
            .reads
            .acquire()
            .await
            .map_err(|_| ApiError::internal())?;
        reader.position = reader
            .state
            .store
            .read_changes(actor, None, FEED_LIMIT)
            .await?
            .position;
    }
    match parsed {
        None => {}
        // A position this broker never sent: the reader reads everything again from the newest one.
        Some(None) => {
            reader.pending.push_back(
                event_for(
                    &ChangeRead {
                        position: reader.position.clone(),
                        keys: vec![],
                        projects: vec![],
                        access_changed: false,
                        behind: true,
                    },
                    None,
                )
                .expect("a reset"),
            );
        }
        Some(Some(cursor)) => {
            reader.position = cursor;
            reader.read().await;
        }
    }
    // A reader refused while catching up has its stream closed; it is never told the stream is ready.
    if !reader.done {
        reader.pending.push_back(
            Event::default()
                .id(reader.position.render())
                .event("ready")
                .data("{}"),
        );
    }
    let events = stream::unfold(reader, |mut reader| async move {
        loop {
            if let Some(event) = reader.pending.pop_front() {
                return Some((Ok::<Event, Infallible>(event), reader));
            }
            if reader.done {
                return None;
            }
            let left = STREAM_LIFETIME.saturating_sub(reader.opened.elapsed());
            if left.is_zero() {
                reader.done = true;
                let event = Event::default()
                    .event("stream_complete")
                    .data("{\"reason\":\"renew\"}");
                return Some((Ok(event), reader));
            }
            tokio::select! {
                _ = sleep(left) => continue,
                _ = sleep(QUIET_READ) => {}
                woke = reader.wake.recv() => {
                    if matches!(woke, Err(broadcast::error::RecvError::Closed)) {
                        reader.done = true;
                        continue;
                    }
                    sleep(SETTLE).await;
                    while reader.wake.try_recv().is_ok() {}
                }
            }
            reader.read().await;
        }
    });
    Ok(Sse::new(events)
        .keep_alive(
            KeepAlive::new()
                .interval(Duration::from_secs(15))
                .text("keepalive"),
        )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::super::test_support::{api, person, serve};
    use super::*;
    use crate::broker::project::{CreateProject, Visibility};

    fn read(keys: &[&str], behind: bool, access_changed: bool) -> ChangeRead {
        ChangeRead {
            position: ChangeCursor { txid: 7, id: 2 },
            keys: keys.iter().map(|k| (*k).to_owned()).collect(),
            projects: vec![],
            access_changed,
            behind,
        }
    }

    async fn text(event: Event) -> String {
        // An event's wire form, as the browser receives it.
        let response = Sse::new(stream::iter([Ok::<Event, Infallible>(event)])).into_response();
        let body = axum::body::to_bytes(response.into_body(), 1 << 20)
            .await
            .unwrap();
        String::from_utf8(body.to_vec()).unwrap()
    }

    #[test]
    fn one_person_holds_at_most_sixteen_streams_until_one_ends() {
        let hub = Arc::new(ChangeHub {
            wake: broadcast::channel(16).0,
            reads: Semaphore::new(FEED_READS),
            open: std::sync::Mutex::new(HashMap::new()),
        });
        let (me, other) = (Uuid::new_v4(), Uuid::new_v4());
        let mut mine: Vec<Slot> = (0..STREAMS_PER_PERSON)
            .map(|_| hub.take_slot(me).unwrap())
            .collect();
        assert!(hub.take_slot(me).is_none());
        let theirs = hub
            .take_slot(other)
            .expect("another person is not held back");
        mine.pop();
        let again = hub
            .take_slot(me)
            .expect("a stream that ended frees its place");
        drop((mine, again, theirs));
        assert!(hub.open.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_read_becomes_reset_changed_or_nothing() {
        assert!(event_for(&read(&[], false, false), None).is_none());
        let reset = text(event_for(&read(&["inbox"], true, true), None).unwrap()).await;
        assert!(
            reset.contains("event: reset\n") && reset.contains("\"reason\":\"behind\""),
            "{reset}"
        );
        assert!(reset.contains("id: x7-2\n"), "{reset}");
        let access = text(event_for(&read(&["inbox"], false, true), None).unwrap()).await;
        assert!(access.contains("\"reason\":\"access_changed\""), "{access}");
        let busy = json!({"5d9e2d5c-1111-4222-8333-944455556666": true});
        let changed =
            text(event_for(&read(&["inbox", "projects"], false, false), Some(&busy)).unwrap())
                .await;
        assert!(changed.contains("event: changed\n"), "{changed}");
        assert!(
            changed.contains("\"keys\":[\"inbox\",\"projects\"]"),
            "{changed}"
        );
        assert!(
            changed.contains("\"busy\":{\"5d9e2d5c-1111-4222-8333-944455556666\":true}"),
            "{changed}"
        );
        // No project named, no busy map.
        let plain =
            text(event_for(&read(&["inbox"], false, false), Some(&json!({}))).unwrap()).await;
        assert!(!plain.contains("busy"), "{plain}");
        // More keys than one event names (the workspace refuses an event of more than 500) is a reset at the new
        // position, so the reader reads everything once and goes on from there.
        let keys: Vec<String> = (0..=super::MAX_EVENT_KEYS)
            .map(|i| format!("task:00000000-0000-4000-8000-{i:012}"))
            .collect();
        let names: Vec<&str> = keys.iter().map(String::as_str).collect();
        let many = text(event_for(&read(&names, false, false), None).unwrap()).await;
        assert!(
            many.contains("event: reset\n") && many.contains("\"reason\":\"behind\""),
            "{many}"
        );
        assert!(many.contains("id: x7-2\n"), "{many}");
        let most = text(event_for(&read(&names[1..], false, false), None).unwrap()).await;
        assert!(most.contains("event: changed\n"), "{}", &most[..80]);
    }

    /// The next event of a stream: its name, id and data.
    async fn next_event(
        response: &mut reqwest::Response,
        buffer: &mut String,
    ) -> (String, Option<String>, serde_json::Value) {
        loop {
            if let Some(end) = buffer.find("\n\n") {
                let frame: String = buffer.drain(..end + 2).collect();
                let field = |name: &str| {
                    frame
                        .lines()
                        .find_map(|line| line.strip_prefix(name))
                        .map(|v| v.trim_start().to_owned())
                };
                if let (Some(event), Some(data)) = (field("event:"), field("data:")) {
                    return (event, field("id:"), serde_json::from_str(&data).unwrap());
                }
                continue;
            }
            let chunk = response.chunk().await.unwrap().expect("the stream ended");
            buffer.push_str(std::str::from_utf8(&chunk).unwrap());
        }
    }

    async fn open(origin: &str, query: &str) -> (reqwest::Response, String) {
        let response = reqwest::Client::new()
            .get(format!("{origin}/api/v1/changes/stream{query}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status().as_u16(), 200);
        assert_eq!(
            response.headers()[header::CONTENT_TYPE],
            "text/event-stream"
        );
        (response, String::new())
    }

    #[tokio::test]
    #[ignore = "isolated PostgreSQL through qualify_remote_broker.sh change-feed"]
    async fn a_change_stream_names_what_changed_and_reads_answer_304_when_unchanged() {
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
                 VALUES ('research-site','changes@example.invalid','pilot-lab','researcher'), \
                   ('research-site','changes-other@example.invalid','pilot-lab','researcher') ON CONFLICT DO NOTHING;",
            )
            .await
            .unwrap();
        let dsn = dsn.replace("user=postgres", "user=broker_api_qual");
        let root = tempfile::tempdir().unwrap();
        let reader = person("changes-reader", "changes@example.invalid");
        let mut state = api(&dsn, reader.clone(), root.path());
        state.changes = Some(ChangeHub::start(state.store.clone()));
        let actor = state.store.resolve_principal(&reader).await.unwrap();
        let origin = serve(state.clone()).await;
        let client = reqwest::Client::new();
        let me: serde_json::Value = client
            .get(format!("{origin}/api/v1/me"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(me["change_stream"], true);
        // Each lab says whether it takes lab data; the rule itself is checked in tests/broker_change_feed.rs.
        assert!(me["groups"][0]["lab_data"].is_boolean(), "{me}");

        // A first connection gets `ready` with the newest position and nothing else.
        let (mut stream, mut buffer) = open(&origin, "").await;
        let (name, ready_id, data) = next_event(&mut stream, &mut buffer).await;
        assert_eq!((name.as_str(), &data), ("ready", &json!({})));
        let ready_id = ready_id.unwrap();
        assert!(ChangeCursor::parse(&ready_id).is_some(), "{ready_id}");

        // A new project reaches the open stream through the listener, with whether work runs there.
        let project = Uuid::new_v4();
        state
            .store
            .create_project(
                actor,
                &CreateProject {
                    project_id: project,
                    group: "pilot-lab".into(),
                    title: "Live list".into(),
                    visibility: Visibility::Lab,
                },
            )
            .await
            .unwrap();
        let (name, changed_id, changed) = tokio::time::timeout(
            Duration::from_secs(10),
            next_event(&mut stream, &mut buffer),
        )
        .await
        .expect("a change within ten seconds");
        assert_eq!(name, "changed");
        let keys: Vec<&str> = changed["keys"]
            .as_array()
            .unwrap()
            .iter()
            .map(|k| k.as_str().unwrap())
            .collect();
        assert!(
            keys.contains(&"projects") && keys.contains(&format!("project:{project}").as_str()),
            "{keys:?}"
        );
        assert_eq!(changed["busy"], json!({ project.to_string(): false }));
        assert_ne!(changed_id.as_deref(), Some(ready_id.as_str()));

        // A reconnection from the earlier position gets what it missed, then `ready`; the browser's own retry header
        // means the same; a position this broker never sent reads as behind.
        let (mut again, mut again_buffer) = open(&origin, &format!("?after={ready_id}")).await;
        let (name, _, missed) = next_event(&mut again, &mut again_buffer).await;
        assert_eq!(name, "changed");
        assert_eq!(missed["keys"], changed["keys"]);
        assert_eq!(next_event(&mut again, &mut again_buffer).await.0, "ready");
        let mut retried = client
            .get(format!("{origin}/api/v1/changes/stream"))
            .header("last-event-id", &ready_id)
            .send()
            .await
            .unwrap();
        let mut retried_buffer = String::new();
        assert_eq!(
            next_event(&mut retried, &mut retried_buffer).await.0,
            "changed"
        );
        let (mut unknown, mut unknown_buffer) = open(&origin, "?after=not-a-position").await;
        let (name, _, reset) = next_event(&mut unknown, &mut unknown_buffer).await;
        assert_eq!(
            (name.as_str(), &reset),
            ("reset", &json!({"reason": "behind"}))
        );
        assert_eq!(
            next_event(&mut unknown, &mut unknown_buffer).await.0,
            "ready"
        );
        // Sixteen streams are open with the twelve below; a seventeenth is refused, and another person still connects.
        let mut held = Vec::new();
        for _ in 0..12 {
            held.push(open(&origin, "").await.0);
        }
        let full = client
            .get(format!("{origin}/api/v1/changes/stream"))
            .send()
            .await
            .unwrap();
        assert_eq!(full.status().as_u16(), 429);
        let refusal: serde_json::Value = full.json().await.unwrap();
        assert_eq!(refusal["error"]["code"], "change_streams_full");
        let mut other = api(
            &dsn,
            person("changes-other", "changes-other@example.invalid"),
            root.path(),
        );
        other.changes = state.changes.clone();
        let other = serve(other).await;
        let (mut theirs, mut theirs_buffer) = open(&other, "").await;
        assert_eq!(next_event(&mut theirs, &mut theirs_buffer).await.0, "ready");
        drop(held);

        // Reads answer with a weak ETag of their body, a matching If-None-Match with 304 and no body, and a new tag
        // once the answer changes.
        let list = client
            .get(format!("{origin}/api/v1/projects"))
            .send()
            .await
            .unwrap();
        assert_eq!(list.status().as_u16(), 200);
        let tag = list.headers()[header::ETAG].to_str().unwrap().to_owned();
        assert!(tag.starts_with("W/\"") && tag.len() == 36, "{tag}");
        let same = client
            .get(format!("{origin}/api/v1/projects"))
            .header(header::IF_NONE_MATCH, &tag)
            .send()
            .await
            .unwrap();
        assert_eq!(same.status().as_u16(), 304);
        assert_eq!(same.headers()[header::ETAG].to_str().unwrap(), tag);
        assert!(same.bytes().await.unwrap().is_empty());
        let strong = client
            .get(format!("{origin}/api/v1/projects"))
            .header(header::IF_NONE_MATCH, tag.trim_start_matches("W/"))
            .send()
            .await
            .unwrap();
        assert_eq!(strong.status().as_u16(), 304, "a weak comparison");
        let other = format!("W/\"{}\"", "0".repeat(32));
        for (asked, status) in [
            ("*".to_owned(), 304),
            (format!("{other}, {tag}"), 304),
            (format!("{other} ,{}", tag.trim_start_matches("W/")), 304),
            (other.clone(), 200),
            (format!("{other}, \"{}\"", "1".repeat(32)), 200),
        ] {
            let answer = client
                .get(format!("{origin}/api/v1/projects"))
                .header(header::IF_NONE_MATCH, &asked)
                .send()
                .await
                .unwrap();
            assert_eq!(answer.status().as_u16(), status, "{asked}");
            assert_eq!(
                answer.headers()[header::ETAG].to_str().unwrap(),
                tag,
                "{asked}"
            );
        }
        state
            .store
            .create_project(
                actor,
                &CreateProject {
                    project_id: Uuid::new_v4(),
                    group: "pilot-lab".into(),
                    title: "Another".into(),
                    visibility: Visibility::Private,
                },
            )
            .await
            .unwrap();
        let changed_list = client
            .get(format!("{origin}/api/v1/projects"))
            .header(header::IF_NONE_MATCH, &tag)
            .send()
            .await
            .unwrap();
        assert_eq!(changed_list.status().as_u16(), 200);
        assert_ne!(changed_list.headers()[header::ETAG].to_str().unwrap(), tag);
        // A change the reader makes is not tagged: only reads are.
        let created = client
            .post(format!("{origin}/api/v1/projects"))
            .header(MUTATION_INTENT, "mutate")
            .json(&json!({"project_id": Uuid::new_v4(), "group": "pilot-lab", "title": "Posted", "visibility": "private"}))
            .send()
            .await
            .unwrap();
        assert!(created.status().is_success(), "{}", created.status());
        assert!(created.headers().get(header::ETAG).is_none());

        // With the stream off, `/me` says so and the route is not served.
        let mut off = api(&dsn, reader.clone(), root.path());
        off.changes = None;
        let off = serve(off).await;
        let me: serde_json::Value = client
            .get(format!("{off}/api/v1/me"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(me["change_stream"], false);
        let missing = client
            .get(format!("{off}/api/v1/changes/stream"))
            .send()
            .await
            .unwrap();
        assert_eq!(missing.status().as_u16(), 404);
    }
}
