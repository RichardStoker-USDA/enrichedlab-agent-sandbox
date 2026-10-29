//! Metadata-only runtime observations in the existing broker task event ledger.
use anyhow::{Result, ensure};
use async_trait::async_trait;
use serde_json::{Value, json};

use super::{
    model::ClaimedTask,
    sandbox::{RuntimeObservation, SandboxHandle},
};

#[derive(Clone, Debug)]
pub enum RuntimeEvent {
    SandboxCreated {
        handle: SandboxHandle,
    },
    PodObserved {
        handle: SandboxHandle,
        observation: RuntimeObservation,
    },
    TerminalObserved {
        handle: SandboxHandle,
        observation: RuntimeObservation,
        exit_code: i32,
    },
    RecoveryNeeded {
        handle: Option<SandboxHandle>,
        code: String,
    },
}

/// A false result means the current worker no longer has the lease fence.
/// An error does not prove that the write failed: exact replay is idempotent.
#[async_trait]
pub trait RuntimeEventRecorder: Send + Sync {
    async fn record_runtime_event(
        &self,
        task: &ClaimedTask,
        worker_id: &str,
        event: &RuntimeEvent,
    ) -> Result<bool>;
}

impl RuntimeEvent {
    pub fn code(&self) -> &'static str {
        match self {
            Self::SandboxCreated { .. } => "sandbox_created",
            Self::PodObserved { .. } => "sandbox_pod_observed",
            Self::TerminalObserved { .. } => "sandbox_terminal_observed",
            Self::RecoveryNeeded { .. } => "sandbox_recovery_needed",
        }
    }

    /// Closed, bounded machine fields only. Never accept logs, prompts or result rows.
    pub fn metadata(&self, task: &ClaimedTask, worker_id: &str) -> Result<Value> {
        ensure!(
            task.attempt > 0 && task.lease_generation > 0,
            "runtime_event_invalid"
        );
        let mut metadata = json!({
            "schema_version": 1,
            "attempt": task.attempt,
            "lease_generation": task.lease_generation,
        });
        let handle = match self {
            Self::SandboxCreated { handle }
            | Self::PodObserved { handle, .. }
            | Self::TerminalObserved { handle, .. } => Some(handle),
            Self::RecoveryNeeded { handle, code } => {
                ensure!(recovery_code(code), "runtime_event_invalid");
                metadata["reason"] = json!(code);
                handle.as_ref()
            }
        };
        if let Some(handle) = handle {
            ensure!(
                handle.task_id == task.id
                    && handle.worker_id == worker_id
                    && handle.lease_generation == task.lease_generation
                    && handle.sandbox_generation > 0,
                "runtime_event_identity_mismatch"
            );
            for (key, value) in [
                ("namespace", &handle.namespace),
                ("sandbox_name", &handle.sandbox_name),
                ("sandbox_uid", &handle.sandbox_uid),
            ] {
                ensure!(identifier(value), "runtime_event_invalid");
                metadata[key] = json!(value);
            }
            metadata["sandbox_generation"] = json!(handle.sandbox_generation);
        }
        if let Self::PodObserved {
            handle,
            observation,
        }
        | Self::TerminalObserved {
            handle,
            observation,
            ..
        } = self
        {
            ensure!(
                observation.sandbox_uid == handle.sandbox_uid
                    && observation.sandbox_generation == handle.sandbox_generation,
                "runtime_event_identity_mismatch"
            );
            for (key, value) in [
                ("pod_name", &observation.pod_name),
                ("pod_uid", &observation.pod_uid),
                ("runtime_class_name", &observation.runtime_class_name),
            ] {
                ensure!(identifier(value), "runtime_event_invalid");
                metadata[key] = json!(value);
            }
        }
        if let Self::TerminalObserved { exit_code, .. } = self {
            metadata["exit_code"] = json!(exit_code);
        }
        Ok(metadata)
    }
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 253
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_'))
}

fn recovery_code(value: &str) -> bool {
    matches!(
        value,
        "cancellation_requested"
            | "shutdown_requested"
            | "deadline_elapsed"
            | "lease_lost"
            | "lease_check_failed"
            | "start_failed"
            | "observation_failed"
            | "retainer_preparation_failed"
            | "finalization_failed"
            | "runtime_event_failed"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observations_bind_to_the_task_lease_and_exact_runtime() {
        let task = ClaimedTask {
            id: uuid::Uuid::new_v4(),
            principal_id: uuid::Uuid::new_v4(),
            group_key: "example".into(),
            profile_key: "research".into(),
            objective: "Objective must not enter runtime metadata".into(),
            logical_agent: "example-agent".into(),
            lease_generation: 7,
            attempt: 2,
            max_task_attempts: 3,
            max_provider_attempts: 1,
            harness: String::new(),
            thinking: None,
            route: String::new(),
            controller_profile: String::new(),
            gateway_profile: String::new(),
            image_ref: String::new(),
            cpu_limit: String::new(),
            memory_limit: String::new(),
            state_profile: String::new(),
            role: "research".into(),
            max_run_seconds: 300,
        };
        let handle = SandboxHandle {
            task_id: task.id,
            worker_id: "example-worker".into(),
            lease_generation: 7,
            namespace: "example-lab".into(),
            sandbox_name: "example-sandbox".into(),
            sandbox_uid: "sandbox-uid".into(),
            sandbox_generation: 1,
            lab_ref: "lab://example/research".into(),
        };
        let mut observation = RuntimeObservation {
            sandbox_uid: handle.sandbox_uid.clone(),
            sandbox_generation: 1,
            pod_name: "example-pod".into(),
            pod_uid: "pod-uid".into(),
            runtime_class_name: "kata-clh".into(),
        };
        let event = RuntimeEvent::TerminalObserved {
            handle: handle.clone(),
            observation: observation.clone(),
            exit_code: 0,
        };
        let metadata = event.metadata(&task, "example-worker").unwrap();
        assert_eq!(metadata["pod_uid"], "pod-uid");
        assert_eq!(metadata["lease_generation"], 7);
        assert!(!metadata.to_string().contains(&task.objective));
        assert!(event.metadata(&task, "another-worker").is_err());
        let mut next_claim = task.clone();
        next_claim.lease_generation += 1;
        assert!(event.metadata(&next_claim, "example-worker").is_err());
        next_claim = task.clone();
        next_claim.id = uuid::Uuid::new_v4();
        assert!(event.metadata(&next_claim, "example-worker").is_err());
        observation.sandbox_uid = "replacement".into();
        let event = RuntimeEvent::PodObserved {
            handle,
            observation,
        };
        assert!(event.metadata(&task, "example-worker").is_err());
        let event = RuntimeEvent::RecoveryNeeded {
            handle: None,
            code: "start_failed".into(),
        };
        assert_eq!(
            event
                .metadata(&task, "example-worker")
                .unwrap()
                .as_object()
                .unwrap()
                .len(),
            4
        );
    }

    #[test]
    fn event_fields_are_bounded_machine_metadata() {
        assert!(identifier("sandbox-123"));
        for value in [
            "",
            "private/input.csv",
            "line\ncontents",
            "SELECT * FROM data",
        ] {
            assert!(!identifier(value));
        }
        assert!(!identifier(&"a".repeat(254)));
        assert!(recovery_code("runtime_event_failed"));
        assert!(!recovery_code("raw model output"));
    }
}
