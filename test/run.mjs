// End-to-end regression coverage for the homepage draft-save race:
// after clicking save, the user keeps typing while waiting for the response.
// Those later keystrokes must neither be lost when the save succeeds nor be
// falsely reported as saved. Scenarios exercise the real form, status text,
// draft list, a second save and reopening the draft — never just the API.
//
// Run: node test/run.mjs   (CHROME_BIN can override the browser executable)
import assert from 'node:assert/strict';
import { setTimeout as sleep } from 'node:timers/promises';
import { Browser } from './cdp.mjs';
import { TestServer } from './server.mjs';
import { RawHttp, splitFrames } from './httpraw.mjs';
import * as ui from './ui.mjs';

const tests = [];
function test(name, fn) { tests.push({ name, fn }); }

// Poll the server directly until it has persisted at least `count` articles.
// Used with the save gate's capture mode, where the save request really
// reaches the server and only its response is parked in the page.
async function waitForServerArticles(server, count) {
  const deadline = Date.now() + 5000;
  for (;;) {
    const articles = await server.listArticles();
    if (articles.length >= count) return articles;
    if (Date.now() > deadline) throw new Error(`server did not persist ${count} articles in time`);
    await sleep(25);
  }
}

async function seed(server, fields) {
  const res = await server.api('/api/articles', {
    method: 'POST',
    body: JSON.stringify({ summary: '', ...fields }),
  });
  assert.equal(res.status, 201, `seed failed: ${res.status}`);
  return (await res.json()).article;
}

// Seeds happen out-of-band (as if saved in another tab), so the page must
// reload its list before the user can click the new draft's 编辑 button.
async function seedAndReload(server, page, fields) {
  const article = await seed(server, fields);
  await ui.reload(page);
  return article;
}

async function freshContext(browser) {
  const server = await TestServer.start();
  const page = await ui.open(browser, server.url);
  return { server, page };
}

async function closeContext({ server, page }, browser) {
  ui.assertNoPageErrors(page);
  await browser.closePage(page);
  await server.stop();
}

// Body text exercising plain-text fidelity: Chinese, other scripts, newlines,
// blank lines and HTML-like markup that must never be parsed or executed.
const RICH_BODY = '多行正文第一段\n\n中间空行\n\n中文 English café 日本語 العربية 한국어\n\n'
  + '<script>alert(1)</script>\n<img src=x onerror=alert(2)><b>不应加粗</b>\n<div>末段</div>';

async function getArticle(server, id) {
  const res = await server.api(`/api/articles/${id}`);
  assert.equal(res.status, 200);
  return (await res.json()).article;
}

// An out-of-band update, exactly as if another tab saved the draft.
async function updateOut(server, id, fields, version) {
  const res = await server.api(`/api/articles/${id}`, {
    method: 'PUT',
    body: JSON.stringify({ summary: '', ...fields, version }),
  });
  const data = await res.json().catch(() => ({}));
  return { status: res.status, article: data.article, error: data.error };
}

// Puts the page into the post-409 state: it opened v1, holds `mine` in the
// form, and another tab has since saved v2, so the page's save is rejected
// with the side-by-side conflict comparison.
async function stageStaleConflict(server, page, v1, mine, v2) {
  const article = await seedAndReload(server, page, v1);
  await ui.clickEdit(page, v1.title);
  await ui.waitLoadedDraft(page);
  await ui.fill(page, mine);
  const up = await updateOut(server, article.id, v2, 1);
  assert.equal(up.status, 200, `other-tab update should succeed: ${up.status} ${up.error || ''}`);
  await ui.clickSave(page);
  await ui.waitForStatus(page, '409', 'err');
  await ui.waitConflictShown(page);
  return { article, saved: up.article };
}

// The comparison columns render with textContent: markup-like text must appear
// verbatim and must not create elements or run handlers (no dialog may fire).
async function assertConflictPlainText(page) {
  const injected = await page.eval(
    `document.querySelectorAll('#conflict-box .ver script,#conflict-box .ver img,#conflict-box .ver b').length`);
  assert.equal(injected, 0);
  assert.equal(page.dialogs.length, 0);
}

// ---------------------------------------------------------------------------
// 1. Editing an existing draft: save, keep typing during the wait, save again.
// ---------------------------------------------------------------------------
test('编辑态保存等待期间继续输入：内容保留、只提示未保存字段、再次保存生效、重开一致', async (browser) => {
  const ctx = await freshContext(browser);
  const { server, page } = ctx;
  try {
    const a = await seedAndReload(server, page, {
      title: '原标题',
      summary: '原摘要',
      body: '原正文第一段\n\n原正文第二段',
    });

    // 用户实际操作：打开已有草稿（正文以单篇接口返回为准）。
    await ui.clickEdit(page, '原标题');
    await ui.waitLoadedDraft(page);
    let s = await ui.state(page);
    assert.equal(s.saveBtnText, '保存修改');
    assert.deepEqual(s.form, { title: '原标题', summary: '原摘要', body: '原正文第一段\n\n原正文第二段' });
    assert.equal(s.bannerId, a.id);

    // 提交标题、摘要、正文。
    await ui.fill(page, {
      title: '  修改后的标题  ',
      summary: '实际提交的摘要 <b>x</b>',
      body: '实际提交的正文第一段\n\n实际提交的正文第二段',
    });
    await ui.gateOn(page);
    await ui.clickSave(page);
    await ui.waitSavingPending(page, 1);

    // 等待响应期间：标题不动，摘要补写、正文重写（含换行、空行、多语言、类 HTML 文本）。
    await ui.fill(page, {
      summary: '等待期间补写的摘要',
      body: '等待期间的正文\n\n空行保留\n\n中文 English café 日本語 العربية\n\n<html>原样文本</html>',
    });
    // 保存进行中重复点击不会产生第二次提交。
    await ui.clickSave(page);
    s = await ui.state(page);
    assert.equal(s.saveDisabled, true);
    assert.deepEqual(await page.eval(`__qtest.gateState()`), { enabled: true, hits: 1, pending: 1 });
    // 成功返回前列表仍是旧内容：不能提前显示尚未保存成功的东西。
    assert.equal(s.cards[0].title, '原标题');
    assert.equal(s.cards[0].summary, '原摘要');

    await ui.releaseSaves(page);
    await ui.gateOff(page);
    await ui.waitForStatus(page, '尚未保存', 'warn');
    s = await ui.state(page);

    // 明确指出哪些字段还有未保存修改（标题不在其列）。
    assert.match(s.statusText, /但摘要、正文在保存期间又被修改/);
    assert.ok(!s.statusText.includes('标题在保存期间'));
    // 表单保留等待期间输入的当前值，一个字符都不动。
    assert.equal(s.form.title, '修改后的标题'); // 本次提交的标题按规则去首尾空白
    assert.equal(s.form.summary, '等待期间补写的摘要');
    assert.equal(s.form.body,
      '等待期间的正文\n\n空行保留\n\n中文 English café 日本語 العربية\n\n<html>原样文本</html>');
    // 仍编辑同一篇草稿。
    assert.equal(s.formEditing, true);
    assert.equal(s.bannerHidden, false);
    assert.equal(s.bannerId, a.id);
    assert.equal(s.bannerTitle, '修改后的标题');
    assert.equal(s.saveBtnText, '保存修改');
    assert.equal(s.saveDisabled, false);
    // 列表反映的是本次实际提交并保存的标题与摘要，不是等待期间的新输入。
    assert.equal(s.listCount, 1);
    assert.equal(s.cards[0].title, '修改后的标题');
    assert.equal(s.cards[0].summary, '实际提交的摘要 <b>x</b>');
    assert.equal(s.cards[0].editing, true);
    // 类 HTML 的摘要按文本渲染，不执行任何标记。
    assert.equal(await page.eval(`document.querySelectorAll('#article-list .summary').length`), 1);
    assert.equal(await page.eval(`document.querySelector('#article-list .summary b')`), null);

    // 服务端状态：同一篇草稿，版本推进，标识/创建时间/状态不变，存的是实际提交内容。
    let saved = await (await server.api(`/api/articles/${a.id}`)).json();
    assert.equal(saved.article.id, a.id);
    assert.equal(saved.article.createdAt, a.createdAt);
    assert.equal(saved.article.status, 'draft');
    assert.equal(saved.article.version, 2);
    assert.equal(saved.article.title, '修改后的标题');
    assert.equal(saved.article.summary, '实际提交的摘要 <b>x</b>');
    assert.equal(saved.article.body, '实际提交的正文第一段\n\n实际提交的正文第二段');

    // 再次点击“保存修改”：刚才保留的内容才成为已保存内容，且不能误报冲突。
    await ui.clickSave(page);
    await ui.waitForStatus(page, '修改已保存', 'ok');
    s = await ui.state(page);
    assert.equal(s.conflictHidden, true);
    assert.equal(s.cards[0].summary, '等待期间补写的摘要');
    saved = await (await server.api(`/api/articles/${a.id}`)).json();
    assert.equal(saved.article.version, 3);
    assert.equal(saved.article.id, a.id);
    assert.equal(saved.article.createdAt, a.createdAt);
    assert.equal(saved.article.body,
      '等待期间的正文\n\n空行保留\n\n中文 English café 日本語 العربية\n\n<html>原样文本</html>');
    assert.equal(s.listCount, 1);

    // 再次打开草稿：完整正文（含空行与类 HTML 文本）以单篇接口返回为准被载入。
    await ui.clickCancel(page);
    await ui.clickEdit(page, '修改后的标题');
    await ui.waitLoadedDraft(page);
    s = await ui.state(page);
    assert.equal(s.form.body, saved.article.body);
    assert.equal(s.form.summary, '等待期间补写的摘要');
    assert.equal(s.bannerId, a.id);

    // 重开后基于最新版本保存，仍应成功（版本基准来自单篇接口，不报 409）。
    await ui.fill(page, { body: `${saved.article.body}\n重开后再补充一行` });
    await ui.clickSave(page);
    await ui.waitForStatus(page, '修改已保存', 'ok');
    saved = await (await server.api(`/api/articles/${a.id}`)).json();
    assert.equal(saved.article.version, 4);
    assert.equal(saved.article.body,
      '等待期间的正文\n\n空行保留\n\n中文 English café 日本語 العربية\n\n<html>原样文本</html>\n重开后再补充一行');
    assert.equal((await server.listArticles()).length, 1);
  } finally {
    await closeContext(ctx, browser);
  }
});

// ---------------------------------------------------------------------------
// 2. During the wait the user may clear summary/body entirely.
// ---------------------------------------------------------------------------
test('编辑态等待期间清空摘要和正文：清空被保留并明确提示，再次保存真的清空', async (browser) => {
  const ctx = await freshContext(browser);
  const { server, page } = ctx;
  try {
    const a = await seedAndReload(server, page, { title: '清空测试', summary: '原有摘要', body: '原有正文' });
    await ui.clickEdit(page, '清空测试');
    await ui.waitLoadedDraft(page);

    await ui.fill(page, { title: '清空测试-改', summary: '提交的摘要', body: '提交的正文' });
    await ui.gateOn(page);
    await ui.clickSave(page);
    await ui.waitSavingPending(page, 1);
    await ui.fill(page, { summary: '', body: '' });
    await ui.releaseSaves(page);
    await ui.gateOff(page);

    await ui.waitForStatus(page, '尚未保存', 'warn');
    let s = await ui.state(page);
    assert.match(s.statusText, /但摘要、正文在保存期间又被修改/);
    assert.deepEqual(s.form, { title: '清空测试-改', summary: '', body: '' });
    assert.equal(s.cards[0].title, '清空测试-改');
    assert.equal(s.cards[0].summary, '提交的摘要'); // 列表只显示已保存内容
    assert.equal(s.bannerId, a.id);

    await ui.clickSave(page);
    await ui.waitForStatus(page, '修改已保存', 'ok');
    const saved = await (await server.api(`/api/articles/${a.id}`)).json();
    assert.equal(saved.article.summary, '');
    assert.equal(saved.article.body, '');
    assert.equal(saved.article.version, 3);
    s = await ui.state(page);
    assert.equal(s.cards[0].summary, null); // 空摘要不渲染摘要段
    assert.equal(s.listCount, 1);
  } finally {
    await closeContext(ctx, browser);
  }
});

// ---------------------------------------------------------------------------
// 2b. Any field may keep changing during the wait — including the title. A
//     kept title change must be named as unsaved (raw value with surrounding
//     spaces retained); the second save trims and persists it.
// ---------------------------------------------------------------------------
test('编辑态等待期间连标题也改：三个未保存字段都点名，保留原值，再次保存标题按规则去空白', async (browser) => {
  const ctx = await freshContext(browser);
  const { server, page } = ctx;
  try {
    const a = await seedAndReload(server, page, { title: '旧标题', summary: '旧摘要', body: '旧正文' });
    await ui.clickEdit(page, '旧标题');
    await ui.waitLoadedDraft(page);

    await ui.fill(page, { title: '提交标题', summary: '提交摘要', body: '提交正文' });
    await ui.gateOn(page);
    await ui.clickSave(page);
    await ui.waitSavingPending(page, 1);
    await ui.fill(page, {
      title: '  等待期间的新标题  ',
      summary: '等待期间的新摘要',
      body: '等待期间的新正文',
    });
    await ui.releaseSaves(page);
    await ui.gateOff(page);

    await ui.waitForStatus(page, '尚未保存', 'warn');
    let s = await ui.state(page);
    // 明确点名所有仍有未保存修改的字段，顺序固定为标题、摘要、正文。
    assert.match(s.statusText, /但标题、摘要、正文在保存期间又被修改/);
    // 表单保留等待期间输入的当前值，连首尾空白都原样保留。
    assert.deepEqual(s.form, {
      title: '  等待期间的新标题  ',
      summary: '等待期间的新摘要',
      body: '等待期间的新正文',
    });
    // 列表只显示本次实际保存的内容。
    assert.equal(s.cards[0].title, '提交标题');
    assert.equal(s.cards[0].summary, '提交摘要');
    assert.equal(s.bannerId, a.id);
    assert.equal(s.bannerTitle, '提交标题');

    await ui.clickSave(page);
    await ui.waitForStatus(page, '修改已保存', 'ok');
    const saved = await (await server.api(`/api/articles/${a.id}`)).json();
    assert.equal(saved.article.title, '等待期间的新标题'); // 第二次保存才去空白落库
    assert.equal(saved.article.summary, '等待期间的新摘要');
    assert.equal(saved.article.body, '等待期间的新正文');
    assert.equal(saved.article.id, a.id);
    assert.equal(saved.article.createdAt, a.createdAt);
    assert.equal(saved.article.version, 3);
    s = await ui.state(page);
    assert.equal(s.cards[0].title, '等待期间的新标题');
    assert.equal(s.listCount, 1);
    assert.equal(s.form.title, '等待期间的新标题'); // 保存后表单同步为规范值
  } finally {
    await closeContext(ctx, browser);
  }
});

// ---------------------------------------------------------------------------
// 3. Unsaved detection uses final field values vs. what was actually saved:
//    edits reverted during the wait, and a title that only differs by the
//    server's end trimming, must not be reported as unsaved.
// ---------------------------------------------------------------------------
test('等待期间改动后恢复原值/标题等价于服务端规范值：不误报未保存', async (browser) => {
  const ctx = await freshContext(browser);
  const { server, page } = ctx;
  try {
    await seedAndReload(server, page, { title: '基准标题', summary: '基准摘要', body: '基准正文' });
    await ui.clickEdit(page, '基准标题');
    await ui.waitLoadedDraft(page);

    await ui.fill(page, { title: '  规范标题  ', summary: '提交摘要', body: '提交正文' });
    await ui.gateOn(page);
    await ui.clickSave(page);
    await ui.waitSavingPending(page, 1);
    // 标题先改成别的、再改成与服务端保存后相同的文字（去空白后的“规范标题”）。
    await ui.fill(page, { title: '暂时的标题', body: '暂时的正文' });
    await ui.fill(page, { title: '规范标题', body: '提交正文' }); // 正文改回提交值
    await ui.releaseSaves(page);
    await ui.gateOff(page);

    await ui.waitForStatus(page, '修改已保存', 'ok');
    const s = await ui.state(page);
    assert.equal(s.statusKind, 'ok');
    assert.ok(!s.statusText.includes('尚未保存'));
    assert.deepEqual(s.form, { title: '规范标题', summary: '提交摘要', body: '提交正文' });
    assert.equal(s.bannerTitle, '规范标题');
    assert.equal(s.listCount, 1);
    assert.equal(s.cards[0].title, '规范标题');
    const articles = await server.listArticles();
    assert.equal(articles[0].version, 2);
  } finally {
    await closeContext(ctx, browser);
  }
});

// ---------------------------------------------------------------------------
// 4. New draft: type more during the wait -> one draft only, form associates
//    with the created article in editing mode; later saves update it.
// ---------------------------------------------------------------------------
test('新建草稿等待期间补写：只产生一篇、自动关联进入编辑态、后续更新同一篇', async (browser) => {
  const ctx = await freshContext(browser);
  const { server, page } = ctx;
  try {
    await ui.fill(page, {
      title: '  新草稿标题  ',
      summary: '首版摘要',
      body: '首版正文',
    });
    await ui.gateOn(page);
    await ui.clickSave(page);
    await ui.waitSavingPending(page, 1);
    await ui.fill(page, {
      body: '首版正文\n\n等待期间补写：中文 English café\n\n<html>按文本处理</html>',
    });
    await ui.releaseSaves(page);
    await ui.gateOff(page);

    await ui.waitForStatus(page, '尚未保存', 'warn');
    let s = await ui.state(page);
    assert.match(s.statusText, /但正文在保存期间又被修改/);
    assert.equal(s.listCount, 1);
    assert.equal(s.cards[0].title, '新草稿标题');
    assert.equal(s.cards[0].summary, '首版摘要');
    assert.equal(s.cards[0].editing, true);
    // 表单关联到刚创建的文章并进入编辑状态。
    assert.equal(s.formEditing, true);
    assert.equal(s.saveBtnText, '保存修改');
    assert.equal(s.cancelHidden, false);
    assert.ok(s.bannerId);
    assert.equal(s.form.body, '首版正文\n\n等待期间补写：中文 English café\n\n<html>按文本处理</html>');
    assert.equal(s.form.title, '新草稿标题');

    const articles1 = await server.listArticles();
    assert.equal(articles1.length, 1);
    assert.equal(articles1[0].version, 1);
    assert.equal(articles1[0].title, '新草稿标题');
    assert.equal(articles1[0].body, '首版正文');
    const createdId = articles1[0].id;
    assert.equal(s.bannerId, createdId);

    // 后续保存更新这篇文章，而不是再新建一篇。
    await ui.clickSave(page);
    await ui.waitForStatus(page, '修改已保存', 'ok');
    s = await ui.state(page);
    assert.equal(s.bannerId, createdId);
    const articles2 = await server.listArticles();
    assert.equal(articles2.length, 1);
    assert.equal(articles2[0].id, createdId);
    assert.equal(articles2[0].version, 2);
    assert.equal(articles2[0].body, '首版正文\n\n等待期间补写：中文 English café\n\n<html>按文本处理</html>');
    assert.equal(s.listCount, 1);

    // 取消后再打开，仍是这一篇且内容完整。
    await ui.clickCancel(page);
    await ui.clickEdit(page, '新草稿标题');
    await ui.waitLoadedDraft(page);
    s = await ui.state(page);
    assert.equal(s.bannerId, createdId);
    assert.equal(s.form.body, articles2[0].body);
  } finally {
    await closeContext(ctx, browser);
  }
});

// ---------------------------------------------------------------------------
// 5. New draft with no further input (including edits reverted during wait):
//    form clears and stays in create mode; creating again adds a second draft.
// ---------------------------------------------------------------------------
test('新建无后续输入（含等待期间改回原值）：表单清空留在新建态，可继续新建', async (browser) => {
  const ctx = await freshContext(browser);
  const { server, page } = ctx;
  try {
    await ui.fill(page, { title: '  第一篇  ', summary: '摘要一', body: '正文一' });
    await ui.gateOn(page);
    await ui.clickSave(page);
    await ui.waitSavingPending(page, 1);
    // 改过又恢复：与提交快照一致，不算未保存。
    await ui.fill(page, { summary: '临时', body: '临时正文' });
    await ui.fill(page, { summary: '摘要一', body: '正文一' });
    await ui.releaseSaves(page);
    await ui.gateOff(page);

    await ui.waitForStatus(page, '草稿已保存', 'ok');
    let s = await ui.state(page);
    assert.deepEqual(s.form, { title: '', summary: '', body: '' });
    assert.equal(s.saveBtnText, '保存草稿');
    assert.equal(s.bannerHidden, true);
    assert.equal(s.cancelHidden, true);
    assert.equal(s.formEditing, false);
    assert.equal(s.listCount, 1);
    assert.equal(s.cards[0].editing, false);

    await ui.fill(page, { title: '第二篇', body: '正文二' });
    await ui.clickSave(page);
    await ui.waitForStatus(page, '草稿已保存', 'ok');
    s = await ui.state(page);
    assert.equal(s.listCount, 2);
    // 列表按创建时间倒序；用服务端数据按同一规则计算期望顺序。
    const apiArticles = await server.listArticles();
    const expectedOrder = apiArticles
      .slice()
      .sort((x, y) => (x.createdAt < y.createdAt ? 1 : x.createdAt > y.createdAt ? -1 : 0))
      .map((art) => art.title);
    assert.deepEqual(s.cards.map((card) => card.title), expectedOrder);
    assert.deepEqual(expectedOrder.slice().sort(), ['第一篇', '第二篇']);
    assert.deepEqual(s.form, { title: '', summary: '', body: '' });
    assert.equal(apiArticles.length, 2);
  } finally {
    await closeContext(ctx, browser);
  }
});

// ---------------------------------------------------------------------------
// 6. Server explicitly rejects while editing (oversize body): every input
//    present when the response arrives is kept, mode and target unchanged,
//    failure reason shown, saving becomes available again, no success/record.
// ---------------------------------------------------------------------------
test('编辑态保存被服务端拒绝：保留响应到达时的全部输入与编辑状态，恢复后可再保存', async (browser) => {
  const ctx = await freshContext(browser);
  const { server, page } = ctx;
  try {
    const a = await seedAndReload(server, page, { title: '拒绝测试', summary: '原摘要', body: '原正文' });
    await ui.clickEdit(page, '拒绝测试');
    await ui.waitLoadedDraft(page);

    const hugeBody = 'x'.repeat(5 * 1024 * 1024 + 64); // 超过 5MiB 请求体上限
    await ui.fill(page, { title: '尝试保存的标题', summary: '尝试保存的摘要', body: hugeBody });
    await ui.gateOn(page);
    await ui.clickSave(page);
    await ui.waitSavingPending(page, 1);
    // 等待响应期间继续改动；响应到达时这些当前值必须原样保留。
    await ui.fill(page, { title: '响应到达时的标题', summary: '响应到达时的摘要' });
    await ui.releaseSaves(page);
    await ui.gateOff(page);

    await ui.waitForStatus(page, '失败', 'err');
    const s = await ui.state(page);
    assert.ok(s.statusText.includes('保存修改失败'));
    assert.ok(s.statusText.length > '保存修改失败：'.length, '必须显示非空失败原因');
    assert.equal(s.saveBtnText, '保存修改');
    assert.equal(s.saveDisabled, false);
    assert.equal(s.formEditing, true);
    assert.equal(s.bannerId, a.id);
    assert.equal(s.form.title, '响应到达时的标题');
    assert.equal(s.form.summary, '响应到达时的摘要');
    assert.equal(s.form.body.length, hugeBody.length);
    assert.equal(s.conflictHidden, true);
    // 不显示成功、列表不变、不新增记录、原文章不动。
    assert.equal(s.statusKind, 'err');
    assert.equal(s.cards[0].title, '拒绝测试');
    assert.equal(s.listCount, 1);
    const stored = await (await server.api(`/api/articles/${a.id}`)).json();
    assert.equal(stored.article.title, '拒绝测试');
    assert.equal(stored.article.version, 1);

    // 恢复可保存状态：改小后再次保存成功，更新的还是同一篇。
    await ui.fill(page, { body: '修正后的正文' });
    await ui.clickSave(page);
    await ui.waitForStatus(page, '修改已保存', 'ok');
    const after = await (await server.api(`/api/articles/${a.id}`)).json();
    assert.equal(after.article.id, a.id);
    assert.equal(after.article.title, '响应到达时的标题');
    assert.equal(after.article.summary, '响应到达时的摘要');
    assert.equal(after.article.body, '修正后的正文');
    assert.equal(after.article.version, 2);
    assert.equal((await server.listArticles()).length, 1);
  } finally {
    await closeContext(ctx, browser);
  }
});

// ---------------------------------------------------------------------------
// 7. Same rejection while creating: stay in create mode, keep all input, add
//    no record; fixing it then creates exactly one draft.
// ---------------------------------------------------------------------------
test('新建态保存被服务端拒绝：保留新建状态与全部输入，修正后只创建一篇', async (browser) => {
  const ctx = await freshContext(browser);
  const { server, page } = ctx;
  try {
    const hugeBody = 'y'.repeat(5 * 1024 * 1024 + 64);
    await ui.fill(page, { title: '新建被拒', summary: '摘要', body: hugeBody });
    await ui.gateOn(page);
    await ui.clickSave(page);
    await ui.waitSavingPending(page, 1);
    await ui.fill(page, { title: '新建被拒-响应时标题' });
    await ui.releaseSaves(page);
    await ui.gateOff(page);

    await ui.waitForStatus(page, '失败', 'err');
    const s = await ui.state(page);
    assert.ok(s.statusText.includes('保存失败'));
    assert.equal(s.saveBtnText, '保存草稿');
    assert.equal(s.bannerHidden, true);
    assert.equal(s.formEditing, false);
    assert.equal(s.cancelHidden, true);
    assert.equal(s.form.title, '新建被拒-响应时标题');
    assert.equal(s.form.body.length, hugeBody.length);
    assert.equal(s.saveDisabled, false);
    assert.equal(s.listCount, 0);
    assert.deepEqual(await server.listArticles(), []);

    await ui.fill(page, { body: '正常正文' });
    await ui.clickSave(page);
    await ui.waitForStatus(page, '草稿已保存', 'ok');
    const articles = await server.listArticles();
    assert.equal(articles.length, 1);
    assert.equal(articles[0].title, '新建被拒-响应时标题');
    assert.equal(articles[0].body, '正常正文');
    const now = await ui.state(page);
    assert.deepEqual(now.form, { title: '', summary: '', body: '' }); // 无后续输入→清空
    assert.equal(now.saveBtnText, '保存草稿');
  } finally {
    await closeContext(ctx, browser);
  }
});

// ---------------------------------------------------------------------------
// 8. Two real pages open the same draft. Page A saves first; page B, still on
//    the old version, saves and must receive 409, keep every input and see a
//    side-by-side comparison. After the comparison is shown page A saves AGAIN;
//    page B's “载入最新内容” must fetch that newer version (not reuse the stale
//    comparison), then a further save updates the same draft with no conflict.
// ---------------------------------------------------------------------------
test('两个页面先后保存：旧版本收到409并保留输入对照；对照后对方再存，载入取本次读取内容并可继续更新同一篇', async (browser) => {
  const server = await TestServer.start();
  const pageA = await ui.open(browser, server.url);
  const pageB = await ui.open(browser, server.url);
  try {
    const v1 = await seed(server, { title: '共享草稿', summary: '初始摘要', body: '初始正文' });
    await ui.reload(pageA);
    await ui.reload(pageB);

    // 两个页面都在任何保存之前打开同一篇草稿，版本基准都是 1。
    await ui.clickEdit(pageA, '共享草稿');
    await ui.waitLoadedDraft(pageA);
    await ui.clickEdit(pageB, '共享草稿');
    await ui.waitLoadedDraft(pageB);

    // 页面一先保存，成功推进到版本 2。
    await ui.fill(pageA, {
      title: '页面一标题',
      summary: '页面一摘要 <b>不应加粗</b>',
      body: '页面一正文第一段\n\n页面一正文第二段 <img src=x>',
    });
    await ui.clickSave(pageA);
    await ui.waitForStatus(pageA, '修改已保存', 'ok');

    // 页面二基于旧版本 1 提交自己的内容：必须收到 409。
    const mine = {
      title: '页面二标题',
      summary: '页面二摘要',
      body: '页面二正文<script>alert("x")</script>',
    };
    await ui.fill(pageB, mine);
    await ui.clickSave(pageB);
    await ui.waitForStatus(pageB, '409', 'err');
    await ui.waitConflictShown(pageB);

    let s = await ui.state(pageB);
    // 明确提示有更新的已保存版本，本次保存被拒绝；全部输入原样保留。
    assert.match(s.statusText, /存在较新的已保存版本（409）/);
    assert.deepEqual(s.form, mine);
    assert.equal(s.formEditing, true);
    assert.equal(s.bannerId, v1.id);
    assert.equal(s.saveBtnText, '保存修改');
    assert.equal(s.saveDisabled, false);
    // 并排展示双方标题、摘要、正文：左为未保存输入，右为最新已保存内容。
    const conflict = await ui.getConflict(pageB);
    assert.equal(conflict.hidden, false);
    assert.equal(conflict.heading, '检测到较新的已保存内容');
    assert.equal(conflict.mine.heading, '你正在编辑的内容（未保存）');
    assert.deepEqual(
      { title: conflict.mine.title, summary: conflict.mine.summary, body: conflict.mine.body }, mine);
    assert.deepEqual(
      { title: conflict.saved.title, summary: conflict.saved.summary, body: conflict.saved.body },
      {
        title: '页面一标题',
        summary: '页面一摘要 <b>不应加粗</b>',
        body: '页面一正文第一段\n\n页面一正文第二段 <img src=x>',
      });
    // 对照中的类 HTML 文本按纯文本呈现，不产生任何元素、不执行标记。
    await assertConflictPlainText(pageB);
    // 本地版本基准没有被 409 推进：再点一次保存仍是 409（而不是误判成功）。
    await ui.clickSave(pageB);
    await ui.waitForStatus(pageB, '409', 'err');
    await ui.waitConflictShown(pageB);
    let stored = await getArticle(server, v1.id);
    assert.equal(stored.version, 2);
    assert.equal((await server.listArticles()).length, 1);

    // 对照出现之后，页面一又保存了新版本（版本 3，含多语言/空行/类 HTML 正文）。
    const v3 = {
      title: '第三次标题',
      summary: '三次摘要 <img src=x onerror=alert(3)>',
      body: RICH_BODY,
    };
    await ui.fill(pageA, v3);
    await ui.clickSave(pageA);
    await ui.waitForStatus(pageA, '修改已保存', 'ok');
    stored = await getArticle(server, v1.id);
    assert.equal(stored.version, 3);

    // 页面二点击“放弃当前输入并载入最新内容”：读取等待期间没有新修改，直接载入。
    // 得到的必须是本次读取时实际保存的版本 3，而不是先前 409 对照里的版本 2。
    await ui.clickDiscardLoad(pageB);
    await ui.waitLoadedDraft(pageB);
    s = await ui.state(pageB);
    assert.deepEqual(s.form, { title: v3.title, summary: v3.summary, body: v3.body });
    assert.equal(s.bannerId, v1.id);
    assert.equal(s.conflictHidden, true);
    assert.equal(s.cards[0].title, v3.title);
    assert.equal(s.cards[0].summary, v3.summary);
    assert.equal(s.cards[0].editing, true);
    assert.equal(
      await pageB.eval(`document.querySelectorAll('#article-list .summary b,#article-list .summary img,#article-list .summary script').length`),
      0);
    stored = await getArticle(server, v1.id);
    assert.equal(stored.version, 3); // 仅读取，不改变版本
    assert.equal(stored.body, RICH_BODY);
    assert.equal(stored.id, v1.id);
    assert.equal(stored.status, 'draft');
    assert.equal(stored.createdAt, v1.createdAt);

    // 载入后仍编辑同一篇草稿；在此基础上修改并保存，更新原文章且不误报冲突。
    await ui.fill(pageB, {
      title: '页面二接管后的标题',
      summary: '页面二接管摘要',
      body: `${RICH_BODY}\n\n接管后补充一行`,
    });
    await ui.clickSave(pageB);
    await ui.waitForStatus(pageB, '修改已保存', 'ok');
    s = await ui.state(pageB);
    assert.equal(s.conflictHidden, true);
    assert.equal(s.cards[0].title, '页面二接管后的标题');
    assert.equal(s.bannerId, v1.id);
    stored = await getArticle(server, v1.id);
    assert.equal(stored.version, 4);
    assert.equal(stored.id, v1.id);
    assert.equal(stored.createdAt, v1.createdAt);
    assert.equal(stored.body, `${RICH_BODY}\n\n接管后补充一行`);
    const articles = await server.listArticles();
    assert.equal(articles.length, 1);
    assert.equal(articles[0].id, v1.id);

    ui.assertNoPageErrors(pageA);
  } finally {
    ui.assertNoPageErrors(pageB);
    await browser.closePage(pageB);
    await browser.closePage(pageA);
    await server.stop();
  }
});

// ---------------------------------------------------------------------------
// 9. While the post-conflict reload read is in flight: form is not cleared
//    early, fields stay editable, page shows “正在读取”, save and a repeat
//    reload are unavailable. Fields changed then reverted count as unchanged,
//    so the freshly read article loads directly — and it is the version saved
//    AFTER the comparison, never the stale comparison content.
// ---------------------------------------------------------------------------
test('载入等待期间不丢输入且可编辑、保存与重复载入不可用；改动后恢复原值则直接载入本次读取的新版本', async (browser) => {
  const ctx = await freshContext(browser);
  const { server, page } = ctx;
  try {
    const v1 = { title: '载入基准', summary: '基准摘要', body: '基准正文' };
    const mine = { title: '我改的标题', summary: '我改的摘要', body: '我改的正文' };
    const { article } = await stageStaleConflict(server, page, v1, mine, {
      title: '对方第二版', summary: '对方摘要二', body: '对方正文二',
    });

    // 对照出现后，对方又保存了第三版；读取请求先挂起，期间对方内容才落库。
    const v3 = { title: '对方第三版', summary: '对方摘要三', body: '对方正文三' };
    await ui.readGateOn(page);
    await ui.clickDiscardLoad(page);
    await ui.waitReadPending(page, 1);

    let s = await ui.state(page);
    // 不提前清空：开始读取时的输入一字不动。
    assert.deepEqual(s.form, mine);
    assert.deepEqual(s.fieldsEditable, { title: true, summary: true, body: true });
    assert.equal(s.saveBtnText, '读取中…');
    assert.equal(s.saveDisabled, true);
    assert.match(s.statusText, /正在读取/);
    // 保存与重复载入在读取期间都不可用。
    await ui.gateOn(page);
    await ui.clickSave(page); // 禁用按钮：不会产生保存请求
    assert.deepEqual(await ui.gateState(page), { enabled: true, hits: 0, pending: 0 });
    const actionsDuring = (await ui.getConflict(page)).actions;
    assert.deepEqual(actionsDuring.map((b) => b.disabled), [true, true]);
    await ui.clickDiscardLoad(page); // 重复点击不会发起第二次读取
    assert.deepEqual(await ui.readGateState(page), { enabled: true, hits: 1, pending: 1 });

    // 读取期间三个字段都改过，随后又逐一恢复为开始读取时的原值。
    await ui.fill(page, { title: '临时标题', summary: '临时摘要', body: '临时正文' });
    await ui.fill(page, mine);
    s = await ui.state(page);
    assert.deepEqual(s.form, mine);

    // 挂起期间对方第三版落库；放行读取，返回的应当是版本 3。
    const up = await updateOut(server, article.id, v3, 2);
    assert.equal(up.status, 200);
    await ui.releaseReads(page);
    await ui.readGateOff(page);
    await ui.gateOff(page);
    await ui.waitLoadedDraft(page);

    s = await ui.state(page);
    // 改动后恢复原值视为未修改：直接载入，不弹确认。
    assert.deepEqual((await ui.confirmState(page)).calls, []);
    assert.deepEqual(s.form, v3); // 不是 409 对照里的第二版
    assert.equal(s.bannerId, article.id);
    assert.equal(s.cards[0].title, v3.title);
    assert.equal(s.cards[0].editing, true);
    // 读取本身不保存、不推进版本。
    const stored = await getArticle(server, article.id);
    assert.equal(stored.version, 3);
    assert.deepEqual({ title: stored.title, summary: stored.summary, body: stored.body }, v3);
    assert.equal((await server.listArticles()).length, 1);
  } finally {
    await closeContext(ctx, browser);
  }
});

// ---------------------------------------------------------------------------
// 10. During the reload read a field still has a new edit when the response
//     arrives: the page must first explain that loading discards those edits.
//     Choosing “保留” keeps every input, the original editing target and the
//     original version baseline; nothing is saved and no version changes.
// ---------------------------------------------------------------------------
test('读取返回时仍有新修改：说明会放弃输入；选择保留则输入/编辑对象/版本基准全部不变且不保存', async (browser) => {
  const ctx = await freshContext(browser);
  const { server, page } = ctx;
  try {
    const { article } = await stageStaleConflict(server, page,
      { title: '保留基准', summary: '基准摘要', body: '基准正文' },
      { title: '冲突时标题', summary: '冲突时摘要', body: '冲突时正文' },
      { title: '对方第二版', summary: '对方摘要二', body: '对方正文二' });

    await ui.setConfirm(page, 'cancel'); // 用户选择保留当前输入
    await ui.readGateOn(page);
    await ui.resetConfirm(page);
    await ui.clickDiscardLoad(page);
    await ui.waitReadPending(page, 1);
    const kept = {
      title: '读取期间又改的标题',
      summary: '读取期间又改的摘要 <script>alert(1)</script>',
      body: '读取期间又改的正文\n\n第二段',
    };
    await ui.fill(page, kept);
    await ui.releaseReads(page);
    await ui.readGateOff(page);
    await ui.waitForStatus(page, '已保留当前输入，未载入最新内容');

    const confirm = await ui.confirmState(page);
    assert.equal(confirm.calls.length, 1);
    assert.match(confirm.calls[0], /载入最新内容将放弃这些新修改/);
    let s = await ui.state(page);
    // 当前全部输入原样保留（含类 HTML 文本，按纯文本留在表单中）。
    assert.deepEqual(s.form, kept);
    // 原编辑对象不变。
    assert.equal(s.formEditing, true);
    assert.equal(s.bannerId, article.id);
    assert.equal(s.bannerHidden, false);
    assert.equal(s.saveBtnText, '保存修改');
    assert.equal(s.saveDisabled, false);
    // 仍停留在冲突处理界面，按钮恢复可用，没有显示载入成功。
    assert.equal(s.conflictHidden, false);
    assert.deepEqual((await ui.getConflict(page)).actions.map((b) => b.disabled), [false, false]);
    assert.ok(!s.statusText.includes('已载入'));
    // 读取/保留都不保存：文章仍是对方保存的第二版，记录数不变。
    let stored = await getArticle(server, article.id);
    assert.equal(stored.version, 2);
    assert.equal((await server.listArticles()).length, 1);
    // 类 HTML 文本没有被解析或执行。
    assert.equal(await page.eval(`document.querySelectorAll('#conflict-box script,#conflict-box img').length`), 0);

    // 版本基准保持旧值：再次保存仍应收到 409（而不是沿用载入内容误判成功）。
    await ui.clickSave(page);
    await ui.waitForStatus(page, '409', 'err');
    await ui.waitConflictShown(page);
    s = await ui.state(page);
    assert.deepEqual(s.form, kept);
    stored = await getArticle(server, article.id);
    assert.equal(stored.version, 2);
    assert.equal((await server.listArticles()).length, 1);
  } finally {
    await closeContext(ctx, browser);
  }
});

// ---------------------------------------------------------------------------
// 11. Same in-flight edit, but the user confirms “放弃”: editing continues
//     with this read's complete content; a follow-up save updates the original
//     draft on the new baseline with no false conflict.
// ---------------------------------------------------------------------------
test('读取返回时有新修改：选择放弃则以本次读取的完整内容继续编辑，再保存更新同一篇且不误报冲突', async (browser) => {
  const ctx = await freshContext(browser);
  const { server, page } = ctx;
  try {
    const { article } = await stageStaleConflict(server, page,
      { title: '放弃基准', summary: '基准摘要', body: '基准正文' },
      { title: '冲突时标题', summary: '冲突时摘要', body: '冲突时正文' },
      { title: '对方第二版', summary: '对方摘要二', body: '对方正文二' });

    // 对方在对照出现后又保存第三版。
    const v3 = { title: '对方第三版', summary: '对方摘要三', body: `对方正文三\n\n空行\n中文 English` };
    const up = await updateOut(server, article.id, v3, 2);
    assert.equal(up.status, 200);

    await ui.setConfirm(page, 'accept'); // 用户选择放弃等待期间的输入
    await ui.readGateOn(page);
    await ui.resetConfirm(page);
    await ui.clickDiscardLoad(page);
    await ui.waitReadPending(page, 1);
    await ui.fill(page, { title: '读取期间新改', summary: '读取期间新摘要', body: '读取期间新正文' });
    await ui.releaseReads(page);
    await ui.readGateOff(page);
    await ui.waitLoadedDraft(page);

    const confirm = await ui.confirmState(page);
    assert.equal(confirm.calls.length, 1);
    assert.match(confirm.calls[0], /放弃新修改并载入最新内容/);
    let s = await ui.state(page);
    // 等待期间的新修改被放弃，以本次读取到的第三版完整内容继续编辑。
    assert.deepEqual(s.form, v3);
    assert.equal(s.bannerId, article.id);
    assert.equal(s.conflictHidden, true);
    assert.equal(s.cards[0].title, v3.title);
    assert.equal(s.cards[0].summary, v3.summary);
    assert.equal(s.cards[0].editing, true);
    // 仅完成读取与放弃选择，尚未保存：版本仍是 3。
    assert.equal((await getArticle(server, article.id)).version, 3);

    // 在载入内容基础上修改保存：按新版本基准更新同一篇，不能误报冲突。
    await ui.fill(page, { title: '放弃后再编辑标题', body: `${v3.body}\n再补充一段` });
    await ui.clickSave(page);
    await ui.waitForStatus(page, '修改已保存', 'ok');
    s = await ui.state(page);
    assert.equal(s.conflictHidden, true);
    assert.equal(s.bannerId, article.id);
    const stored = await getArticle(server, article.id);
    assert.equal(stored.version, 4);
    assert.equal(stored.id, article.id);
    assert.equal(stored.title, '放弃后再编辑标题');
    assert.equal(stored.body, `${v3.body}\n再补充一段`);
    assert.equal((await server.listArticles()).length, 1);
  } finally {
    await closeContext(ctx, browser);
  }
});

// ---------------------------------------------------------------------------
// 12. Clearing the summary or body during the read is itself an edit that must
//     be protected: confirmation is required, “保留” keeps the emptied fields;
//     a later clean reload restores the saved content.
// ---------------------------------------------------------------------------
test('读取等待期间清空摘要或正文：清空属于受保护修改，保留时空字段不丢，重新读取可恢复', async (browser) => {
  const ctx = await freshContext(browser);
  const { server, page } = ctx;
  try {
    const { article } = await stageStaleConflict(server, page,
      { title: '清空保护基准', summary: '原摘要非空', body: '原正文非空' },
      { title: '冲突时标题', summary: '冲突时摘要非空', body: '冲突时正文非空' },
      { title: '对方第二版', summary: '对方摘要二非空', body: '对方正文二非空' });

    await ui.setConfirm(page, 'cancel');
    await ui.readGateOn(page);
    await ui.resetConfirm(page);
    await ui.clickDiscardLoad(page);
    await ui.waitReadPending(page, 1);
    // 等待期间把摘要和正文都清空。
    await ui.fill(page, { summary: '', body: '' });
    await ui.releaseReads(page);
    await ui.readGateOff(page);
    await ui.waitForStatus(page, '已保留当前输入，未载入最新内容');

    // 清空操作同样触发了“载入将放弃修改”的说明。
    assert.equal((await ui.confirmState(page)).calls.length, 1);
    let s = await ui.state(page);
    assert.deepEqual(s.form, { title: '冲突时标题', summary: '', body: '' });
    assert.equal(s.bannerId, article.id);
    assert.equal(s.formEditing, true);
    // 文章内容与版本不受影响。
    let stored = await getArticle(server, article.id);
    assert.equal(stored.version, 2);
    assert.notEqual(stored.summary, '');
    assert.notEqual(stored.body, '');

    // 不再改动，重新载入：直接恢复对方保存的完整内容（无需再确认）。
    const callsBefore = (await ui.confirmState(page)).calls.length;
    await ui.clickDiscardLoad(page);
    await ui.waitLoadedDraft(page);
    s = await ui.state(page);
    assert.deepEqual(s.form, { title: '对方第二版', summary: '对方摘要二非空', body: '对方正文二非空' });
    assert.equal((await ui.confirmState(page)).calls.length, callsBefore);
    stored = await getArticle(server, article.id);
    assert.equal(stored.version, 2); // 仍然只是读取
    assert.equal((await server.listArticles()).length, 1);
  } finally {
    await closeContext(ctx, browser);
  }
});

// ---------------------------------------------------------------------------
// 13. The reload read reports the draft is gone (404), fails server-side (500)
//     or fails on the network: show the concrete reason, keep the input present
//     when the response arrived and the original editing state, restore the
//     buttons, never claim success or fall back to create mode. Nothing is
//     saved; once reads work again, loading and then saving succeed.
// ---------------------------------------------------------------------------
test('载入读取返回404/500/网络失败：显示具体原因、保留响应到达时的输入与编辑状态、恢复按钮，不退回新建态', async (browser) => {
  const ctx = await freshContext(browser);
  const { server, page } = ctx;
  try {
    const mine = { title: '失败时标题', summary: '失败时摘要', body: '失败时正文' };
    const { article } = await stageStaleConflict(server, page,
      { title: '读失败基准', summary: '基准摘要', body: '基准正文' },
      mine,
      { title: '对方第二版', summary: '对方摘要二', body: '对方正文二' });

    // 404（草稿已不存在）：先挂起读取，等待期间继续输入，再让失败响应到达。
    await ui.readGateOn(page);
    await ui.armReadFailure(page, 404);
    await ui.clickDiscardLoad(page);
    await ui.waitReadPending(page, 1);
    let s = await ui.state(page);
    assert.match(s.statusText, /正在读取/);
    assert.deepEqual(s.fieldsEditable, { title: true, summary: true, body: true });
    await ui.fill(page, { title: `${mine.title} 响应到达前补写` });
    await ui.releaseReads(page);
    await ui.readGateOff(page);
    await ui.waitForStatus(page, '载入最新内容失败', 'err');
    s = await ui.state(page);
    assert.match(s.statusText, /该草稿已不存在/);
    assert.equal(s.statusKind, 'err');
    assert.deepEqual(s.form, { title: `${mine.title} 响应到达前补写`, summary: mine.summary, body: mine.body });
    assert.equal(s.formEditing, true);
    assert.equal(s.bannerHidden, false);
    assert.equal(s.bannerId, article.id);
    assert.equal(s.cancelHidden, false);
    assert.equal(s.saveBtnText, '保存修改');
    assert.equal(s.saveDisabled, false);
    assert.equal(s.conflictHidden, false);
    assert.deepEqual((await ui.getConflict(page)).actions.map((b) => b.disabled), [false, false]);
    assert.ok(!s.statusText.includes('已载入'));
    assert.equal(await ui.readFailureArmed(page), null);
    let stored = await getArticle(server, article.id);
    assert.equal(stored.version, 2);
    assert.equal((await server.listArticles()).length, 1);

    // 500：读取失败，同样保留全部输入与编辑状态。
    await ui.armReadFailure(page, 500);
    await ui.clickDiscardLoad(page);
    await ui.waitForStatus(page, '载入最新内容失败', 'err');
    s = await ui.state(page);
    assert.match(s.statusText, /单篇读取暂时不可用/);
    assert.equal(s.statusKind, 'err');
    assert.equal(s.bannerId, article.id);
    assert.equal(s.formEditing, true);
    assert.equal(s.saveDisabled, false);
    assert.deepEqual(s.form, { title: `${mine.title} 响应到达前补写`, summary: mine.summary, body: mine.body });
    stored = await getArticle(server, article.id);
    assert.equal(stored.version, 2);

    // 网络层失败：错误原因仍然具体可见。
    await ui.armReadFailure(page, 'network');
    await ui.clickDiscardLoad(page);
    await ui.waitForStatus(page, '载入最新内容失败', 'err');
    s = await ui.state(page);
    assert.match(s.statusText, /网络连接中断/);
    assert.equal(s.bannerId, article.id);
    assert.equal(s.formEditing, true);
    assert.equal(s.saveBtnText, '保存修改');
    assert.equal(s.saveDisabled, false);
    stored = await getArticle(server, article.id);
    assert.equal(stored.version, 2);
    assert.equal((await server.listArticles()).length, 1);

    // 读取恢复后可正常载入，且编辑状态/版本基准正确，随后能保存更新同一篇。
    await ui.clickDiscardLoad(page);
    await ui.waitLoadedDraft(page);
    s = await ui.state(page);
    assert.deepEqual(s.form, { title: '对方第二版', summary: '对方摘要二', body: '对方正文二' });
    assert.equal(s.bannerId, article.id);
    await ui.fill(page, { body: '恢复后保存的正文' });
    await ui.clickSave(page);
    await ui.waitForStatus(page, '修改已保存', 'ok');
    stored = await getArticle(server, article.id);
    assert.equal(stored.version, 3);
    assert.equal(stored.id, article.id);
    assert.equal(stored.body, '恢复后保存的正文');
    assert.equal((await server.listArticles()).length, 1);
  } finally {
    await closeContext(ctx, browser);
  }
});

// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// 14. First-load race, stale-snapshot variant: the initial GET /api/articles
//     was answered before the page saved (a genuinely stale response) and is
//     only delivered afterwards. The just-saved draft must stay, the drafts
//     brought back by the late response must also appear, nothing duplicates —
//     same title with a different id is still two records — and order stays
//     newest-first.
// ---------------------------------------------------------------------------
test('首次列表结果（保存前的旧快照）晚于保存成功到达：新草稿保留、其他草稿并入、不重复、不倒序', async (browser) => {
  const server = await TestServer.start();
  try {
    // 这两篇在页面打开前就已落库，会出现在被挂起的旧快照里。
    const seeded1 = await seed(server, { title: '同名草稿', summary: '服务端同名第一篇', body: 'b1' });
    const seeded2 = await seed(server, { title: '旧草稿', summary: '旧摘要', body: 'b2' });

    // captured：页面加载时列表请求已发出，服务端以保存前的数据应答，响应被扣在页面里。
    const page = await ui.openWithListHeld(browser, server.url, { hold: 'captured' });
    try {
      let s = await ui.state(page);
      assert.equal(s.listBusy, true);
      assert.equal(s.emptyTip, '草稿加载中…');
      assert.equal(s.listCount, 0);

      // 加载期间照常新建：标题与已有草稿相同，但标识不同，仍是两条记录。
      await ui.fill(page, { title: '同名草稿', summary: '本页面新建的同名稿', body: '本页面正文' });
      await ui.clickSave(page);
      await ui.waitForStatus(page, '草稿已保存', 'ok');
      s = await ui.state(page);
      // 旧结果到达前，列表只显示本页面已保存成功的这一篇。
      assert.equal(s.listCount, 1);
      assert.equal(s.cards[0].title, '同名草稿');
      assert.equal(s.cards[0].summary, '本页面新建的同名稿');
      assert.deepEqual(s.form, { title: '', summary: '', body: '' }); // 表单已清空，留在新建态

      // 迟到的旧快照（只含保存前的两篇）到达。
      await ui.releaseList(page);
      await ui.waitListBusy(page, false);
      s = await ui.state(page);

      assert.equal(s.listCount, 3, '本页面草稿与结果带回的草稿都应显示');
      assert.equal(s.listNoticeHidden, true);
      assert.equal(s.emptyTip, '');
      // 顺序与服务端数据按创建时间倒序的结果一致（同名两篇标题序列相同即可）。
      const apiArticles = await server.listArticles();
      const expectedTitles = apiArticles
        .slice()
        .sort((x, y) => (x.createdAt < y.createdAt ? 1 : x.createdAt > y.createdAt ? -1 : 0))
        .map((art) => art.title);
      assert.deepEqual(s.cards.map((card) => card.title), expectedTitles);
      assert.deepEqual(s.cards.map((card) => card.title).slice(0, 1), ['同名草稿']); // 本页面新建的最新
      // 三篇标识互不相同，新草稿没有被显示两次。
      const ids = apiArticles.map((art) => art.id);
      assert.equal(new Set(ids).size, 3);
      assert.ok(ids.includes(seeded1.id) && ids.includes(seeded2.id));
      assert.equal(apiArticles.length, 3);
    } finally {
      ui.assertNoPageErrors(page);
      await browser.closePage(page);
      await server.stop();
    }
  } catch (error) {
    await server.stop();
    throw error;
  }
});

// ---------------------------------------------------------------------------
// 15. First-load race, fresh-response variant: the list request itself is
//     delayed until after the save, so the response already contains the
//     just-saved draft. It must appear exactly once.
// ---------------------------------------------------------------------------
test('首次列表请求晚于保存发出、结果已含新草稿：只显示一条且顺序正确', async (browser) => {
  const server = await TestServer.start();
  try {
    await seed(server, { title: '已有草稿', summary: '早先摘要', body: '早先正文' });
    // deferred：列表请求在放行前根本没有发出，服务端将按保存后的当前数据应答。
    const page = await ui.openWithListHeld(browser, server.url, { hold: 'deferred' });
    try {
      await ui.fill(page, { title: '页面新草稿', summary: '页面摘要', body: '页面正文' });
      await ui.clickSave(page);
      await ui.waitForStatus(page, '草稿已保存', 'ok');
      let s = await ui.state(page);
      assert.equal(s.listCount, 1);

      await ui.releaseList(page);
      await ui.waitListBusy(page, false);
      s = await ui.state(page);
      assert.equal(s.listCount, 2);
      assert.deepEqual(s.cards.map((card) => card.title), ['页面新草稿', '已有草稿']);
      assert.equal(s.cards.filter((card) => card.title === '页面新草稿').length, 1);
      assert.equal(s.cards[0].summary, '页面摘要');
    } finally {
      ui.assertNoPageErrors(page);
      await browser.closePage(page);
      await server.stop();
    }
  } catch (error) {
    await server.stop();
    throw error;
  }
});

// ---------------------------------------------------------------------------
// 15b. First-load race, response-order variant: the create request is saved
//     on the server, but its success response is still in flight when the
//     initial list response — already containing the new draft — arrives
//     first. Processing the late create response must not add a second card
//     for the same id; same-title drafts with different ids stay separate;
//     the form is still cleared and create mode kept.
// ---------------------------------------------------------------------------
test('首次列表结果（已含新草稿）先于创建响应到达：同一标识只一张卡片、同名不同标识各自保留', async (browser) => {
  const server = await TestServer.start();
  try {
    // 与新草稿同名的已有草稿：标识不同，绝不能被按标题合并。
    const seededSameTitle = await seed(server, { title: '同名草稿', summary: '服务端同名稿摘要', body: 'b1' });
    const seededOld = await seed(server, { title: '旧草稿', summary: '旧摘要', body: 'b2' });
    // deferred：列表请求在放行前没有发出，放行时服务端应答已包含新草稿。
    const page = await ui.openWithListHeld(browser, server.url, { hold: 'deferred' });
    try {
      // capture：创建请求真实到达服务端并落库，只有成功响应被扣在页面里。
      await ui.gateCaptureOn(page);
      await ui.fill(page, { title: '同名草稿', summary: '页面新摘要', body: '页面新正文' });
      await ui.clickSave(page);
      await ui.waitSavingPending(page, 1);
      // 服务端已保存成功（响应仍扣在页面里，尚未到达）。
      const persisted = await waitForServerArticles(server, 3);
      const created = persisted.find((a) => a.summary === '页面新摘要');
      assert.ok(created, '创建请求应已在服务端落库');

      // 首次列表结果先到达，已包含这篇新草稿。
      await ui.releaseList(page);
      await ui.waitListBusy(page, false);
      let s = await ui.state(page);
      assert.equal(s.listCount, 3);
      assert.equal(s.cards.filter((card) => card.title === '同名草稿').length, 2);
      assert.equal(s.cards[0].summary, '页面新摘要', '新草稿最新，排在最前');

      // 创建成功响应随后到达：同一标识不能再出现第二张卡片。
      await ui.releaseSaves(page);
      await ui.gateOff(page);
      await ui.waitForStatus(page, '草稿已保存', 'ok');
      s = await ui.state(page);
      assert.equal(s.listCount, 3, '迟到的创建响应不得重复添加同一篇草稿');
      assert.equal(s.cards.filter((card) => card.title === '同名草稿').length, 2,
        '标题相同但标识不同的两篇都必须保留');
      assert.deepEqual(s.cards.map((card) => card.title), ['同名草稿', '旧草稿', '同名草稿']);
      assert.equal(s.cards[0].summary, '页面新摘要');
      assert.equal(s.cards[2].summary, '服务端同名稿摘要');
      // 表单没有未保存内容：成功后照常清空并保留新建入口。
      assert.deepEqual(s.form, { title: '', summary: '', body: '' });
      assert.equal(s.formEditing, false);
      assert.equal(s.saveBtnText, '保存草稿');
      // 服务端没有因为这次保存多出记录，三篇标识互不相同。
      const finalArticles = await server.listArticles();
      assert.equal(finalArticles.length, 3);
      const ids = finalArticles.map((a) => a.id);
      assert.equal(new Set(ids).size, 3);
      assert.ok(ids.includes(seededSameTitle.id) && ids.includes(seededOld.id) && ids.includes(created.id));
    } finally {
      ui.assertNoPageErrors(page);
      await browser.closePage(page);
      await server.stop();
    }
  } catch (error) {
    await server.stop();
    throw error;
  }
});

// ---------------------------------------------------------------------------
// 15c. Same response order as 15b, but the user keeps typing while the create
//     response is still parked after the list already showed the draft: the
//     late response must link the form to that same draft (编辑态/保存修改),
//     keep the unsaved input, show only the saved content on the single card,
//     and a following save must update that same draft (id/createdAt fixed).
// ---------------------------------------------------------------------------
test('列表先显示新草稿、创建响应后到且等待期间有未保存修改：表单关联同一篇、列表只显示已保存内容', async (browser) => {
  const server = await TestServer.start();
  try {
    const page = await ui.openWithListHeld(browser, server.url, { hold: 'deferred' });
    try {
      await ui.gateCaptureOn(page);
      await ui.fill(page, { title: '竞态新草稿', summary: '实际保存的摘要', body: '实际保存的正文' });
      await ui.clickSave(page);
      await ui.waitSavingPending(page, 1);
      const persisted = await waitForServerArticles(server, 1);
      const created = persisted[0];

      // 列表先到：新草稿按已保存内容显示一张卡片。
      await ui.releaseList(page);
      await ui.waitListBusy(page, false);
      let s = await ui.state(page);
      assert.equal(s.listCount, 1);
      assert.equal(s.cards[0].title, '竞态新草稿');

      // 创建响应仍未到达，用户继续修改摘要和正文（尚未保存）。
      await ui.fill(page, { summary: '等待期间改写的摘要', body: '等待期间改写的正文' });

      // 创建响应到达：表单关联到刚创建的同一篇草稿，未保存输入保留并被点名。
      await ui.releaseSaves(page);
      await ui.gateOff(page);
      await ui.waitForStatus(page, '尚未保存', 'warn');
      s = await ui.state(page);
      assert.match(s.statusText, /摘要、正文|正文、摘要/);
      assert.equal(s.formEditing, true, '表单应关联到刚创建的草稿进入编辑态');
      assert.equal(s.bannerId, created.id);
      assert.equal(s.saveBtnText, '保存修改');
      assert.deepEqual(s.form, {
        title: '竞态新草稿',
        summary: '等待期间改写的摘要',
        body: '等待期间改写的正文',
      });
      // 列表只有一张卡片、只反映实际保存的内容，不能把未提交输入写到卡片上。
      assert.equal(s.listCount, 1, '同一篇草稿只能有一张卡片');
      assert.equal(s.cards[0].summary, '实际保存的摘要');
      assert.equal(s.cards.filter((card) => card.editing).length, 1, '只能有一张“编辑中”卡片');

      // 随后保存修改：更新同一篇草稿，标识与创建时间不变，版本按规则递增。
      await ui.clickSave(page);
      await ui.waitForStatus(page, '修改已保存', 'ok');
      const stored = await getArticle(server, created.id);
      assert.equal(stored.id, created.id);
      assert.equal(stored.createdAt, created.createdAt);
      assert.equal(stored.version, created.version + 1);
      assert.equal(stored.summary, '等待期间改写的摘要');
      assert.equal(stored.body, '等待期间改写的正文');
      s = await ui.state(page);
      assert.equal(s.listCount, 1);
      assert.equal(s.cards[0].summary, '等待期间改写的摘要');
      assert.equal((await server.listArticles()).length, 1, '不能另建文章');
    } finally {
      ui.assertNoPageErrors(page);
      await browser.closePage(page);
      await server.stop();
    }
  } catch (error) {
    await server.stop();
    throw error;
  }
});

// ---------------------------------------------------------------------------
// 15d. The create request is persisted as version 1, but while its success
//      response is parked another page opens that draft, edits title/summary/
//      body and saves version 2. The deferred initial list is released first,
//      so the page shows version 2 before the create response arrives. The
//      late version-1 create result must NOT roll the card back: the card
//      keeps the newer saved title/summary, other drafts in that list are all
//      retained, same-title/different-id drafts stay separate, order stays
//      newest-first, and — with no further input — the form is still cleared
//      into create mode and the save is reported as success, not failure.
// ---------------------------------------------------------------------------
test('创建响应晚到且列表先显示其他页面保存的版本2：卡片不退回版本1，其他草稿保留，表单照常清空', async (browser) => {
  const server = await TestServer.start();
  try {
    // 一篇与版本2最终标题同名但标识不同的旧草稿，再加一篇其他草稿。
    const seededSameTitle = await seed(server, { title: '第二版标题', summary: '另一页同名第二版', body: 'a' });
    const seededOther = await seed(server, { title: '其他草稿', summary: '其他摘要', body: 'b' });
    // deferred：列表请求放行后才发出，服务端按放行时刻（已含版本2）的数据应答。
    const page = await ui.openWithListHeld(browser, server.url, { hold: 'deferred' });
    try {
      // capture：创建请求真实落库为版本 1，只有成功响应被扣在页面里。
      await ui.gateCaptureOn(page);
      await ui.fill(page, { title: '新建时的标题', summary: '第一版摘要', body: '第一版正文' });
      await ui.clickSave(page);
      await ui.waitSavingPending(page, 1);
      const persisted = await waitForServerArticles(server, 3);
      const created = persisted.find((a) => a.summary === '第一版摘要');
      assert.ok(created, '创建请求应已在服务端落库为版本 1');
      assert.equal(created.version, 1);

      // 另一页面读取这篇草稿，修改标题、摘要、正文并保存为版本 2。
      const up = await updateOut(server, created.id, {
        title: '第二版标题',
        summary: '第二版摘要',
        body: '第二版正文',
      }, 1);
      assert.equal(up.status, 200);

      // 首次列表先返回：卡片显示的是版本 2 的较新已保存内容。
      await ui.releaseList(page);
      await ui.waitListBusy(page, false);
      let s = await ui.state(page);
      assert.equal(s.listCount, 3);
      const createdCardBefore = s.cards.find((card) => card.summary === '第二版摘要');
      assert.ok(createdCardBefore, '列表应先显示其他页面保存的版本 2 摘要');
      assert.equal(createdCardBefore.title, '第二版标题');

      // 版本 1 的创建成功响应随后到达。
      await ui.releaseSaves(page);
      await ui.gateOff(page);
      await ui.waitForStatus(page, '草稿已保存', 'ok');
      s = await ui.state(page);

      // 迟到的创建结果不能把卡片退回版本 1 的标题或摘要。
      assert.equal(s.listCount, 3, '同一标识仍只有一张卡片，其他草稿一条都不能丢');
      assert.equal(s.cards.filter((card) => card.summary === '第一版摘要').length, 0,
        '卡片不能退回版本 1 的摘要');
      assert.equal(s.cards.filter((card) => card.title === '新建时的标题').length, 0,
        '卡片不能退回版本 1 的标题');
      const createdCardAfter = s.cards.find((card) => card.summary === '第二版摘要');
      assert.ok(createdCardAfter, '版本 2 的较新已保存内容必须继续显示');
      assert.equal(createdCardAfter.title, '第二版标题');
      // 标题相同、标识不同的两篇仍分别保留；顺序继续按创建时间倒序。
      assert.equal(s.cards.filter((card) => card.title === '第二版标题').length, 2);
      const apiArticles = await server.listArticles();
      const expectedTitles = apiArticles
        .slice()
        .sort((x, y) => (x.createdAt < y.createdAt ? 1 : x.createdAt > y.createdAt ? -1 : 0))
        .map((art) => art.title);
      assert.deepEqual(s.cards.map((card) => card.title), expectedTitles);
      const expectedSummaries = apiArticles
        .slice()
        .sort((x, y) => (x.createdAt < y.createdAt ? 1 : x.createdAt > y.createdAt ? -1 : 0))
        .map((art) => art.summary);
      assert.deepEqual(s.cards.map((card) => card.summary), expectedSummaries);
      const ids = apiArticles.map((a) => a.id);
      assert.equal(new Set(ids).size, 3);
      assert.ok(ids.includes(created.id) && ids.includes(seededSameTitle.id) && ids.includes(seededOther.id));

      // 等待期间没有新修改：创建成功后仍清空表单、保留新建入口，不当成失败。
      assert.deepEqual(s.form, { title: '', summary: '', body: '' });
      assert.equal(s.formEditing, false);
      assert.equal(s.bannerHidden, true);
      assert.equal(s.saveBtnText, '保存草稿');
      assert.equal(s.statusKind, 'ok');

      // 服务端这篇草稿仍是其他页面保存的版本 2，迟到响应没有改回版本 1 或另建记录。
      const stored = await getArticle(server, created.id);
      assert.equal(stored.version, 2);
      assert.equal(stored.title, '第二版标题');
      assert.equal(stored.summary, '第二版摘要');
      assert.equal(stored.body, '第二版正文');
      assert.equal((await server.listArticles()).length, 3);
    } finally {
      ui.assertNoPageErrors(page);
      await browser.closePage(page);
      await server.stop();
    }
  } catch (error) {
    await server.stop();
    throw error;
  }
});

// ---------------------------------------------------------------------------
// 15e. Same ordering as 15d, but the user keeps editing while the version-1
//      create response is parked after the list already showed version 2.
//      When that response arrives the single card stays at version 2 while
//      the form links to the just-created draft: unsaved inputs are kept and
//      named, the list never shows them, and the editing baseline remains the
//      create result's version 1 (NOT the card's version 2 — the user never
//      loaded the other page's body). A further save therefore PUTs version 1
//      and is rejected with the ordinary 409, which keeps the input and shows
//      the newer saved content; nothing is overwritten and no second draft is
//      created. Explicitly choosing to load the latest content then reads the
//      current full article and editing continues from version 2.
// ---------------------------------------------------------------------------
test('列表先到版本2、创建响应（版本1）后到且期间有未保存输入：表单按版本1关联，再保存走409而非覆盖或另建', async (browser) => {
  const server = await TestServer.start();
  try {
    const page = await ui.openWithListHeld(browser, server.url, { hold: 'deferred' });
    try {
      await ui.gateCaptureOn(page);
      await ui.fill(page, { title: '第一版标题', summary: '第一版摘要', body: '第一版正文' });
      await ui.clickSave(page);
      await ui.waitSavingPending(page, 1);
      const persisted = await waitForServerArticles(server, 1);
      const created = persisted[0];
      assert.equal(created.version, 1);

      // 另一页面把同一篇草稿保存为版本 2（标题、摘要、正文都改了）。
      const v2 = { title: '第二版标题', summary: '第二版摘要', body: '第二版正文' };
      const up = await updateOut(server, created.id, v2, 1);
      assert.equal(up.status, 200);

      // 首次列表先到：卡片已经是版本 2。
      await ui.releaseList(page);
      await ui.waitListBusy(page, false);
      let s = await ui.state(page);
      assert.equal(s.cards[0].title, '第二版标题');
      assert.equal(s.cards[0].summary, '第二版摘要');

      // 创建响应仍被扣着，用户基于自己创建时的内容继续改摘要和正文（未提交）。
      await ui.fill(page, { summary: '等待期间改写的摘要', body: '等待期间改写的正文' });

      // 版本 1 的创建成功响应到达。
      await ui.releaseSaves(page);
      await ui.gateOff(page);
      await ui.waitForStatus(page, '尚未保存', 'warn');
      s = await ui.state(page);
      assert.match(s.statusText, /摘要、正文|正文、摘要/);
      // 表单关联到刚创建的同一篇草稿，未保存输入原样保留。
      assert.equal(s.formEditing, true);
      assert.equal(s.bannerId, created.id);
      assert.equal(s.saveBtnText, '保存修改');
      assert.deepEqual(s.form, {
        title: '第一版标题',
        summary: '等待期间改写的摘要',
        body: '等待期间改写的正文',
      });
      // 卡片继续显示版本 2 的已保存内容，绝不显示未提交输入；只有一张“编辑中”卡片。
      assert.equal(s.listCount, 1);
      assert.equal(s.cards[0].title, '第二版标题');
      assert.equal(s.cards[0].summary, '第二版摘要');
      assert.equal(s.cards.filter((card) => card.editing).length, 1);

      // 再次保存沿用创建结果的版本基准（版本 1）：服务端当前为版本 2，
      // 应由现有冲突处理拒绝（409），而不是覆盖另一页面的修改或另建草稿。
      await ui.clickSave(page);
      await ui.waitForStatus(page, '409', 'err');
      await ui.waitConflictShown(page);
      s = await ui.state(page);
      const conflict = await ui.getConflict(page);
      assert.deepEqual(
        { title: conflict.mine.title, summary: conflict.mine.summary, body: conflict.mine.body },
        { title: '第一版标题', summary: '等待期间改写的摘要', body: '等待期间改写的正文' });
      assert.deepEqual(
        { title: conflict.saved.title, summary: conflict.saved.summary, body: conflict.saved.body }, v2);
      // 输入与编辑状态保留，卡片仍是版本 2，服务端未被覆盖、记录数不变。
      assert.deepEqual(s.form, {
        title: '第一版标题',
        summary: '等待期间改写的摘要',
        body: '等待期间改写的正文',
      });
      assert.equal(s.bannerId, created.id);
      assert.equal(s.cards[0].title, '第二版标题');
      let stored = await getArticle(server, created.id);
      assert.equal(stored.version, 2);
      assert.deepEqual({ title: stored.title, summary: stored.summary, body: stored.body }, v2);
      assert.equal((await server.listArticles()).length, 1);

      // 用户明确选择载入最新内容：按已有方式重新读取当前完整文章（版本 2）再继续编辑。
      await ui.clickDiscardLoad(page);
      await ui.waitLoadedDraft(page);
      s = await ui.state(page);
      assert.deepEqual(s.form, { title: v2.title, summary: v2.summary, body: v2.body });
      assert.equal(s.bannerId, created.id);
      assert.equal(s.cards[0].title, v2.title);

      // 以版本 2 为新基准保存，更新同一篇草稿到版本 3，不报冲突、不另建。
      await ui.fill(page, { body: `${v2.body}\n载入后补充一行` });
      await ui.clickSave(page);
      await ui.waitForStatus(page, '修改已保存', 'ok');
      stored = await getArticle(server, created.id);
      assert.equal(stored.version, 3);
      assert.equal(stored.id, created.id);
      assert.equal(stored.createdAt, created.createdAt);
      assert.equal(stored.body, `${v2.body}\n载入后补充一行`);
      assert.equal((await server.listArticles()).length, 1);
    } finally {
      ui.assertNoPageErrors(page);
      await browser.closePage(page);
      await server.stop();
    }
  } catch (error) {
    await server.stop();
    throw error;
  }
});

// ---------------------------------------------------------------------------
// 16. The page saves the draft, then edits and saves it again while the stale
//     list response is still in flight. That response carries the older
//     title/summary/version; merging must not roll the card back, must not
//     change the editing target or version baseline (a further save updates
//     the same draft with no false 409), and unsaved typing during the first
//     save must never be treated as saved.
// ---------------------------------------------------------------------------
test('迟到列表结果带着已保存文章的旧版本：列表不退回旧内容，编辑对象/版本基准/未提交输入不变', async (browser) => {
  const server = await TestServer.start();
  try {
    const page = await ui.openWithListHeld(browser, server.url, { hold: 'captured' });
    try {
      // 第一篇：保存等待期间补写正文（尚未提交），保存成功后表单关联到该草稿。
      await ui.gateOn(page);
      await ui.fill(page, { title: '第一版标题', summary: '第一版摘要', body: '第一版正文' });
      await ui.clickSave(page);
      await ui.waitSavingPending(page, 1);
      await ui.fill(page, { body: '第一版正文\n\n保存期间补写、尚未提交的段落' });
      await ui.releaseSaves(page);
      await ui.gateOff(page);
      await ui.waitForStatus(page, '尚未保存', 'warn');
      let s = await ui.state(page);
      assert.equal(s.listCount, 1);
      assert.equal(s.cards[0].title, '第一版标题');
      assert.equal(s.formEditing, true);
      const bannerId = s.bannerId;

      // 再保存一次，把等待期间的补写落库为版本 2，并改标题、摘要。
      await ui.fill(page, { title: '第二版标题', summary: '第二版摘要' });
      await ui.clickSave(page);
      await ui.waitForStatus(page, '修改已保存', 'ok');
      s = await ui.state(page);
      assert.equal(s.cards[0].title, '第二版标题');
      assert.equal(s.cards[0].summary, '第二版摘要');

      // 第二版已保存后，再补写一段但不提交；此时放行旧快照。
      await ui.fill(page, { body: `${s.form.body}\n\n又补写了一段、列表到达时仍未提交` });
      const storedV2 = await (await server.api(`/api/articles/${bannerId}`)).json();
      assert.equal(storedV2.article.version, 2);
      assert.ok(!storedV2.article.body.includes('仍未提交'));

      // 此时放行被扣住的首次列表结果：它只知道版本 1 的“第一版标题/第一版摘要”。
      await ui.releaseList(page);
      await ui.waitListBusy(page, false);
      s = await ui.state(page);
      assert.equal(s.listCount, 1, '同一篇草稿不能显示两次');
      assert.equal(s.cards[0].title, '第二版标题', '列表不能退回旧标题');
      assert.equal(s.cards[0].summary, '第二版摘要', '列表不能退回旧摘要');
      assert.equal(s.cards[0].editing, true, '编辑中的标记不能丢');
      assert.equal(s.bannerId, bannerId, '编辑对象不能被列表更新改变');
      assert.equal(s.saveBtnText, '保存修改');
      // 保存期间补写但尚未提交的内容既不能丢，也不能被列表更新当成已保存。
      assert.match(s.form.body, /又补写了一段、列表到达时仍未提交/);
      const storedAfterMerge = await (await server.api(`/api/articles/${bannerId}`)).json();
      assert.equal(storedAfterMerge.article.version, 2);
      assert.ok(!storedAfterMerge.article.body.includes('仍未提交'),
        '列表合并只读取/合并数据，绝不能把未提交内容保存上去');

      // 版本基准仍是版本 2：再保存应更新为版本 3，而不是误报 409。
      await ui.clickSave(page);
      await ui.waitForStatus(page, '修改已保存', 'ok');
      const stored = await (await server.api(`/api/articles/${bannerId}`)).json();
      assert.equal(stored.article.version, 3);
      assert.equal(stored.article.title, '第二版标题');
      assert.ok(stored.article.body.includes('又补写了一段、列表到达时仍未提交'));
      assert.equal((await server.listArticles()).length, 1);
    } finally {
      ui.assertNoPageErrors(page);
      await browser.closePage(page);
      await server.stop();
    }
  } catch (error) {
    await server.stop();
    throw error;
  }
});

// ---------------------------------------------------------------------------
// 17. The initial list fetch fails after a draft was saved successfully on
//     this page: the saved card stays usable, the page clearly distinguishes
//     "list load failed" from "no drafts", never frames the save as failed,
//     and the card can still be opened for editing. Network failure carries
//     its concrete reason too.
// ---------------------------------------------------------------------------
test('首次列表读取失败但本页面已保存草稿：保留卡片与成功状态、说明列表不完整、仍可打开编辑', async (browser) => {
  for (const failMode of ['500', 'network']) {
    const server = await TestServer.start();
    try {
      const page = await ui.openWithListHeld(browser, server.url, { fail: failMode });
      try {
        await ui.fill(page, { title: '失败前已保存', summary: '不会丢的摘要', body: '不会丢的正文' });
        await ui.clickSave(page);
        await ui.waitForStatus(page, '草稿已保存', 'ok');
        let s = await ui.state(page);
        assert.equal(s.listCount, 1);

        await ui.releaseList(page);
        await ui.waitListBusy(page, false);
        s = await ui.state(page);
        // 已保存的卡片仍然保留，且仍可打开。
        assert.equal(s.listCount, 1);
        assert.equal(s.cards[0].title, '失败前已保存');
        assert.equal(s.cards[0].summary, '不会丢的摘要');
        // 明确说明列表加载失败、已有保存结果仍保留；不显示“没有草稿”。
        assert.equal(s.listNoticeHidden, false);
        assert.match(s.listNotice, /草稿列表加载失败/);
        assert.match(s.listNotice, /完整列表尚未取得/);
        assert.match(s.listNotice, /已经保存成功/);
        assert.ok(!s.listNotice.includes('还没有保存的草稿'));
        assert.equal(s.emptyTip, '');
        if (failMode === '500') assert.match(s.listNotice, /文章列表暂时不可用/);
        else assert.match(s.listNotice, /网络连接中断/);
        // 表单区的成功提示不能被列表失败改写成保存失败。
        assert.match(s.statusText, /草稿已保存/);
        assert.equal(s.statusKind, 'ok');

        // 已有卡片仍可正常打开编辑。
        await ui.clickEdit(page, '失败前已保存');
        await ui.waitLoadedDraft(page);
        s = await ui.state(page);
        assert.equal(s.form.title, '失败前已保存');
        assert.equal(s.form.body, '不会丢的正文');
        assert.equal(s.cards[0].editing, true);
      } finally {
        ui.assertNoPageErrors(page);
        await browser.closePage(page);
        await server.stop();
      }
    } catch (error) {
      await server.stop();
      throw error;
    }
  }
});

// ---------------------------------------------------------------------------
// 18. The initial list fetch fails and nothing was saved on this page: show
//     the real failure reason, never the "no drafts" success message. Saving
//     afterwards still works and the new draft appears.
// ---------------------------------------------------------------------------
test('首次列表读取失败且本页面无保存：只显示真实失败原因，不显示空列表提示；之后仍可新建', async (browser) => {
  for (const failMode of ['500', 'network']) {
    const server = await TestServer.start();
    try {
      const page = await ui.openWithListHeld(browser, server.url, { fail: failMode });
      try {
        assert.equal((await ui.state(page)).listCount, 0);
        await ui.releaseList(page);
        await ui.waitListBusy(page, false);
        let s = await ui.state(page);
        assert.equal(s.listCount, 0);
        assert.equal(s.listNoticeHidden, false);
        assert.match(s.listNotice, /草稿列表加载失败/);
        assert.ok(!s.listNotice.includes('还没有保存的草稿'));
        assert.ok(!s.listNotice.includes('已经保存成功'));
        assert.equal(s.emptyTip, '', '不能把加载失败显示成空列表');
        if (failMode === '500') assert.match(s.listNotice, /文章列表暂时不可用/);
        else assert.match(s.listNotice, /网络连接中断/);

        // 失败之后新建仍正常（保存请求不受列表读取影响）。
        await ui.fill(page, { title: '失败后新建', body: '正文' });
        await ui.clickSave(page);
        await ui.waitForStatus(page, '草稿已保存', 'ok');
        s = await ui.state(page);
        assert.equal(s.listCount, 1);
        assert.equal(s.cards[0].title, '失败后新建');
        assert.equal((await server.listArticles()).length, 1);
      } finally {
        ui.assertNoPageErrors(page);
        await browser.closePage(page);
        await server.stop();
      }
    } catch (error) {
      await server.stop();
      throw error;
    }
  }
});

// Find a list card by its exact title (titles are unique within a scenario).
function cardByTitle(s, title) {
  const card = s.cards.find((c) => c.title === title);
  assert.ok(card, `列表中应存在标题为 ${JSON.stringify(title)} 的卡片`);
  return card;
}

// ---------------------------------------------------------------------------
// 19. Opening another draft from the list, editing-mode start: the dirty
//     guard asks first — 取消 issues no read and changes nothing; 继续 starts
//     the read. While the read is parked the form keeps every input, all
//     fields stay editable, the page shows 正在读取, saving is unavailable and
//     the target card is not marked editing. Edits made then reverted during
//     the wait count as unchanged: the freshly read target loads directly
//     (content saved by another tab AFTER the read started), and a follow-up
//     save updates the target on that baseline — no false conflict.
// ---------------------------------------------------------------------------
test('编辑态打开另一篇草稿：取消不读取、继续才读取；等待期间可输入不提前清空；改动后恢复则直接载入目标当前内容', async (browser) => {
  const ctx = await freshContext(browser);
  const { server, page } = ctx;
  try {
    const a = await seed(server, { title: '原文草稿', summary: '原文摘要', body: '原文正文' });
    const b = await seed(server, { title: '目标草稿', summary: '目标摘要', body: '目标正文' });
    await ui.reload(page);
    await ui.clickEdit(page, '原文草稿');
    await ui.waitLoadedDraft(page);

    const dirty = { title: '未保存的标题', summary: '未保存的摘要', body: '未保存的正文' };
    await ui.fill(page, dirty);

    // 选择“取消”：不读取目标草稿，原输入及编辑状态保持不变。
    await ui.readGateOn(page);
    await ui.setConfirm(page, 'cancel');
    await ui.resetConfirm(page);
    await ui.clickEdit(page, '目标草稿');
    let confirm = await ui.confirmState(page);
    assert.equal(confirm.calls.length, 1);
    assert.match(confirm.calls[0], /打开另一篇草稿将放弃这些输入/);
    assert.deepEqual(await ui.readGateState(page), { enabled: true, hits: 0, pending: 0 });
    let s = await ui.state(page);
    assert.deepEqual(s.form, dirty);
    assert.equal(s.bannerId, a.id);
    assert.equal(s.saveBtnText, '保存修改');
    assert.equal(s.saveDisabled, false);
    assert.equal(cardByTitle(s, '原文草稿').editing, true);
    assert.equal(cardByTitle(s, '目标草稿').editing, false);

    // 选择“继续”：才开始读取目标草稿。
    await ui.setConfirm(page, 'accept');
    await ui.resetConfirm(page);
    await ui.clickEdit(page, '目标草稿');
    await ui.waitReadPending(page, 1);
    confirm = await ui.confirmState(page);
    assert.equal(confirm.calls.length, 1, '读取前只询问一次');

    s = await ui.state(page);
    // 等待期间：不提前清空表单，三个字段仍可输入，页面显示正在读取，保存暂不可用。
    assert.deepEqual(s.form, dirty);
    assert.deepEqual(s.fieldsEditable, { title: true, summary: true, body: true });
    assert.match(s.statusText, /正在读取/);
    assert.equal(s.saveBtnText, '读取中…');
    assert.equal(s.saveDisabled, true);
    // 编辑对象与卡片标记不变：目标卡片不能被提前标记为编辑中。
    assert.equal(s.bannerId, a.id);
    assert.equal(cardByTitle(s, '原文草稿').editing, true);
    assert.equal(cardByTitle(s, '目标草稿').editing, false);
    // 读取期间保存不可用：点击不会产生保存请求。
    await ui.gateOn(page);
    await ui.clickSave(page);
    assert.deepEqual(await ui.gateState(page), { enabled: true, hits: 0, pending: 0 });
    // 读取期间重复点击“编辑”不会发起第二次读取。
    await ui.clickEdit(page, '目标草稿');
    assert.deepEqual(await ui.readGateState(page), { enabled: true, hits: 1, pending: 1 });

    // 等待期间三个字段都改过，随后又逐一恢复为开始读取时的值。
    await ui.fill(page, { title: '临时标题', summary: '临时摘要', body: '临时正文' });
    await ui.fill(page, dirty);

    // 挂起期间另一页面把目标草稿保存为版本 2：本次读取返回的应是它。
    const b2 = { title: '目标第二版标题', summary: '目标第二版摘要', body: '目标第二版正文\n\n第二段' };
    const up = await updateOut(server, b.id, b2, 1);
    assert.equal(up.status, 200);

    await ui.releaseReads(page);
    await ui.readGateOff(page);
    await ui.gateOff(page);
    await ui.waitLoadedDraft(page);

    // 改动后恢复原值视为未修改：直接载入，不多问一次。
    confirm = await ui.confirmState(page);
    assert.equal(confirm.calls.length, 1);
    s = await ui.state(page);
    assert.deepEqual(s.form, b2, '应载入本次读取时目标草稿实际保存的内容，而不是打开前的旧版本');
    assert.equal(s.bannerId, b.id);
    assert.equal(cardByTitle(s, b2.title).editing, true);
    assert.equal(cardByTitle(s, '原文草稿').editing, false);
    assert.equal(s.saveBtnText, '保存修改');
    assert.equal(s.saveDisabled, false);
    // 打开与等待本身不保存：原文草稿仍是版本 1，目标草稿是另一页面保存的版本 2。
    assert.equal((await getArticle(server, a.id)).version, 1);
    assert.equal((await getArticle(server, b.id)).version, 2);

    // 继续保存以本次读取的版本为基准更新目标草稿，不误报冲突、不产生第二条记录。
    await ui.fill(page, { body: `${b2.body}\n载入后补充一行` });
    await ui.clickSave(page);
    await ui.waitForStatus(page, '修改已保存', 'ok');
    s = await ui.state(page);
    assert.equal(s.conflictHidden, true);
    assert.equal(s.bannerId, b.id);
    const storedB = await getArticle(server, b.id);
    assert.equal(storedB.version, 3);
    assert.equal(storedB.createdAt, b.createdAt);
    assert.equal(storedB.body, `${b2.body}\n载入后补充一行`);
    assert.equal((await getArticle(server, a.id)).version, 1);
    assert.equal((await server.listArticles()).length, 2);
  } finally {
    await closeContext(ctx, browser);
  }
});

// ---------------------------------------------------------------------------
// 20. Editing-mode start, edits still pending when the read returns
//     (including cleared summary/body): the page must say opening the target
//     discards them. Choosing 保留 keeps every input, the original editing
//     target and the original version baseline; a later save still updates
//     the ORIGINAL draft. Opening and choosing save nothing by themselves.
// ---------------------------------------------------------------------------
test('编辑态打开另一篇草稿且等待期间有新修改（含清空摘要正文）：选择保留则输入/编辑对象/版本基准不变，保存仍作用于原文', async (browser) => {
  const ctx = await freshContext(browser);
  const { server, page } = ctx;
  try {
    const a = await seed(server, { title: '原文草稿', summary: '原文摘要', body: '原文正文' });
    const b = await seed(server, { title: '目标草稿', summary: '目标摘要', body: '目标正文' });
    await ui.reload(page);
    await ui.clickEdit(page, '原文草稿');
    await ui.waitLoadedDraft(page);
    await ui.fill(page, { title: '未保存的标题', summary: '未保存的摘要', body: '未保存的正文' });

    await ui.readGateOn(page);
    await ui.setConfirm(page, 'accept'); // 读取前的“继续”
    await ui.resetConfirm(page);
    await ui.clickEdit(page, '目标草稿');
    await ui.waitReadPending(page, 1);

    // 等待期间清空摘要和正文（清空也是受保护的修改），标题保留未保存的新值。
    await ui.fill(page, { summary: '', body: '' });
    await ui.setConfirm(page, 'cancel'); // 读取完成后的“保留当前输入”
    await ui.resetConfirm(page);
    await ui.releaseReads(page);
    await ui.readGateOff(page);
    await ui.waitForStatus(page, '已保留当前输入，未打开目标草稿');

    const confirm = await ui.confirmState(page);
    assert.equal(confirm.calls.length, 1);
    assert.match(confirm.calls[0], /打开目标草稿将放弃这些新修改/);
    const s = await ui.state(page);
    // 返回那一刻的全部输入原样保留（含被清空的摘要和正文）。
    assert.deepEqual(s.form, { title: '未保存的标题', summary: '', body: '' });
    // 原编辑对象与版本基准不变，目标卡片未被标记为编辑中。
    assert.equal(s.formEditing, true);
    assert.equal(s.bannerHidden, false);
    assert.equal(s.bannerId, a.id);
    assert.equal(s.saveBtnText, '保存修改');
    assert.equal(s.saveDisabled, false);
    assert.equal(cardByTitle(s, '原文草稿').editing, true);
    assert.equal(cardByTitle(s, '目标草稿').editing, false);
    assert.equal(cardByTitle(s, '目标草稿').summary, '目标摘要');
    // 打开与取舍都不保存：两篇草稿的已保存内容与版本不变。
    const storedA = await getArticle(server, a.id);
    assert.equal(storedA.version, 1);
    assert.equal(storedA.title, '原文草稿');
    assert.equal(storedA.summary, '原文摘要');
    assert.equal(storedA.body, '原文正文');
    assert.equal((await getArticle(server, b.id)).version, 1);

    // 此后保存仍作用于原来的文章：同一篇更新为版本 2，目标草稿不受影响。
    await ui.clickSave(page);
    await ui.waitForStatus(page, '修改已保存', 'ok');
    const savedA = await getArticle(server, a.id);
    assert.equal(savedA.version, 2);
    assert.equal(savedA.id, a.id);
    assert.equal(savedA.createdAt, a.createdAt);
    assert.equal(savedA.title, '未保存的标题');
    assert.equal(savedA.summary, '');
    assert.equal(savedA.body, '');
    assert.equal((await getArticle(server, b.id)).version, 1);
    assert.equal((await server.listArticles()).length, 2);
    const after = await ui.state(page);
    assert.equal(after.bannerId, a.id);
    assert.equal(cardByTitle(after, '未保存的标题').editing, true);
  } finally {
    await closeContext(ctx, browser);
  }
});

// ---------------------------------------------------------------------------
// 21. Same in-flight edits, but the user confirms 放弃: the target's three
//     fields load in full (HTML-like text stays plain text, nothing runs),
//     the editing marker and card sync, and a follow-up save updates the
//     target — no second record, no stale-baseline conflict.
// ---------------------------------------------------------------------------
test('编辑态打开另一篇草稿且等待期间有新修改：选择放弃则载入目标完整内容（类HTML按纯文本），保存更新目标不产生第二记录', async (browser) => {
  const ctx = await freshContext(browser);
  const { server, page } = ctx;
  try {
    const a = await seed(server, { title: '原文草稿', summary: '原文摘要', body: '原文正文' });
    const b = await seed(server, {
      title: '目标草稿',
      summary: '目标摘要 <b>不应加粗</b>',
      body: RICH_BODY,
    });
    await ui.reload(page);
    await ui.clickEdit(page, '原文草稿');
    await ui.waitLoadedDraft(page);
    await ui.fill(page, { title: '未保存的标题' });

    await ui.readGateOn(page);
    await ui.setConfirm(page, 'accept');
    await ui.resetConfirm(page);
    await ui.clickEdit(page, '目标草稿');
    await ui.waitReadPending(page, 1);
    await ui.fill(page, { title: '等待期间的标题', summary: '等待期间的摘要', body: '等待期间的正文' });
    await ui.resetConfirm(page);
    await ui.releaseReads(page);
    await ui.readGateOff(page);
    await ui.waitLoadedDraft(page);

    const confirm = await ui.confirmState(page);
    assert.equal(confirm.calls.length, 1);
    assert.match(confirm.calls[0], /放弃新修改并打开目标草稿/);
    let s = await ui.state(page);
    // 等待期间的新修改被放弃，目标草稿当前已保存的完整内容填入三个字段。
    assert.deepEqual(s.form, { title: '目标草稿', summary: '目标摘要 <b>不应加粗</b>', body: RICH_BODY });
    assert.equal(s.bannerId, b.id);
    assert.equal(cardByTitle(s, '目标草稿').editing, true);
    assert.equal(cardByTitle(s, '原文草稿').editing, false);
    // 类 HTML 文本按纯文本呈现，不产生任何元素、不执行标记。
    assert.equal(
      await page.eval(`document.querySelectorAll('#article-list script,#article-list img,#article-list b').length`),
      0);
    assert.equal(page.dialogs.length, 0);
    // 打开与取舍本身不保存：两篇草稿的版本都还是 1。
    assert.equal((await getArticle(server, a.id)).version, 1);
    assert.equal((await getArticle(server, b.id)).version, 1);

    // 继续保存更新目标草稿：以打开时读到的版本为基准，不报冲突、不产生第二条记录。
    await ui.fill(page, { body: `${RICH_BODY}\n\n接管后补充一行` });
    await ui.clickSave(page);
    await ui.waitForStatus(page, '修改已保存', 'ok');
    s = await ui.state(page);
    assert.equal(s.conflictHidden, true);
    assert.equal(s.bannerId, b.id);
    const storedB = await getArticle(server, b.id);
    assert.equal(storedB.version, 2);
    assert.equal(storedB.id, b.id);
    assert.equal(storedB.createdAt, b.createdAt);
    assert.equal(storedB.body, `${RICH_BODY}\n\n接管后补充一行`);
    assert.equal((await getArticle(server, a.id)).version, 1);
    assert.equal((await server.listArticles()).length, 2);
  } finally {
    await closeContext(ctx, browser);
  }
});

// ---------------------------------------------------------------------------
// 22. New-draft start: 取消 at the guard issues no read; when the read
//     returns with fresh input still pending, 保留 keeps the form a CREATE
//     form — it must not silently associate with the target. A later save
//     creates a second draft instead of updating the target.
// ---------------------------------------------------------------------------
test('新建态打开另一篇草稿：取消不读取；读取等待期间补写后选择保留则仍是新建态，保存创建新草稿而不关联目标', async (browser) => {
  const ctx = await freshContext(browser);
  const { server, page } = ctx;
  try {
    const b = await seed(server, { title: '已有草稿', summary: '已有摘要', body: '已有正文' });
    await ui.reload(page);
    await ui.fill(page, { title: '新建未保存标题', summary: '新建未保存摘要', body: '新建未保存正文' });

    // 选择“取消”：不读取，新建表单与输入不变。
    await ui.readGateOn(page);
    await ui.setConfirm(page, 'cancel');
    await ui.resetConfirm(page);
    await ui.clickEdit(page, '已有草稿');
    let confirm = await ui.confirmState(page);
    assert.equal(confirm.calls.length, 1);
    assert.match(confirm.calls[0], /打开另一篇草稿将放弃这些输入/);
    assert.deepEqual(await ui.readGateState(page), { enabled: true, hits: 0, pending: 0 });
    let s = await ui.state(page);
    assert.deepEqual(s.form, { title: '新建未保存标题', summary: '新建未保存摘要', body: '新建未保存正文' });
    assert.equal(s.formEditing, false);
    assert.equal(s.bannerHidden, true);
    assert.equal(s.saveBtnText, '保存草稿');
    assert.equal(cardByTitle(s, '已有草稿').editing, false);

    // 选择“继续”：开始读取；等待期间表单不提前清空、仍可继续补写。
    await ui.setConfirm(page, 'accept');
    await ui.clickEdit(page, '已有草稿');
    await ui.waitReadPending(page, 1);
    s = await ui.state(page);
    assert.match(s.statusText, /正在读取/);
    assert.deepEqual(s.form, { title: '新建未保存标题', summary: '新建未保存摘要', body: '新建未保存正文' });
    assert.deepEqual(s.fieldsEditable, { title: true, summary: true, body: true });
    assert.equal(s.saveDisabled, true);
    assert.equal(s.bannerHidden, true);
    assert.equal(cardByTitle(s, '已有草稿').editing, false);
    await ui.fill(page, { summary: '等待期间补写的摘要', body: '等待期间补写的正文' });

    // 读取完成时仍有新修改：选择保留。
    await ui.setConfirm(page, 'cancel');
    await ui.resetConfirm(page);
    await ui.releaseReads(page);
    await ui.readGateOff(page);
    await ui.waitForStatus(page, '已保留当前输入，未打开目标草稿');
    confirm = await ui.confirmState(page);
    assert.equal(confirm.calls.length, 1);
    assert.match(confirm.calls[0], /打开目标草稿将放弃这些新修改/);
    s = await ui.state(page);
    // 全部输入保留，新建表单不能悄悄关联到目标草稿。
    assert.deepEqual(s.form, { title: '新建未保存标题', summary: '等待期间补写的摘要', body: '等待期间补写的正文' });
    assert.equal(s.formEditing, false);
    assert.equal(s.bannerHidden, true);
    assert.equal(s.cancelHidden, true);
    assert.equal(s.saveBtnText, '保存草稿');
    assert.equal(s.saveDisabled, false);
    assert.equal(cardByTitle(s, '已有草稿').editing, false);
    // 打开与取舍不保存：服务端仍只有目标草稿，版本不变。
    assert.equal((await getArticle(server, b.id)).version, 1);
    assert.equal((await server.listArticles()).length, 1);

    // 此后保存按原来的新建状态创建草稿：是第二篇记录，而不是更新目标草稿。
    await ui.clickSave(page);
    await ui.waitForStatus(page, '草稿已保存', 'ok');
    const articles = await server.listArticles();
    assert.equal(articles.length, 2);
    const created = articles.find((art) => art.id !== b.id);
    assert.ok(created, '应按原来的新建状态创建一篇新草稿');
    assert.equal(created.title, '新建未保存标题');
    assert.equal(created.summary, '等待期间补写的摘要');
    assert.equal(created.body, '等待期间补写的正文');
    assert.equal(created.version, 1);
    assert.equal(created.status, 'draft');
    const storedB = await getArticle(server, b.id);
    assert.equal(storedB.version, 1);
    assert.equal(storedB.title, '已有草稿');
    s = await ui.state(page);
    assert.equal(s.listCount, 2);
  } finally {
    await closeContext(ctx, browser);
  }
});

// ---------------------------------------------------------------------------
// 23. New-draft start, user confirms 放弃: the target loads into a real
//     editing state; a follow-up save updates the target — no second record,
//     no stale-baseline conflict.
// ---------------------------------------------------------------------------
test('新建态打开另一篇草稿且等待期间有补写：选择放弃则进入目标编辑态，保存更新目标而不是新建', async (browser) => {
  const ctx = await freshContext(browser);
  const { server, page } = ctx;
  try {
    const b = await seed(server, { title: '已有草稿', summary: '已有摘要', body: '已有正文' });
    await ui.reload(page);
    await ui.fill(page, { title: '新建未保存标题', summary: '新建未保存摘要', body: '新建未保存正文' });

    await ui.readGateOn(page);
    await ui.setConfirm(page, 'accept');
    await ui.resetConfirm(page);
    await ui.clickEdit(page, '已有草稿');
    await ui.waitReadPending(page, 1);
    await ui.fill(page, { body: '等待期间改写的正文' });
    await ui.resetConfirm(page);
    await ui.releaseReads(page);
    await ui.readGateOff(page);
    await ui.waitLoadedDraft(page);

    const confirm = await ui.confirmState(page);
    assert.equal(confirm.calls.length, 1);
    assert.match(confirm.calls[0], /放弃新修改并打开目标草稿/);
    let s = await ui.state(page);
    // 新建输入被放弃，目标草稿的三个字段完整填入，进入明确的编辑状态。
    assert.deepEqual(s.form, { title: '已有草稿', summary: '已有摘要', body: '已有正文' });
    assert.equal(s.formEditing, true);
    assert.equal(s.bannerHidden, false);
    assert.equal(s.bannerId, b.id);
    assert.equal(s.saveBtnText, '保存修改');
    assert.equal(cardByTitle(s, '已有草稿').editing, true);
    assert.equal((await getArticle(server, b.id)).version, 1);
    assert.equal((await server.listArticles()).length, 1);

    // 继续保存更新目标草稿：不出现第二条记录，也不因版本基准产生错误冲突。
    await ui.fill(page, { body: '已有正文\n\n打开后补充一段' });
    await ui.clickSave(page);
    await ui.waitForStatus(page, '修改已保存', 'ok');
    s = await ui.state(page);
    assert.equal(s.conflictHidden, true);
    assert.equal(s.bannerId, b.id);
    const stored = await getArticle(server, b.id);
    assert.equal(stored.version, 2);
    assert.equal(stored.createdAt, b.createdAt);
    assert.equal(stored.body, '已有正文\n\n打开后补充一段');
    assert.equal((await server.listArticles()).length, 1);
  } finally {
    await closeContext(ctx, browser);
  }
});

// ---------------------------------------------------------------------------
// 24. The open read reports the target is gone (404), fails server-side
//     (500) or fails on the network: show the concrete reason, keep the
//     input present when the response arrived and the original editing
//     state, restore operability. Nothing is saved; once reads work again,
//     opening the target and saving it succeed.
// ---------------------------------------------------------------------------
test('编辑态打开另一篇草稿读取返回404/500/网络失败：显示具体原因、保留等待期间输入与原编辑状态、恢复可操作', async (browser) => {
  const ctx = await freshContext(browser);
  const { server, page } = ctx;
  try {
    const a = await seed(server, { title: '原文草稿', summary: '原文摘要', body: '原文正文' });
    const b = await seed(server, { title: '目标草稿', summary: '目标摘要', body: '目标正文' });
    await ui.reload(page);
    await ui.clickEdit(page, '原文草稿');
    await ui.waitLoadedDraft(page);
    const dirty = { title: '未保存的标题', summary: '未保存的摘要', body: '未保存的正文' };
    await ui.fill(page, dirty);
    await ui.setConfirm(page, 'accept');

    // 404（目标草稿已不存在）：等待期间继续输入，失败响应到达时全部保留。
    await ui.readGateOn(page);
    await ui.armReadFailure(page, 404);
    await ui.clickEdit(page, '目标草稿');
    await ui.waitReadPending(page, 1);
    let s = await ui.state(page);
    assert.match(s.statusText, /正在读取/);
    assert.deepEqual(s.fieldsEditable, { title: true, summary: true, body: true });
    await ui.fill(page, { title: '未保存的标题 响应到达前补写' });
    await ui.releaseReads(page);
    await ui.readGateOff(page);
    await ui.waitForStatus(page, '该草稿已不存在', 'err');
    s = await ui.state(page);
    assert.match(s.statusText, /打开草稿失败/);
    assert.deepEqual(s.form, { title: '未保存的标题 响应到达前补写', summary: dirty.summary, body: dirty.body });
    assert.equal(s.formEditing, true);
    assert.equal(s.bannerId, a.id);
    assert.equal(s.saveBtnText, '保存修改');
    assert.equal(s.saveDisabled, false);
    assert.equal(cardByTitle(s, '原文草稿').editing, true);
    assert.equal(cardByTitle(s, '目标草稿').editing, false);
    assert.equal(await ui.readFailureArmed(page), null);
    assert.equal((await getArticle(server, a.id)).version, 1);
    assert.equal((await getArticle(server, b.id)).version, 1);

    // 500：同样保留全部输入与原编辑状态。
    await ui.armReadFailure(page, 500);
    await ui.clickEdit(page, '目标草稿');
    await ui.waitForStatus(page, '单篇读取暂时不可用', 'err');
    s = await ui.state(page);
    assert.match(s.statusText, /打开草稿失败/);
    assert.deepEqual(s.form, { title: '未保存的标题 响应到达前补写', summary: dirty.summary, body: dirty.body });
    assert.equal(s.bannerId, a.id);
    assert.equal(s.saveDisabled, false);

    // 网络层失败：具体原因可见，状态依旧保留。
    await ui.armReadFailure(page, 'network');
    await ui.clickEdit(page, '目标草稿');
    await ui.waitForStatus(page, '网络连接中断', 'err');
    s = await ui.state(page);
    assert.match(s.statusText, /打开草稿失败/);
    assert.equal(s.bannerId, a.id);
    assert.equal(s.formEditing, true);
    assert.equal(s.saveBtnText, '保存修改');
    assert.equal(s.saveDisabled, false);
    assert.equal((await getArticle(server, a.id)).version, 1);
    assert.equal((await getArticle(server, b.id)).version, 1);
    assert.equal((await server.listArticles()).length, 2);

    // 读取恢复后可正常打开目标草稿，随后保存更新目标。
    await ui.clickEdit(page, '目标草稿');
    await ui.waitLoadedDraft(page);
    s = await ui.state(page);
    assert.deepEqual(s.form, { title: '目标草稿', summary: '目标摘要', body: '目标正文' });
    assert.equal(s.bannerId, b.id);
    await ui.fill(page, { body: '恢复后保存的正文' });
    await ui.clickSave(page);
    await ui.waitForStatus(page, '修改已保存', 'ok');
    assert.equal((await getArticle(server, b.id)).version, 2);
    assert.equal((await getArticle(server, a.id)).version, 1);
    assert.equal((await server.listArticles()).length, 2);
  } finally {
    await closeContext(ctx, browser);
  }
});

// ---------------------------------------------------------------------------
// 25. New-draft start, the open read fails: the create-mode form and the
//     input typed during the wait are kept, the page is operable again, and
//     nothing is saved. Once reads work again the target opens normally.
// ---------------------------------------------------------------------------
test('新建态打开另一篇草稿读取失败：保留新建状态与等待期间输入，恢复后可正常打开', async (browser) => {
  const ctx = await freshContext(browser);
  const { server, page } = ctx;
  try {
    const b = await seed(server, { title: '已有草稿', summary: '已有摘要', body: '已有正文' });
    await ui.reload(page);
    await ui.fill(page, { title: '新建未保存标题', summary: '新建未保存摘要', body: '新建未保存正文' });

    await ui.readGateOn(page);
    await ui.armReadFailure(page, 'network');
    await ui.setConfirm(page, 'accept');
    await ui.resetConfirm(page);
    await ui.clickEdit(page, '已有草稿');
    await ui.waitReadPending(page, 1);
    await ui.fill(page, { summary: '等待期间补写的摘要' });
    await ui.releaseReads(page);
    await ui.readGateOff(page);
    await ui.waitForStatus(page, '网络连接中断', 'err');
    let s = await ui.state(page);
    assert.match(s.statusText, /打开草稿失败/);
    // 等待期间的输入与新建状态完整保留，恢复可操作。
    assert.deepEqual(s.form, { title: '新建未保存标题', summary: '等待期间补写的摘要', body: '新建未保存正文' });
    assert.equal(s.formEditing, false);
    assert.equal(s.bannerHidden, true);
    assert.equal(s.cancelHidden, true);
    assert.equal(s.saveBtnText, '保存草稿');
    assert.equal(s.saveDisabled, false);
    assert.equal(cardByTitle(s, '已有草稿').editing, false);
    assert.equal(await ui.readFailureArmed(page), null);
    assert.equal((await getArticle(server, b.id)).version, 1);
    assert.equal((await server.listArticles()).length, 1);

    // 读取恢复后仍可打开目标草稿；本次放弃输入并载入后保存更新目标。
    await ui.clickEdit(page, '已有草稿');
    await ui.waitLoadedDraft(page);
    s = await ui.state(page);
    assert.deepEqual(s.form, { title: '已有草稿', summary: '已有摘要', body: '已有正文' });
    assert.equal(s.bannerId, b.id);
    await ui.fill(page, { body: '恢复后保存的正文' });
    await ui.clickSave(page);
    await ui.waitForStatus(page, '修改已保存', 'ok');
    assert.equal((await getArticle(server, b.id)).version, 2);
    assert.equal((await server.listArticles()).length, 1);
  } finally {
    await closeContext(ctx, browser);
  }
});

// ===========================================================================
// 请求体大小限制的服务端回归保障（上限 5×1024×1024 字节）。
// 上面的页面测试已覆盖“超长英文正文触发保存失败后保留输入”；这里固定服务端
// 对“实际发送的整份 JSON 请求体 UTF-8 字节数”的判断本身：
//   - 标题、摘要、正文、更新时的 version 与 JSON 结构全部计入；
//   - 合法请求恰好达到上限成功，超出哪怕一个字节也拒绝；
//   - 中文等多字节文字按字节而非字符计数，换行（JSON 中转义为 \n）同样计入；
//   - 正文自身未超限、加上其他字段整份请求才超限，也算超限；
//   - Content-Length 与分块传输遵守同一上限、同一结论；分块时前帧尚在限内、
//     后帧才让累计字节超限，客户端仍能读到完整、可解析的 400 与非空 error，
//     而不是连接中断或残缺响应，连接随后还能继续使用；
//   - 拒绝新建不新增记录；拒绝更新不改动原文章任何字段，其他草稿不受影响；
//   - 合法新建 201/版本1，合法更新 200/标识与创建时间不变/版本只加一，
//     多语言与换行内容经现有 GET 读取入口原样取回，标题按规则去首尾空白。
// 这些场景直接驱动 HTTP（原始 TCP 套接字精确控制线上字节与分块帧），持久化
// 内容仍只通过现有读取入口核对，不改变首页编辑方式与已公开的保存行为。
// ===========================================================================

const MAX_BODY_BYTES = 5 * 1024 * 1024;

// 以固定字段顺序构造与页面一致的负载（{title,summary,body}[,version]），
// 把 padField 用 padUnit 重复填充（剩余字节用 ASCII 'a' 补齐），使整份 JSON
// 请求体经 UTF-8 编码后恰好为 size 个字节。换行等需 JSON 转义的字符按其
// 在线上 JSON 文本中的实际字节数计量。
function buildPaddedObject(version, padField, pad) {
  const obj = { title: 't', summary: 's', body: 'b' };
  if (version !== null) obj.version = version; // version 始终位于 body 之后
  obj[padField] = pad; // 更新已有键，不改变字段顺序
  return obj;
}

function sizedObjectBody(size, { version = null, padField = 'body', padUnit = 'x' } = {}) {
  const overhead = Buffer.byteLength(JSON.stringify(buildPaddedObject(version, padField, '')), 'utf8');
  const unitWire = Buffer.byteLength(JSON.stringify(buildPaddedObject(version, padField, padUnit)), 'utf8') - overhead;
  assert.ok(unitWire > 0);
  const units = Math.floor((size - overhead) / unitWire);
  let pad = padUnit.repeat(units);
  let body = Buffer.from(JSON.stringify(buildPaddedObject(version, padField, pad)), 'utf8');
  if (body.length < size) {
    pad += 'a'.repeat(size - body.length); // 差额必小于 unitWire，1 字节 ASCII 可精确补齐
    body = Buffer.from(JSON.stringify(buildPaddedObject(version, padField, pad)), 'utf8');
  }
  assert.equal(body.length, size, `无法精确拼出 ${size} 字节的请求体（实际 ${body.length}）`);
  return body;
}

// 统一核对超限拒绝：完整可读的 400、非空且明确说明“超过允许大小”的 error。
function assertTooLargeResponse(res, transport) {
  assert.equal(res.status, 400, `${transport}：超过上限哪怕一个字节也必须是 400，实际 ${res.status}`);
  assert.match(res.headers['content-type'] || '', /application\/json/, `${transport}：400 应为 JSON`);
  const declared = res.headers['content-length'];
  assert.notEqual(declared, undefined, `${transport}：400 必须带 Content-Length`);
  assert.equal(res.body.length, Number(declared),
    `${transport}：必须读到完整 400 响应体（${res.body.length}/${declared}），不能是连接中断或残缺响应`);
  let data;
  assert.doesNotThrow(() => { data = JSON.parse(res.body.toString('utf8')); },
    `${transport}：400 响应体必须是可解析的 JSON`);
  assert.ok(typeof data.error === 'string' && data.error.trim() !== '',
    `${transport}：error 必须非空`);
  assert.match(data.error, /allowed size|too large|exceed/i,
    `${transport}：error 必须明确说明超过允许大小：${data.error}`);
  return data.error;
}

async function startApiOnly() {
  const server = await TestServer.start();
  const http = await RawHttp.connect(server.url);
  return { server, http };
}

async function stopApiOnly({ server, http }) {
  await http.close();
  await server.stop();
}

async function fetchArticle(server, id) {
  const res = await server.api(`/api/articles/${id}`);
  assert.equal(res.status, 200);
  return (await res.json()).article;
}

// --- 创建（POST）：恰好到限成功、超一个字节两种传输都拒绝且不新增记录 -------
test('请求体大小：POST 恰好5MiB成功（201/版本1/内容经GET一致），5MiB+1经Content-Length与分块均完整400且不新增', async () => {
  const ctx = await startApiOnly();
  const { server, http } = ctx;
  try {
    const exact = sizedObjectBody(MAX_BODY_BYTES); // 全 ASCII，字段字符数=字节数
    const exactFields = JSON.parse(exact.toString('utf8'));

    // 恰好达到上限：Content-Length 声明，必须成功。
    let res = await http.send('POST', '/api/articles', exact);
    assert.equal(res.status, 201, `恰好达到上限应成功，实际 ${res.status}：${res.body.toString().slice(0, 200)}`);
    const created = JSON.parse(res.body.toString('utf8')).article;
    assert.equal(typeof created.id, 'string');
    assert.equal(created.status, 'draft');
    assert.equal(created.version, 1);
    assert.ok(!Number.isNaN(Date.parse(created.createdAt)));
    // 成功保存的内容经现有单篇读取入口取得时与提交一致。
    const fetched = await fetchArticle(server, created.id);
    assert.deepEqual(fetched, created);
    assert.equal(fetched.title, exactFields.title);
    assert.equal(fetched.summary, exactFields.summary);
    assert.equal(fetched.body, exactFields.body);
    assert.equal((await server.listArticles()).length, 1);

    // 超出一个字节：Content-Length 与分块传输必须得到同一个 400。
    const over = sizedObjectBody(MAX_BODY_BYTES + 1);
    assert.equal(over.length, exact.length + 1);
    const errLength = assertTooLargeResponse(
      await http.send('POST', '/api/articles', over), 'Content-Length');
    const frames = splitFrames(over, [1024]); // 第一帧 1024 字节（限内），第二帧才越过累计上限
    const errChunked = assertTooLargeResponse(
      await http.sendChunked('POST', '/api/articles', frames, { delayMs: 100 }), '分块');
    assert.equal(errChunked, errLength, '两种传输方式对同一请求体的判断必须一致');
    assert.deepEqual(await server.listArticles(), [created], '拒绝新建不能多出记录');

    // 分块合法请求（恰好到限）不能因为传输方式被拒绝。
    const exactChunked = sizedObjectBody(MAX_BODY_BYTES, { padField: 'summary' });
    res = await http.sendChunked('POST', '/api/articles', splitFrames(exactChunked, [4096, 4096 * 100]));
    assert.equal(res.status, 201, `分块发送的到限合法请求应成功，实际 ${res.status}`);
    assert.equal(JSON.parse(res.body.toString('utf8')).article.version, 1);
    assert.equal((await server.listArticles()).length, 2);

    // 被拒后同一连接仍完整可用：400 不是靠中断连接表达的。
    const health = await http.get('/health');
    assert.equal(health.status, 200);
    assert.match(health.body.toString('utf8'), /"status":"ok"/);
  } finally {
    await stopApiOnly(ctx);
  }
});

// --- 按 UTF-8 字节而非字符计数：中文（3字节/字）、中文+换行 -----------------
test('请求体大小：中文等多字节文字与换行按UTF-8字节计数，字符数远低于上限但整份请求超一个字节仍拒绝', async () => {
  const ctx = await startApiOnly();
  const { server, http } = ctx;
  try {
    // 正文字符数约 174 万、远小于 5,242,880 个“字符”，但整份请求恰好到限：
    // 必须成功，证明边界按字节、且到限允许。
    const cnExact = sizedObjectBody(MAX_BODY_BYTES, { padField: 'body', padUnit: '中' });
    let parsed = JSON.parse(cnExact.toString('utf8'));
    const exactChars = Array.from(parsed.body).length;
    assert.ok(exactChars < MAX_BODY_BYTES, '多字节正文字符数本就低于按字符算出的上限');
    let res = await http.send('POST', '/api/articles', cnExact);
    assert.equal(res.status, 201, `按字节恰好到限的中文请求应成功，实际 ${res.status}`);
    const cnArticle = JSON.parse(res.body.toString('utf8')).article;
    assert.equal((await fetchArticle(server, cnArticle.id)).body, parsed.body);

    // 同样字符计量方式再多一个字节：字符数仍远低于上限，两种传输都必须拒绝。
    const cnOver = sizedObjectBody(MAX_BODY_BYTES + 1, { padField: 'body', padUnit: '中' });
    parsed = JSON.parse(cnOver.toString('utf8'));
    const overChars = Array.from(parsed.body).length;
    assert.ok(overChars < MAX_BODY_BYTES, '被拒请求的正文字符数仍低于按字符误算的上限');
    assert.ok(Buffer.byteLength(parsed.body, 'utf8') < cnOver.length, '正文自身未到上限，是整份请求超限');
    const e1 = assertTooLargeResponse(await http.send('POST', '/api/articles', cnOver), 'Content-Length中文');
    const e2 = assertTooLargeResponse(
      await http.sendChunked('POST', '/api/articles', splitFrames(cnOver, [2048]), { delayMs: 50 }),
      '分块中文');
    assert.equal(e1, e2);

    // 中文+换行：换行在 JSON 文本中占实际字节（\n 转义），一个都不能漏算。
    const nlOver = sizedObjectBody(MAX_BODY_BYTES + 1, { padField: 'body', padUnit: '中\n' });
    parsed = JSON.parse(nlOver.toString('utf8'));
    assert.ok(Array.from(parsed.body).length < MAX_BODY_BYTES);
    assert.ok(parsed.body.includes('\n'), '填充确实包含换行');
    assertTooLargeResponse(await http.send('POST', '/api/articles', nlOver), '中文换行');

    // 对照组：把同一填充缩短到恰好到限，必须成功（不是针对内容类型的拒绝）。
    const nlExact = sizedObjectBody(MAX_BODY_BYTES, { padField: 'body', padUnit: '中\n' });
    res = await http.send('POST', '/api/articles', nlExact);
    assert.equal(res.status, 201, `按字节恰好到限的中文+换行请求应成功，实际 ${res.status}`);

    const list = await server.listArticles();
    assert.equal(list.length, 2, '只有两次合法新建落库');
  } finally {
    await stopApiOnly(ctx);
  }
});

// --- 正文自身未超限，靠摘要/JSON 结构把整份请求顶过上限 ----------------------
test('请求体大小：正文远低于上限但摘要把整份请求顶到5MiB+1同样拒绝；分块跨限响应完整、连接可复用', async () => {
  const ctx = await startApiOnly();
  const { server, http } = ctx;
  try {
    // 摘要超大、正文仅 1 个字符：超限判断对象是整份请求而非正文字段。
    const over = sizedObjectBody(MAX_BODY_BYTES + 1, { padField: 'summary', padUnit: '中' });
    const parsed = JSON.parse(over.toString('utf8'));
    assert.equal(parsed.body, 'b');
    assert.ok(Buffer.byteLength(parsed.summary, 'utf8') < over.length);
    assertTooLargeResponse(await http.send('POST', '/api/articles', over), '摘要超限Content-Length');

    // 分块：先发一小帧（限内），稍候再发把累计字节顶过上限的大帧。
    assertTooLargeResponse(
      await http.sendChunked('POST', '/api/articles', splitFrames(over, [512]), { delayMs: 100 }),
      '摘要超限分块');

    // 同一份填充恰好到限时成功——边界差一个字节结论相反。
    const exact = sizedObjectBody(MAX_BODY_BYTES, { padField: 'summary', padUnit: '中' });
    const res = await http.send('POST', '/api/articles', exact);
    assert.equal(res.status, 201, `摘要填充使整份请求恰好到限应成功，实际 ${res.status}`);

    // 分块的小体积合法多语言请求不能被误拒。
    const small = Buffer.from(JSON.stringify({
      title: '分块合法',
      summary: '摘要含中文',
      body: '第一行\n\n空行后 café 日本語',
    }), 'utf8');
    const smallRes = await http.sendChunked('POST', '/api/articles', splitFrames(small, [16, 40]));
    assert.equal(smallRes.status, 201);
    const smallArticle = JSON.parse(smallRes.body.toString('utf8')).article;
    const fetchedSmall = await fetchArticle(server, smallArticle.id);
    assert.equal(fetchedSmall.body, '第一行\n\n空行后 café 日本語');
    assert.equal(fetchedSmall.summary, '摘要含中文');

    // 分块超限被拒后同一连接继续完整可用。
    const again = await http.get('/api/articles');
    assert.equal(again.status, 200);
    const rawIds = JSON.parse(again.body.toString('utf8')).articles.map((x) => x.id).sort();
    assert.deepEqual(rawIds, (await server.listArticles()).map((x) => x.id).sort());
    assert.equal(rawIds.length, 2);
  } finally {
    await stopApiOnly(ctx);
  }
});

// --- 更新（PUT）：到限成功并只加一个版本；超限拒绝则原文章与其他草稿不变 ----
test('请求体大小：PUT 到限成功（200/标识创建时间状态不变/版本只加一），5MiB+1两种传输完整400且原文章与其他草稿均不变', async () => {
  const ctx = await startApiOnly();
  const { server, http } = ctx;
  try {
    const seeded = await seed(server, {
      title: '原标题',
      summary: '原摘要',
      body: '原正文第一段\n\n原正文第二段',
    });
    const other = await seed(server, { title: '另一篇草稿', summary: '别的摘要', body: '别的正文' });
    const before = await fetchArticle(server, seeded.id);
    const otherBefore = await fetchArticle(server, other.id);
    assert.equal(before.version, 1);

    const assertUntouched = async (label) => {
      assert.deepEqual(await fetchArticle(server, seeded.id), before,
        `${label}：原文章的标题、摘要、正文、标识、创建时间、草稿状态与版本必须保持原值`);
      assert.deepEqual(await fetchArticle(server, other.id), otherBefore,
        `${label}：其他草稿不能受影响`);
      assert.equal((await server.listArticles()).length, 2, `${label}：不能新增或减少记录`);
    };

    // 超过一个字节：Content-Length 与分块（前帧限内、后帧越限）都完整 400。
    const over = sizedObjectBody(MAX_BODY_BYTES + 1, { version: 1 });
    assertTooLargeResponse(
      await http.send('PUT', `/api/articles/${seeded.id}`, over), 'PUT Content-Length');
    await assertUntouched('PUT超限(Content-Length)');
    assertTooLargeResponse(
      await http.sendChunked('PUT', `/api/articles/${seeded.id}`, splitFrames(over, [1024]), { delayMs: 100 }),
      'PUT 分块');
    await assertUntouched('PUT超限(分块)');

    // 恰好到限：两种传输都成功；标识/创建时间/草稿状态不变，版本只加一次。
    const exactV1 = sizedObjectBody(MAX_BODY_BYTES, { version: 1 });
    const exactFields = JSON.parse(exactV1.toString('utf8'));
    let res = await http.send('PUT', `/api/articles/${seeded.id}`, exactV1);
    assert.equal(res.status, 200, `PUT 恰好到限应成功，实际 ${res.status}`);
    let updated = JSON.parse(res.body.toString('utf8')).article;
    assert.equal(updated.id, before.id);
    assert.equal(updated.createdAt, before.createdAt);
    assert.equal(updated.status, 'draft');
    assert.equal(updated.version, 2, '版本只能增加一次');
    assert.deepEqual(
      { title: updated.title, summary: updated.summary, body: updated.body },
      { title: exactFields.title, summary: exactFields.summary, body: exactFields.body });
    let stored = await fetchArticle(server, seeded.id);
    assert.deepEqual(stored, updated);

    // 分块到限同样成功，版本再只加一次（2→3）。
    const exactV2 = sizedObjectBody(MAX_BODY_BYTES, { version: 2, padField: 'summary' });
    const exactV2Fields = JSON.parse(exactV2.toString('utf8'));
    res = await http.sendChunked('PUT', `/api/articles/${seeded.id}`,
      splitFrames(exactV2, [8192, 8192 * 200]), { delayMs: 30 });
    assert.equal(res.status, 200, `分块 PUT 恰好到限应成功，实际 ${res.status}`);
    updated = JSON.parse(res.body.toString('utf8')).article;
    assert.equal(updated.id, before.id);
    assert.equal(updated.createdAt, before.createdAt);
    assert.equal(updated.status, 'draft');
    assert.equal(updated.version, 3, '每次合法更新版本只增加一次');
    assert.equal(updated.body, exactV2Fields.body);
    stored = await fetchArticle(server, seeded.id);
    assert.deepEqual(stored, updated);

    // 另一篇草稿自始至终未被改动；被拒/成功都未增减记录。
    assert.deepEqual(await fetchArticle(server, other.id), otherBefore);
    assert.equal((await server.listArticles()).length, 2);

    // 分块被拒（针对不存在的文章也是先判大小）后连接仍可完整复用。
    const overMissing = sizedObjectBody(MAX_BODY_BYTES + 1, { version: 1 });
    assertTooLargeResponse(
      await http.sendChunked('PUT', '/api/articles/does-not-exist', splitFrames(overMissing, [1024]), { delayMs: 50 }),
      'PUT 不存在文章超限');
    const health = await http.get('/health');
    assert.equal(health.status, 200);
    const reread = await http.get(`/api/articles/${seeded.id}`);
    assert.equal(reread.status, 200);
    assert.deepEqual(JSON.parse(reread.body.toString('utf8')).article, stored);
  } finally {
    await stopApiOnly(ctx);
  }
});

// --- 合法保存的多语言/换行/空行内容经现有读取入口原样往返；标题去空白 --------
test('请求体大小：常规合法保存的中文、其他语言、换行空行经GET原样取回；标题去首尾空白；版本每次只加一', async () => {
  const ctx = await startApiOnly();
  const { server } = ctx;
  try {
    const rich = {
      title: '  标题 两侧空白将被去除 \t ',
      summary: '摘要第一行\n摘要第二行\n',
      body: '多行正文第一段\n\n空行之后是中文 English café 日本語 العربية 한국어\n\n'
        + '<script>alert(1)</script>\n<html>按原文保存</html>\n\n末行后保留换行吗？不，结尾无换行',
    };
    const createRes = await server.api('/api/articles', { method: 'POST', body: JSON.stringify(rich) });
    assert.equal(createRes.status, 201);
    const created = (await createRes.json()).article;
    assert.equal(created.version, 1);
    assert.equal(created.status, 'draft');
    assert.equal(created.title, rich.title.trim(), '标题按现有规则去除首尾空白');
    let saved = await fetchArticle(server, created.id);
    assert.equal(saved.title, rich.title.trim());
    assert.equal(saved.summary, rich.summary, '摘要中的换行必须原样保存');
    assert.equal(saved.body, rich.body, '正文（多语言、换行、空行、类HTML）必须原样保存');

    // 连续两次更新：标识与创建时间不变，版本每次只加一，内容逐次原样往返。
    for (let i = 0; i < 2; i++) {
      const next = {
        title: `  第${'二三四'[i]}次标题  `,
        summary: '',
        body: `${rich.body}\n\n第${i + 2}版新增空行：\n\t制表符与中文保留`,
        version: i + 1,
      };
      const res = await server.api(`/api/articles/${created.id}`, { method: 'PUT', body: JSON.stringify(next) });
      assert.equal(res.status, 200);
      const article = (await res.json()).article;
      assert.equal(article.id, created.id);
      assert.equal(article.createdAt, created.createdAt);
      assert.equal(article.status, 'draft');
      assert.equal(article.version, i + 2);
      saved = await fetchArticle(server, created.id);
      assert.equal(saved.title, next.title.trim());
      assert.equal(saved.summary, '');
      assert.equal(saved.body, next.body);
      assert.deepEqual(saved, article);
    }
    assert.equal((await server.listArticles()).length, 1);
  } finally {
    await stopApiOnly(ctx);
  }
});

// ---------------------------------------------------------------------------

const only = tests.filter((t) => process.env.TEST_FILTER && t.name.includes(process.env.TEST_FILTER));
const selected = only.length ? only : tests;

const browser = await Browser.launch();
let failed = 0;
for (const t of selected) {
  const started = Date.now();
  try {
    await t.fn(browser);
    console.log(`  ✓ ${t.name} (${Date.now() - started}ms)`);
  } catch (error) {
    failed++;
    console.error(`  ✗ ${t.name}\n${error.stack || error}`);
  }
}
await browser.close();
if (failed) {
  console.error(`\n${failed}/${selected.length} 个回归场景失败`);
  process.exit(1);
}
console.log(`\n全部 ${selected.length} 个回归场景通过`);
