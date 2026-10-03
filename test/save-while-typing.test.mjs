// 首页草稿保存的端到端回归测试。
//
// 重点场景：用户点击保存后、等待响应期间继续输入——后写的内容不能因为
// 保存成功而丢失，也不能被误报为已保存。测试通过 CDP 驱动真实浏览器
// 打开首页，用 Fetch 拦截把保存请求挂起，精确制造“保存进行中”的窗口，
// 然后走真实表单、提示、列表、再次保存与重新打开草稿的完整流程。
//
// 运行：npm test（需要本机 Chrome，可用 CHROME_BIN 指定路径）。

import { test, before, after } from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { mkdtempSync, rmSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join, dirname } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = dirname(dirname(fileURLToPath(import.meta.url)));
const CHROME_BIN = process.env.CHROME_BIN || 'google-chrome';

// ---------------------------------------------------------------------------
// 最小 CDP 客户端（Node 内置 WebSocket，无第三方依赖）
// ---------------------------------------------------------------------------

class CDP {
  static connect(url) {
    return new Promise((resolve, reject) => {
      const ws = new WebSocket(url);
      ws.addEventListener('open', () => resolve(new CDP(ws)));
      ws.addEventListener('error', () => reject(new Error('无法连接 Chrome DevTools')));
    });
  }

  constructor(ws) {
    this.ws = ws;
    this.nextId = 1;
    this.pending = new Map();
    this.listeners = [];
    ws.addEventListener('message', (event) => {
      const msg = JSON.parse(event.data);
      if (msg.id !== undefined) {
        const entry = this.pending.get(msg.id);
        if (!entry) return;
        this.pending.delete(msg.id);
        if (msg.error) entry.reject(new Error(`${entry.method}: ${msg.error.message}`));
        else entry.resolve(msg.result);
      } else if (msg.method) {
        for (const fn of this.listeners) fn(msg);
      }
    });
  }

  send(method, params = {}, sessionId = undefined) {
    const id = this.nextId++;
    return new Promise((resolve, reject) => {
      this.pending.set(id, { resolve, reject, method });
      this.ws.send(JSON.stringify(sessionId ? { id, method, params, sessionId } : { id, method, params }));
    });
  }

  onEvent(fn) {
    this.listeners.push(fn);
  }

  close() {
    try { this.ws.close(); } catch { /* 忽略 */ }
  }
}

// ---------------------------------------------------------------------------
// 页面驱动：导航、求值、等待，以及“挂起保存请求”的拦截控制
// ---------------------------------------------------------------------------

class Page {
  constructor(cdp, sessionId, targetId) {
    this.cdp = cdp;
    this.sessionId = sessionId;
    this.targetId = targetId;
    this.holdSaves = false;
    this.held = [];
    this.heldWaiters = [];
  }

  static async open(cdp, url) {
    const { targetId } = await cdp.send('Target.createTarget', { url: 'about:blank' });
    const { sessionId } = await cdp.send('Target.attachToTarget', { targetId, flatten: true });
    const page = new Page(cdp, sessionId, targetId);
    await page.send('Runtime.enable');
    await page.send('Page.enable');
    await page.send('Fetch.enable', { patterns: [{ urlPattern: '*', requestStage: 'Request' }] });
    cdp.onEvent((msg) => {
      if (msg.sessionId !== sessionId || msg.method !== 'Fetch.requestPaused') return;
      page.handlePaused(msg.params);
    });
    await page.send('Page.navigate', { url });
    return page;
  }

  send(method, params = {}) {
    return this.cdp.send(method, params, this.sessionId);
  }

  handlePaused(params) {
    const { request, requestId } = params;
    const isSave = (request.method === 'POST' || request.method === 'PUT')
      && request.url.includes('/api/articles');
    if (this.holdSaves && isSave) {
      this.held.push(requestId);
      for (const notify of this.heldWaiters.splice(0)) notify(requestId);
      return;
    }
    this.send('Fetch.continueRequest', { requestId }).catch(() => {});
  }

  // 等待下一个被挂起的保存请求（测试据此确认“保存进行中”窗口已开始）
  waitHeld() {
    if (this.held.length) return Promise.resolve(this.held[0]);
    return new Promise((resolve) => this.heldWaiters.push(resolve));
  }

  // 放行被挂起的保存请求，让服务端正常处理
  async releaseHeld() {
    this.holdSaves = false;
    for (const requestId of this.held.splice(0)) {
      await this.send('Fetch.continueRequest', { requestId });
    }
  }

  // 以指定的失败响应直接应答被挂起的保存请求，模拟服务端明确拒绝
  async fulfillHeld(status, body) {
    this.holdSaves = false;
    const payload = Buffer.from(JSON.stringify(body)).toString('base64');
    for (const requestId of this.held.splice(0)) {
      await this.send('Fetch.fulfillRequest', {
        requestId,
        responseCode: status,
        responseHeaders: [{ name: 'content-type', value: 'application/json; charset=utf-8' }],
        body: payload,
      });
    }
  }

  async eval(expression) {
    const result = await this.send('Runtime.evaluate', {
      expression,
      returnByValue: true,
      awaitPromise: true,
    });
    if (result.exceptionDetails) {
      throw new Error(`页面求值失败: ${JSON.stringify(result.exceptionDetails.exception?.description || result.exceptionDetails.text)}`);
    }
    return result.result.value;
  }

  async waitFor(expression, desc, timeoutMs = 10000) {
    const deadline = Date.now() + timeoutMs;
    for (;;) {
      try {
        if (await this.eval(expression)) return;
      } catch { /* 导航期间上下文可能暂不存在，继续轮询 */ }
      if (Date.now() > deadline) throw new Error(`等待超时: ${desc}`);
      await new Promise((resolve) => setTimeout(resolve, 50));
    }
  }
}

// ---------------------------------------------------------------------------
// 测试基础设施：每个用例一套独立服务与数据目录，共享一个无头 Chrome
// ---------------------------------------------------------------------------

let chromeProc;
let cdp;

before(async () => {
  const profileDir = mkdtempSync(join(tmpdir(), 'quillpress-chrome-'));
  chromeProc = spawn(CHROME_BIN, [
    '--headless=new',
    '--remote-debugging-port=0',
    '--no-sandbox',
    '--disable-gpu',
    '--disable-dev-shm-usage',
    '--no-first-run',
    `--user-data-dir=${profileDir}`,
    'about:blank',
  ], { stdio: ['ignore', 'ignore', 'pipe'] });
  const wsUrl = await new Promise((resolve, reject) => {
    let err = '';
    chromeProc.stderr.on('data', (chunk) => {
      err += chunk;
      const m = err.match(/DevTools listening on (ws:\/\/\S+)/);
      if (m) resolve(m[1]);
    });
    chromeProc.on('exit', (code) => reject(new Error(`Chrome 提前退出 (${code}): ${err}`)));
    chromeProc.on('error', reject);
  });
  cdp = await CDP.connect(wsUrl);
});

after(() => {
  cdp?.close();
  chromeProc?.kill('SIGKILL');
});

function startServer(t) {
  const dataDir = mkdtempSync(join(tmpdir(), 'quillpress-data-'));
  const proc = spawn(process.execPath, [
    join(ROOT, 'server.ts'), 'serve',
    '--host', '127.0.0.1', '--port', '0', '--data-dir', dataDir,
  ], { stdio: ['ignore', 'pipe', 'pipe'] });
  const ready = new Promise((resolve, reject) => {
    let out = '';
    proc.stdout.on('data', (chunk) => {
      out += chunk;
      const m = out.match(/listening on http:\/\/127\.0\.0\.1:(\d+)/);
      if (m) resolve(Number(m[1]));
    });
    proc.on('exit', (code) => reject(new Error(`服务提前退出 (${code}): ${out}`)));
    proc.on('error', reject);
  });
  t.after(() => {
    proc.kill('SIGTERM');
    rmSync(dataDir, { recursive: true, force: true });
  });
  return ready;
}

async function api(port, method, path, body) {
  const res = await fetch(`http://127.0.0.1:${port}${path}`, {
    method,
    headers: body ? { 'content-type': 'application/json' } : undefined,
    body: body ? JSON.stringify(body) : undefined,
  });
  return { status: res.status, data: await res.json() };
}

async function listArticles(port) {
  return (await api(port, 'GET', '/api/articles')).data.articles;
}

// 页面快照：表单、按钮、提示、编辑标识、冲突区
const FORM_STATE = `({
  title: document.getElementById('title').value,
  summary: document.getElementById('summary').value,
  body: document.getElementById('body').value,
  saveLabel: document.getElementById('save-btn').textContent,
  saveDisabled: document.getElementById('save-btn').disabled,
  cancelHidden: document.getElementById('cancel-btn').hidden,
  bannerHidden: document.getElementById('edit-banner').hidden,
  bannerText: document.getElementById('edit-banner').textContent,
  statusText: document.getElementById('status').textContent,
  statusClass: document.getElementById('status').className,
  conflictHidden: document.getElementById('conflict-box').hidden,
  conflictText: document.getElementById('conflict-box').textContent,
  formEditing: document.getElementById('draft-form').classList.contains('editing'),
})`;

// 列表快照：标题、摘要（无摘要段落时为 null）、是否标记“编辑中”
const LIST_STATE = `Array.from(document.querySelectorAll('#article-list > li')).map((li) => ({
  title: li.querySelector('h3').firstChild.textContent,
  summary: li.querySelector('.summary') ? li.querySelector('.summary').textContent : null,
  editing: Boolean(li.querySelector('.editing-tag')),
}))`;

const SAVE_SETTLED = `document.getElementById('save-btn').disabled === false`;

async function openHome(t, port) {
  const page = await Page.open(cdp, `http://127.0.0.1:${port}/`);
  t.after(async () => {
    await cdp.send('Target.closeTarget', { targetId: page.targetId }).catch(() => {});
  });
  await page.waitFor(
    `document.readyState === 'complete' && document.getElementById('empty-tip') !== null`
    + ` && document.getElementById('empty-tip').textContent !== '草稿加载中…'`,
    '首页与草稿列表加载完成',
  );
  // 记录并默认确认 window.confirm，用例可断言是否被意外调用
  await page.eval(`window.__confirmCalls = [];
    window.confirm = (message) => { window.__confirmCalls.push(message); return true; };
    true`);
  return page;
}

async function setField(page, id, value) {
  await page.eval(`(() => {
    const el = document.getElementById(${JSON.stringify(id)});
    el.value = ${JSON.stringify(value)};
    el.dispatchEvent(new Event('input', { bubbles: true }));
  })()`);
}

async function clickSave(page) {
  await page.eval(`document.getElementById('save-btn').click()`);
}

// 点击列表中指定标题草稿的“编辑”，等待进入编辑状态
async function openDraftForEdit(page, title) {
  await page.eval(`(() => {
    const cards = Array.from(document.querySelectorAll('#article-list > li'));
    const card = cards.find((li) => li.querySelector('h3').firstChild.textContent === ${JSON.stringify(title)});
    if (!card) throw new Error('列表中找不到草稿: ' + ${JSON.stringify(title)});
    card.querySelector('.actions button').click();
  })()`);
  await page.waitFor(
    `document.getElementById('save-btn').textContent === '保存修改'`,
    `进入《${title}》的编辑状态`,
  );
}

// ---------------------------------------------------------------------------
// 场景一：编辑已有草稿，保存等待期间继续输入
// ---------------------------------------------------------------------------

test('编辑草稿：保存等待期间继续输入的内容不丢失、不误报已保存', { timeout: 60000 }, async (t) => {
  const port = await startServer(t);
  const created = (await api(port, 'POST', '/api/articles', {
    title: '原始标题', summary: '原始摘要', body: '原始正文',
  })).data.article;

  const page = await openHome(t, port);
  await page.waitFor(`document.querySelectorAll('#article-list > li').length === 1`, '草稿出现在列表');
  await openDraftForEdit(page, '原始标题');

  await setField(page, 'title', '第一次提交标题');
  await setField(page, 'summary', '第一次提交摘要');
  await setField(page, 'body', '第一次提交正文');

  page.holdSaves = true;
  const held = page.waitHeld();
  await clickSave(page);
  await held; // 保存请求已发出、响应尚未返回

  // 等待响应期间继续编辑：改标题、清空摘要、重写正文
  await setField(page, 'title', '等待期间改的标题');
  await setField(page, 'summary', '');
  await setField(page, 'body', '等待期间重写的正文\n\n第二段');

  await page.releaseHeld();
  await page.waitFor(SAVE_SETTLED, '第一次保存完成');

  // 表单保留等待期间输入的当前值，仍编辑同一篇草稿
  let form = await page.eval(FORM_STATE);
  assert.equal(form.title, '等待期间改的标题');
  assert.equal(form.summary, '');
  assert.equal(form.body, '等待期间重写的正文\n\n第二段');
  assert.equal(form.saveLabel, '保存修改');
  assert.equal(form.formEditing, true);
  assert.equal(form.bannerHidden, false);
  assert.ok(form.bannerText.includes('正在编辑草稿'));
  assert.ok(form.bannerText.includes(created.id));
  // 明确指出哪些字段还有未保存的修改
  assert.match(form.statusClass, /\bwarn\b/);
  assert.ok(form.statusText.includes('标题'), '提示应包含标题');
  assert.ok(form.statusText.includes('摘要'), '提示应包含摘要');
  assert.ok(form.statusText.includes('正文'), '提示应包含正文');
  assert.ok(form.statusText.includes('尚未保存'));
  assert.equal(form.conflictHidden, true);

  // 列表反映本次实际提交并保存的标题与摘要
  let list = await page.eval(LIST_STATE);
  assert.equal(list.length, 1);
  assert.equal(list[0].title, '第一次提交标题');
  assert.equal(list[0].summary, '第一次提交摘要');
  assert.equal(list[0].editing, true);

  // 服务端保存的是本次提交的内容，版本已推进
  let saved = (await api(port, 'GET', `/api/articles/${created.id}`)).data.article;
  assert.equal(saved.title, '第一次提交标题');
  assert.equal(saved.summary, '第一次提交摘要');
  assert.equal(saved.version, 2);

  // 再次点击“保存修改”：刚才保留的内容才成为已保存内容
  await clickSave(page);
  await page.waitFor(SAVE_SETTLED, '第二次保存完成');
  form = await page.eval(FORM_STATE);
  assert.match(form.statusClass, /\bok\b/);
  assert.ok(form.statusText.includes('修改已保存'));
  assert.equal(form.conflictHidden, true, '不应因第一次保存推进了版本而误报冲突');
  assert.equal(form.saveLabel, '保存修改');

  list = await page.eval(LIST_STATE);
  assert.equal(list.length, 1, '列表不增加记录');
  assert.equal(list[0].title, '等待期间改的标题');
  assert.equal(list[0].summary, null, '摘要已清空，不渲染摘要段落');

  const all = await listArticles(port);
  assert.equal(all.length, 1);
  saved = all[0];
  assert.equal(saved.id, created.id, '文章标识不变');
  assert.equal(saved.createdAt, created.createdAt, '创建时间不变');
  assert.equal(saved.status, 'draft', '草稿状态不变');
  assert.equal(saved.version, 3);
  assert.equal(saved.title, '等待期间改的标题');
  assert.equal(saved.summary, '');
  assert.equal(saved.body, '等待期间重写的正文\n\n第二段');

  // 取消编辑后重新打开同一篇草稿，载入的是最终保存的内容
  await page.eval(`document.getElementById('cancel-btn').click()`);
  await page.waitFor(`document.getElementById('save-btn').textContent === '保存草稿'`, '回到新建状态');
  await openDraftForEdit(page, '等待期间改的标题');
  form = await page.eval(FORM_STATE);
  assert.equal(form.title, '等待期间改的标题');
  assert.equal(form.summary, '');
  assert.equal(form.body, '等待期间重写的正文\n\n第二段');
  assert.ok(form.bannerText.includes(created.id));
  assert.equal(await page.eval(`window.__confirmCalls.length`), 0, '无未保存内容时不应弹出确认');
});

// ---------------------------------------------------------------------------
// 场景二：新建草稿，保存等待期间补写内容
// ---------------------------------------------------------------------------

test('新建草稿：保存等待期间补写内容只产生一篇草稿并进入编辑态', { timeout: 60000 }, async (t) => {
  const port = await startServer(t);
  const page = await openHome(t, port);

  await setField(page, 'title', '新建标题');
  await setField(page, 'summary', '新建摘要');
  await setField(page, 'body', '新建正文');

  page.holdSaves = true;
  const held = page.waitHeld();
  await clickSave(page);
  await held;

  // 等待响应期间补写摘要和正文
  await setField(page, 'summary', '补写的摘要');
  await setField(page, 'body', '新建正文\n\n补写的内容');

  await page.releaseHeld();
  await page.waitFor(SAVE_SETTLED, '新建保存完成');

  // 只产生一篇草稿；表单关联到刚创建的文章并进入编辑状态，保留补写内容
  const form = await page.eval(FORM_STATE);
  assert.equal(form.saveLabel, '保存修改', '提交按钮变为“保存修改”');
  assert.equal(form.formEditing, true);
  assert.equal(form.bannerHidden, false);
  assert.equal(form.summary, '补写的摘要');
  assert.equal(form.body, '新建正文\n\n补写的内容');
  assert.match(form.statusClass, /\bwarn\b/);
  assert.ok(form.statusText.includes('摘要、正文'), '提示应指出摘要和正文尚未保存');
  assert.ok(form.statusText.includes('尚未保存'));

  let all = await listArticles(port);
  assert.equal(all.length, 1, '保存成功只产生一篇草稿');
  const draftId = all[0].id;
  assert.ok(form.bannerText.includes(draftId), '表单关联到刚创建的文章');

  // 列表显示本次实际保存的内容
  let list = await page.eval(LIST_STATE);
  assert.equal(list.length, 1);
  assert.equal(list[0].title, '新建标题');
  assert.equal(list[0].summary, '新建摘要');
  assert.equal(list[0].editing, true);

  // 后续保存更新这篇文章，而不是再新建一篇
  await clickSave(page);
  await page.waitFor(SAVE_SETTLED, '后续保存完成');
  all = await listArticles(port);
  assert.equal(all.length, 1, '后续保存不应再新建草稿');
  assert.equal(all[0].id, draftId);
  assert.equal(all[0].version, 2);
  assert.equal(all[0].summary, '补写的摘要');
  assert.equal(all[0].body, '新建正文\n\n补写的内容');

  const after = await page.eval(FORM_STATE);
  assert.match(after.statusClass, /\bok\b/);
  assert.equal(after.conflictHidden, true);
  list = await page.eval(LIST_STATE);
  assert.equal(list.length, 1);
  assert.equal(list[0].summary, '补写的摘要');
});

// ---------------------------------------------------------------------------
// 场景三：新建草稿，保存等待期间没有后续输入
// ---------------------------------------------------------------------------

test('新建草稿：保存期间没有后续输入时表单清空并可继续新建', { timeout: 60000 }, async (t) => {
  const port = await startServer(t);
  const page = await openHome(t, port);

  await setField(page, 'title', '第一篇');
  await setField(page, 'summary', '摘要一');
  await setField(page, 'body', '正文一');

  page.holdSaves = true;
  const held = page.waitHeld();
  await clickSave(page);
  await held;
  await page.releaseHeld();
  await page.waitFor(SAVE_SETTLED, '保存完成');

  // 成功后的表单按已有规则清空，保持新建状态
  const form = await page.eval(FORM_STATE);
  assert.equal(form.title, '');
  assert.equal(form.summary, '');
  assert.equal(form.body, '');
  assert.equal(form.saveLabel, '保存草稿');
  assert.equal(form.bannerHidden, true);
  assert.equal(form.cancelHidden, true);
  assert.equal(form.formEditing, false);
  assert.match(form.statusClass, /\bok\b/);

  let list = await page.eval(LIST_STATE);
  assert.equal(list.length, 1);
  assert.equal(list[0].title, '第一篇');
  assert.equal(list[0].summary, '摘要一');

  // 继续允许新建第二篇
  await setField(page, 'title', '第二篇');
  await setField(page, 'body', '正文二');
  await clickSave(page);
  await page.waitFor(SAVE_SETTLED, '第二次新建完成');
  list = await page.eval(LIST_STATE);
  assert.equal(list.length, 2);
  assert.equal((await listArticles(port)).length, 2);
});

// ---------------------------------------------------------------------------
// 场景四：未保存判断以最终字段值和本次实际保存的内容为准
// ---------------------------------------------------------------------------

test('未保存判断：恢复原值与标题去空白不误报，多语言与类 HTML 文本原样保留', { timeout: 60000 }, async (t) => {
  const port = await startServer(t);
  const created = (await api(port, 'POST', '/api/articles', {
    title: '原标题', summary: '原摘要', body: '原正文',
  })).data.article;

  const page = await openHome(t, port);
  await page.waitFor(`document.querySelectorAll('#article-list > li').length === 1`, '草稿出现在列表');
  await openDraftForEdit(page, '原标题');

  // 标题带首尾空白提交；等待期间：标题改成服务端保存后的同一文字、
  // 摘要改了又改回原值、正文不动 —— 都不应误报未保存。
  await setField(page, 'title', '  规范标题  ');
  await setField(page, 'summary', '摘要甲');
  await setField(page, 'body', '正文甲');

  page.holdSaves = true;
  let held = page.waitHeld();
  await clickSave(page);
  await held;
  await setField(page, 'title', '规范标题'); // 与保存后的标题同一文字
  await setField(page, 'summary', '摘要乙');
  await setField(page, 'summary', '摘要甲'); // 改动后恢复原值
  await page.releaseHeld();
  await page.waitFor(SAVE_SETTLED, '第一次保存完成');

  let form = await page.eval(FORM_STATE);
  assert.match(form.statusClass, /\bok\b/, '改动后恢复原值不应误报未保存');
  assert.ok(form.statusText.includes('修改已保存'));
  assert.equal(form.title, '规范标题', '标题按已有规则去除首尾空白');
  assert.equal(form.summary, '摘要甲');
  assert.equal(form.body, '正文甲');

  // 等待期间补写含中文、其他语言文字、换行空行和类 HTML 的文本
  page.holdSaves = true;
  held = page.waitHeld();
  await clickSave(page);
  await held;
  const richSummary = '多语言摘要 café\n\n第二行 <b>不是加粗</b>';
  const richBody = '中文段落\n\nEnglish paragraph\n\n<html>按原文保存</html>\n\n日本語・한국어';
  await setField(page, 'summary', richSummary);
  await setField(page, 'body', richBody);
  await page.releaseHeld();
  await page.waitFor(SAVE_SETTLED, '第二次保存完成');

  // 真正保留的摘要、正文原样留在表单中
  form = await page.eval(FORM_STATE);
  assert.equal(form.summary, richSummary);
  assert.equal(form.body, richBody);
  assert.match(form.statusClass, /\bwarn\b/);
  assert.ok(form.statusText.includes('摘要、正文'));

  // 再次保存后，保留内容原样成为已保存内容
  await clickSave(page);
  await page.waitFor(SAVE_SETTLED, '第三次保存完成');
  const saved = (await api(port, 'GET', `/api/articles/${created.id}`)).data.article;
  assert.equal(saved.summary, richSummary);
  assert.equal(saved.body, richBody);

  // 类似 HTML 的文字仍作为文本处理，列表不注入任何元素
  assert.equal(
    await page.eval(`document.querySelector('#article-list').querySelector('b, html, img, script') !== null`),
    false,
    '类 HTML 文本不应被解析为元素',
  );
  assert.equal(await page.eval(`document.querySelector('#article-list .summary').textContent`), richSummary);
});

// ---------------------------------------------------------------------------
// 场景五：服务端明确拒绝保存
// ---------------------------------------------------------------------------

test('服务端拒绝保存：保留响应到达时的全部当前输入和原状态', { timeout: 60000 }, async (t) => {
  const port = await startServer(t);
  const created = (await api(port, 'POST', '/api/articles', {
    title: '已有草稿', summary: '旧摘要', body: '旧正文',
  })).data.article;

  const page = await openHome(t, port);
  await page.waitFor(`document.querySelectorAll('#article-list > li').length === 1`, '草稿出现在列表');
  await openDraftForEdit(page, '已有草稿');

  // —— 编辑已有草稿被拒绝（500）——
  await setField(page, 'title', '编辑后的标题');
  page.holdSaves = true;
  let held = page.waitHeld();
  await clickSave(page);
  await held;
  await setField(page, 'summary', '等待期间改的摘要');
  await page.fulfillHeld(500, { error: '模拟的服务端写入失败' });
  await page.waitFor(SAVE_SETTLED, '失败响应处理完成');

  let form = await page.eval(FORM_STATE);
  assert.match(form.statusClass, /\berr\b/);
  assert.ok(form.statusText.includes('保存修改失败'));
  assert.ok(form.statusText.includes('模拟的服务端写入失败'), '显示失败原因');
  assert.ok(!form.statusText.includes('已保存'), '不显示成功');
  assert.equal(form.title, '编辑后的标题', '保留响应到达时的当前输入');
  assert.equal(form.summary, '等待期间改的摘要');
  assert.equal(form.body, '旧正文');
  assert.equal(form.saveLabel, '保存修改', '保留原来的编辑状态');
  assert.equal(form.bannerHidden, false);
  assert.ok(form.bannerText.includes(created.id));
  assert.equal(form.saveDisabled, false, '恢复可保存状态');

  // 列表与服务端都保持原样
  let list = await page.eval(LIST_STATE);
  assert.equal(list.length, 1);
  assert.equal(list[0].title, '已有草稿');
  assert.equal(list[0].summary, '旧摘要');
  let saved = (await api(port, 'GET', `/api/articles/${created.id}`)).data.article;
  assert.equal(saved.title, '已有草稿');
  assert.equal(saved.version, 1);

  // —— 真实版本冲突（409）：保留输入并展示对照 ——
  await api(port, 'PUT', `/api/articles/${created.id}`, {
    title: '其他页面保存的标题', summary: '其他页面的摘要', body: '其他页面的正文', version: 1,
  });
  await clickSave(page);
  await page.waitFor(`document.getElementById('conflict-box').hidden === false`, '冲突对照出现');
  form = await page.eval(FORM_STATE);
  assert.match(form.statusClass, /\berr\b/);
  assert.ok(form.statusText.includes('409'));
  assert.ok(form.conflictText.includes('你正在编辑的内容（未保存）'));
  assert.ok(form.conflictText.includes('最新已保存内容'));
  assert.ok(form.conflictText.includes('编辑后的标题'), '对照中展示未保存的当前输入');
  assert.ok(form.conflictText.includes('其他页面保存的标题'), '对照中展示最新已保存内容');
  assert.equal(form.title, '编辑后的标题', '冲突不重置表单');
  assert.equal(form.summary, '等待期间改的摘要');
  assert.equal(form.saveDisabled, false);

  // —— 新建草稿被拒绝（400）——
  await page.eval(`document.getElementById('cancel-btn').click()`); // confirm 已被替代为确认
  await page.waitFor(`document.getElementById('save-btn').textContent === '保存草稿'`, '回到新建状态');
  await setField(page, 'title', '会被拒绝的草稿');
  await setField(page, 'summary', '摘要');
  await setField(page, 'body', '正文');
  page.holdSaves = true;
  held = page.waitHeld();
  await clickSave(page);
  await held;
  await setField(page, 'body', '正文\n\n等待期间补写');
  await page.fulfillHeld(400, { error: '模拟的服务端拒绝：标题不符合要求' });
  await page.waitFor(SAVE_SETTLED, '失败响应处理完成');

  form = await page.eval(FORM_STATE);
  assert.match(form.statusClass, /\berr\b/);
  assert.ok(form.statusText.includes('保存失败'));
  assert.ok(form.statusText.includes('模拟的服务端拒绝'));
  assert.ok(!form.statusText.includes('已保存'));
  assert.equal(form.title, '会被拒绝的草稿');
  assert.equal(form.body, '正文\n\n等待期间补写', '保留等待期间的补写内容');
  assert.equal(form.saveLabel, '保存草稿', '保留原来的新建状态');
  assert.equal(form.bannerHidden, true);
  assert.equal(form.saveDisabled, false);
  assert.equal(form.conflictHidden, true);

  // 不增加列表记录
  list = await page.eval(LIST_STATE);
  assert.equal(list.length, 1);
  assert.equal((await listArticles(port)).length, 1);
});
