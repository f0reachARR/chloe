//! 標準入出力の MCP のサーバー (公式の Rust SDK の `rmcp` で書く)。
//!
//! ツールは三つ。
//!
//! - `request_network_access`：宛先への通信の許可を頼む。
//! - `ask_user`：人に質問する。
//! - `wait_for_answer`：答えを待つ依頼を待ち直す。
//!
//! 依頼のツールは、決めた時間 (`--wait`) だけ答えを待ち、それまでに答えがなければ ID と
//! `pending` を返す。クライアントのツールの待ち時間に当たらないようにするためである。
//! 待つあいだは、クライアントが進み具合を受け取ると言えば、1 分ごとに知らせる
//! (Claude Code は、知らせのない時間が長いと呼び出しを打ち切る)。
//! クライアントが呼び出しを取り消したら、待つのをやめて依頼も取り下げる。

use std::time::Duration;

use anyhow::Result;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, Implementation, ProgressNotificationParam, ProgressToken,
    ServerCapabilities, ServerConfig,
};
use rmcp::service::{Peer, RequestContext, RoleServer};
use rmcp::{ServerHandler, ServiceExt, tool, tool_handler, tool_router};
use serde::Deserialize;

use crate::client::{Ask, Client};
use crate::describe;

const PROGRESS_INTERVAL: Duration = Duration::from_secs(60);

const INSTRUCTIONS: &str = "This VM is a chloe workspace. Outbound HTTP(S) is limited to an allowlist, and a person may not be watching the terminal. \
A host outside the allowlist does not resolve (\"Could not resolve host\"), and a request to it is refused with 403 \"blocked by chloe\". When you need such a host, call request_network_access. \
When you need a decision or information from a person, call ask_user instead of stopping. \
Both wait for the answer; if they return pending, call wait_for_answer with the id to keep waiting.";

#[derive(Deserialize, schemars::JsonSchema)]
struct NetworkParams {
    /// Host name, like api.example.com or *.example.com.
    host: String,
    /// HTTP methods to allow, like GET. Empty for all.
    #[serde(default)]
    methods: Vec<String>,
    /// Paths to allow, like /v1/*. Empty for all.
    #[serde(default)]
    paths: Vec<String>,
    /// Why you need it, for the person deciding.
    reason: String,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct QuestionParams {
    question: String,
    /// Up to 10 choices. The person may still write another answer.
    #[serde(default)]
    options: Vec<String>,
    /// Context for the person answering.
    #[serde(default)]
    reason: Option<String>,
}

#[derive(Deserialize, schemars::JsonSchema)]
struct WaitParams {
    /// The id of the ask that returned pending.
    id: i64,
}

#[derive(Clone)]
struct Server {
    client: Client,
    wait: Duration,
}

pub async fn serve(client: Client, wait: Duration) -> Result<()> {
    let service = Server { client, wait }
        .serve(rmcp::transport::stdio())
        .await?;
    service.waiting().await?;
    Ok(())
}

#[tool_router]
impl Server {
    #[tool(
        description = "Ask a person to allow outbound HTTP(S) to a host that chloe blocks. Waits for the answer. When allowed, the host is reachable right away (retry the request). Methods and paths narrow what is allowed; omit them for everything on the host."
    )]
    async fn request_network_access(
        &self,
        Parameters(params): Parameters<NetworkParams>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let new = serde_json::json!({
            "kind": "network",
            "host": params.host,
            "methods": params.methods,
            "paths": params.paths,
            "reason": params.reason,
        });
        self.ask(Some(new), None, context).await
    }

    #[tool(
        description = "Ask a person a question and wait for the answer. Use it when you need a decision or information and nobody may be at the terminal. With options, the person picks one or writes another answer."
    )]
    async fn ask_user(
        &self,
        Parameters(params): Parameters<QuestionParams>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let new = serde_json::json!({
            "kind": "question",
            "question": params.question,
            "options": params.options,
            "reason": params.reason,
        });
        self.ask(Some(new), None, context).await
    }

    #[tool(description = "Keep waiting for the answer to an ask that returned pending.")]
    async fn wait_for_answer(
        &self,
        Parameters(params): Parameters<WaitParams>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        self.ask(None, Some(params.id), context).await
    }
}

#[tool_handler]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("chloe", env!("CARGO_PKG_VERSION")))
            .with_instructions(INSTRUCTIONS)
    }
}

impl Server {
    /// 依頼を出すか (`new`)、出した依頼 (`id`) について、答えを待つ。
    async fn ask(
        &self,
        new: Option<serde_json::Value>,
        id: Option<i64>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let ask = match (new, id) {
            (Some(new), _) => self.client.create(&new).await,
            (None, Some(id)) => self.client.get(id).await,
            (None, None) => unreachable!("either a new ask or an id"),
        };
        let ask = match ask {
            Ok(ask) => ask,
            Err(error) => return result_error(format!("chloe: {error:#}")),
        };
        let progress = context.meta.get_progress_token();
        let waited = tokio::select! {
            waited = self.wait_with_progress(&ask, &context.peer, progress) => waited,
            () = context.ct.cancelled() => {
                // 取り消されたら依頼を取り下げる。応答はクライアントが読まない。
                let _ = self.client.cancel(ask.id).await;
                return result_error(format!("withdrew ask {}", ask.id));
            }
        };
        match waited {
            Ok(ask) => {
                let (text, _) = describe(&ask);
                if matches!(ask.status.as_str(), "allowed" | "answered" | "pending") {
                    CallToolResult::success(vec![ContentBlock::text(text)])
                } else {
                    result_error(text)
                }
            }
            Err(error) => result_error(format!("chloe: {error:#}")),
        }
    }

    async fn wait_with_progress(
        &self,
        ask: &Ask,
        peer: &Peer<RoleServer>,
        progress: Option<ProgressToken>,
    ) -> Result<Ask> {
        if !ask.pending() {
            return Ok(ask.clone());
        }
        let waiting = self.client.wait(ask.id, Some(self.wait));
        tokio::pin!(waiting);
        let Some(token) = progress else {
            return waiting.await;
        };
        let mut ticks = tokio::time::interval(PROGRESS_INTERVAL);
        ticks.tick().await;
        let mut count = 0.0;
        loop {
            tokio::select! {
                ask = &mut waiting => return ask,
                _ = ticks.tick() => {
                    count += 1.0;
                    let param = ProgressNotificationParam::new(token.clone(), count)
                        .with_message(format!("waiting for a person to answer ask {}", ask.id));
                    let _ = peer.notify_progress(param).await;
                }
            }
        }
    }
}

fn result_error(text: String) -> CallToolResult {
    CallToolResult::error(vec![ContentBlock::text(text)])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::client::tests::fake_server;
    use serde_json::{Value, json};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    /// MCP のクライアントの代わりに、一行ずつ送って受ける。
    struct Peer {
        write: tokio::io::WriteHalf<tokio::io::DuplexStream>,
        read: tokio::io::Lines<BufReader<tokio::io::ReadHalf<tokio::io::DuplexStream>>>,
    }

    impl Peer {
        async fn send(&mut self, message: Value) {
            let line = format!("{message}\n");
            self.write.write_all(line.as_bytes()).await.unwrap();
        }

        /// `id` への応答を受ける (知らせは飛ばす)。
        async fn reply(&mut self, id: i64) -> Value {
            loop {
                let line = self.read.next_line().await.unwrap().unwrap();
                let message: Value = serde_json::from_str(&line).unwrap();
                if message["id"] == id {
                    return message;
                }
            }
        }
    }

    async fn start(states: Vec<&'static str>) -> Peer {
        let (client, _) = fake_server(states).await;
        let (ours, theirs) = tokio::io::duplex(64 * 1024);
        let server = Server {
            client,
            wait: Duration::from_millis(100),
        };
        tokio::spawn(async move {
            let service = server.serve(theirs).await.unwrap();
            let _ = service.waiting().await;
        });
        let (read, write) = tokio::io::split(ours);
        let mut peer = Peer {
            write,
            read: BufReader::new(read).lines(),
        };
        peer.send(json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": { "name": "test", "version": "0" },
            },
        }))
        .await;
        let init = peer.reply(1).await;
        assert_eq!(init["result"]["protocolVersion"], "2025-06-18", "{init}");
        assert_eq!(init["result"]["serverInfo"]["name"], "chloe");
        assert!(
            init["result"]["instructions"]
                .as_str()
                .unwrap()
                .contains("request_network_access")
        );
        peer.send(json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await;
        peer
    }

    async fn call(peer: &mut Peer, id: i64, name: &str, arguments: Value) -> Value {
        peer.send(json!({
            "jsonrpc": "2.0", "id": id, "method": "tools/call",
            "params": { "name": name, "arguments": arguments },
        }))
        .await;
        peer.reply(id).await["result"].clone()
    }

    fn text(result: &Value) -> &str {
        result["content"][0]["text"].as_str().unwrap()
    }

    #[tokio::test]
    async fn lists_tools_and_waits_for_answers() {
        let mut peer = start(vec!["pending", "pending", "allowed"]).await;
        peer.send(json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}))
            .await;
        let list = peer.reply(2).await;
        let names: Vec<&str> = list["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            ["ask_user", "request_network_access", "wait_for_answer"]
        );

        let result = call(
            &mut peer,
            3,
            "request_network_access",
            json!({"host": "example.com", "reason": "docs"}),
        )
        .await;
        assert_eq!(result["isError"], false, "{result}");
        assert!(text(&result).starts_with("allowed"), "{result}");
    }

    #[tokio::test]
    async fn returns_pending_after_the_wait() {
        let mut peer = start(vec![]).await;
        let result = call(&mut peer, 2, "ask_user", json!({"question": "ok?"})).await;
        assert!(
            text(&result).contains("wait_for_answer with id 5"),
            "{result}"
        );
        let result = call(&mut peer, 3, "wait_for_answer", json!({"id": 5})).await;
        assert!(text(&result).starts_with("pending"), "{result}");
        // 引数が足りなければ、ツールを動かさずに断る。
        peer.send(json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": { "name": "ask_user", "arguments": {} },
        }))
        .await;
        let refused = peer.reply(4).await;
        assert!(
            refused.get("error").is_some() || refused["result"]["isError"] == true,
            "{refused}"
        );
    }
}
