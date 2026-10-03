// Page-level helpers for the draft-save regression tests.
//
// The core of every scenario is the race the product must survive: the user
// clicks save, then keeps typing while the request is in flight. Real network
// round-trips are too fast to type against reliably, so before the page's own
// scripts run we install a tiny gate on window.fetch: a save call (POST/PUT to
// /api/articles) can be held inside the browser until the test releases it.
// Everything else (list reads, article reads) passes through untouched. The
// product itself has no test-only code paths.
const INIT_SOURCE = `(function () {
  if (window.__qtest) return;
  var gate = { enabled: false, hits: 0, pending: [] };
  // A second, independently controlled gate for single-article reads
  // (GET /api/articles/:id). It holds "放弃当前输入并载入最新内容" (and 编辑
  // opens) in flight so tests can type while the read is pending. List reads
  // and everything else pass through untouched.
  var readGate = { enabled: false, hits: 0, pending: [] };
  // The product asks window.confirm before discarding in-flight edits made
  // while a read was pending. Tests queue the user's answer; calls made with
  // no queued answer fall through to the native dialog, so an unexpected
  // guard still surfaces via page.dialogs and fails the scenario.
  var confirmResponses = [];
  var confirmLog = [];
  var origConfirm = window.confirm.bind(window);
  window.confirm = function (message) {
    confirmLog.push(String(message));
    if (confirmResponses.length) return confirmResponses.shift();
    return origConfirm(message);
  };
  var origFetch = window.fetch.bind(window);
  function isSaveCall(input, init) {
    var url = typeof input === 'string' ? input : ((input && input.url) || '');
    var path = url.split('?')[0];
    var method = ((init && init.method) || 'GET').toUpperCase();
    if (method !== 'POST' && method !== 'PUT') return false;
    return path === '/api/articles' || /^\\/api\\/articles\\//.test(path);
  }
  function isArticleRead(input, init) {
    var url = typeof input === 'string' ? input : ((input && input.url) || '');
    var path = url.split('?')[0];
    var method = ((init && init.method) || 'GET').toUpperCase();
    return method === 'GET' && /^\\/api\\/articles\\/[^/]+$/.test(path);
  }
  window.fetch = function (input, init) {
    if (gate.enabled && isSaveCall(input, init)) {
      gate.hits++;
      return new Promise(function (resolve) {
        gate.pending.push(function () { resolve(origFetch(input, init)); });
      });
    }
    if (readGate.enabled && isArticleRead(input, init)) {
      readGate.hits++;
      return new Promise(function (resolve) {
        readGate.pending.push(function () { resolve(origFetch(input, init)); });
      });
    }
    return origFetch(input, init);
  };
  function byId(id) { return document.getElementById(id); }
  // Go through the prototype value setter so React-style change detection and
  // the page's input listeners see a genuine user edit.
  function setValue(id, value) {
    var node = byId(id);
    var proto = node.tagName === 'TEXTAREA'
      ? window.HTMLTextAreaElement.prototype
      : window.HTMLInputElement.prototype;
    Object.getOwnPropertyDescriptor(proto, 'value').set.call(node, value);
    node.dispatchEvent(new Event('input', { bubbles: true }));
    node.dispatchEvent(new Event('change', { bubbles: true }));
  }
  window.__qtest = {
    set: function (id, value) { setValue(id, value); },
    click: function (id) { byId(id).click(); },
    clickEdit: function (title) {
      var cards = document.querySelectorAll('#article-list article.draft');
      for (var i = 0; i < cards.length; i++) {
        var h3 = cards[i].querySelector('h3');
        var text = h3 && h3.firstChild ? h3.firstChild.nodeValue : '';
        if (text === title) { cards[i].querySelector('button').click(); return true; }
      }
      return false;
    },
    gateOn: function () { gate.enabled = true; gate.hits = 0; },
    gateOff: function () { gate.enabled = false; },
    release: function () {
      var pending = gate.pending;
      gate.pending = [];
      pending.forEach(function (run) { run(); });
      return pending.length;
    },
    gateState: function () {
      return { enabled: gate.enabled, hits: gate.hits, pending: gate.pending.length };
    },
    readGateOn: function () { readGate.enabled = true; readGate.hits = 0; },
    readGateOff: function () { readGate.enabled = false; },
    releaseReads: function () {
      var pending = readGate.pending;
      readGate.pending = [];
      pending.forEach(function (run) { run(); });
      return pending.length;
    },
    readGateState: function () {
      return { enabled: readGate.enabled, hits: readGate.hits, pending: readGate.pending.length };
    },
    confirmNext: function (response) { confirmResponses.push(!!response); },
    confirmCalls: function () { return confirmLog.slice(); },
    clickConflictDiscard: function () {
      var btn = document.querySelector('#conflict-box .actions button.danger');
      if (!btn) return false;
      btn.click();
      return true;
    }
  };
  window.__snap = function () {
    function v(id) { return document.getElementById(id).value; }
    var status = document.getElementById('status');
    var banner = document.getElementById('edit-banner');
    var cards = Array.prototype.map.call(
      document.querySelectorAll('#article-list article.draft'),
      function (card) {
        var h3 = card.querySelector('h3');
        var sum = card.querySelector('.summary');
        var btn = card.querySelector('.actions button');
        return {
          title: h3 && h3.firstChild ? h3.firstChild.nodeValue : '',
          summary: sum ? sum.textContent : null,
          editing: !!card.querySelector('.editing-tag'),
          editBtnText: btn ? btn.textContent : '',
          editBtnDisabled: btn ? btn.disabled : null
        };
      });
    var code = banner.querySelector('code');
    var meta = banner.querySelector('.meta');
    return {
      form: { title: v('title'), summary: v('summary'), body: v('body') },
      saveBtnText: document.getElementById('save-btn').textContent,
      saveDisabled: document.getElementById('save-btn').disabled,
      cancelHidden: document.getElementById('cancel-btn').hidden,
      formEditing: document.getElementById('draft-form').classList.contains('editing'),
      bannerHidden: banner.hidden,
      bannerTitle: code ? code.textContent : null,
      bannerId: meta ? (meta.textContent.split('标识：')[1] || '') : '',
      statusText: status.textContent,
      statusKind: (status.className.match(/\\b(ok|err|warn)\\b/) || [])[1] || '',
      conflictHidden: document.getElementById('conflict-box').hidden,
      cards: cards,
      listCount: cards.length,
      emptyTip: document.getElementById('empty-tip').textContent
    };
  };
})();`;

export async function open(browser, baseUrl) {
  const page = await browser.newPage();
  await page.enable();
  await page.addInitScript(INIT_SOURCE);
  await page.goto(baseUrl);
  // Initial list fetch has settled.
  await page.waitFor(
    'function(){return !document.getElementById("empty-tip").textContent.includes("加载中");}',
    { label: 'initial draft list settled' });
  return page;
}

export async function reload(page) {
  await page.reload();
  await page.waitFor(
    'function(){return !document.getElementById("empty-tip").textContent.includes("加载中");}',
    { label: 'draft list settled after reload' });
}

export async function fill(page, fields) {
  for (const [id, value] of Object.entries(fields)) {
    await page.eval(`__qtest.set(${JSON.stringify(id)}, ${JSON.stringify(value)})`);
  }
}

export async function clickSave(page) {
  await page.eval(`__qtest.click('save-btn')`);
}

export async function clickCancel(page) {
  await page.eval(`__qtest.click('cancel-btn')`);
}

export async function clickEdit(page, title) {
  const clicked = await page.eval(`__qtest.clickEdit(${JSON.stringify(title)})`);
  if (!clicked) throw new Error(`no draft card titled ${JSON.stringify(title)} to edit`);
}

export async function gateOn(page) { await page.eval(`__qtest.gateOn()`); }
export async function gateOff(page) { await page.eval(`__qtest.gateOff()`); }
export async function releaseSaves(page) { return page.eval(`__qtest.release()`); }

// The single-article read gate parks "载入最新内容" (and 编辑 opens) inside
// the browser so scenarios can keep typing while the read is in flight.
export async function readGateOn(page) { await page.eval(`__qtest.readGateOn()`); }
export async function readGateOff(page) { await page.eval(`__qtest.readGateOff()`); }
export async function releaseReads(page) { return page.eval(`__qtest.releaseReads()`); }
export function readGateState(page) { return page.eval(`__qtest.readGateState()`); }

// Queue the user's answer to the next confirm the product opens (true =
// 放弃并载入, false = 保留当前输入), and inspect what was asked.
export async function confirmNext(page, response) {
  await page.eval(`__qtest.confirmNext(${response ? 'true' : 'false'})`);
}
export function confirmCalls(page) { return page.eval(`__qtest.confirmCalls()`); }

export async function clickConflictDiscard(page) {
  const clicked = await page.eval(`__qtest.clickConflictDiscard()`);
  if (!clicked) throw new Error('no conflict discard button to click');
}

// The single-article read is parked in the browser and the UI shows its
// in-flight state (保存/取消 and the conflict actions temporarily disabled).
export async function waitReadPending(page, count = 1) {
  await page.waitFor(
    `function(){var g=__qtest.readGateState();return g.pending===${count} && __snap().saveDisabled && __snap().saveBtnText==='读取中…';}`,
    { timeout: 5000, label: `article read #${count} parked while button shows 读取中…` });
}

// The save request is parked in the browser and the UI shows its in-flight state.
export async function waitSavingPending(page, count = 1) {
  await page.waitFor(
    `function(){var g=__qtest.gateState();return g.pending===${count} && __snap().saveDisabled && __snap().saveBtnText==='保存中…';}`,
    { timeout: 5000, label: `save request #${count} parked while button shows 保存中…` });
}

export function state(page) {
  return page.eval(`__snap()`);
}

export async function waitForStatus(page, text, kind) {
  const kindCheck = kind ? ` && s.statusKind===${JSON.stringify(kind)}` : '';
  await page.waitFor(
    `function(){var s=__snap();return s.statusText.includes(${JSON.stringify(text)})${kindCheck};}`,
    { timeout: 8000, label: `status containing ${JSON.stringify(text)}${kind ? ` (${kind})` : ''}` });
}

export async function waitLoadedDraft(page) {
  await waitForStatus(page, '已载入该草稿当前保存的内容');
  await page.waitFor('function(){return !__snap().bannerHidden && __snap().saveBtnText==="保存修改";}',
    { label: 'editing banner and 保存修改 button' });
}

export async function getConflict(page) {
  return page.eval(`(function () {
    var box = document.getElementById('conflict-box');
    function column(selector) {
      var ver = box.querySelector(selector);
      if (!ver) return null;
      var dds = ver.querySelectorAll('dd');
      function val(dd) { return dd.classList.contains('empty-field') ? '' : dd.textContent; }
      return {
        heading: ver.querySelector('h4').textContent,
        title: val(dds[0]),
        summary: val(dds[1]),
        body: val(dds[2])
      };
    }
    function btnInfo(selector) {
      var btn = box.querySelector(selector);
      return btn ? { text: btn.textContent, disabled: btn.disabled } : null;
    }
    return {
      hidden: box.hidden,
      mine: column('.ver.mine'),
      saved: column('.ver.saved'),
      discard: btnInfo('.actions button.danger'),
      keep: btnInfo('.actions button.secondary')
    };
  })()`);
}

export function assertNoPageErrors(page) {
  if (page.pageErrors.length) {
    throw new Error(`uncaught page errors:\n${page.pageErrors.join('\n')}`);
  }
  const errors = page.consoleMessages.filter((m) => m.type === 'error');
  if (errors.length) {
    throw new Error(`console errors:\n${errors.map((m) => m.text).join('\n')}`);
  }
  if (page.dialogs.length) {
    throw new Error(`unexpected native confirm/alert dialogs (dirty-guard fired?):\n${page.dialogs.join('\n---\n')}`);
  }
}
