import http from 'node:http';
import { deflateSync } from 'node:zlib';

function crc32(bytes) { let checksum = 0xffffffff; for (const byte of bytes) { checksum ^= byte; for (let bit = 0; bit < 8; bit += 1) checksum = (checksum >>> 1) ^ (0xedb88320 & -(checksum & 1)); } return (checksum ^ 0xffffffff) >>> 0; }
function chunk(type, data) { const label = Buffer.from(type); const output = Buffer.alloc(data.length + 12); output.writeUInt32BE(data.length, 0); label.copy(output, 4); data.copy(output, 8); output.writeUInt32BE(crc32(Buffer.concat([label, data])), data.length + 8); return output; }
function testImage() {
  const width = 1024; const height = 1024; const raw = Buffer.alloc(height * (width * 3 + 1));
  for (let row = 0; row < height; row += 1) for (let column = 0; column < width; column += 1) { const offset = row * (width * 3 + 1) + column * 3 + 1; const ridge = Math.sin(column / 190) * 75 + 580; const foreground = row > ridge; raw[offset] = foreground ? 64 + Math.floor(row / 20) : 205 + Math.floor(row / 40); raw[offset + 1] = foreground ? 105 + Math.floor(row / 25) : 221 + Math.floor(row / 55); raw[offset + 2] = foreground ? 91 + Math.floor(column / 50) : 229; }
  const header = Buffer.alloc(13); header.writeUInt32BE(width); header.writeUInt32BE(height, 4); header[8] = 8; header[9] = 2;
  return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk('IHDR', header), chunk('IDAT', deflateSync(raw)), chunk('IEND', Buffer.alloc(0))]);
}
const image = testImage();
const metrics = { submissions: 0, edits: 0, downloads: 0, downloadAuthorizationSeen: false, paths: [], models: {} };
function sendJson(response, status, value) { response.writeHead(status, { 'Content-Type': 'application/json', 'x-request-id': `mock-${metrics.submissions}` }); response.end(JSON.stringify(value)); }
const server = http.createServer(async (request, response) => {
  if (request.url === '/metrics') return sendJson(response, 200, metrics);
  if (request.url === '/fixture.png' || request.url === '/result.png') { metrics.downloads += 1; if (request.headers.authorization) metrics.downloadAuthorizationSeen = true; response.writeHead(200, { 'Content-Type': 'image/png', 'Content-Length': image.length }); response.end(image); return; }
  if (request.method !== 'POST') return sendJson(response, 404, { error: 'not found' });
  const chunks = []; let length = 0;
  for await (const part of request) { length += part.length; if (length > 30 * 1024 * 1024) { response.writeHead(413); response.end(); return; } chunks.push(part); }
  const body = Buffer.concat(chunks); const multipart = request.headers['content-type']?.startsWith('multipart/form-data');
  let parsed;
  try { parsed = multipart ? { model: body.toString('latin1').match(/name="model"\r\n\r\n([^\r]+)/)?.[1] || 'mock-ok' } : JSON.parse(body.toString()); } catch { return sendJson(response, 400, { error: 'invalid JSON' }); }
  const model = parsed.model || 'mock-ok'; metrics.submissions += 1; metrics.paths.push(request.url); metrics.models[model] = (metrics.models[model] || 0) + 1;
  if (request.url.includes('/images/edits')) { if (!multipart || !body.includes(Buffer.from([137, 80, 78, 71]))) return sendJson(response, 422, { error: 'real image missing' }); metrics.edits += 1; }
  if (model === 'mock-http-404') return sendJson(response, 404, { error: '<script>upstream not found</script>' });
  if (model === 'mock-http-500') return sendJson(response, 500, { error: 'upstream failed' });
  if (model === 'mock-drop') { request.socket.destroy(); return; }
  if (model === 'mock-read-body') { response.writeHead(200, { 'Content-Type': 'application/json', 'Content-Length': 99999 }); response.write('{"data":['); setTimeout(() => response.destroy(), 100); return; }
  if (model === 'mock-slow') await new Promise(resolve => setTimeout(resolve, 8000));
  if (model === 'mock-binary') { response.writeHead(200, { 'Content-Type': 'image/png' }); response.end(image); return; }
  const result = model === 'mock-url' ? { url: 'http://127.0.0.1:19091/result.png' } : model === 'mock-download-404' ? { url: 'http://127.0.0.1:19091/missing.png' } : model === 'mock-conflict' ? { url: 'http://127.0.0.1:19091/result.png', b64_json: image.toString('base64') } : { b64_json: image.toString('base64') };
  const pictures = model === 'mock-empty' ? [] : model === 'mock-multi' ? [result, result] : [result];
  sendJson(response, 200, request.url.startsWith('/adapter/') ? { images: pictures } : { data: pictures });
});
server.listen(19091, '127.0.0.1', () => console.log('NewPhoto mock HTTP service at 127.0.0.1:19091 — NOT a real upstream validation'));
