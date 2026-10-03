// End-to-end regression coverage for the homepage draft-save race:
// after clicking save, the user keeps typing while waiting for the response.
// Those later keystrokes must neither be lost when the save succeeds nor be
// falsely reported as saved. Scenarios exercise the real form, status text,
// draft list, a second save and reopening the draft — never just the API.
//
// Run: node test/run.mjs   (CHROME_BIN can override the browser executable)
import assert from 'node:assert/strict';
import { Browser } from './cdp.mjs';
import { TestServer } from './server.mjs';
import * as ui from './ui.mjs';

const tests = [];
function test(name, fn) { tests.push({ name, fn }); }

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
