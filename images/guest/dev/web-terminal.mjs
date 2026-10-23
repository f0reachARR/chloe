// Web の端末 (/api/v1/workspace-terminal) をブラウザの代わりに開き、コマンドを打って出力を出す。
//
//   node web-terminal.mjs <ゲートウェイの URL> <ワークスペース> <cookie> [Origin] <<< 'echo hi'
//
// 標準入力を打ち、続けて exit を打つ。出力のあとに `exit=<コード>` を出す。開けなければ
// `refused` か `error=<理由>` を出す。Origin を空にすると付けない。Node の WebSocket (undici) は
// ブラウザと違ってヘッダーを付けられるので、cookie と Origin を自分で渡す。
import { readFileSync } from 'node:fs';

const [base, workspace, cookie, origin = base] = process.argv.slice(2);
const input = readFileSync(0, 'utf8');
const url = new URL(`/api/v1/workspace-terminal/${encodeURIComponent(workspace)}`, base);
url.protocol = url.protocol === 'https:' ? 'wss:' : 'ws:';
url.search = 'cols=100&rows=30';

const headers = { cookie };
if (origin) {
  headers.origin = origin;
}
const socket = new WebSocket(url, { headers });
socket.binaryType = 'arraybuffer';
const decoder = new TextDecoder();
let output = '';
let result = 'refused';
const timer = setTimeout(() => {
  result = 'timeout';
  socket.close();
}, 120_000);

socket.addEventListener('message', (event) => {
  if (typeof event.data !== 'string') {
    output += decoder.decode(event.data, { stream: true });
    return;
  }
  const frame = JSON.parse(event.data);
  if (frame.type === 'started') {
    socket.send(new TextEncoder().encode(`${input.trimEnd()}\rexit\r`));
  } else if (frame.type === 'exit') {
    result = `exit=${frame.code}`;
  } else if (frame.type === 'error') {
    result = `error=${frame.message}`;
  }
});
socket.addEventListener('close', () => {
  clearTimeout(timer);
  process.stdout.write(`${output}\n${result}\n`);
});
