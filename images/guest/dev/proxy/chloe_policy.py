"""段階 2 の検証用に、chloe の通信方針を mitmproxy のアドオンとして書いたもの。

段階 4 で rama に置き換える。方針は次のとおり。

- 中継のみの宛先は、SNI を見て復号せずに中継する。
- 上流への接続先は SNI (平文の HTTP なら Host) から決め、ゲストが接続しようとした IP アドレスは使わない。
- 許可リストにない宛先、SNI と Host が食い違う要求は遮断する。
- プレースホルダーは、規則に登録した宛先と場所に限って本物に差し替える。
  それ以外の宛先でプレースホルダーを見つけたら遮断する。
- ca.chloe.internal への要求には CA 証明書を返す。
- 要求ごとにメタデータを JSON Lines で記録する。本文とヘッダーの値は記録しない。
"""

import base64
import json
import os
import time
from dataclasses import dataclass
from pathlib import Path
from urllib.parse import parse_qsl, urlsplit

from mitmproxy import ctx, http, tls
from mitmproxy.addonmanager import Loader
from mitmproxy.proxy import server_hooks

CA_HOST = "ca.chloe.internal"


def host_matches(host: str, patterns: list[str]) -> bool:
    host = host.lower().rstrip(".")
    for pattern in patterns:
        pattern = pattern.lower()
        if pattern.startswith("*."):
            if host.endswith(pattern[1:]):
                return True
        elif host == pattern:
            return True
    return False


@dataclass
class InjectRule:
    name: str
    placeholder: str
    hosts: list[str]
    header: str
    format: str
    secret: str

    def basic_user(self, value: str) -> str | None:
        """Basic 認証のパスワードがプレースホルダーなら、そのユーザー名を返す。"""
        scheme, _, encoded = value.partition(" ")
        if scheme.lower() != "basic":
            return None
        try:
            user, _, password = base64.b64decode(encoded).decode().partition(":")
        except ValueError:
            return None
        return user if password == self.placeholder else None

    def contains(self, value: str) -> bool:
        if self.placeholder in value:
            return True
        return self.format == "basic-password" and self.basic_user(value) is not None

    def render(self, value: str) -> str | None:
        """ヘッダーの値の中のプレースホルダーを本物に差し替えた値を返す。"""
        if self.format == "basic-password":
            user = self.basic_user(value)
            if user is None:
                return None
            token = base64.b64encode(f"{user}:{self.secret}".encode()).decode()
            return f"Basic {token}"
        if self.placeholder not in value:
            return None
        return value.replace(self.placeholder, self.secret)


class ChloePolicy:
    def __init__(self) -> None:
        self.allow: list[str] = []
        self.passthrough: list[str] = []
        self.rules: list[InjectRule] = []
        self.log_file = None

    def load(self, loader: Loader) -> None:
        loader.add_option("chloe_policy", str, "", "path to the chloe policy JSON")
        loader.add_option("chloe_log", str, "", "path to the JSON Lines request log")

    def configure(self, updates: set[str]) -> None:
        if "chloe_policy" in updates and ctx.options.chloe_policy:
            policy = json.loads(Path(ctx.options.chloe_policy).read_text())
            self.passthrough = policy.get("passthrough", [])
            self.allow = policy.get("allow", []) + self.passthrough
            self.rules = [
                InjectRule(
                    name=r["name"],
                    placeholder=r["placeholder"],
                    hosts=r["hosts"],
                    header=r["header"].lower(),
                    format=r.get("format", "replace"),
                    # 検証用なので、本物の値はランナーの環境変数から読む。
                    secret=os.environ[r["secret_env"]],
                )
                for r in policy.get("inject", [])
            ]
        if "chloe_log" in updates and ctx.options.chloe_log:
            self.log_file = open(ctx.options.chloe_log, "a", buffering=1)

    def log(self, **event) -> None:
        if self.log_file:
            event["ts"] = time.time()
            self.log_file.write(json.dumps(event) + "\n")

    # --- TLS: 復号するかどうかを SNI で決める

    def tls_clienthello(self, data: tls.ClientHelloData) -> None:
        sni = data.client_hello.sni
        if sni and host_matches(sni, self.passthrough):
            data.ignore_connection = True
            self.log(event="passthrough", host=sni)

    # --- 上流の接続先を SNI から決める

    def server_connect(self, data: server_hooks.ServerConnectionHookData) -> None:
        sni = data.server.sni
        if sni:
            data.server.address = (sni, data.server.address[1])

    # --- HTTP

    def block(self, flow: http.HTTPFlow, reason: str, **extra) -> None:
        flow.response = http.Response.make(
            403,
            json.dumps({"error": "blocked by chloe", "reason": reason}),
            {"content-type": "application/json"},
        )
        flow.metadata["chloe_decision"] = f"blocked:{reason}"
        flow.metadata.update({f"chloe_{k}": v for k, v in extra.items()})

    def request(self, flow: http.HTTPFlow) -> None:
        req = flow.request
        host = (req.host_header or req.host).split(":")[0].lower()
        sni = flow.client_conn.sni

        if host == CA_HOST:
            ca = Path(ctx.options.confdir).expanduser() / "mitmproxy-ca-cert.pem"
            flow.response = http.Response.make(
                200, ca.read_bytes(), {"content-type": "application/x-pem-file"}
            )
            flow.metadata["chloe_decision"] = "ca"
            return

        if flow.client_conn.tls_established:
            if not sni:
                return self.block(flow, "no-sni")
            if sni.lower() != host:
                return self.block(flow, "sni-host-mismatch", sni=sni)
        else:
            # 平文の HTTP は Host から接続先を決める。
            req.host = host

        if not host_matches(host, self.allow):
            return self.block(flow, "not-allowed")

        applied = []
        for rule in self.rules:
            bound = host_matches(host, rule.hosts)
            for name, value in list(req.headers.items(multi=True)):
                if not rule.contains(value):
                    continue
                if not bound:
                    return self.block(flow, "placeholder-to-unbound-host", rule=rule.name)
                if name.lower() == rule.header:
                    rendered = rule.render(value)
                    if rendered is not None:
                        req.headers[name] = rendered
                        applied.append(rule.name)
            in_url = rule.placeholder in req.url
            in_body = rule.placeholder.encode() in (req.raw_content or b"")
            if (in_url or in_body) and not bound:
                return self.block(flow, "placeholder-to-unbound-host", rule=rule.name)

        flow.metadata["chloe_decision"] = "allowed"
        flow.metadata["chloe_injected"] = applied

    def response(self, flow: http.HTTPFlow) -> None:
        self.log_flow(flow)

    def error(self, flow: http.HTTPFlow) -> None:
        self.log_flow(flow)

    def log_flow(self, flow: http.HTTPFlow) -> None:
        req = flow.request
        url = urlsplit(req.path)
        self.log(
            event="request",
            host=(req.host_header or req.host).split(":")[0],
            sni=flow.client_conn.sni,
            method=req.method,
            path=url.path,
            query_keys=[k for k, _ in parse_qsl(url.query, keep_blank_values=True)],
            http_version=req.http_version,
            status=flow.response.status_code if flow.response else None,
            request_bytes=len(req.raw_content or b""),
            response_bytes=len(flow.response.raw_content or b"") if flow.response else None,
            error=str(flow.error) if flow.error else None,
            decision=flow.metadata.get("chloe_decision"),
            injected=flow.metadata.get("chloe_injected", []),
            **{
                k.removeprefix("chloe_"): v
                for k, v in flow.metadata.items()
                if k in ("chloe_rule", "chloe_sni")
            },
        )


addons = [ChloePolicy()]
