// End-to-end regression coverage for the homepage draft-save race:
// after clicking save, the user keeps typing while waiting for the response.
// Those later keystrokes must neither be lost when the save succeeds nor be
// falsely reported as saved. Scenarios exercise the real form, status text,
// draft list, a second save and reopening the draft — never just the API.
//
// A second group of scenarios covers the version-conflict flow: two pages
// open the same draft, one saves, the other's save is rejected with 409, and
// "放弃当前输入并载入最新内容" re-reads the draft (never trusting the stale
// comparison), with the read held in flight so the keep/discard choice,
// failure handling and plain-text rendering are all exercised for real.
//
// Run: node test/run.mjs   (CHROME_BIN can override the browser executable)
import assert from 'node:assert/strict';
import { writeFileSync } from 'node:fs';
import { join } from 'node:path';
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
// Version-conflict scenarios: "放弃当前输入并载入最新内容".
//
// Shared fixture: the page edits a draft while another side (the raw API, a
// stand-in for a second tab) saves twice — once to cause the 409, and once
// more *after* the conflict comparison is shown, so the comparison is stale
// by the time the user clicks 载入最新内容. The freshly read LATEST content
// (not the stale comparison) is what must be loaded.
// ---------------------------------------------------------------------------
const SEED = { title: '冲突基准', summary: '基准摘要', body: '基准正文' };
const MINE = { title: '我的未保存标题', summary: '我的未保存摘要', body: '我的未保存正文\n\n第二段' };
const OTHER_V1 = { title: '另一页第一版', summary: '另一页第一版摘要', body: '另一页第一版正文' };
const LATEST = {
  title: '另一页最新标题',
  summary: '最新摘要 <b>按文本显示</b>',
  body: '最新正文第一段\n\n空行之后\n\n中文 English café 日本語 العربية\n\n<html>不执行标记</html>',
};

async function putArticle(server, id, fields, version) {
  const res = await server.api(`/api/articles/${id}`, {
    method: 'PUT',
    body: JSON.stringify({ ...fields, version }),
  });
  assert.equal(res.status, 200, `PUT failed: ${res.status}`);
  return (await res.json()).article;
}

// Drives the page to: conflict comparison displayed (showing OTHER_V1) and
// LATEST already saved afterwards as version 3 by the other side.
async function reachStaleConflict(server, page) {
  const a = await seedAndReload(server, page, SEED);
  await ui.clickEdit(page, SEED.title);
  await ui.waitLoadedDraft(page);
  await putArticle(server, a.id, OTHER_V1, 1); // 另一页保存 → version 2
  await ui.fill(page, MINE);
  await ui.clickSave(page);
  await ui.waitForStatus(page, '409', 'err'); // 旧版本被拒，全部输入保留，对照出现
  const latest = await putArticle(server, a.id, LATEST, 2); // 对照之后另一页又保存 → version 3
  return { a, latest };
}

// Polls the server until the article reaches the expected version (UI status
// text alone cannot distinguish two consecutive identical success messages).
async function waitForServerVersion(server, id, version) {
  const deadline = Date.now() + 5000;
  for (;;) {
    const saved = await (await server.api(`/api/articles/${id}`)).json();
    if (saved.article.version === version) return saved.article;
    if (Date.now() > deadline) {
      throw new Error(`version did not become ${version}; last=${saved.article.version}`);
    }
    await new Promise((resolve) => setTimeout(resolve, 25));
  }
}

// ---------------------------------------------------------------------------
// 8. Two real pages on the same draft: the stale save is rejected with 409,
//    all input is kept, both sides are shown side by side. After the other
//    page saves AGAIN, 载入最新内容 must load what is actually saved at read
//    time (version 3), not the stale comparison (version 2). Editing then
//    continues on the same draft and the next save hits no false conflict.
// ---------------------------------------------------------------------------
test('两页打开同一草稿：409 保留输入并对照，载入以本次读取为准，之后保存不误报冲突', async (browser) => {
  const server = await TestServer.start();
  let pageA = null;
  let pageB = null;
  try {
    const a = await seed(server, { title: '冲突草稿', summary: '初始摘要', body: '初始正文' });
    pageA = await ui.open(browser, server.url);
    pageB = await ui.open(browser, server.url);

    // 两个页面都打开同一篇草稿（版本基准都是 1）。
    await ui.clickEdit(pageA, '冲突草稿');
    await ui.waitLoadedDraft(pageA);
    await ui.clickEdit(pageB, '冲突草稿');
    await ui.waitLoadedDraft(pageB);

    // 页面甲保存第一版修改（含类 HTML 文本与多语言正文）。
    const v1 = {
      title: '甲页第一版标题',
      summary: '甲页第一版摘要 <i>斜体标记</i>',
      body: '甲页第一版正文\n\n中文 English café\n\n<b>不执行</b>',
    };
    await ui.fill(pageA, v1);
    await ui.clickSave(pageA);
    await ui.waitForStatus(pageA, '修改已保存', 'ok');
    await waitForServerVersion(server, a.id, 2);

    // 页面乙基于旧版本保存：被拒 409，全部输入保留，双方内容并排对照。
    const mine = { title: '乙页未保存标题', summary: '乙页未保存摘要', body: '乙页未保存正文\n\n保留我的输入' };
    await ui.fill(pageB, mine);
    await ui.clickSave(pageB);
    await ui.waitForStatus(pageB, '409', 'err');
    let s = await ui.state(pageB);
    assert.equal(s.statusKind, 'err');
    assert.deepEqual(s.form, mine); // 全部输入原样保留，未被清空或重置
    assert.equal(s.formEditing, true);
    assert.equal(s.bannerId, a.id);
    assert.equal(s.conflictHidden, false);
    const conflict = await ui.getConflict(pageB);
    assert.deepEqual(conflict.mine, { heading: '你正在编辑的内容（未保存）', ...mine });
    assert.deepEqual(conflict.saved, { heading: '最新已保存内容', ...v1 });
    // 对照中的类 HTML 文本按纯文本呈现，不执行任何标记。
    assert.equal(await pageB.eval(`document.querySelector('#conflict-box .ver.saved dd b')`), null);
    assert.equal(await pageB.eval(`document.querySelector('#conflict-box .ver.saved dd i')`), null);
    // 冲突本身不改变服务端：仍是版本 2、只有一篇记录。
    assert.equal((await (await server.api(`/api/articles/${a.id}`)).json()).article.version, 2);
    assert.equal((await server.listArticles()).length, 1);

    // 对照显示之后，页面甲又保存了第二版（版本 3）——对照内容就此过时。
    const v2 = {
      title: '甲页第二版标题',
      summary: '第二版摘要 <b>按文本显示</b>',
      body: '第二版正文第一段\n\n空行保留\n\n中文 English café 日本語 العربية\n\n<html>不执行标记</html>',
    };
    await ui.fill(pageA, v2);
    await ui.clickSave(pageA);
    await waitForServerVersion(server, a.id, 3);

    // 页面乙点击“放弃当前输入并载入最新内容”：得到的是本次读取时实际保存
    // 的标题、摘要、完整正文与版本（v2/版本 3），不是先前对照里的 v1。
    await ui.clickConflictDiscard(pageB);
    await ui.waitLoadedDraft(pageB);
    s = await ui.state(pageB);
    assert.deepEqual(s.form, v2);
    assert.deepEqual(await ui.confirmCalls(pageB), []); // 等待期间无新修改，直接载入
    // 仍在编辑同一篇草稿：标识与列表都反映载入结果。
    assert.equal(s.formEditing, true);
    assert.equal(s.bannerId, a.id);
    assert.equal(s.bannerTitle, v2.title);
    assert.equal(s.saveBtnText, '保存修改');
    assert.equal(s.conflictHidden, true);
    assert.equal(s.listCount, 1);
    assert.equal(s.cards[0].title, v2.title);
    assert.equal(s.cards[0].summary, v2.summary);
    assert.equal(s.cards[0].editing, true);
    // 列表中的类 HTML 摘要按纯文本呈现。
    assert.equal(await pageB.eval(`document.querySelector('#article-list .summary b')`), null);
    // 载入本身不保存：版本仍是 3，仍只有一篇。
    let saved = await (await server.api(`/api/articles/${a.id}`)).json();
    assert.equal(saved.article.version, 3);
    assert.equal((await server.listArticles()).length, 1);

    // 载入后继续修改并保存：更新原文章（标识、创建时间、状态不变），
    // 版本基准来自本次读取，不因沿用旧版本而误报冲突。
    await ui.fill(pageB, { body: `${v2.body}\n\n乙页载入后补充的一行` });
    await ui.clickSave(pageB);
    await ui.waitForStatus(pageB, '修改已保存', 'ok');
    s = await ui.state(pageB);
    assert.equal(s.conflictHidden, true);
    assert.equal(s.bannerId, a.id);
    assert.equal(s.listCount, 1);
    saved = await (await server.api(`/api/articles/${a.id}`)).json();
    assert.equal(saved.article.id, a.id);
    assert.equal(saved.article.createdAt, a.createdAt);
    assert.equal(saved.article.status, 'draft');
    assert.equal(saved.article.version, 4);
    assert.equal(saved.article.title, v2.title);
    assert.equal(saved.article.body, `${v2.body}\n\n乙页载入后补充的一行`);
    assert.equal((await server.listArticles()).length, 1);
  } finally {
    if (pageA) { ui.assertNoPageErrors(pageA); await browser.closePage(pageA); }
    if (pageB) { ui.assertNoPageErrors(pageB); await browser.closePage(pageB); }
    await server.stop();
  }
});

// ---------------------------------------------------------------------------
// 9. During the load wait the form is not cleared early and stays editable;
//    edits reverted to the read-start values count as "no new modification",
//    so the read result loads directly with no confirm.
// ---------------------------------------------------------------------------
test('载入等待期间无新修改（含改回原值）：表单不提前清空、显示读取中、直接载入不询问', async (browser) => {
  const ctx = await freshContext(browser);
  const { server, page } = ctx;
  try {
    const { a } = await reachStaleConflict(server, page);
    // 对照里还是 OTHER_V1，服务端实际已是 LATEST（版本 3）。
    const before = await ui.getConflict(page);
    assert.equal(before.saved.title, OTHER_V1.title);

    await ui.readGateOn(page);
    await ui.clickConflictDiscard(page);
    await ui.waitReadPending(page, 1);

    // 等待期间：表单不提前清空，仍是自己的输入；页面显示正在读取；
    // 保存与重复载入暂时不可用。
    let s = await ui.state(page);
    assert.deepEqual(s.form, MINE);
    assert.match(s.statusText, /正在读取/);
    assert.equal(s.saveDisabled, true);
    const parked = await ui.getConflict(page);
    assert.equal(parked.discard.disabled, true);
    assert.equal(parked.keep.disabled, true);
    // 重复点击载入不会产生第二次读取。
    await ui.clickConflictDiscard(page);
    assert.deepEqual(await ui.readGateState(page), { enabled: true, hits: 1, pending: 1 });

    // 三个字段都改过、又都恢复为开始读取时的值：不算新修改。
    await ui.fill(page, { title: '临时标题', summary: '临时摘要', body: '临时正文' });
    await ui.fill(page, MINE);
    await ui.releaseReads(page);
    await ui.readGateOff(page);

    await ui.waitLoadedDraft(page);
    s = await ui.state(page);
    assert.deepEqual(await ui.confirmCalls(page), []); // 没有询问，直接载入
    // 载入的是本次读取时实际保存的最新内容，不是对照里的旧内容。
    assert.deepEqual(s.form, LATEST);
    assert.equal(s.bannerId, a.id);
    assert.equal(s.bannerTitle, LATEST.title);
    assert.equal(s.conflictHidden, true);
    assert.equal(s.cards[0].title, LATEST.title);
    assert.equal(s.cards[0].summary, LATEST.summary);
    assert.equal(s.cards[0].editing, true);
    assert.equal(await page.eval(`document.querySelector('#article-list .summary b')`), null);
    // 载入本身不保存：版本仍 3、仍一篇。
    assert.equal((await (await server.api(`/api/articles/${a.id}`)).json()).article.version, 3);
    assert.equal((await server.listArticles()).length, 1);

    // 载入后以新版本为基准：再保存更新同一篇，不报 409。
    await ui.fill(page, { summary: '载入后修改的摘要' });
    await ui.clickSave(page);
    await ui.waitForStatus(page, '修改已保存', 'ok');
    const saved = await (await server.api(`/api/articles/${a.id}`)).json();
    assert.equal(saved.article.version, 4);
    assert.equal(saved.article.id, a.id);
    assert.equal(saved.article.createdAt, a.createdAt);
    assert.equal(saved.article.summary, '载入后修改的摘要');
    assert.equal(saved.article.body, LATEST.body);
    s = await ui.state(page);
    assert.equal(s.conflictHidden, true);
    assert.equal(s.listCount, 1);
  } finally {
    await closeContext(ctx, browser);
  }
});

// ---------------------------------------------------------------------------
// 10. New modifications during the load wait (including clearing summary and
//     body) must be confirmed first. Choosing 保留 keeps every input, the
//     editing target and the old version baseline exactly as they were.
// ---------------------------------------------------------------------------
test('载入等待期间有新修改（含清空摘要正文）：选择保留则输入、编辑对象与版本基准原状不动', async (browser) => {
  const ctx = await freshContext(browser);
  const { server, page } = ctx;
  try {
    const { a } = await reachStaleConflict(server, page);
    await ui.readGateOn(page);
    await ui.clickConflictDiscard(page);
    await ui.waitReadPending(page, 1);
    // 等待期间表单不被提前清空。
    assert.deepEqual((await ui.state(page)).form, MINE);

    // 等待期间的新修改：改标题，并清空摘要和正文（清空同样受保护）。
    const kept = { title: '等待期间改定的标题', summary: '', body: '' };
    await ui.fill(page, kept);
    await ui.confirmNext(page, false); // 用户选择“保留当前输入”
    await ui.releaseReads(page);
    await ui.readGateOff(page);

    await ui.waitForStatus(page, '已保留当前输入');
    let s = await ui.state(page);
    const calls = await ui.confirmCalls(page);
    assert.equal(calls.length, 1);
    assert.match(calls[0], /放弃/); // 先说明载入将放弃这些新修改
    // 全部输入（含被清空的摘要、正文）一个字符不动。
    assert.deepEqual(s.form, kept);
    // 原编辑对象不变，冲突操作恢复可用。
    assert.equal(s.formEditing, true);
    assert.equal(s.bannerId, a.id);
    assert.equal(s.saveBtnText, '保存修改');
    assert.equal(s.saveDisabled, false);
    const conflict = await ui.getConflict(page);
    assert.equal(conflict.hidden, false);
    assert.equal(conflict.discard.disabled, false);
    // 读取与保留选择本身不保存：版本仍 3、仍一篇。
    assert.equal((await (await server.api(`/api/articles/${a.id}`)).json()).article.version, 3);
    assert.equal((await server.listArticles()).length, 1);

    // 原版本基准保持原状：再次保存仍按打开时的旧版本提交，
    // 继续收到 409 而不是用新读取的版本覆盖。
    await ui.clickSave(page);
    await ui.waitForStatus(page, '409', 'err');
    s = await ui.state(page);
    assert.deepEqual(s.form, kept); // 输入仍然保留
    const again = await ui.getConflict(page);
    assert.equal(again.hidden, false);
    assert.equal(again.saved.title, LATEST.title);
    assert.equal(again.saved.body, LATEST.body);
    assert.equal((await (await server.api(`/api/articles/${a.id}`)).json()).article.version, 3);
    assert.equal((await server.listArticles()).length, 1);
  } finally {
    await closeContext(ctx, browser);
  }
});

// ---------------------------------------------------------------------------
// 11. Choosing 放弃 after new modifications during the wait loads the freshly
//     read content and continues editing the same draft on its new baseline.
// ---------------------------------------------------------------------------
test('载入等待期间有新修改：选择放弃则以本次读取的完整内容继续编辑同一篇', async (browser) => {
  const ctx = await freshContext(browser);
  const { server, page } = ctx;
  try {
    const { a } = await reachStaleConflict(server, page);
    await ui.readGateOn(page);
    await ui.clickConflictDiscard(page);
    await ui.waitReadPending(page, 1);
    await ui.fill(page, { body: `${MINE.body}\n\n等待期间又补一段` });
    await ui.confirmNext(page, true); // 用户选择“放弃新修改并载入”
    await ui.releaseReads(page);
    await ui.readGateOff(page);

    await ui.waitLoadedDraft(page);
    let s = await ui.state(page);
    assert.equal((await ui.confirmCalls(page)).length, 1);
    // 以本次读取的完整内容（含多语言、空行、类 HTML 文本）继续编辑。
    assert.deepEqual(s.form, LATEST);
    assert.equal(s.bannerId, a.id);
    assert.equal(s.bannerTitle, LATEST.title);
    assert.equal(s.cards[0].title, LATEST.title);
    assert.equal(s.cards[0].editing, true);
    assert.equal(s.conflictHidden, true);
    // 放弃与载入本身不保存、不新增记录、不改变版本。
    assert.equal((await (await server.api(`/api/articles/${a.id}`)).json()).article.version, 3);
    assert.equal((await server.listArticles()).length, 1);

    // 以载入内容为基准继续编辑保存：更新同一篇，版本从 3 推进，不误报冲突。
    await ui.fill(page, { title: '放弃后改定的标题' });
    await ui.clickSave(page);
    await ui.waitForStatus(page, '修改已保存', 'ok');
    const saved = await (await server.api(`/api/articles/${a.id}`)).json();
    assert.equal(saved.article.version, 4);
    assert.equal(saved.article.id, a.id);
    assert.equal(saved.article.createdAt, a.createdAt);
    assert.equal(saved.article.title, '放弃后改定的标题');
    assert.equal(saved.article.body, LATEST.body);
    s = await ui.state(page);
    assert.equal(s.conflictHidden, true);
    assert.equal(s.listCount, 1);
  } finally {
    await closeContext(ctx, browser);
  }
});

// ---------------------------------------------------------------------------
// 12. The reload read fails (draft gone, then unreadable data): the concrete
//     reason is shown, inputs present at response arrival and the original
//     editing state are kept, actions become available again, and there is no
//     false success nor fallback to create mode. Recovery then works.
// ---------------------------------------------------------------------------
test('载入最新内容读取失败（不存在/读取错误）：显示原因、保留输入与编辑状态、恢复可操作', async (browser) => {
  const ctx = await freshContext(browser);
  const { server, page } = ctx;
  try {
    const a = await seedAndReload(server, page, SEED);
    await ui.clickEdit(page, SEED.title);
    await ui.waitLoadedDraft(page);
    const v2 = await putArticle(server, a.id, OTHER_V1, 1); // 另一页保存 → version 2
    await ui.fill(page, MINE);
    await ui.clickSave(page);
    await ui.waitForStatus(page, '409', 'err');

    const dataFile = join(server.dataDir, 'articles.json');
    // 情形一：草稿已不存在（另一页面/进程删掉了它）。
    writeFileSync(dataFile, '[]\n');
    await ui.readGateOn(page);
    await ui.clickConflictDiscard(page);
    await ui.waitReadPending(page, 1);
    // 响应到达前又改了输入：这些当前值必须原样保留。
    const atResponse = { title: '响应到达时的标题', summary: '响应到达时的摘要', body: '响应到达时的正文' };
    await ui.fill(page, atResponse);
    await ui.releaseReads(page);
    await ui.readGateOff(page);

    await ui.waitForStatus(page, '载入最新内容失败', 'err');
    let s = await ui.state(page);
    assert.ok(s.statusText.includes('article not found'), `应显示具体失败原因: ${s.statusText}`);
    assert.ok(!s.statusText.includes('已载入'), '不能显示载入成功');
    assert.deepEqual(s.form, atResponse); // 响应到达时的输入原样保留
    // 原编辑状态不变，不退回新建状态。
    assert.equal(s.formEditing, true);
    assert.equal(s.bannerHidden, false);
    assert.equal(s.bannerId, a.id);
    assert.equal(s.saveBtnText, '保存修改');
    assert.equal(s.saveDisabled, false);
    assert.equal(s.cancelHidden, false);
    // 冲突操作按钮恢复可用，可稍后重试。
    let conflict = await ui.getConflict(page);
    assert.equal(conflict.hidden, false);
    assert.equal(conflict.discard.disabled, false);
    assert.deepEqual(await ui.confirmCalls(page), []);
    assert.deepEqual(await server.listArticles(), []); // 不新增记录

    // 情形二：读取本身失败（数据损坏 → 500）。
    writeFileSync(dataFile, '{ 不是合法 JSON');
    await ui.clickConflictDiscard(page);
    await ui.waitForStatus(page, 'unable to read', 'err');
    s = await ui.state(page);
    assert.ok(s.statusText.includes('载入最新内容失败'));
    assert.deepEqual(s.form, atResponse);
    assert.equal(s.formEditing, true);
    assert.equal(s.bannerId, a.id);
    assert.equal(s.saveBtnText, '保存修改');
    assert.equal(s.saveDisabled, false);

    // 数据恢复后重试：正常载入另一页保存的版本 2，之后保存推进到 3。
    writeFileSync(dataFile, `${JSON.stringify([v2], null, 2)}\n`);
    await ui.clickConflictDiscard(page);
    await ui.waitLoadedDraft(page);
    s = await ui.state(page);
    assert.deepEqual(s.form, OTHER_V1);
    assert.equal(s.bannerId, a.id);
    await ui.fill(page, { body: `${OTHER_V1.body}\n\n恢复后补充` });
    await ui.clickSave(page);
    await ui.waitForStatus(page, '修改已保存', 'ok');
    const saved = await (await server.api(`/api/articles/${a.id}`)).json();
    assert.equal(saved.article.version, 3);
    assert.equal(saved.article.id, a.id);
    assert.equal(saved.article.createdAt, a.createdAt);
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
