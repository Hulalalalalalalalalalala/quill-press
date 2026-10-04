// Minimal HTTP/1.1 client on top of a raw TCP socket, used by the request-size
// regression tests. global fetch cannot express what these cases need:
//   - exact wire byte sizes (including Content-Length set precisely),
//   - hand-framed Transfer-Encoding: chunked requests with control over each
//     frame boundary (and a delay between frames),
//   - verifying the connection stays usable after a rejected oversized body.
// Zero dependencies, like the rest of the suite.
import net from 'node:net';
import { setTimeout as sleep } from 'node:timers/promises';

export class RawHttp {
  static async connect(baseUrl) {
    const client = new RawHttp(baseUrl);
    await client.open();
    return client;
  }

  constructor(baseUrl) {
    this.url = new URL(baseUrl);
    this.sock = null;
    this.buf = Buffer.alloc(0);
    this.queue = [];
    this.waiters = [];
    this.ended = false;
    this.fatal = null;
    // Requests on one connection are serialized: write request, read its full
    // response, then the next request may use the same socket.
    this.chain = Promise.resolve();
  }

  open() {
    return new Promise((resolve, reject) => {
      const sock = net.connect(Number(this.url.port) || 80, this.url.hostname);
      sock.once('connect', () => {
        sock.on('data', (chunk) => {
          if (this.waiters.length) this.waiters.shift().resolve(chunk);
          else this.queue.push(chunk);
        });
        sock.on('end', () => {
          this.ended = true;
          while (this.waiters.length) this.waiters.shift().resolve(null);
        });
        sock.on('close', () => {
          this.ended = true;
          while (this.waiters.length) this.waiters.shift().resolve(null);
        });
        sock.on('error', (err) => {
          this.fatal = err;
          while (this.waiters.length) this.waiters.shift().reject(err);
        });
        this.sock = sock;
        resolve();
      });
      sock.once('error', reject);
    });
  }

  nextChunk() {
    if (this.queue.length) return Promise.resolve(this.queue.shift());
    if (this.fatal) return Promise.reject(this.fatal);
    if (this.ended) return Promise.resolve(null);
    return new Promise((resolve, reject) => this.waiters.push({ resolve, reject }));
  }

  async fill() {
    const chunk = await this.nextChunk();
    if (chunk === null) throw new Error('connection closed before a complete response was received');
    this.buf = Buffer.concat([this.buf, chunk]);
  }

  async readResponse() {
    let sep;
    while ((sep = this.buf.indexOf('\r\n\r\n')) === -1) {
      await this.fill();
    }
    const head = this.buf.slice(0, sep).toString('latin1');
    const lines = head.split('\r\n');
    const status = Number(lines[0].split(' ')[1]);
    const headers = {};
    for (const line of lines.slice(1)) {
      const colon = line.indexOf(':');
      headers[line.slice(0, colon).trim().toLowerCase()] = line.slice(colon + 1).trim();
    }
    let pos = sep + 4;
    let body;
    const te = (headers['transfer-encoding'] || '').toLowerCase();
    if (te.includes('chunked')) {
      const parts = [];
      for (;;) {
        let lineEnd;
        while ((lineEnd = this.buf.indexOf('\r\n', pos)) === -1) {
          await this.fill();
        }
        const sizeLine = this.buf.slice(pos, lineEnd).toString('latin1');
        const size = parseInt(sizeLine.split(';')[0].trim(), 16);
        pos = lineEnd + 2;
        while (this.buf.length < pos + size + 2) {
          await this.fill();
        }
        parts.push(Buffer.from(this.buf.subarray(pos, pos + size)));
        pos += size + 2; // chunk data plus its trailing CRLF
        if (size === 0) break;
      }
      // Skip trailer fields (if any) up to the terminating empty line.
      for (;;) {
        let lineEnd;
        while ((lineEnd = this.buf.indexOf('\r\n', pos)) === -1) {
          await this.fill();
        }
        const line = this.buf.slice(pos, lineEnd).toString('latin1');
        pos = lineEnd + 2;
        if (line === '') break;
      }
      body = Buffer.concat(parts);
    } else if (headers['content-length'] !== undefined) {
      const length = Number(headers['content-length']);
      while (this.buf.length < pos + length) {
        await this.fill();
      }
      body = Buffer.from(this.buf.subarray(pos, pos + length));
      pos += length;
    } else {
      while (!this.ended) {
        await this.fill();
      }
      body = Buffer.from(this.buf.subarray(pos));
      pos = this.buf.length;
    }
    // Keep any pipelined leftover (e.g. the next response) for the next read.
    this.buf = this.buf.subarray(pos);
    return { status, headers, body };
  }

  async writeAll(data) {
    if (!this.sock || this.sock.destroyed) throw new Error('socket is not open');
    if (!this.sock.write(data)) {
      await new Promise((resolve) => this.sock.once('drain', resolve));
    }
  }

  run(task) {
    const result = this.chain.then(task);
    this.chain = result.then(() => undefined, () => undefined);
    return result;
  }

  headerBlock(extra) {
    const merged = { 'content-type': 'application/json', ...(extra || {}) };
    return Object.entries(merged)
      .map(([name, value]) => `${name}: ${value}`)
      .join('\r\n');
  }

  // Bodyless GET on the same connection (used to prove a rejected request
  // left a usable, fully-framed keep-alive socket behind).
  get(path) {
    return this.run(async () => {
      const head = Buffer.from(
        `GET ${path} HTTP/1.1\r\nHost: ${this.url.host}\r\nconnection: keep-alive\r\n\r\n`,
        'latin1');
      await this.writeAll(head);
      return this.readResponse();
    });
  }

  // One request declaring its total size with Content-Length. `body` is the
  // exact byte sequence put on the wire.
  send(method, path, body, options = {}) {
    return this.run(async () => {
      const head = Buffer.from(
        `${method} ${path} HTTP/1.1\r\nHost: ${this.url.host}\r\n`
        + this.headerBlock(options.headers)
        + `\r\ncontent-length: ${body.length}\r\nconnection: keep-alive\r\n\r\n`,
        'latin1');
      await this.writeAll(head);
      await this.writeAll(body);
      return this.readResponse();
    });
  }

  // One request using Transfer-Encoding: chunked. Each entry of `frames`
  // becomes one chunk, so frame boundaries are fully controlled. With
  // delayMs > 0 the earlier frames sit on the wire (still within the limit)
  // before later frames push the cumulative size over it.
  sendChunked(method, path, frames, options = {}) {
    return this.run(async () => {
      const delayMs = options.delayMs || 0;
      const head = Buffer.from(
        `${method} ${path} HTTP/1.1\r\nHost: ${this.url.host}\r\n`
        + this.headerBlock(options.headers)
        + '\r\ntransfer-encoding: chunked\r\nconnection: keep-alive\r\n\r\n',
        'latin1');
      await this.writeAll(head);
      for (let i = 0; i < frames.length; i++) {
        if (delayMs && i > 0) await sleep(delayMs);
        const frame = frames[i];
        await this.writeAll(Buffer.from(`${frame.length.toString(16)}\r\n`, 'ascii'));
        await this.writeAll(frame);
        await this.writeAll(Buffer.from('\r\n', 'ascii'));
      }
      await this.writeAll(Buffer.from('0\r\n\r\n', 'ascii'));
      return this.readResponse();
    });
  }

  async close() {
    try {
      await this.chain;
    } catch { /* closing regardless */ }
    if (this.sock && !this.sock.destroyed) this.sock.end();
  }
}

// Split a buffer into frames at the given absolute byte offsets.
export function splitFrames(buffer, cuts) {
  const frames = [];
  let start = 0;
  for (const cut of cuts) {
    frames.push(buffer.subarray(start, cut));
    start = cut;
  }
  frames.push(buffer.subarray(start));
  return frames;
}
