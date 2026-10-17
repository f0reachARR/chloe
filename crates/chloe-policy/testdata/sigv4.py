"""SigV4 の署名し直しを確かめるデータを botocore で作る。

VM が送る要求 (プレースホルダーの鍵で署名したもの) と、本物の鍵で botocore が署名した
Authorization ヘッダーを sigv4.json に書き出す。

    uv run --with botocore python crates/chloe-policy/testdata/sigv4.py
"""

import datetime
import json
import pathlib

import botocore.auth
from botocore.auth import S3SigV4Auth, SigV4Auth
from botocore.awsrequest import AWSRequest
from botocore.credentials import Credentials

botocore.auth.get_current_datetime = lambda: datetime.datetime(2015, 8, 30, 12, 36, 0)

PLACEHOLDER = Credentials("AKIAchloePLACEHOLDER", "chloe-secret-placeholder", "chloe-token-placeholder")
REAL = Credentials("AKIDEXAMPLE", "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY", "real-session-token")

CASES = [
    {
        "name": "get-vanilla",
        "service": "service",
        "method": "GET",
        "url": "https://example.amazonaws.com/",
        "headers": [],
        "body": "",
    },
    {
        "name": "iam-query",
        "service": "iam",
        "method": "GET",
        "url": "https://iam.amazonaws.com/?Version=2010-05-08&Action=ListUsers",
        "headers": [["Content-Type", "application/x-www-form-urlencoded; charset=utf-8"]],
        "body": "",
    },
    {
        "name": "double-encoded-path",
        "service": "lambda",
        "method": "POST",
        "url": "https://lambda.us-east-1.amazonaws.com/2015-03-31/functions/my%20fn/./x/../invocations?Qualifier=a%2Fb&empty",
        "headers": [["Content-Type", "application/json"], ["X-Custom", "  a   b  "]],
        "body": '{"hello": "world"}',
    },
    {
        "name": "s3-session-token",
        "service": "s3",
        "method": "PUT",
        "url": "https://bucket.s3.us-east-1.amazonaws.com/a%20b/c%2Bd.txt?versionId=x&acl",
        "headers": [["Content-Type", "text/plain"]],
        "body": "hello",
        "token": True,
    },
    {
        "name": "http2-no-host-header",
        "service": "sts",
        "method": "POST",
        "url": "https://sts.amazonaws.com:8443/",
        "headers": [["Content-Type", "application/x-www-form-urlencoded"]],
        "body": "Action=GetCallerIdentity&Version=2011-06-15",
        "drop_host": True,
    },
]


def sign(case, credentials):
    if not case.get("token"):
        credentials = Credentials(credentials.access_key, credentials.secret_key)
    request = AWSRequest(
        method=case["method"], url=case["url"], headers=dict(case["headers"]), data=case["body"]
    )
    request.headers["Host"] = case["url"].split("/")[2]
    auth_class = S3SigV4Auth if case["service"] == "s3" else SigV4Auth
    auth_class(credentials, case["service"], "us-east-1").add_auth(request)
    return request


out = []
for case in CASES:
    vm = sign(case, PLACEHOLDER)
    real = sign(case, REAL)
    headers = [[k.lower(), v] for k, v in vm.headers.items()]
    if case.get("drop_host"):
        headers = [h for h in headers if h[0] != "host"]
    url = case["url"]
    out.append(
        {
            "name": case["name"],
            "method": case["method"],
            "authority": url.split("/")[2],
            "path_and_query": "/" + url.split("/", 3)[3],
            "headers": headers,
            "body": case["body"],
            "expected_authorization": real.headers["Authorization"],
            "expected_security_token": real.headers.get("X-Amz-Security-Token"),
        }
    )

path = pathlib.Path(__file__).with_name("sigv4.json")
placeholder = {
    "access_key_id": PLACEHOLDER.access_key,
    "secret_access_key": PLACEHOLDER.secret_key,
    "session_token": PLACEHOLDER.token,
}
real = {
    "access_key_id": REAL.access_key,
    "secret_access_key": REAL.secret_key,
    "session_token": REAL.token,
}
path.write_text(
    json.dumps({"placeholder": placeholder, "real": real, "cases": out}, indent=2) + "\n"
)
