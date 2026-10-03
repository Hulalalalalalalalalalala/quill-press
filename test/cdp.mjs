// Minimal Chrome DevTools Protocol client built only on Node 24 primitives
// (global WebSocket/fetch, child_process). The product itself ships with no
// dependencies, and the regression suite keeps that property: it drives the
// real page in the real system Chrome instead of a mocked DOM.
import { spawn } from 'node:child_process';
import { existsSync, mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { setTimeout as sleep } from 'node:timers/promises';

let nextId = 1;

class CdpSession {
  constructor(connection, sessionId) {
    this.conn = connection;
    this.sessionId = sessionId;
  }

  send(method, params = {}) {
    return this.conn.send(method, params, this.sessionId);
  }
}

class CdpConnection {
  constructor(ws) {
    this.ws = ws;
    this.pending = new Map();
    this.eventListeners = [];
    ws.addEventListener('message', (event) => {
      const msg = JSON.parse(event.data);
      if (msg.id && this.pending.has(msg.id)) {
        const { resolve, reject } = this.pending.get(msg.id);
        this.pending.delete(msg.id);
        if (msg.error) reject(new Error(`${msg.error.message || 'CDP error'} (${msg.error.code})`));
        else resolve(msg.result);
        return;
      }
      if (msg.method) {
        for (const listener of this.eventListeners) listener(msg);
      }
    });
  }

  onEvent(listener) {
    this.eventListeners.push(listener);
  }

  send(method, params = {}, sessionId) {
    const id = nextId++;
    const message = { id, method, params };
    if (sessionId) message.sessionId = sessionId;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject });
      this.ws.send(JSON.stringify(message));
    });
  }

  close() {
    try { this.ws.close(); } catch { /* already closed */ }
  }
}

export class Browser {
  constructor(chrome, userDir, connection) {
    this.chrome = chrome;
    this.userDir = userDir;
    this.conn = connection;
    this.pages = [];
  }

  static async launch() {
    const userDir = mkdtempSync(join(tmpdir(), 'quillpress-chrome-'));
    const chromeBin = process.env.CHROME_BIN || 'google-chrome';
    const chrome = spawn(chromeBin, [
      '--headless=new',
      '--no-sandbox',
      '--disable-gpu',
      '--disable-dev-shm-usage',
      '--no-first-run',
      '--no-default-browser-check',
      '--disable-extensions',
      '--remote-debugging-port=0',
      `--user-data-dir=${userDir}`,
      'about:blank',
    ], { stdio: ['ignore', 'ignore', 'pipe'], detached: true });

    let launchError = '';
    chrome.stderr.on('data', (chunk) => { launchError += chunk.toString(); });

    // Port 0 makes Chrome write the chosen port to DevToolsActivePort.
    const portFile = join(userDir, 'DevToolsActivePort');
    const deadline = Date.now() + 15000;
    let port = 0;
    for (;;) {
      try {
        const { readFileSync } = await import('node:fs');
        const text = readFileSync(portFile, 'utf8');
        port = Number(text.split('\n')[0]);
        if (port > 0) break;
      } catch { /* file not written yet */ }
      if (Date.now() > deadline) {
        throw new Error(`Chrome did not open its debugging port.\n${launchError.slice(-2000)}`);
      }
      await sleep(50);
    }

    const version = await (await fetch(`http://127.0.0.1:${port}/json/version`)).json();
    const ws = new WebSocket(version.webSocketDebuggerUrl);
    await new Promise((resolve, reject) => {
      ws.addEventListener('open', resolve, { once: true });
      ws.addEventListener('error', () => reject(new Error('CDP websocket failed')), { once: true });
    });
    const connection = new CdpConnection(ws);
    return new Browser(chrome, userDir, connection);
  }

  async newPage() {
    const { targetId } = await this.conn.send('Target.createTarget', { url: 'about:blank' });
    const { sessionId } = await this.conn.send('Target.attachToTarget', { targetId, flatten: true });
    const session = new CdpSession(this.conn, sessionId);
    const page = new Page(session);
    page.targetId = targetId;
    this.pages.push(page);
    return page;
  }

  async closePage(page) {
    try {
      await this.conn.send('Target.closeTarget', { targetId: page.targetId });
    } catch { /* target already gone */ }
    const index = this.pages.indexOf(page);
    if (index >= 0) this.pages.splice(index, 1);
  }

  async close() {
    for (const page of this.pages) page.dispose();
    try {
      await this.conn.send('Browser.close');
    } catch { /* shutting down regardless */ }
    this.conn.close();
    // detached => Chrome leads its own process group; signal the whole group
    // so crashpad/zygote children cannot linger holding the profile open.
    const exit = new Promise((resolve) => this.chrome.on('exit', resolve));
    await Promise.race([exit, sleep(3000)]);
    if (this.chrome.exitCode === null) {
      try { process.kill(-this.chrome.pid, 'SIGKILL'); } catch { /* group already gone */ }
      await Promise.race([exit, sleep(2000)]);
    }
    removeDirWithRetry(this.userDir);
  }
}

// Chrome's background children can still be unlinking profile files for a
// beat after exit; retry briefly rather than leaving the temp dir behind.
function removeDirWithRetry(dir) {
  for (let attempt = 0; attempt < 10; attempt++) {
    try {
      rmSync(dir, { recursive: true, force: true });
      if (!existsSync(dir)) return;
    } catch { /* race with crashpad shutdown */ }
    Atomics.wait(new Int32Array(new SharedArrayBuffer(4)), 0, 0, 100);
  }
}

export class Page {
  constructor(session) {
    this.session = session;
    this.consoleMessages = [];
    this.pageErrors = [];
    this.dialogs = [];
    session.conn.onEvent((msg) => {
      if (msg.sessionId !== session.sessionId) return;
      if (msg.method === 'Runtime.exceptionThrown') {
        const details = msg.params.exceptionDetails;
        this.pageErrors.push(details.exception ? details.exception.description : details.text);
      } else if (msg.method === 'Runtime.consoleAPICalled') {
        const text = (msg.params.args || [])
          .map((arg) => arg.value !== undefined ? String(arg.value) : (arg.description || ''))
          .join(' ');
        this.consoleMessages.push({ type: msg.params.type, text });
      }
    });
  }

  async enable() {
    if (this.enabled) return;
    this.enabled = true;
    await this.session.send('Page.enable');
    await this.session.send('Runtime.enable');
    // No scenario in this suite expects a native dialog (all dirty-check
    // guards are avoided by construction). Record and accept them anyway so
    // an unexpected dialog cannot deadlock the page; assertNoPageErrors then
    // reports that a guard fired.
    this.session.conn.onEvent((msg) => {
      if (msg.sessionId !== this.session.sessionId) return;
      if (msg.method === 'Page.javascriptDialogOpening') {
        this.dialogs.push(msg.params.message || '');
        this.session.send('Page.handleJavaScriptDialog', { accept: true }).catch(() => {});
      }
    });
  }

  async goto(url) {
    await this.enable();
    const loaded = this.waitForEvent('Page.loadEventFired');
    await this.session.send('Page.navigate', { url });
    await loaded;
  }

  async addInitScript(source) {
    await this.session.send('Page.addScriptToEvaluateOnNewDocument', { source });
  }

  async reload() {
    const loaded = this.waitForEvent('Page.loadEventFired');
    await this.session.send('Page.reload');
    await loaded;
  }

  waitForEvent(method) {
    return new Promise((resolve) => {
      const listener = (msg) => {
        if (msg.sessionId !== this.session.sessionId || msg.method !== method) return;
        this.session.conn.eventListeners.splice(
          this.session.conn.eventListeners.indexOf(listener), 1);
        resolve(msg.params);
      };
      this.session.conn.onEvent(listener);
    });
  }

  // Evaluates an expression and returns its JSON-serializable result.
  async eval(expression) {
    const { result, exceptionDetails } = await this.session.send('Runtime.evaluate', {
      expression,
      returnByValue: true,
      awaitPromise: true,
    });
    if (exceptionDetails) {
      const description = exceptionDetails.exception
        ? exceptionDetails.exception.description
        : `${exceptionDetails.text} ${exceptionDetails.lineNumber || ''}`;
      throw new Error(`page evaluation failed: ${description}`);
    }
    return result.value;
  }

  // fnSource is the source of a predicate function, e.g. "() => document.readyState === 'complete'".
  // It is re-evaluated until it returns a truthy value or the timeout elapses.
  async waitFor(fnSource, { timeout = 5000, interval = 20, label } = {}) {
    const deadline = Date.now() + timeout;
    let last;
    for (;;) {
      try {
        last = await this.eval(`(${fnSource})()`);
      } catch (error) {
        last = `eval error: ${error.message}`;
      }
      if (last === true) return last;
      if (Date.now() > deadline) {
        throw new Error(`waitFor timed out after ${timeout}ms (${label || fnSource.slice(0, 160)}); last=${JSON.stringify(last)}`);
      }
      await sleep(interval);
    }
  }

  dispose() {
    // Session lifetime is bound to the target; nothing extra to tear down.
  }
}
