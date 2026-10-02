import { createServer } from 'node:http';
import type { IncomingMessage, ServerResponse } from 'node:http';
import { mkdirSync, readFileSync, writeFileSync, renameSync } from 'node:fs';
import { join } from 'node:path';
import { randomUUID } from 'node:crypto';

const PRODUCT: string = 'QuillPress';
const RESOURCE: string = 'articles';
const PAGE: string = `<!doctype html>
<html lang="zh-CN">
<meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1">
<title>QuillPress · 内容编辑与刊物发布</title>
<style>
body{font-family:system-ui,sans-serif;max-width:52rem;margin:3rem auto;padding:0 1rem;line-height:1.7;color:#222}
h1{margin-bottom:0}
.tagline{color:#666;margin-top:0}
form{display:flex;flex-direction:column;gap:.75rem;margin:1rem 0}
label{display:flex;flex-direction:column;gap:.25rem;font-weight:600;font-size:.9rem}
input,textarea{font:inherit;padding:.5rem;border:1px solid #bbb;border-radius:4px}
button{font:inherit;padding:.5rem 1.25rem;border:1px solid #175b9c;background:#175b9c;color:#fff;border-radius:4px;cursor:pointer;align-self:flex-start}
button[disabled]{opacity:.6;cursor:not-allowed}
#status{min-height:1.5em;margin:.25rem 0;font-size:.95rem}
#status.saving{color:#666}
#status.success{color:#1a7f37}
#status.error{color:#b00020}
#article-list{list-style:none;padding:0;margin:0 0 1rem}
#article-list li{padding:.75rem 0;border-bottom:1px solid #eee}
#article-list .meta{color:#888;font-weight:400;font-size:.85rem;margin-left:.5rem}
#article-list .summary{color:#555;white-space:pre-wrap;margin-top:.25rem}
#empty-hint{color:#888}
a{color:#175b9c}
</style>
<main>
<h1>QuillPress</h1>
<p class="tagline">内容编辑与刊物发布</p>

<section>
<h2>保存草稿</h2>
<form id="draft-form">
  <label>标题 <input id="title" name="title" autocomplete="off"></label>
  <label>摘要 <textarea id="summary" name="summary" rows="2"></textarea></label>
  <label>正文 <textarea id="body" name="body" rows="8"></textarea></label>
  <button id="save-btn" type="submit">保存草稿</button>
  <p id="status" role="status" aria-live="polite"></p>
</form>
</section>

<section>
<h2>文章列表</h2>
<ul id="article-list"></ul>
<p id="empty-hint">还没有草稿记录。</p>
</section>

<p><a href="/api/articles">查看文章列表接口</a> · <a href="/health">服务状态</a></p>
</main>
<script>
(function () {
  var form = document.getElementById('draft-form');
  var titleEl = document.getElementById('title');
  var summaryEl = document.getElementById('summary');
  var bodyEl = document.getElementById('body');
  var btn = document.getElementById('save-btn');
  var statusEl = document.getElementById('status');
  var listEl = document.getElementById('article-list');
  var emptyEl = document.getElementById('empty-hint');

  function setStatus(text, cls) {
    statusEl.textContent = text;
    statusEl.className = cls || '';
  }

  function pad(n) { return (n < 10 ? '0' : '') + n; }

  function formatDate(iso) {
    var d = new Date(iso);
    if (isNaN(d.getTime())) return iso;
    return d.getFullYear() + '-' + pad(d.getMonth() + 1) + '-' + pad(d.getDate()) +
      ' ' + pad(d.getHours()) + ':' + pad(d.getMinutes());
  }

  function render(articles) {
    listEl.innerHTML = '';
    var items = (articles || []).slice().sort(function (a, b) {
      return new Date(b.createdAt).getTime() - new Date(a.createdAt).getTime();
    });
    if (!items.length) {
      emptyEl.hidden = false;
      return;
    }
    emptyEl.hidden = true;
    items.forEach(function (a) {
      var li = document.createElement('li');
      var head = document.createElement('div');
      var strong = document.createElement('strong');
      strong.textContent = a.title;
      var meta = document.createElement('span');
      meta.className = 'meta';
      meta.textContent = formatDate(a.createdAt);
      head.appendChild(strong);
      head.appendChild(document.createTextNode(' '));
      head.appendChild(meta);
      var sum = document.createElement('div');
      sum.className = 'summary';
      sum.textContent = a.summary || '';
      li.appendChild(head);
      li.appendChild(sum);
      listEl.appendChild(li);
    });
  }

  function load() {
    return fetch('/api/articles').then(function (r) { return r.json(); }).then(function (data) {
      render(data.articles);
    }).catch(function () { /* 保留空列表提示 */ });
  }

  form.addEventListener('submit', function (e) {
    e.preventDefault();
    var title = titleEl.value;
    if (title.trim() === '') {
      setStatus('保存失败：标题不能为空', 'error');
      titleEl.focus();
      return;
    }
    btn.disabled = true;
    setStatus('正在保存…', 'saving');
    var payload = JSON.stringify({ title: title, summary: summaryEl.value, body: bodyEl.value });
    fetch('/api/articles', {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: payload
    }).then(function (res) {
      return res.json().catch(function () { return null; }).then(function (data) {
        if (!res.ok) {
          var msg = (data && data.error) ? data.error : ('保存失败（HTTP ' + res.status + '）');
          setStatus('保存失败：' + msg, 'error');
          return;
        }
        form.reset();
        setStatus('草稿已保存', 'success');
        return load();
      });
    }).catch(function (err) {
      setStatus('保存失败：' + (err && err.message ? err.message : '网络错误'), 'error');
    }).then(function () {
      btn.disabled = false;
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

const ROUTES = new Set(['/', '/health', '/api/articles']);
const METHODS: Record<string, string[]> = {
  '/': ['GET'],
  '/health': ['GET'],
  '/api/articles': ['GET', 'POST']
};

function respond(res: ServerResponse, status: number, value: unknown, html = false, allow?: string): void {
  const body = html ? String(value) : JSON.stringify(value);
  res.writeHead(status, { 'content-type': html ? 'text/html; charset=utf-8' : 'application/json; charset=utf-8',
    'content-length': Buffer.byteLength(body), ...(status === 405 && allow ? { allow } : {}) });
  res.end(body);
}

function readArticles(): unknown[] {
  const records: unknown = JSON.parse(readFileSync(dataFile, 'utf8'));
  if (!Array.isArray(records)) throw new Error('Invalid record list');
  return records;
}

let writeChain: Promise<void> = Promise.resolve();
function writeArticles(records: unknown[]): Promise<void> {
  const task = writeChain.then(() => {
    const tmpFile = dataFile + '.tmp';
    writeFileSync(tmpFile, JSON.stringify(records, null, 2) + '\n');
    renameSync(tmpFile, dataFile);
  });
  writeChain = task.catch(() => { /* 错误已在调用处处理，这里保持队列不断 */ });
  return task;
}

function readBody(req: IncomingMessage): Promise<string> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = [];
    req.on('data', (chunk: Buffer) => chunks.push(chunk));
    req.on('end', () => resolve(Buffer.concat(chunks).toString('utf8')));
    req.on('error', reject);
  });
}

type ParsedInput = { ok: true; title: string; summary: string; body: string } | { ok: false; error: string };

function parseArticleInput(text: string): ParsedInput {
  let value: unknown;
  try {
    value = JSON.parse(text);
  } catch {
    return { ok: false, error: '请求体不是有效的 JSON' };
  }
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    return { ok: false, error: '请求体必须是 JSON 对象' };
  }
  const input = value as Record<string, unknown>;
  if (typeof input.title !== 'string') {
    return { ok: false, error: '标题必须是字符串' };
  }
  const title = input.title.trim();
  if (title.length === 0) {
    return { ok: false, error: '标题不能为空' };
  }
  if (input.summary !== undefined && typeof input.summary !== 'string') {
    return { ok: false, error: '摘要必须是字符串' };
  }
  if (typeof input.body !== 'string') {
    return { ok: false, error: '正文必须是字符串' };
  }
  return { ok: true, title, summary: typeof input.summary === 'string' ? input.summary : '', body: input.body };
}

const server = createServer((req: IncomingMessage, res: ServerResponse): void => {
  let route: string;
  try { route = new URL(req.url ?? '/', 'http://localhost').pathname; } catch { respond(res, 400, { error: 'invalid request path' }); return; }
  if (!ROUTES.has(route)) { respond(res, 404, { error: 'not found' }); return; }
  const allowed = METHODS[route] ?? [];
  if (!allowed.includes(req.method ?? '')) { respond(res, 405, { error: 'method not allowed' }, false, allowed.join(', ')); return; }
  if (route === '/') { respond(res, 200, PAGE, true); return; }
  if (route === '/health') { respond(res, 200, { status: 'ok', product: PRODUCT }); return; }

  if (req.method === 'GET') {
    try {
      respond(res, 200, { [RESOURCE]: readArticles() });
    } catch {
      respond(res, 500, { error: 'unable to read articles' });
    }
    return;
  }

  readBody(req).then((text: string) => {
    const parsed = parseArticleInput(text);
    if (!parsed.ok) { respond(res, 400, { error: parsed.error }); return; }
    let records: unknown[];
    try {
      records = readArticles();
    } catch {
      respond(res, 500, { error: 'unable to read articles' });
      return;
    }
    const article = {
      id: randomUUID(),
      title: parsed.title,
      summary: parsed.summary,
      body: parsed.body,
      status: 'draft',
      createdAt: new Date().toISOString()
    };
    records.push(article);
    writeArticles(records).then(() => {
      respond(res, 201, { article });
    }).catch(() => {
      respond(res, 500, { error: 'unable to save article' });
    });
  }).catch(() => {
    respond(res, 400, { error: 'invalid request body' });
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
