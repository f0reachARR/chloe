//! 復号した HTTP の要求 (と平文の HTTP の要求) を方針に従って処理する。

use std::convert::Infallible;
use std::sync::Arc;

use chloe_policy::{Decision, Policy, Request as PolicyRequest};
use rama::Service;
use rama::error::{BoxError, ErrorContext as _};
use rama::extensions::{Extension, ExtensionsRef as _};
use rama::http::body::StreamingBody as _;
use rama::http::body::util::BodyExt as _;
use rama::http::header::{CONTENT_LENGTH, CONTENT_TYPE, HOST};
use rama::http::{Body, HeaderName, HeaderValue, Request, Response, StatusCode};
use rama::net::address::Domain;

use crate::log::{Event, Logger, query_keys};

/// これより大きい本文はプレースホルダーを探さずに送る。
pub const BODY_SCAN_LIMIT: u64 = 1 << 20;

/// TLS を復号した接続に付ける、クライアントが送った SNI。
#[derive(Debug, Clone, Extension)]
pub struct IngressSni(pub Domain);

pub struct EgressService<C> {
    pub policy: Arc<Policy>,
    pub client: C,
    pub logger: Arc<Logger>,
    pub ca_pem: Arc<str>,
}

impl<C> Service<Request> for EgressService<C>
where
    C: Service<Request, Output = Response, Error: Into<BoxError>>,
{
    type Output = Response;
    type Error = Infallible;

    async fn serve(&self, req: Request) -> Result<Self::Output, Self::Error> {
        match self.handle(req).await {
            Ok(resp) => Ok(resp),
            Err(error) => {
                tracing::warn!("failed to handle a request: {error}");
                Ok(error_response(StatusCode::BAD_REQUEST, "invalid request"))
            }
        }
    }
}

impl<C> EgressService<C>
where
    C: Service<Request, Output = Response, Error: Into<BoxError>>,
{
    async fn handle(&self, req: Request) -> Result<Response, BoxError> {
        let sni = req
            .extensions()
            .get_ref::<IngressSni>()
            .map(|s| s.0.to_string());
        let host = request_host(&req).unwrap_or_default();
        let method = req.method().to_string();
        let path = req
            .uri()
            .path()
            .map(|p| p.to_string())
            .unwrap_or_else(|| "/".into());
        let query = req.uri().query().map(|q| q.to_string());
        let http_version = format!("{:?}", req.version());
        let (mut parts, body) = req.into_parts();

        // 本文は大きさが分かっていて上限以下のときだけ読み込んで調べる。
        let content_length = parts
            .headers
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        // 大きすぎるか大きさの分からない本文は、調べずにそのまま流す。
        let (scanned_body, passthrough_body) = match content_length {
            Some(len) if len <= BODY_SCAN_LIMIT => {
                let bytes = body
                    .collect()
                    .await
                    .context("read request body")?
                    .to_bytes();
                (Some(bytes), None)
            }
            _ if body.is_end_stream() => (Some(Default::default()), None),
            _ => (None, Some(body)),
        };
        let scanned = scanned_body.is_some();

        let mut headers: Vec<(String, Vec<u8>)> = parts
            .headers
            .iter()
            .map(|(name, value)| (name.as_str().to_string(), value.as_bytes().to_vec()))
            .collect();
        let path_and_query = match &query {
            Some(query) => format!("{path}?{query}"),
            None => path.clone(),
        };
        let decision = self.policy.evaluate(&mut PolicyRequest {
            host: &host,
            sni: sni.as_deref(),
            tls: sni.is_some(),
            path_and_query: &path_and_query,
            headers: &mut headers,
            body: scanned_body.as_deref(),
        });

        let log = |status: Option<u16>,
                   decision: &str,
                   injected: &[String],
                   rule: Option<&str>,
                   error: Option<String>| {
            self.logger.log(Event::Request {
                host: chloe_policy::normalize_host(&host).as_str(),
                sni: sni.as_deref(),
                method: &method,
                path: &path,
                query_keys: query_keys(query.as_deref()),
                http_version: &http_version,
                status,
                request_bytes: content_length,
                body_scanned: scanned,
                error,
                decision,
                injected,
                rule,
            });
        };

        let injected = match decision {
            Decision::Ca => {
                log(Some(200), "ca", &[], None, None);
                return Ok(ca_response(&self.ca_pem));
            }
            Decision::Blocked { reason, rule } => {
                let decision = format!("blocked:{}", reason.as_str());
                log(Some(403), &decision, &[], rule.as_deref(), None);
                return Ok(blocked_response(reason.as_str()));
            }
            Decision::Allowed { injected } => injected,
        };

        // 差し替えたヘッダーを書き戻す。名前の順は元のまま保つ。
        parts.headers.clear();
        for (name, value) in headers {
            parts.headers.append(
                HeaderName::from_bytes(name.as_bytes()).context("header name")?,
                HeaderValue::from_bytes(&value).context("header value")?,
            );
        }
        let body = match (scanned_body, passthrough_body) {
            (Some(bytes), _) => Body::from(bytes),
            (None, Some(body)) => body,
            (None, None) => Body::empty(),
        };
        let req = Request::from_parts(parts, body);

        match self.client.serve(req).await {
            Ok(resp) => {
                log(
                    Some(resp.status().as_u16()),
                    "allowed",
                    &injected,
                    None,
                    None,
                );
                Ok(resp)
            }
            Err(error) => {
                let error: BoxError = error.into();
                log(None, "allowed", &injected, None, Some(error.to_string()));
                Ok(error_response(
                    StatusCode::BAD_GATEWAY,
                    "upstream request failed",
                ))
            }
        }
    }
}

fn request_host(req: &Request) -> Option<String> {
    req.uri()
        .host()
        .map(|h| h.to_string())
        .or_else(|| req.headers().get(HOST)?.to_str().ok().map(str::to_string))
}

fn ca_response(pem: &str) -> Response {
    let mut resp = Response::new(Body::from(pem.to_string()));
    resp.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("application/x-pem-file"),
    );
    resp
}

fn blocked_response(reason: &str) -> Response {
    let body = serde_json::json!({ "error": "blocked by chloe", "reason": reason });
    let mut resp = Response::new(Body::from(body.to_string()));
    *resp.status_mut() = StatusCode::FORBIDDEN;
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    resp
}

fn error_response(status: StatusCode, message: &str) -> Response {
    let mut resp = blocked_response(message);
    *resp.status_mut() = status;
    resp
}
