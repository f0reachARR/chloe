// 公開したポートの WebSocket を、プロキシのホストを通して開き、一つ送って返ってきたものを出す。
//
//   node proxy-ws.mjs <ゲートウェイのアドレス:ポート> <プロキシのホスト> <トークン> <送る文字列>
//
// プロキシのホストの名前を引かずに、ゲートウェイのアドレスへつないで `Host` で指す (curl の
// --resolve と同じ)。ホストの *.localhost は ::1 に向くことがあり、確かめるゲートウェイは
// 127.0.0.1 で待ち受けるので。返ってきたテキストを出す。切り替えられなければ応答の最初の行を出す。
import { randomBytes } from 'node:crypto';
import { connect } from 'node:net';

const [address, host, token, text] = process.argv.slice(2);
const [ip, port] = address.split(':');

const socket = connect(Number(port), ip);
socket.setTimeout(60_000, () => {
  console.log('timeout');
  socket.destroy();
});
socket.on('connect', () => {
  socket.write(
    `GET /ws HTTP/1.1\r\nHost: ${host}\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n` +
      `Sec-WebSocket-Key: ${randomBytes(16).toString('base64')}\r\nSec-WebSocket-Version: 13\r\n` +
      `Authorization: Bearer ${token}\r\n\r\n`,
  );
});

let buffer = Buffer.alloc(0);
let upgraded = false;
socket.on('data', (chunk) => {
  buffer = Buffer.concat([buffer, chunk]);
  if (!upgraded) {
    const end = buffer.indexOf('\r\n\r\n');
    if (end < 0) return;
    const status = buffer.subarray(0, buffer.indexOf('\r\n')).toString();
    if (!status.includes(' 101 ')) {
      console.log(status);
      socket.destroy();
      return;
    }
    upgraded = true;
    buffer = buffer.subarray(end + 4);
    // クライアントからのフレームはマスクする。
    const payload = Buffer.from(text);
    const mask = randomBytes(4);
    const masked = payload.map((b, i) => b ^ mask[i % 4]);
    socket.write(Buffer.concat([Buffer.from([0x81, 0x80 | payload.length]), mask, masked]));
  }
  if (buffer.length >= 2 && buffer.length >= 2 + (buffer[1] & 0x7f)) {
    console.log(buffer.subarray(2, 2 + (buffer[1] & 0x7f)).toString());
    socket.destroy();
  }
});
socket.on('error', (error) => console.log(`error=${error.message}`));
