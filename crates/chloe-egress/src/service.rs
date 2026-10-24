//! 復号した HTTP の要求 (と平文の HTTP の要求) を方針に従って処理する。

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Instant;

use chloe_audit::Request as LogRequest;
use chloe_policy::{Decision, Request as PolicyRequest};
use rama::Service;
use rama::error::{BoxError, ErrorContext as _};
use rama::extensions::{Extension, ExtensionsRef as _};
use rama::http::body::StreamingBody as _;
use rama::http::body::util::BodyExt as _;
use rama::http::header::{CONTENT_LENGTH, CONTENT_TYPE, HOST, UPGRADE};
use rama::http::{Body, HeaderName, HeaderValue, Request, Response, StatusCode, Version};
use rama::net::address::Domain;
use rama::net::http::TargetHttpVersion;

use crate::log::{LoggedBody, Logger, query_keys};

/// これより大きい本文はプレースホルダーを探さずに送る。
pub const BODY_SCAN_LIMIT: u64 = 1 << 20;
/// エージェントからの依頼の本文の上限。
const ASK_BODY_LIMIT: usize = 64 * 1024;

/// TLS を復号した接続に付ける、クライアントが送った SNI。
#[derive(Debug, Clone, Extension)]
pub struct IngressSni(pub Domain);

pub struct EgressService<C> {
    pub policy: crate::control::SharedPolicy,
    pub client: C,
    pub logger: Arc<Logger>,
    pub ca_pem: Arc<str>,
    pub secrets: crate::secrets::SecretSource,
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
        let started = Instant::now();
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
        let policy = self.policy.current();
        let (decision, plan) = policy.evaluate(&PolicyRequest {
            host: &host,
            sni: sni.as_deref(),
            tls: sni.is_some(),
            method: &method,
            path_and_query: &path_and_query,
            headers: &headers,
            body: scanned_body.as_deref(),
        });

        // 記録の共通の欄。判定の結果と応答は、記録するときに埋める。
        let record = |status: Option<u16>, decision: &str, injected: &[String]| LogRequest {
            host: chloe_policy::normalize_host(&host),
            sni: sni.clone(),
            method: method.clone(),
            path: path.clone(),
            query_keys: query_keys(query.as_deref()),
            http_version: http_version.clone(),
            status,
            request_bytes: content_length,
            body_scanned: scanned,
            decision: decision.to_string(),
            injected: injected.to_vec(),
            ..Default::default()
        };

        let injected = match decision {
            Decision::Ca => {
                self.logger
                    .log_request(record(Some(200), "ca", &[]), started);
                return Ok(ca_response(&self.ca_pem));
            }
            Decision::Ask => {
                let resp = self
                    .ask(&method, &path, scanned_body.as_deref().unwrap_or_default())
                    .await;
                self.logger
                    .log_request(record(Some(resp.status().as_u16()), "ask", &[]), started);
                return Ok(resp);
            }
            Decision::Blocked { reason, rule } => {
                let decision = format!("blocked:{}", reason.as_str());
                let record = LogRequest {
                    rule,
                    ..record(Some(403), &decision, &[])
                };
                self.logger.log_request(record, started);
                return Ok(blocked_response(reason.as_str(), &host));
            }
            Decision::Allowed { injected } => injected,
        };

        // 本物の値はここで取り出し、ヘッダーを書き換えたらすぐ捨てる。
        if !plan.is_empty() {
            let names = policy.secrets_needed(&plan);
            let applied = match self.secrets.fetch(&names).await {
                Ok(values) => policy
                    .apply(&mut headers, &plan, |name| values.get(name).cloned())
                    .map_err(|name| anyhow::anyhow!("no value for {name}")),
                Err(error) => Err(error),
            };
            if let Err(error) = applied {
                tracing::warn!("failed to get a secret: {error:#}");
                let record = LogRequest {
                    error: Some(format!("{error:#}")),
                    ..record(Some(503), "blocked:secret-unavailable", &injected)
                };
                self.logger.log_request(record, started);
                return Ok(error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "the secret for this request is not available",
                ));
            }
        }

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
        // Upgrade (WebSocket) は HTTP/1.1 でしか送れないので、上流との ALPN を HTTP/1.1 に固定する。
        if parts.headers.contains_key(UPGRADE) {
            parts.extensions.insert(TargetHttpVersion(Version::HTTP_11));
        }
        let req = Request::from_parts(parts, body);

        match self.client.serve(req).await {
            Ok(resp) => {
                // 本文を流し終えてから、大きさとかかった時間を入れて記録する。
                let record = record(Some(resp.status().as_u16()), "allowed", &injected);
                let (parts, body) = resp.into_parts();
                let body = LoggedBody::wrap(body, self.logger.clone(), record, started);
                Ok(Response::from_parts(parts, body))
            }
            Err(error) => {
                let error: BoxError = error.into();
                let record = LogRequest {
                    error: Some(error.to_string()),
                    ..record(None, "allowed", &injected)
                };
                self.logger.log_request(record, started);
                Ok(error_response(
                    StatusCode::BAD_GATEWAY,
                    "upstream request failed",
                ))
            }
        }
    }
}

impl<C> EgressService<C> {
    /// エージェントからの依頼と、公開するポート (`ask.chloe.internal`) を問い合わせ用ソケットへ送る。
    ///
    /// ```text
    /// POST   /v1/asks          依頼を出す (本文は依頼の JSON)
    /// GET    /v1/asks/{id}     依頼の状態を見る
    /// DELETE /v1/asks/{id}     依頼を取り下げる
    /// POST   /v1/ports         ポートを公開する ({"port": 5173, "scheme": "http" | "https"})
    /// GET    /v1/ports         公開したポートの一覧
    /// DELETE /v1/ports/{port}  公開を取り消す
    /// ```
    async fn ask(&self, method: &str, path: &str, body: &[u8]) -> Response {
        let object = || {
            if body.len() > ASK_BODY_LIMIT {
                return Err((StatusCode::PAYLOAD_TOO_LARGE, "the body is too large"));
            }
            match serde_json::from_slice::<serde_json::Value>(body) {
                Ok(value) if value.is_object() => Ok(value),
                _ => Err((StatusCode::BAD_REQUEST, "the body must be a JSON object")),
            }
        };
        let id = path
            .strip_prefix("/v1/asks/")
            .and_then(|id| id.parse::<i64>().ok());
        let port = path
            .strip_prefix("/v1/ports/")
            .and_then(|port| port.parse::<u16>().ok());
        let question = match (method, path, id, port) {
            ("POST", "/v1/asks", _, _) => match object() {
                Ok(ask) => serde_json::json!({ "ask": { "create": ask } }),
                Err((status, message)) => return error_response(status, message),
            },
            ("GET", _, Some(id), _) => serde_json::json!({ "ask": { "get": id } }),
            ("DELETE", _, Some(id), _) => serde_json::json!({ "ask": { "cancel": id } }),
            ("POST", "/v1/ports", _, _) => match object() {
                Ok(port) => serde_json::json!({ "port": { "publish": port } }),
                Err((status, message)) => return error_response(status, message),
            },
            ("GET", "/v1/ports", _, _) => serde_json::json!({ "port": "list" }),
            ("DELETE", _, _, Some(port)) => serde_json::json!({ "port": { "unpublish": port } }),
            _ => return error_response(StatusCode::NOT_FOUND, "no such endpoint"),
        };
        let answer = match self.secrets.relay(&question).await {
            Ok(line) => serde_json::from_str::<serde_json::Value>(&line).unwrap_or_default(),
            Err(error) => {
                tracing::warn!("failed to send a question to the control plane: {error:#}");
                return error_response(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "the control plane is not reachable",
                );
            }
        };
        for key in ["ask", "port", "ports"] {
            if let Some(value) = answer.get(key) {
                return json_response(StatusCode::OK, value);
            }
        }
        match answer.get("unpublished").and_then(|v| v.as_bool()) {
            Some(true) => return json_response(StatusCode::OK, &answer),
            Some(false) => {
                return error_response(StatusCode::NOT_FOUND, "the port is not published");
            }
            None => {}
        }
        let status = match answer.get("code").and_then(|c| c.as_str()) {
            Some("invalid") => StatusCode::BAD_REQUEST,
            Some("not_found") => StatusCode::NOT_FOUND,
            _ => StatusCode::BAD_GATEWAY,
        };
        let message = answer
            .get("error")
            .and_then(|e| e.as_str())
            .unwrap_or("invalid answer from the control plane");
        json_response(status, &serde_json::json!({ "error": message }))
    }
}

/// 要求先のホスト名。ポートがあれば含める (署名し直すときに `host` ヘッダーの代わりに使う)。
fn request_host(req: &Request) -> Option<String> {
    req.uri()
        .authority()
        .map(|a| a.to_string())
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

/// 遮断したときの応答。宛先に届かないときは、人に許可を頼めることを書いておく。エージェントが
/// 依頼のツールを知らなくても、遮断された応答から辿れるようにするためである。
fn blocked_response(reason: &str, host: &str) -> Response {
    let mut body = serde_json::json!({ "error": "blocked by chloe", "reason": reason });
    if reason == "not-allowed" || reason == "path-denied" {
        let host = chloe_policy::normalize_host(host);
        body["hint"] = format!(
            "ask a person to allow it: chloe-ask network {host} --reason '...' (or the request_network_access tool of the chloe MCP server)"
        )
        .into();
    }
    json_response(StatusCode::FORBIDDEN, &body)
}

fn error_response(status: StatusCode, message: &str) -> Response {
    json_response(status, &serde_json::json!({ "error": message }))
}

fn json_response(status: StatusCode, body: &serde_json::Value) -> Response {
    let mut resp = Response::new(Body::from(body.to_string()));
    *resp.status_mut() = status;
    resp.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    resp
}
