//! Completion-aware package response stream.
//!
//! The archive builder reports a terminal result after its final chunk. Dropping
//! the response before that result is observed is a client cancellation.

use std::{
    io,
    pin::Pin,
    task::{Context, Poll},
};

use axum::body::Bytes;
use futures_util::Stream;
use tokio_stream::wrappers::ReceiverStream;

use super::feature_events::{ErrorCode, Outcome};

pub(crate) enum PackageStreamMessage {
    Chunk(Bytes),
    Complete,
    Changed,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PackageStreamOutcome {
    Success,
    Cancelled,
    Changed,
    Failure,
}

impl PackageStreamOutcome {
    pub(crate) const fn feature_event(
        self,
        missing: u64,
    ) -> (Outcome, Option<ErrorCode>, u64, u64) {
        match self {
            Self::Success => (Outcome::Success, None, 1, missing),
            Self::Cancelled => (Outcome::Cancelled, Some(ErrorCode::Cancelled), 0, 0),
            Self::Changed => (
                Outcome::Failure,
                Some(ErrorCode::PackageChangedWhileStreaming),
                0,
                1,
            ),
            Self::Failure => (Outcome::Failure, Some(ErrorCode::PackageFailed), 0, 1),
        }
    }
}

pub(crate) struct PackageStream {
    receiver: ReceiverStream<PackageStreamMessage>,
    completion: Option<Box<dyn FnOnce(PackageStreamOutcome) + Send>>,
    ended: bool,
}

impl PackageStream {
    pub(crate) fn new(
        receiver: tokio::sync::mpsc::Receiver<PackageStreamMessage>,
        completion: impl FnOnce(PackageStreamOutcome) + Send + 'static,
    ) -> Self {
        Self {
            receiver: ReceiverStream::new(receiver),
            completion: Some(Box::new(completion)),
            ended: false,
        }
    }

    fn finish(&mut self, outcome: PackageStreamOutcome) {
        self.ended = true;
        if let Some(completion) = self.completion.take() {
            completion(outcome);
        }
    }
}

impl Stream for PackageStream {
    type Item = Result<Bytes, io::Error>;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.ended {
            return Poll::Ready(None);
        }
        match Pin::new(&mut self.receiver).poll_next(context) {
            Poll::Ready(Some(PackageStreamMessage::Chunk(bytes))) => Poll::Ready(Some(Ok(bytes))),
            Poll::Ready(Some(PackageStreamMessage::Complete)) => {
                self.finish(PackageStreamOutcome::Success);
                Poll::Ready(None)
            }
            Poll::Ready(Some(PackageStreamMessage::Changed)) => {
                self.finish(PackageStreamOutcome::Changed);
                Poll::Ready(Some(Err(io::Error::other(
                    "package_changed_while_streaming",
                ))))
            }
            Poll::Ready(Some(PackageStreamMessage::Failed)) | Poll::Ready(None) => {
                self.finish(PackageStreamOutcome::Failure);
                Poll::Ready(Some(Err(io::Error::other("package_stream_failed"))))
            }
            Poll::Pending => Poll::Pending,
        }
    }
}

impl Drop for PackageStream {
    fn drop(&mut self) {
        if self.completion.is_some() {
            self.finish(PackageStreamOutcome::Cancelled);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::StreamExt;
    use std::sync::{Arc, Mutex};

    fn stream() -> (
        tokio::sync::mpsc::Sender<PackageStreamMessage>,
        PackageStream,
        Arc<Mutex<Vec<PackageStreamOutcome>>>,
    ) {
        let (sender, receiver) = tokio::sync::mpsc::channel(2);
        let outcomes = Arc::new(Mutex::new(Vec::new()));
        let observed = outcomes.clone();
        let stream = PackageStream::new(receiver, move |outcome| {
            observed.lock().unwrap().push(outcome);
        });
        (sender, stream, outcomes)
    }

    #[tokio::test]
    async fn complete_archive_reports_success_only_at_stream_end() {
        let (sender, mut stream, outcomes) = stream();
        sender
            .send(PackageStreamMessage::Chunk(Bytes::from_static(b"archive")))
            .await
            .unwrap();
        sender.send(PackageStreamMessage::Complete).await.unwrap();
        assert_eq!(stream.next().await.unwrap().unwrap(), "archive");
        assert!(outcomes.lock().unwrap().is_empty());
        assert!(stream.next().await.is_none());
        assert_eq!(*outcomes.lock().unwrap(), [PackageStreamOutcome::Success]);
        assert_eq!(
            PackageStreamOutcome::Success.feature_event(3),
            (Outcome::Success, None, 1, 3)
        );
    }

    #[tokio::test]
    async fn changed_archive_reports_failure_with_an_error_item() {
        let (sender, mut stream, outcomes) = stream();
        sender.send(PackageStreamMessage::Changed).await.unwrap();
        assert!(stream.next().await.unwrap().is_err());
        assert_eq!(*outcomes.lock().unwrap(), [PackageStreamOutcome::Changed]);
        assert_eq!(
            PackageStreamOutcome::Changed.feature_event(0),
            (
                Outcome::Failure,
                Some(ErrorCode::PackageChangedWhileStreaming),
                0,
                1
            )
        );
    }

    #[test]
    fn dropped_response_reports_client_cancellation() {
        let (_sender, stream, outcomes) = stream();
        drop(stream);
        assert_eq!(*outcomes.lock().unwrap(), [PackageStreamOutcome::Cancelled]);
        assert_eq!(
            PackageStreamOutcome::Cancelled.feature_event(0),
            (Outcome::Cancelled, Some(ErrorCode::Cancelled), 0, 0)
        );
    }
}
