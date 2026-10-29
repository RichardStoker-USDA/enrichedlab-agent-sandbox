//! The broker logs a panic by its location only: a panic message can quote part of a stored file. The hook is the
//! process's, so this runs in a test binary of its own. A task the worker spawned and joined is logged the same way.
use enrichedlab_agent_sandbox::broker;
use std::sync::{Arc, Mutex};

#[derive(Clone, Default)]
struct Buffer(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Buffer {
    fn write(&mut self, data: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(data);
        Ok(data.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Buffer {
    type Writer = Buffer;
    fn make_writer(&'a self) -> Buffer {
        self.clone()
    }
}

#[test]
fn a_panic_is_logged_by_where_it_happened_and_never_by_its_message() {
    let buffer = Buffer::default();
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_writer(buffer.clone())
            .finish(),
    )
    .unwrap();
    broker::log_panics_by_location();
    let line = line!() + 1;
    let panicked = std::thread::spawn(|| panic!("row 7: Example private value")).join();
    assert!(panicked.is_err());
    let logged = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
    assert!(logged.contains("panic"), "{logged}");
    assert!(
        logged.contains(&format!("tests/broker_panic_log.rs:{line}")),
        "{logged}"
    );
    assert!(!logged.contains("Example private value"), "{logged}");

    // A broker task that panics inside the worker's task set: the join names how it ended and the broker task, and
    // the hook names where; the message appears nowhere.
    let task_id = uuid::Uuid::new_v4();
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let joined_line = runtime.block_on(async {
        let mut tasks = tokio::task::JoinSet::new();
        let mut running = std::collections::BTreeMap::new();
        let line = line!() + 1;
        let spawned = tasks.spawn(async { panic!("row 9: Example joined value") });
        running.insert(spawned.id(), task_id);
        broker::worker::forget_joined_task(tasks.join_next_with_id().await, &mut running);
        assert!(running.is_empty());
        line
    });
    let logged = String::from_utf8(buffer.0.lock().unwrap().clone()).unwrap();
    assert!(
        logged.contains(&format!("tests/broker_panic_log.rs:{joined_line}")),
        "{logged}"
    );
    assert!(
        logged.contains("panicked") && logged.contains(&task_id.to_string()),
        "{logged}"
    );
    assert!(!logged.contains("Example joined value"), "{logged}");
}
