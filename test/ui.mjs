// Page-level helpers for the draft-save regression tests.
//
// The core of every scenario is a race the product must survive: the user
// clicks save (or asks to reload the latest saved content), then keeps typing
// while the request is in flight. Real network round-trips are too fast to
// type against reliably, so before the page's own scripts run we install two
// independent gates on window.fetch:
//   - save gate: holds POST/PUT /api/articles calls until the test releases;
//   - read gate: holds GET /api/articles/:id calls (the single-article read
//     used by “编辑” and by the post-conflict “载入最新内容” button);
//   - list gate: parks GET /api/articles (the homepage list read) in two
//     stages — the request is released so the server snapshots its state at a
//     chosen moment, and the response is delivered later — so the race “save
//     returns first, the pre-save list snapshot arrives afterwards” is
//     reproduced deterministically. It can also fail the list read once.
// The list gate is off unless the page was opened with openHoldingList.
// A one-shot failure arm can also make the next single-article read fail
// (404/500/network) without any test-only branch in the product.
//
// window.confirm is replaced by a controllable double (accept/cancel/native),
// because the reload flow asks the user to choose between keeping the current
// input and discarding it. The product itself has no test-only code paths.
const INIT_SOURCE = `(function () {
  if (window.__qtest) return;
  var origFetch = window.fetch.bind(window);
  var saveGate = { enabled: false, hits: 0, pending: [] };
  var readGate = { enabled: false, hits: 0, pending: [] };
  // null = off; 404/500 = respond with that status once; 'network' = reject once.
  var failNextRead = null;
  // List gate: requests park in the requests queue; releaseList() issues the
  // real fetch (the server snapshots its state at that moment) and parks the
  // outcome in the responses queue; deliverList() then hands it to the page.
  // The answered counter tracks settled outcomes so tests can wait for the
  // server to have produced the snapshot before saving again.
  var listGate = {
    enabled: !!window.__qtestHoldList,
    hits: 0, requests: [], responses: [], answered: 0
  };
  // null = off; 404/500 = respond with that status once; 'network' = reject once.
  var failNextList = null;

  function methodOf(init) { return ((init && init.method) || 'GET').toUpperCase(); }
  function pathOf(input) {
    var url = typeof input === 'string' ? input : ((input && input.url) || '');
    return url.split('?')[0];
  }
  function isSaveCall(path, method) {
    if (method !== 'POST' && method !== 'PUT') return false;
    return path === '/api/articles' || /^\\/api\\/articles\\/[^/]+$/.test(path);
  }
  function isItemRead(path, method) {
    return method === 'GET' && /^\\/api\\/articles\\/[^/]+$/.test(path);
  }
  function isListRead(path, method) {
    return method === 'GET' && path === '/api/articles';
  }
  function hold(gate, input, init) {
    gate.hits++;
    return new Promise(function (resolve) {
      gate.pending.push(function () { resolve(origFetch(input, init)); });
    });
  }

  function failureResponse(mode) {
    var body = JSON.stringify({
      error: mode === 404 ? '模拟失败：该草稿已不存在' : '模拟失败：单篇读取暂时不可用'
    });
    return new Response(body, {
      status: mode,
      headers: { 'content-type': 'application/json' }
    });
  }
  // A single-article read may be parked by the read gate and/or fail once.
  // When both are armed the call parks; the failure is consumed at release so
  // the test can type during the wait before the error response arrives.
  function runItemRead(input, init) {
    if (failNextRead !== null) {
      var mode = failNextRead;
      failNextRead = null;
      if (mode === 'network') return Promise.reject(new Error('模拟的网络连接中断'));
      return Promise.resolve(failureResponse(mode));
    }
    return origFetch(input, init);
  }

  function listFailure(mode) {
    if (mode === 'network') return Promise.reject(new Error('模拟的列表网络连接中断'));
    return Promise.resolve(new Response(
      JSON.stringify({ error: '模拟失败：列表读取暂时不可用' }),
      { status: mode, headers: { 'content-type': 'application/json' } }));
  }
  // The list read parks before the request is even sent. releaseList() sends
  // it (or consumes an armed failure) and parks the outcome; deliverList()
  // resolves the page-visible promise. Splitting the two lets a test save a
  // draft after the server produced its (pre-save) snapshot but before the
  // page receives it — the exact race the homepage must survive.
  function parkListRead(input, init) {
    listGate.hits++;
    return new Promise(function (resolve, reject) {
      listGate.requests.push(function () {
        var outcome = failNextList !== null
          ? listFailure(failNextList)
          : origFetch(input, init);
        failNextList = null;
        outcome.then(
          function () { listGate.answered++; },
          function () { listGate.answered++; });
        // The held outcome is only delivered by deliverList(); swallow
        // rejections until then so a parked failure stays unobserved.
        outcome.catch(function () {});
        listGate.responses.push(function () { outcome.then(resolve, reject); });
      });
    });
  }

  window.fetch = function (input, init) {
    var path = pathOf(input);
    var method = methodOf(init);
    if (isItemRead(path, method)) {
      if (readGate.enabled) {
        readGate.hits++;
        return new Promise(function (resolve) {
          readGate.pending.push(function () { resolve(runItemRead(input, init)); });
        });
      }
      return runItemRead(input, init);
    }
    if (saveGate.enabled && isSaveCall(path, method)) return hold(saveGate, input, init);
    if (listGate.enabled && isListRead(path, method)) return parkListRead(input, init);
    return origFetch(input, init);
  };

  // Deterministic confirm double: 'accept' = 放弃当前输入, 'cancel' = 保留,
  // 'native' = fall through to the browser dialog (the harness auto-accepts
  // unexpected native dialogs and then fails the scenario).
  var nativeConfirm = typeof window.confirm === 'function' ? window.confirm.bind(window)
    : function () { return true; };
  var confirmBox = { mode: 'native', calls: [] };
  window.confirm = function (message) {
    confirmBox.calls.push(String(message));
    if (confirmBox.mode === 'accept') return true;
    if (confirmBox.mode === 'cancel') return false;
    return nativeConfirm(String(message));
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
  function releaseGate(gate) {
    var pending = gate.pending;
    gate.pending = [];
    pending.forEach(function (run) { run(); });
    return pending.length;
  }
  function gateView(gate) {
    return { enabled: gate.enabled, hits: gate.hits, pending: gate.pending.length };
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
    clickConflictAction: function (label) {
      var btns = document.querySelectorAll('#conflict-box .actions button');
      for (var i = 0; i < btns.length; i++) {
        if (btns[i].textContent === label) { btns[i].click(); return true; }
      }
      return false;
    },
    gateOn: function () { saveGate.enabled = true; saveGate.hits = 0; },
    gateOff: function () { saveGate.enabled = false; },
    release: function () { return releaseGate(saveGate); },
    gateState: function () { return gateView(saveGate); },
    readGateOn: function () { readGate.enabled = true; readGate.hits = 0; },
    readGateOff: function () { readGate.enabled = false; },
    releaseReads: function () { return releaseGate(readGate); },
    readGateState: function () { return gateView(readGate); },
    armReadFailure: function (mode) { failNextRead = mode; },
    readFailureArmed: function () { return failNextRead; },
    releaseList: function () {
      var queued = listGate.requests;
      listGate.requests = [];
      queued.forEach(function (send) { send(); });
      return queued.length;
    },
    deliverList: function () {
      var queued = listGate.responses;
      listGate.responses = [];
      queued.forEach(function (deliver) { deliver(); });
      return queued.length;
    },
    listGateState: function () {
      return {
        enabled: listGate.enabled,
        hits: listGate.hits,
        requests: listGate.requests.length,
        responses: listGate.responses.length,
        answered: listGate.answered
      };
    },
    armListFailure: function (mode) { failNextList = mode; },
    listFailureArmed: function () { return failNextList; },
    confirmMode: function (mode) { confirmBox.mode = mode; },
    confirmReset: function () { confirmBox.calls = []; },
    confirmState: function () {
      return { mode: confirmBox.mode, calls: confirmBox.calls.slice() };
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
      fieldsEditable: {
        title: !byId('title').disabled,
        summary: !byId('summary').disabled,
        body: !byId('body').disabled
      },
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

// The initial list read has settled (success or failure — both replace the
// “加载中” placeholder; a failure message never contains that substring).
export async function waitListSettled(page) {
  await page.waitFor(
    'function(){return !document.getElementById("empty-tip").textContent.includes("加载中");}',
    { label: 'draft list settled' });
}

async function openPage(browser, baseUrl, holdList) {
  const page = await browser.newPage();
  await page.enable();
  // Arms the list gate before the page's own scripts run, so the very first
  // list read is parked. Init scripts run in order of registration.
  if (holdList) await page.addInitScript('window.__qtestHoldList = true;');
  await page.addInitScript(INIT_SOURCE);
  await page.goto(baseUrl);
  if (!holdList) await waitListSettled(page);
  return page;
}

export function open(browser, baseUrl) {
  return openPage(browser, baseUrl, false);
}

// Opens the homepage with the initial list read parked in the list gate:
// the page shows “草稿加载中…” until the test releases and delivers it.
export function openHoldingList(browser, baseUrl) {
  return openPage(browser, baseUrl, true);
}

export async function reload(page) {
  await page.reload();
  await waitListSettled(page);
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

// Buttons inside the conflict comparison box.
export async function clickConflictAction(page, label) {
  const clicked = await page.eval(`__qtest.clickConflictAction(${JSON.stringify(label)})`);
  if (!clicked) throw new Error(`no conflict action button ${JSON.stringify(label)}`);
}

export const DISCARD_AND_LOAD = '放弃当前输入并载入最新内容';
export const KEEP_MINE = '保留我的输入，继续修改';
export async function clickDiscardLoad(page) {
  await clickConflictAction(page, DISCARD_AND_LOAD);
}

export async function gateOn(page) { await page.eval(`__qtest.gateOn()`); }
export async function gateOff(page) { await page.eval(`__qtest.gateOff()`); }
export async function releaseSaves(page) { return page.eval(`__qtest.release()`); }
export function gateState(page) { return page.eval(`__qtest.gateState()`); }

// Single-article read gate (GET /api/articles/:id).
export async function readGateOn(page) { await page.eval(`__qtest.readGateOn()`); }
export async function readGateOff(page) { await page.eval(`__qtest.readGateOff()`); }
export async function releaseReads(page) { return page.eval(`__qtest.releaseReads()`); }
export function readGateState(page) { return page.eval(`__qtest.readGateState()`); }

// Make the next single-article GET fail once: 404, 500 or 'network'.
export async function armReadFailure(page, mode) {
  await page.eval(`__qtest.armReadFailure(${JSON.stringify(mode)})`);
}
export function readFailureArmed(page) {
  return page.eval(`__qtest.readFailureArmed()`);
}

// List gate (GET /api/articles): releaseList sends parked requests so the
// server snapshots its state at that moment; deliverList hands the parked
// responses to the page.
export async function releaseListRequests(page) { return page.eval(`__qtest.releaseList()`); }
export async function deliverListResponses(page) { return page.eval(`__qtest.deliverList()`); }
export function listGateState(page) { return page.eval(`__qtest.listGateState()`); }

// Make the next list read fail once when its request is released: 404, 500
// or 'network'.
export async function armListFailure(page, mode) {
  await page.eval(`__qtest.armListFailure(${JSON.stringify(mode)})`);
}

// The initial list read is parked in the gate (page still shows “加载中”).
export async function waitListParked(page, count = 1) {
  await page.waitFor(
    `function(){var g=__qtest.listGateState();return g.requests===${count}
      && document.getElementById('empty-tip').textContent.includes('加载中');}`,
    { timeout: 5000, label: `list read #${count} parked while page shows 加载中` });
}

// The released list request has been answered by the server (its snapshot is
// fixed) and the response is parked awaiting delivery.
export async function waitListAnswered(page, count = 1) {
  await page.waitFor(
    `function(){var g=__qtest.listGateState();return g.answered===${count} && g.responses===${count};}`,
    { timeout: 5000, label: `list response #${count} answered and parked` });
}

// Waits a few task turns so the page finishes processing a just-delivered
// list response whose correct visible outcome may be identical to the prior
// state (e.g. a stale empty snapshot must leave the saved draft untouched).
export async function settlePage(page) {
  await page.eval(`new Promise(function (resolve) {
    var turns = 0;
    function next() { if (++turns >= 6) resolve(true); else setTimeout(next, 0); }
    next();
  })`);
}

// Control the page's window.confirm double.
export async function setConfirm(page, mode) {
  await page.eval(`__qtest.confirmMode(${JSON.stringify(mode)})`);
}
export async function resetConfirm(page) {
  await page.eval(`__qtest.confirmReset()`);
}
export function confirmState(page) {
  return page.eval(`__qtest.confirmState()`);
}

// The save request is parked in the browser and the UI shows its in-flight state.
export async function waitSavingPending(page, count = 1) {
  await page.waitFor(
    `function(){var g=__qtest.gateState();return g.pending===${count} && __snap().saveDisabled && __snap().saveBtnText==='保存中…';}`,
    { timeout: 5000, label: `save request #${count} parked while button shows 保存中…` });
}

// The single-article read triggered by the conflict “载入最新内容” button is
// parked; the page shows the reading state without clearing the form.
export async function waitReadPending(page, count = 1) {
  await page.waitFor(
    `function(){var g=__qtest.readGateState();return g.pending===${count} && __snap().saveDisabled && __snap().saveBtnText==='读取中…';}`,
    { timeout: 5000, label: `article read #${count} parked while button shows 读取中…` });
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

// The 409 comparison box is visible with its two action buttons.
export async function waitConflictShown(page) {
  await page.waitFor(
    `function(){return !__snap().conflictHidden
      && document.querySelectorAll('#conflict-box .ver.mine dd').length===3
      && document.querySelectorAll('#conflict-box .ver.saved dd').length===3
      && document.querySelectorAll('#conflict-box .actions button').length===2;}`,
    { timeout: 5000, label: 'conflict comparison box with both action buttons' });
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
    return {
      hidden: box.hidden,
      heading: box.querySelector('h3') ? box.querySelector('h3').textContent : '',
      mine: column('.ver.mine'),
      saved: column('.ver.saved'),
      actions: Array.prototype.map.call(
        box.querySelectorAll('.actions button'),
        function (btn) { return { text: btn.textContent, disabled: btn.disabled }; })
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
