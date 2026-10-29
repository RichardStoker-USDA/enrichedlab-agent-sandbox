//! Trusted composition of admitted direct Sandbox execution.
//!
//! This driver does not select a task, create admission, choose mounts, or call
//! Kubernetes itself. Its caller supplies only an already-admitted task and a
//! server-owned retainer factory after the exact Sandbox handle exists.
use super::{
    admission::AdmittedSandbox,
    model::ClaimedTask,
    runtime_events::{RuntimeEvent, RuntimeEventRecorder},
    sandbox::{
        ArtifactRetainer, BrokerLeaseFence, RuntimeCompletion, SandboxHandle, SandboxRuntime,
        SandboxTransport,
    },
};
use anyhow::{Result, bail};
use async_trait::async_trait;
use std::{future::Future, time::Duration};
use tokio::time::{Instant, sleep};
use tokio_util::sync::CancellationToken;

#[derive(Clone, Copy, Debug)]
pub struct DirectExecutionTiming {
    pub lease_seconds: i32,
    pub poll_interval: Duration,
    pub maximum_wait: Duration,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DirectExecutionInterruption {
    CancellationRequested,
    ShutdownRequested,
    DeadlineElapsed,
    LeaseLost,
    LeaseCheckFailed,
    StartFailed,
    ObservationFailed,
    RetainerPreparationFailed,
    FinalizationFailed,
    RuntimeEventFailed,
}

impl DirectExecutionInterruption {
    fn code(self) -> &'static str {
        match self {
            Self::CancellationRequested => "cancellation_requested",
            Self::ShutdownRequested => "shutdown_requested",
            Self::DeadlineElapsed => "deadline_elapsed",
            Self::LeaseLost => "lease_lost",
            Self::LeaseCheckFailed => "lease_check_failed",
            Self::StartFailed => "start_failed",
            Self::ObservationFailed => "observation_failed",
            Self::RetainerPreparationFailed => "retainer_preparation_failed",
            Self::FinalizationFailed => "finalization_failed",
            Self::RuntimeEventFailed => "runtime_event_failed",
        }
    }
}

#[derive(Clone, Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "only a finalized controller outcome retains the authoritative Sandbox handle"
)]
pub enum DirectExecutionOutcome {
    /// The authoritative controller/Pod completion was retained and fenced.
    Finalized {
        handle: SandboxHandle,
        completion: RuntimeCompletion,
    },
    /// The caller must reconnect or recover using this exact handle. It is not
    /// a task success and does not authorize deletion of a running Sandbox.
    RecoveryNeeded {
        handle: Option<SandboxHandle>,
        reason: DirectExecutionInterruption,
    },
}

pub struct DirectExecutionControl<'a> {
    pub cancellation: &'a CancellationToken,
    pub shutdown: &'a CancellationToken,
}

pub struct DirectExecutionDriver {
    timing: DirectExecutionTiming,
}

/// Server-owned terminal export plan. Implementations close over their staged
/// export record, approved roots, and selected file digests; none comes from a
/// task or model request. Preparation starts only after current terminal
/// evidence, when a producer can freeze output selection.
#[async_trait]
pub trait TerminalArtifactRetainerFactory: Send + Sync {
    type Retainer: ArtifactRetainer;

    async fn prepare_terminal_retainer(
        &self,
        task: &ClaimedTask,
        handle: &SandboxHandle,
        completion: &RuntimeCompletion,
        cancellation: &CancellationToken,
    ) -> Result<Self::Retainer>;
}

impl DirectExecutionDriver {
    pub fn new(timing: DirectExecutionTiming) -> Result<Self> {
        if !(60..=900).contains(&timing.lease_seconds)
            || timing.poll_interval.is_zero()
            || timing.poll_interval > Duration::from_secs((timing.lease_seconds / 3) as u64)
            || timing.maximum_wait.is_zero()
        {
            bail!("direct execution timing is invalid")
        }
        Ok(Self { timing })
    }

    /// Runs a single task that has already passed staged-admission validation.
    /// `retainer_factory` is server code that closes over approved roots and a
    /// staged terminal export selection. It receives no task-selected path or
    /// mount and is called only after terminal controller/Pod evidence.
    #[expect(
        clippy::too_many_arguments,
        reason = "the direct runtime boundary keeps lease, recorder, task, control, and retainer authorities explicit"
    )]
    pub async fn run<T, L, E, F>(
        &self,
        runtime: &SandboxRuntime<T>,
        lease: &L,
        recorder: &E,
        task: &ClaimedTask,
        worker_id: &str,
        admitted: &AdmittedSandbox,
        control: DirectExecutionControl<'_>,
        retainer_factory: &F,
    ) -> Result<DirectExecutionOutcome>
    where
        T: SandboxTransport,
        L: BrokerLeaseFence,
        E: RuntimeEventRecorder,
        F: TerminalArtifactRetainerFactory,
    {
        if worker_id.is_empty()
            || task.max_run_seconds < 1
            || self.timing.maximum_wait > Duration::from_secs(task.max_run_seconds as u64)
        {
            bail!("direct execution task timing is invalid")
        }
        let deadline = Instant::now() + self.timing.maximum_wait;
        if let Some(reason) = self
            .interruption(lease, task, worker_id, &control, deadline)
            .await
        {
            return Ok(self
                .recovery(
                    lease, recorder, task, worker_id, &control, deadline, None, reason,
                )
                .await);
        }
        if let Err(reason) = self
            .renew_with_control(lease, task, worker_id, &control, deadline)
            .await
        {
            return Ok(self
                .recovery(
                    lease, recorder, task, worker_id, &control, deadline, None, reason,
                )
                .await);
        }

        let handle = match self
            .supervise(
                lease,
                task,
                worker_id,
                &control,
                deadline,
                runtime.start(
                    lease,
                    task,
                    worker_id,
                    &admitted.canonical,
                    &admitted.authorization,
                ),
            )
            .await
        {
            Supervised::Completed(Ok(handle)) => handle,
            Supervised::Completed(Err(_)) => {
                return Ok(self
                    .recovery(
                        lease,
                        recorder,
                        task,
                        worker_id,
                        &control,
                        deadline,
                        None,
                        DirectExecutionInterruption::StartFailed,
                    )
                    .await);
            }
            Supervised::Interrupted(reason) => {
                // A create whose transport future was interrupted has no safe handle.
                return Ok(self
                    .recovery(
                        lease, recorder, task, worker_id, &control, deadline, None, reason,
                    )
                    .await);
            }
        };
        if let Some(reason) = self
            .required_event(
                lease,
                recorder,
                task,
                worker_id,
                &control,
                deadline,
                RuntimeEvent::SandboxCreated {
                    handle: handle.clone(),
                },
            )
            .await
        {
            return Ok(self
                .recovery(
                    lease,
                    recorder,
                    task,
                    worker_id,
                    &control,
                    deadline,
                    Some(handle),
                    reason,
                )
                .await);
        }

        let mut first_pod = None;
        loop {
            if let Some(reason) = self
                .interruption(lease, task, worker_id, &control, deadline)
                .await
            {
                return Ok(self
                    .recovery(
                        lease,
                        recorder,
                        task,
                        worker_id,
                        &control,
                        deadline,
                        Some(handle),
                        reason,
                    )
                    .await);
            }
            if let Err(reason) = self
                .renew_with_control(lease, task, worker_id, &control, deadline)
                .await
            {
                return Ok(self
                    .recovery(
                        lease,
                        recorder,
                        task,
                        worker_id,
                        &control,
                        deadline,
                        Some(handle),
                        reason,
                    )
                    .await);
            }

            if let Some(observed) = &first_pod {
                match self
                    .supervise(
                        lease,
                        task,
                        worker_id,
                        &control,
                        deadline,
                        runtime.observe_terminal(task, &handle, observed),
                    )
                    .await
                {
                    Supervised::Completed(Ok(Some(completion))) => {
                        if let Some(reason) = self
                            .required_event(
                                lease,
                                recorder,
                                task,
                                worker_id,
                                &control,
                                deadline,
                                RuntimeEvent::TerminalObserved {
                                    handle: handle.clone(),
                                    observation: completion.observation().clone(),
                                    exit_code: completion.exit_code(),
                                },
                            )
                            .await
                        {
                            return Ok(self
                                .recovery(
                                    lease,
                                    recorder,
                                    task,
                                    worker_id,
                                    &control,
                                    deadline,
                                    Some(handle),
                                    reason,
                                )
                                .await);
                        }
                        let retainer = match self
                            .prepare_terminal_retainer(
                                lease,
                                task,
                                worker_id,
                                &control,
                                deadline,
                                &handle,
                                &completion,
                                retainer_factory,
                            )
                            .await
                        {
                            Ok(retainer) => retainer,
                            Err(reason) => {
                                return Ok(self
                                    .recovery(
                                        lease,
                                        recorder,
                                        task,
                                        worker_id,
                                        &control,
                                        deadline,
                                        Some(handle),
                                        reason,
                                    )
                                    .await);
                            }
                        };
                        if let Some(reason) = self
                            .interruption(lease, task, worker_id, &control, deadline)
                            .await
                        {
                            return Ok(self
                                .recovery(
                                    lease,
                                    recorder,
                                    task,
                                    worker_id,
                                    &control,
                                    deadline,
                                    Some(handle),
                                    reason,
                                )
                                .await);
                        }
                        return match self
                            .supervise(
                                lease,
                                task,
                                worker_id,
                                &control,
                                deadline,
                                runtime.finish_observed(
                                    lease,
                                    &retainer,
                                    task,
                                    &handle,
                                    &completion,
                                ),
                            )
                            .await
                        {
                            Supervised::Completed(Ok(())) => {
                                Ok(DirectExecutionOutcome::Finalized { handle, completion })
                            }
                            Supervised::Completed(Err(_)) => Ok(self
                                .recovery(
                                    lease,
                                    recorder,
                                    task,
                                    worker_id,
                                    &control,
                                    deadline,
                                    Some(handle),
                                    DirectExecutionInterruption::FinalizationFailed,
                                )
                                .await),
                            Supervised::Interrupted(reason) => Ok(self
                                .recovery(
                                    lease,
                                    recorder,
                                    task,
                                    worker_id,
                                    &control,
                                    deadline,
                                    Some(handle),
                                    reason,
                                )
                                .await),
                        };
                    }
                    Supervised::Completed(Ok(None)) => {}
                    Supervised::Completed(Err(_)) => {
                        return Ok(self
                            .recovery(
                                lease,
                                recorder,
                                task,
                                worker_id,
                                &control,
                                deadline,
                                Some(handle),
                                DirectExecutionInterruption::ObservationFailed,
                            )
                            .await);
                    }
                    Supervised::Interrupted(reason) => {
                        return Ok(self
                            .recovery(
                                lease,
                                recorder,
                                task,
                                worker_id,
                                &control,
                                deadline,
                                Some(handle),
                                reason,
                            )
                            .await);
                    }
                }
            } else {
                match self
                    .supervise(
                        lease,
                        task,
                        worker_id,
                        &control,
                        deadline,
                        runtime.observe_pod(task, &handle),
                    )
                    .await
                {
                    Supervised::Completed(Ok(Some(observed))) => {
                        if let Some(reason) = self
                            .required_event(
                                lease,
                                recorder,
                                task,
                                worker_id,
                                &control,
                                deadline,
                                RuntimeEvent::PodObserved {
                                    handle: handle.clone(),
                                    observation: observed.clone(),
                                },
                            )
                            .await
                        {
                            return Ok(self
                                .recovery(
                                    lease,
                                    recorder,
                                    task,
                                    worker_id,
                                    &control,
                                    deadline,
                                    Some(handle),
                                    reason,
                                )
                                .await);
                        }
                        first_pod = Some(observed);
                    }
                    Supervised::Completed(Ok(None)) => {}
                    Supervised::Completed(Err(_)) => {
                        return Ok(self
                            .recovery(
                                lease,
                                recorder,
                                task,
                                worker_id,
                                &control,
                                deadline,
                                Some(handle),
                                DirectExecutionInterruption::ObservationFailed,
                            )
                            .await);
                    }
                    Supervised::Interrupted(reason) => {
                        return Ok(self
                            .recovery(
                                lease,
                                recorder,
                                task,
                                worker_id,
                                &control,
                                deadline,
                                Some(handle),
                                reason,
                            )
                            .await);
                    }
                }
            }

            tokio::select! {
                _ = control.cancellation.cancelled() => return Ok(self.recovery(lease, recorder, task, worker_id, &control, deadline, Some(handle), DirectExecutionInterruption::CancellationRequested).await),
                _ = control.shutdown.cancelled() => return Ok(self.recovery(lease, recorder, task, worker_id, &control, deadline, Some(handle), DirectExecutionInterruption::ShutdownRequested).await),
                _ = sleep(self.timing.poll_interval) => {}
            }
        }
    }

    async fn supervise<T, F, L>(
        &self,
        lease: &L,
        task: &ClaimedTask,
        worker_id: &str,
        control: &DirectExecutionControl<'_>,
        deadline: Instant,
        operation: F,
    ) -> Supervised<T>
    where
        F: Future<Output = T>,
        L: BrokerLeaseFence,
    {
        tokio::pin!(operation);
        let mut interval = tokio::time::interval(self.timing.poll_interval);
        interval.tick().await;
        loop {
            tokio::select! {
                result = &mut operation => return Supervised::Completed(result),
                _ = control.cancellation.cancelled() => return Supervised::Interrupted(DirectExecutionInterruption::CancellationRequested),
                _ = control.shutdown.cancelled() => return Supervised::Interrupted(DirectExecutionInterruption::ShutdownRequested),
                _ = tokio::time::sleep_until(deadline) => return Supervised::Interrupted(DirectExecutionInterruption::DeadlineElapsed),
                _ = interval.tick() => {
                    if let Err(reason) = self.renew_with_control(lease, task, worker_id, control, deadline).await {
                        return Supervised::Interrupted(reason);
                    }
                    if let Some(reason) = self.cancellation_request(lease, task, worker_id, control, deadline).await {
                        return Supervised::Interrupted(reason);
                    }
                }
            }
        }
    }

    async fn renew_with_control<L: BrokerLeaseFence>(
        &self,
        lease: &L,
        task: &ClaimedTask,
        worker_id: &str,
        control: &DirectExecutionControl<'_>,
        deadline: Instant,
    ) -> std::result::Result<(), DirectExecutionInterruption> {
        if control.cancellation.is_cancelled() {
            return Err(DirectExecutionInterruption::CancellationRequested);
        }
        if control.shutdown.is_cancelled() {
            return Err(DirectExecutionInterruption::ShutdownRequested);
        }
        if Instant::now() >= deadline {
            return Err(DirectExecutionInterruption::DeadlineElapsed);
        }
        tokio::select! {
            result = lease.renew_lease(task.id, worker_id, task.lease_generation, self.timing.lease_seconds) => match result {
                Ok(true) => Ok(()),
                _ => Err(DirectExecutionInterruption::LeaseLost),
            },
            _ = control.cancellation.cancelled() => Err(DirectExecutionInterruption::CancellationRequested),
            _ = control.shutdown.cancelled() => Err(DirectExecutionInterruption::ShutdownRequested),
            _ = tokio::time::sleep_until(deadline) => Err(DirectExecutionInterruption::DeadlineElapsed),
        }
    }

    async fn interruption<L: BrokerLeaseFence>(
        &self,
        lease: &L,
        task: &ClaimedTask,
        worker_id: &str,
        control: &DirectExecutionControl<'_>,
        deadline: Instant,
    ) -> Option<DirectExecutionInterruption> {
        if control.cancellation.is_cancelled() {
            return Some(DirectExecutionInterruption::CancellationRequested);
        }
        if control.shutdown.is_cancelled() {
            return Some(DirectExecutionInterruption::ShutdownRequested);
        }
        if Instant::now() >= deadline {
            return Some(DirectExecutionInterruption::DeadlineElapsed);
        }
        self.cancellation_request(lease, task, worker_id, control, deadline)
            .await
    }

    async fn cancellation_request<L: BrokerLeaseFence>(
        &self,
        lease: &L,
        task: &ClaimedTask,
        worker_id: &str,
        control: &DirectExecutionControl<'_>,
        deadline: Instant,
    ) -> Option<DirectExecutionInterruption> {
        tokio::select! {
            result = lease.cancel_requested(task.id, worker_id, task.lease_generation) => match result {
                Ok(true) => Some(DirectExecutionInterruption::CancellationRequested),
                Ok(false) => None,
                Err(_) => Some(DirectExecutionInterruption::LeaseCheckFailed),
            },
            _ = control.cancellation.cancelled() => Some(DirectExecutionInterruption::CancellationRequested),
            _ = control.shutdown.cancelled() => Some(DirectExecutionInterruption::ShutdownRequested),
            _ = tokio::time::sleep_until(deadline) => Some(DirectExecutionInterruption::DeadlineElapsed),
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "event recording is supervised by separately fenced runtime and lease authorities"
    )]
    async fn required_event<E, L>(
        &self,
        lease: &L,
        recorder: &E,
        task: &ClaimedTask,
        worker_id: &str,
        control: &DirectExecutionControl<'_>,
        deadline: Instant,
        event: RuntimeEvent,
    ) -> Option<DirectExecutionInterruption>
    where
        E: RuntimeEventRecorder,
        L: BrokerLeaseFence,
    {
        match self
            .supervise(
                lease,
                task,
                worker_id,
                control,
                deadline,
                record(recorder, task, worker_id, event),
            )
            .await
        {
            Supervised::Completed(Ok(true)) => None,
            Supervised::Completed(Ok(false)) => Some(DirectExecutionInterruption::LeaseLost),
            Supervised::Completed(Err(_)) => Some(DirectExecutionInterruption::RuntimeEventFailed),
            Supervised::Interrupted(reason) => Some(reason),
        }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "recovery must retain exact task, lease, recorder, control, deadline, and handle boundaries"
    )]
    async fn recovery<E, L>(
        &self,
        lease: &L,
        recorder: &E,
        task: &ClaimedTask,
        worker_id: &str,
        control: &DirectExecutionControl<'_>,
        deadline: Instant,
        handle: Option<SandboxHandle>,
        reason: DirectExecutionInterruption,
    ) -> DirectExecutionOutcome
    where
        E: RuntimeEventRecorder,
        L: BrokerLeaseFence,
    {
        let _ = self
            .supervise(
                lease,
                task,
                worker_id,
                control,
                deadline,
                record(
                    recorder,
                    task,
                    worker_id,
                    RuntimeEvent::RecoveryNeeded {
                        handle: handle.clone(),
                        code: reason.code().into(),
                    },
                ),
            )
            .await;
        DirectExecutionOutcome::RecoveryNeeded { handle, reason }
    }

    #[expect(
        clippy::too_many_arguments,
        reason = "terminal custody requires explicit lease, task, control, handle, completion, and factory authority"
    )]
    async fn prepare_terminal_retainer<L, F>(
        &self,
        lease: &L,
        task: &ClaimedTask,
        worker_id: &str,
        control: &DirectExecutionControl<'_>,
        deadline: Instant,
        handle: &SandboxHandle,
        completion: &RuntimeCompletion,
        factory: &F,
    ) -> std::result::Result<F::Retainer, DirectExecutionInterruption>
    where
        L: BrokerLeaseFence,
        F: TerminalArtifactRetainerFactory,
    {
        let cancellation = CancellationToken::new();
        let _cancel_on_drop = cancellation.clone().drop_guard();
        let preparation =
            factory.prepare_terminal_retainer(task, handle, completion, &cancellation);
        tokio::pin!(preparation);
        let mut interval = tokio::time::interval(self.timing.poll_interval);
        interval.tick().await;
        loop {
            tokio::select! {
                result = &mut preparation => return result.map_err(|_| DirectExecutionInterruption::RetainerPreparationFailed),
                _ = control.cancellation.cancelled() => {
                    cancellation.cancel();
                    return Err(DirectExecutionInterruption::CancellationRequested);
                }
                _ = control.shutdown.cancelled() => {
                    cancellation.cancel();
                    return Err(DirectExecutionInterruption::ShutdownRequested);
                }
                _ = tokio::time::sleep_until(deadline) => {
                    cancellation.cancel();
                    return Err(DirectExecutionInterruption::DeadlineElapsed);
                }
                _ = interval.tick() => match self.renew_with_control(lease, task, worker_id, control, deadline).await {
                    Ok(()) => {}
                    Err(reason) => {
                        cancellation.cancel();
                        return Err(reason);
                    }
                },
            }
        }
    }
}

enum Supervised<T> {
    Completed(T),
    Interrupted(DirectExecutionInterruption),
}

async fn record<E: RuntimeEventRecorder>(
    recorder: &E,
    task: &ClaimedTask,
    worker_id: &str,
    event: RuntimeEvent,
) -> Result<bool> {
    recorder.record_runtime_event(task, worker_id, &event).await
}
