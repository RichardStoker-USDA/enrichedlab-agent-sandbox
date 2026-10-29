//! Permits that bound the API's heavy work: listing and reading packages, parsing tables, sorting them, summarizing them
//! and building large response bodies. Each permit moves into the blocking work it covers. A request that ends first,
//! because its client left, cannot then free the permit while that work still runs, so the bound holds for the work
//! itself and not only for the requests waiting on it.
use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::ApiError;

#[derive(Clone)]
pub(super) struct Permits(Arc<Semaphore>);

impl Permits {
    pub(super) fn new(permits: usize) -> Self {
        Self(Arc::new(Semaphore::new(permits)))
    }

    /// Runs `work` on a blocking thread once a permit is free; the permit is released when `work` returns.
    pub(super) async fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Result<T, ApiError> {
        let permit = self
            .0
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| ApiError::internal())?;
        Ok(Self::run_held(permit, work).await?.0)
    }

    /// A permit if one is free now, for work in more than one step: each blocking step runs with `run_held`.
    pub(super) fn try_take(&self) -> Option<OwnedSemaphorePermit> {
        self.0.clone().try_acquire_owned().ok()
    }

    /// Runs `work` on a blocking thread that holds `permit`, and gives the permit back with the result for the next step.
    pub(super) async fn run_held<T: Send + 'static>(
        permit: OwnedSemaphorePermit,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> Result<(T, OwnedSemaphorePermit), ApiError> {
        tokio::task::spawn_blocking(move || (work(), permit))
            .await
            .map_err(|_| ApiError::internal())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, time::Duration};

    /// Starts blocking work that runs until `finish` sends, as a request that is then dropped; returns once it runs.
    async fn abandoned_work(permits: &Permits, held: bool) -> mpsc::Sender<()> {
        let (started, has_started) = mpsc::channel();
        let (finish, may_finish) = mpsc::channel::<()>();
        let work = move || {
            started.send(()).unwrap();
            may_finish.recv().unwrap();
        };
        let request = if held {
            let permit = permits.try_take().unwrap();
            tokio::spawn(async move { Permits::run_held(permit, work).await.map(|_| ()) })
        } else {
            let permits = permits.clone();
            tokio::spawn(async move { permits.run(work).await })
        };
        tokio::task::spawn_blocking(move || has_started.recv().unwrap())
            .await
            .unwrap();
        // The client leaves: the request ends while its work still runs.
        request.abort();
        assert!(request.await.unwrap_err().is_cancelled());
        finish
    }

    #[tokio::test]
    async fn work_keeps_its_permit_after_its_request_ends() {
        for held in [false, true] {
            let permits = Permits::new(1);
            let finish = abandoned_work(&permits, held).await;
            assert!(permits.try_take().is_none(), "held {held}");
            let (ran, has_run) = mpsc::channel();
            let waiting = tokio::spawn({
                let permits = permits.clone();
                async move { permits.run(move || ran.send(()).unwrap()).await }
            });
            tokio::time::sleep(Duration::from_millis(200)).await;
            assert!(
                has_run.try_recv().is_err(),
                "other work started while the abandoned work ran (held {held})"
            );
            finish.send(()).unwrap();
            tokio::time::timeout(Duration::from_secs(10), waiting)
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            assert!(has_run.try_recv().is_ok());
        }
    }
}
