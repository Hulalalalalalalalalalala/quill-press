import { createServer } from 'node:http';
import type { IncomingMessage, ServerResponse } from 'node:http';
import { mkdirSync, readFileSync, renameSync, rmSync, writeFileSync } from 'node:fs';
import { randomUUID } from 'node:crypto';
import { join } from 'node:path';

const PRODUCT: string = 'QuillPress';
const RESOURCE: string = 'articles';
const MAX_BODY_BYTES: number = 5 * 1024 * 1024;
const PAGE: string = `<!doctype html>
<html lang="zh-CN">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>QuillPress · 内容编辑与刊物发布</title>
<style>
body{font-family:system-ui,sans-serif;max-width:52rem;margin:3rem auto;padding:0 1rem;line-height:1.7;color:#1f2328}
a{color:#175b9c}
h2{margin-top:2rem}
form{border:1px solid #d0d7de;border-radius:8px;padding:1rem 1.25rem;margin:1rem 0 1.5rem}
label{display:block;font-weight:600;margin:.9rem 0 .3rem}
input[type=text],textarea{width:100%;box-sizing:border-box;font:inherit;padding:.5rem .6rem;border:1px solid #d0d7de;border-radius:6px;background:#fff;color:inherit}
textarea{resize:vertical}
button{margin-top:1.1rem;font:inherit;padding:.5rem 1.3rem;border-radius:6px;border:1px solid #175b9c;background:#175b9c;color:#fff;cursor:pointer}
button:disabled{opacity:.55;cursor:default}
.status{margin:.8rem 0 0;min-height:1.4em}
.status.ok{color:#1a7f37}.status.err{color:#c0392b}
ul.articles{list-style:none;padding:0;margin:0}
article.draft{border:1px solid #d0d7de;border-radius:8px;padding:1rem 1.25rem;margin:1rem 0}
.draft h3{margin:0 0 .4rem;word-break:break-word}
.draft .meta{color:#656d76;font-size:.85rem;margin:0 0 .5rem}
.draft .summary{white-space:pre-wrap;word-break:break-word;margin:0;color:#3a4149}
.empty{color:#656d76}
.empty.err{color:#c0392b}
</style>
<main>
<h1>QuillPress</h1>
<p>内容编辑与刊物发布</p>

<section aria-labelledby="compose-heading">
<h2 id="compose-heading">保存草稿</h2>
<form id="draft-form" autocomplete="off">
  <label for="title">标题</label>
  <input id="title" name="title" type="text">
  <label for="summary">摘要（可选）</label>
  <textarea id="summary" name="summary" rows="3"></textarea>
  <label for="body">正文</label>
  <textarea id="body" name="body" rows="10"></textarea>
  <button id="save-btn" type="submit">保存草稿</button>
  <p id="status" class="status" role="status" aria-live="polite"></p>
</form>
</section>

<section aria-labelledby="list-heading">
<h2 id="list-heading">草稿列表</h2>
<ul id="article-list" class="articles"></ul>
<p id="empty-tip" class="empty">草稿加载中…</p>
</section>

<p><a href="/api/articles">查看文章列表接口</a> · <a href="/health">服务状态</a></p>
</main>
<script>
(function () {
  'use strict';
  var form = document.getElementById('draft-form');
  var titleInput = document.getElementById('title');
  var summaryInput = document.getElementById('summary');
  var bodyInput = document.getElementById('body');
  var saveBtn = document.getElementById('save-btn');
  var statusEl = document.getElementById('status');
  var listEl = document.getElementById('article-list');
  var emptyTip = document.getElementById('empty-tip');
  var articles = [];
  var saving = false;

  function setStatus(text, kind) {
    statusEl.textContent = text || '';
    statusEl.className = 'status' + (kind ? ' ' + kind : '');
  }
  function setEmpty(text, kind) {
    emptyTip.textContent = text || '';
    emptyTip.className = 'empty' + (kind ? ' ' + kind : '');
    emptyTip.hidden = !text;
  }
  function formatTime(iso) {
    var d = new Date(iso);
    return isNaN(d.getTime()) ? String(iso) : d.toLocaleString();
  }
  function render() {
    var sorted = articles.slice().sort(function (a, b) {
      var ta = a && a.createdAt ? String(a.createdAt) : '';
      var tb = b && b.createdAt ? String(b.createdAt) : '';
      if (ta !== tb) return ta < tb ? 1 : -1;
      return 0;
    });
    listEl.textContent = '';
    sorted.forEach(function (article) {
      var card = document.createElement('article');
      card.className = 'draft';
      var h = document.createElement('h3');
      h.textContent = article.title == null ? '' : String(article.title);
      var meta = document.createElement('p');
      meta.className = 'meta';
      meta.textContent = '创建时间：' + formatTime(article.createdAt);
      var summary = document.createElement('p');
      summary.className = 'summary';
      summary.textContent = article.summary == null ? '' : String(article.summary);
      card.appendChild(h);
      card.appendChild(meta);
      card.appendChild(summary);
      var li = document.createElement('li');
      li.appendChild(card);
      listEl.appendChild(li);
    });
    setEmpty(sorted.length === 0 ? '还没有保存的草稿。' : '');
  }
  function load() {
    fetch('/api/articles', { headers: { accept: 'application/json' } })
      .then(function (r) {
        if (!r.ok) throw new Error('HTTP ' + r.status);
        return r.json();
      })
      .then(function (data) {
        articles = data && Array.isArray(data.articles) ? data.articles : [];
        render();
      })
      .catch(function (err) {
        articles = [];
        listEl.textContent = '';
        setEmpty('草稿列表加载失败：' + (err && err.message ? err.message : err), 'err');
      });
  }

  function setSaving(on) {
    saving = on;
    saveBtn.disabled = on;
    saveBtn.textContent = on ? '保存中…' : '保存草稿';
  }

  form.addEventListener('submit', function (event) {
    event.preventDefault();
    if (saving) return;
    var trimmedTitle = titleInput.value.trim();
    if (!trimmedTitle) {
      setStatus('标题不能为空。', 'err');
      titleInput.focus();
      return;
    }
    setSaving(true);
    setStatus('正在保存…', null);
    fetch('/api/articles', {
      method: 'POST',
      headers: { 'content-type': 'application/json', accept: 'application/json' },
      body: JSON.stringify({
        title: titleInput.value,
        summary: summaryInput.value,
        body: bodyInput.value
      })
    })
      .then(function (r) {
        return r.json().catch(function () { return null; }).then(function (data) {
          if (!r.ok || !data || !data.article) {
            throw new Error(data && data.error ? data.error : ('HTTP ' + r.status));
          }
          return data.article;
        });
      })
      .then(function (article) {
        articles.push(article);
        render();
        form.reset();
        setStatus('草稿已保存', 'ok');
        setSaving(false);
      })
      .catch(function (err) {
        setStatus('保存失败：' + (err && err.message ? err.message : err) + '，可修改后再次保存。', 'err');
        setSaving(false);
      });
  });

  load();
})();
</script>
</html>`;
const args: string[] = process.argv.slice(2);
const help: string = `QuillPress - 内容编辑与刊物发布
Usage: node server.ts serve [--host ADDRESS] [--port PORT] [--data-dir DIRECTORY]
       node server.ts --help
Defaults: --host 127.0.0.1 --port 8080 --data-dir data
Port 0 selects an available port.
`;
if (args.length === 0 || args.includes('--help') || args.includes('-h')) {
  process.stdout.write(help);
  process.exit(args.length === 0 ? 2 : 0);
}
if (args.shift() !== 'serve') { console.error('Expected serve or --help'); process.exit(2); }
let host: string = '127.0.0.1';
let port: number = 8080;
let dataDir: string = 'data';
while (args.length) {
  const flag = args.shift();
  const value = args.shift();
  if (!value || !['--host', '--port', '--data-dir'].includes(flag ?? '')) {
    console.error('Each option must be --host, --port or --data-dir followed by a value'); process.exit(2);
  }
  if (flag === '--host') host = value;
  else if (flag === '--port') {
    if (!/^\d+$/.test(value) || Number(value) > 65535) { console.error('Port must be between 0 and 65535'); process.exit(2); }
    port = Number(value);
  } else dataDir = value;
}
mkdirSync(dataDir, { recursive: true });
const dataFile = join(dataDir, 'articles.json');
try { writeFileSync(dataFile, '[]\n', { flag: 'wx' }); } catch (error) {
  if (!(error instanceof Error && 'code' in error && error.code === 'EEXIST')) throw error;
}

interface Article {
  id: string;
  title: string;
  summary: string;
  body: string;
  status: 'draft';
  createdAt: string;
}
interface DraftInput {
  title: string;
  summary: string;
  body: string;
}

class HttpError extends Error {
  status: number;
  constructor(status: number, message: string) {
    super(message);
    this.name = 'HttpError';
    this.status = status;
  }
}

function respond(res: ServerResponse, status: number, value: unknown, html = false): void {
  const body = html ? String(value) : JSON.stringify(value);
  res.writeHead(status, {
    'content-type': html ? 'text/html; charset=utf-8' : 'application/json; charset=utf-8',
    'content-length': Buffer.byteLength(body),
  });
  res.end(body);
}

function methodNotAllowed(res: ServerResponse, allow: string): void {
  const body = JSON.stringify({ error: 'method not allowed' });
  res.writeHead(405, {
    'content-type': 'application/json; charset=utf-8',
    'content-length': Buffer.byteLength(body),
    allow,
  });
  res.end(body);
}

function readArticles(): unknown[] {
  let parsed: unknown;
  try {
    parsed = JSON.parse(readFileSync(dataFile, 'utf8'));
  } catch (error) {
    throw new Error(`unable to read articles: ${(error as Error).message}`);
  }
  if (!Array.isArray(parsed)) throw new Error('unable to read articles: stored data is not a list');
  return parsed;
}

// Serialize read-modify-write so concurrent saves never lose or corrupt records.
let writeQueue: Promise<void> = Promise.resolve();
function saveArticle(article: Article): Promise<void> {
  const run = writeQueue.then((): void => {
    const records = readArticles();
    records.push(article);
    const tmpFile = join(dataDir, `.articles.${process.pid}.tmp`);
    try {
      writeFileSync(tmpFile, `${JSON.stringify(records, null, 2)}\n`);
      renameSync(tmpFile, dataFile);
    } catch (error) {
      try { rmSync(tmpFile, { force: true }); } catch { /* best effort cleanup */ }
      throw new Error(`unable to save articles: ${(error as Error).message}`);
    }
  });
  writeQueue = run.then(() => undefined, () => undefined);
  return run;
}

function readBody(req: IncomingMessage): Promise<string> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = [];
    let size = 0;
    let settled = false;
    req.on('data', (chunk: unknown) => {
      if (settled) return;
      const buf = Buffer.isBuffer(chunk) ? chunk : Buffer.from(String(chunk));
      size += buf.length;
      if (size > MAX_BODY_BYTES) {
        settled = true;
        reject(new HttpError(400, 'request body is too large'));
        req.destroy();
        return;
      }
      chunks.push(buf);
    });
    req.on('end', () => {
      if (!settled) resolve(Buffer.concat(chunks).toString('utf8'));
    });
    req.on('error', (error: Error) => {
      if (!settled) reject(new HttpError(400, `could not read request body: ${error.message}`));
    });
  });
}

function validateDraftPayload(value: unknown): DraftInput {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    throw new HttpError(400, 'request body must be a JSON object');
  }
  const data = value as Record<string, unknown>;
  if (typeof data.title !== 'string') {
    throw new HttpError(400, 'title must be a string');
  }
  const title = data.title.trim();
  if (title.length === 0) {
    throw new HttpError(400, 'title must not be empty or only whitespace');
  }
  let summary = '';
  if (Object.prototype.hasOwnProperty.call(data, 'summary')) {
    if (typeof data.summary !== 'string') throw new HttpError(400, 'summary must be a string when provided');
    summary = data.summary;
  }
  if (typeof data.body !== 'string') {
    throw new HttpError(400, 'body must be a string');
  }
  return { title, summary, body: data.body };
}

async function handle(req: IncomingMessage, res: ServerResponse): Promise<void> {
  let route: string;
  try { route = new URL(req.url ?? '/', 'http://localhost').pathname; }
  catch { respond(res, 400, { error: 'invalid request path' }); return; }

  if (!['/', '/health', '/api/articles'].includes(route)) { respond(res, 404, { error: 'not found' }); return; }

  if (route === '/') {
    if (req.method === 'GET') { respond(res, 200, PAGE, true); return; }
    methodNotAllowed(res, 'GET');
    return;
  }
  if (route === '/health') {
    if (req.method === 'GET') { respond(res, 200, { status: 'ok', product: PRODUCT }); return; }
    methodNotAllowed(res, 'GET');
    return;
  }

  if (req.method === 'GET') {
    respond(res, 200, { [RESOURCE]: readArticles() });
    return;
  }
  if (req.method === 'POST') {
    const text = await readBody(req);
    let payload: unknown;
    try {
      payload = JSON.parse(text);
    } catch (error) {
      throw new HttpError(400, `invalid JSON: ${(error as Error).message}`);
    }
    const fields = validateDraftPayload(payload);
    const article: Article = {
      id: randomUUID(),
      title: fields.title,
      summary: fields.summary,
      body: fields.body,
      status: 'draft',
      createdAt: new Date().toISOString(),
    };
    await saveArticle(article);
    respond(res, 201, { article });
    return;
  }
  methodNotAllowed(res, 'GET, POST');
}

const server = createServer((req: IncomingMessage, res: ServerResponse): void => {
  handle(req, res).catch((error: unknown) => {
    if (res.writableEnded) return;
    if (error instanceof HttpError) {
      respond(res, error.status, { error: error.message });
      return;
    }
    respond(res, 500, { error: error instanceof Error ? error.message : 'internal error' });
  });
});
server.once('error', (error: Error): void => { console.error(error.message); process.exitCode = 1; });
server.listen(port, host, (): void => {
  const address = server.address();
  if (address && typeof address !== 'string') {
    const displayedHost = address.address.includes(':') ? `[${address.address}]` : address.address;
    console.log(`${PRODUCT} listening on http://${displayedHost}:${address.port}`);
  }
});
for (const signal of ['SIGINT', 'SIGTERM'] as const) {
  process.on(signal, (): void => { server.close(() => { process.exitCode = 0; }); server.closeIdleConnections(); });
}
