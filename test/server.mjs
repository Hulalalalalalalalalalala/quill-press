// Launches the real server (node server.ts serve) on an ephemeral port with a
// throwaway data directory, exactly as documented in README. No test-only
// branches exist in the product; tests only talk HTTP/UI to it.
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';
import { setTimeout as sleep } from 'node:timers/promises';

const here = dirname(fileURLToPath(import.meta.url));
export const SERVER_TS = process.env.QUILL_SERVER_TS || join(here, '..', 'server.ts');

export class TestServer {
  constructor(process_, url, dataDir) {
    this.process = process_;
    this.url = url;
    this.dataDir = dataDir;
  }

  static async start() {
    const dataDir = mkdtempSync(join(tmpdir(), 'quillpress-data-'));
    const child = spawn(process.execPath, [
      SERVER_TS, 'serve',
      '--host', '127.0.0.1',
      '--port', '0',
      '--data-dir', dataDir,
    ], { stdio: ['ignore', 'pipe', 'pipe'] });

    let output = '';
    const url = await new Promise((resolve, reject) => {
      const onData = (chunk) => {
        output += chunk.toString();
        const match = /listening on (http:\/\/[^\s]+)/.exec(output);
        if (match) {
          child.stdout.off('data', onData);
          child.stderr.off('data', onData);
          resolve(match[1].replace(/\/$/, ''));
        }
      };
      child.stdout.on('data', onData);
      child.stderr.on('data', onData);
      child.on('error', reject);
      child.on('exit', (code) => {
        reject(new Error(`server exited early (${code}):\n${output}`));
      });
    });

    // Wait for the port to actually accept connections.
    const deadline = Date.now() + 10000;
    for (;;) {
      try {
        const res = await fetch(`${url}/health`);
        if (res.ok) break;
      } catch { /* not ready yet */ }
      if (Date.now() > deadline) throw new Error('server did not become healthy');
      await sleep(25);
    }
    return new TestServer(child, url, dataDir);
  }

  // Raw API access for verifying the persisted state behind the UI.
  api(path, options) {
    return fetch(`${this.url}${path}`, {
      ...options,
      headers: { 'content-type': 'application/json', accept: 'application/json', ...(options && options.headers) },
    });
  }

  async listArticles() {
    const res = await this.api('/api/articles');
    if (!res.ok) throw new Error(`GET /api/articles -> ${res.status}`);
    return (await res.json()).articles;
  }

  async stop() {
    if (this.process.exitCode === null) {
      this.process.kill('SIGTERM');
      await Promise.race([
        new Promise((resolve) => this.process.on('exit', resolve)),
        sleep(3000),
      ]);
    }
    if (this.process.exitCode === null) this.process.kill('SIGKILL');
    rmSync(this.dataDir, { recursive: true, force: true });
  }
}
