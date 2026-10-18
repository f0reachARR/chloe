//! 要求ごとのメタデータを JSON Lines で記録する。本文とヘッダーの値は記録しない。
//! 一行の形式は `chloe-audit` の [`Record`]。

use std::fs::File;
use std::io::Write;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use chloe_audit::{Event, Record, Request};
use rama::bytes::Bytes;
use rama::error::BoxError;
use rama::http::Body;
use rama::http::body::{Frame, SizeHint, StreamingBody};

pub struct Logger {
    file: Option<Mutex<File>>,
}

impl Logger {
    pub fn open(path: Option<&Path>) -> std::io::Result<Self> {
        let file = match path {
            Some(path) => Some(Mutex::new(
                File::options().create(true).append(true).open(path)?,
            )),
            None => None,
        };
        Ok(Self { file })
    }

    pub fn log(&self, event: Event) {
        let Some(file) = &self.file else { return };
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or_default();
        let Ok(mut line) = serde_json::to_vec(&Record { ts, event }) else {
            return;
        };
        line.push(b'\n');
        if let Ok(mut file) = file.lock() {
            let _ = file.write_all(&line);
        }
    }

    pub fn log_request(&self, request: Request, started: Instant) {
        self.log(Event::Request(Box::new(Request {
            duration_ms: Some(elapsed_ms(started)),
            ..request
        })));
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

/// 応答の本文を数えながら流し、流し終えたか途中で捨てられたときに要求を記録する。
pub struct LoggedBody {
    inner: Body,
    pending: Option<Pending>,
}

struct Pending {
    logger: Arc<Logger>,
    request: Request,
    started: Instant,
    bytes: u64,
}

impl LoggedBody {
    pub fn wrap(body: Body, logger: Arc<Logger>, request: Request, started: Instant) -> Body {
        Body::new(Self {
            inner: body,
            pending: Some(Pending {
                logger,
                request,
                started,
                bytes: 0,
            }),
        })
    }

    fn finish(&mut self, error: Option<String>) {
        let Some(pending) = self.pending.take() else {
            return;
        };
        let mut request = pending.request;
        request.response_bytes = Some(pending.bytes);
        if request.error.is_none() {
            request.error = error;
        }
        pending.logger.log_request(request, pending.started);
    }
}

impl StreamingBody for LoggedBody {
    type Data = Bytes;
    type Error = BoxError;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Bytes>, BoxError>>> {
        let result = Pin::new(&mut self.inner).poll_frame(cx);
        match &result {
            Poll::Ready(Some(Ok(frame))) => {
                if let (Some(data), Some(pending)) = (frame.data_ref(), self.pending.as_mut()) {
                    pending.bytes += data.len() as u64;
                }
            }
            Poll::Ready(Some(Err(error))) => {
                let error = format!("response body: {error}");
                self.finish(Some(error));
            }
            Poll::Ready(None) => self.finish(None),
            Poll::Pending => {}
        }
        result
    }

    fn is_end_stream(&self) -> bool {
        self.inner.is_end_stream()
    }

    fn size_hint(&self) -> SizeHint {
        self.inner.size_hint()
    }
}

impl Drop for LoggedBody {
    fn drop(&mut self) {
        let incomplete = self.pending.is_some() && !self.inner.is_end_stream();
        self.finish(incomplete.then(|| "the response body was not fully sent".to_string()));
    }
}

/// クエリのキーだけを取り出す。値にトークンが入っていることがあるので記録しない。
pub fn query_keys(query: Option<&str>) -> Vec<String> {
    query
        .unwrap_or_default()
        .split('&')
        .filter(|pair| !pair.is_empty())
        .map(|pair| {
            pair.split_once('=')
                .map_or(pair, |(key, _)| key)
                .to_string()
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use rama::http::body::util::BodyExt as _;

    #[test]
    fn query_keys_drop_values() {
        assert_eq!(query_keys(Some("t=secret&a&b=1")), ["t", "a", "b"]);
        assert!(query_keys(None).is_empty());
    }

    fn logger() -> (tempfile::TempDir, Arc<Logger>) {
        let dir = tempfile::tempdir().unwrap();
        let logger = Logger::open(Some(&dir.path().join("requests.jsonl"))).unwrap();
        (dir, Arc::new(logger))
    }

    fn logged(dir: &tempfile::TempDir) -> Vec<Request> {
        chloe_audit::tail(&dir.path().join("requests.jsonl"), 10)
            .unwrap()
            .into_iter()
            .map(|record| match record.event {
                Event::Request(request) => *request,
                Event::Passthrough { .. } => panic!("expected a request"),
            })
            .collect()
    }

    #[tokio::test]
    async fn logs_the_response_size_after_the_body_is_sent() {
        let (dir, logger) = logger();
        let body = LoggedBody::wrap(
            Body::from("hello"),
            logger,
            Request {
                host: "a.example".into(),
                ..Default::default()
            },
            Instant::now(),
        );
        assert!(logged(&dir).is_empty());
        let bytes = body.collect().await.unwrap().to_bytes();
        assert_eq!(bytes, "hello");
        let requests = logged(&dir);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].response_bytes, Some(5));
        assert!(requests[0].duration_ms.is_some());
        assert_eq!(requests[0].error, None);
    }

    #[tokio::test]
    async fn logs_a_dropped_body_as_incomplete() {
        let (dir, logger) = logger();
        let body = Body::from_stream(rama::stream::pending::<Result<Bytes, BoxError>>());
        drop(LoggedBody::wrap(
            body,
            logger,
            Request::default(),
            Instant::now(),
        ));
        let requests = logged(&dir);
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].response_bytes, Some(0));
        assert!(requests[0].error.is_some());
    }
}
