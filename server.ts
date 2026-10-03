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
form.editing{border-color:#175b9c;box-shadow:0 0 0 2px rgba(23,91,156,.15)}
label{display:block;font-weight:600;margin:.9rem 0 .3rem}
input[type=text],textarea{width:100%;box-sizing:border-box;font:inherit;padding:.5rem .6rem;border:1px solid #d0d7de;border-radius:6px;background:#fff;color:inherit}
textarea{resize:vertical}
button{margin-top:1.1rem;font:inherit;padding:.5rem 1.3rem;border-radius:6px;border:1px solid #175b9c;background:#175b9c;color:#fff;cursor:pointer}
button.secondary{background:#fff;color:#175b9c;margin-left:.6rem}
button:disabled{opacity:.55;cursor:default}
button.danger{border-color:#c0392b;background:#fff;color:#c0392b}
.status{margin:.8rem 0 0;min-height:1.4em}
.status.ok{color:#1a7f37}.status.err{color:#c0392b}.status.warn{color:#8a5a00}
.edit-banner{margin:0 0 .25rem;padding:.6rem .8rem;border-radius:6px;background:#eef4fb;border:1px solid #b6d2ee;color:#14467a;font-size:.92rem}
.edit-banner code{background:#dde9f6;padding:0 .3rem;border-radius:3px}
.edit-banner .meta{color:#3d6391;font-size:.85rem;margin-top:.2rem}
ul.articles{list-style:none;padding:0;margin:0}
article.draft{border:1px solid #d0d7de;border-radius:8px;padding:1rem 1.25rem;margin:1rem 0}
article.draft.editing{border-color:#175b9c;box-shadow:0 0 0 2px rgba(23,91,156,.15)}
.draft h3{margin:0 0 .4rem;word-break:break-word}
.draft .meta{color:#656d76;font-size:.85rem;margin:0 0 .5rem}
.draft .summary{white-space:pre-wrap;word-break:break-word;margin:0 0 .4rem;color:#3a4149}
.draft .actions{margin:.6rem 0 0}
.draft .actions button{margin:0;padding:.3rem .9rem;font-size:.88rem}
.draft .editing-tag{margin-left:.5rem;font-size:.78rem;font-weight:600;color:#175b9c;border:1px solid #175b9c;border-radius:999px;padding:0 .55rem;vertical-align:middle}
.empty{color:#656d76}
.empty.err{color:#c0392b}
.conflict{margin-top:1rem;border:1px solid #d9a441;border-radius:8px;background:#fffaf0;padding:1rem 1.25rem}
.conflict h3{margin:0 0 .3rem;color:#8a5a00;font-size:1.05rem}
.conflict .conflict-grid{display:grid;grid-template-columns:1fr 1fr;gap:1rem;margin-top:.7rem}
@media (max-width:46rem){.conflict .conflict-grid{grid-template-columns:1fr}}
.conflict .ver{border:1px solid #d0d7de;border-radius:6px;padding:.7rem .85rem;background:#fff;min-width:0}
.conflict .ver.saved{border-color:#8ab46a;background:#f6fbf2}
.conflict .ver h4{margin:0 0 .5rem;font-size:.92rem}
.conflict .ver dl{margin:0}
.conflict .ver dt{font-weight:600;font-size:.82rem;color:#57606a;margin-top:.5rem}
.conflict .ver dt:first-child{margin-top:0}
.conflict .ver dd{margin:.15rem 0 0;white-space:pre-wrap;word-break:break-word;font-size:.88rem;background:#f6f8fa;border-radius:4px;padding:.35rem .5rem;max-height:12rem;overflow:auto}
.conflict .ver.saved dd{background:#eef5e8}
.conflict .ver dd.empty-field{color:#8a8f98;font-style:italic;background:transparent;padding:0}
.conflict .actions{margin-top:.8rem}
.conflict .actions button{margin-top:0}
.conflict .actions .tip{font-size:.82rem;color:#8a5a00;margin-left:.6rem}
</style>
<main>
<h1>QuillPress</h1>
<p>内容编辑与刊物发布</p>

<section aria-labelledby="compose-heading">
<h2 id="compose-heading">保存草稿</h2>
<form id="draft-form" autocomplete="off">
  <div id="edit-banner" class="edit-banner" hidden></div>
  <label for="title">标题</label>
  <input id="title" name="title" type="text">
  <label for="summary">摘要（可选）</label>
  <textarea id="summary" name="summary" rows="3"></textarea>
  <label for="body">正文</label>
  <textarea id="body" name="body" rows="10"></textarea>
  <button id="save-btn" type="submit">保存草稿</button>
  <button id="cancel-btn" class="secondary" type="button" hidden>取消编辑</button>
  <p id="status" class="status" role="status" aria-live="polite"></p>
  <div id="conflict-box" class="conflict" hidden></div>
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
  var cancelBtn = document.getElementById('cancel-btn');
  var statusEl = document.getElementById('status');
  var editBanner = document.getElementById('edit-banner');
  var conflictBox = document.getElementById('conflict-box');
  var listEl = document.getElementById('article-list');
  var emptyTip = document.getElementById('empty-tip');
  var articles = [];
  var saving = false;
  // null = 新建草稿模式；对象 = 正在编辑的服务端版本 {id,title,summary,body,version}
  var editing = null;

  function setStatus(text, kind) {
    statusEl.textContent = text || '';
    statusEl.className = 'status' + (kind ? ' ' + kind : '');
  }
  function setEmpty(text, kind) {
    emptyTip.textContent = text || '';
    emptyTip.className = 'empty' + (kind ? ' ' + kind : '');
    emptyTip.hidden = !text;
  }
  function hideConflict() {
    conflictBox.hidden = true;
    conflictBox.textContent = '';
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
      var isEditing = editing && editing.id === article.id;
      var card = document.createElement('article');
      card.className = 'draft' + (isEditing ? ' editing' : '');
      var h = document.createElement('h3');
      h.textContent = article.title == null ? '' : String(article.title);
      if (isEditing) {
        var tag = document.createElement('span');
        tag.className = 'editing-tag';
        tag.textContent = '编辑中';
        h.appendChild(tag);
      }
      var meta = document.createElement('p');
      meta.className = 'meta';
      meta.textContent = '创建时间：' + formatTime(article.createdAt);
      var summary = document.createElement('p');
      summary.className = 'summary';
      var summaryText = article.summary == null ? '' : String(article.summary);
      summary.textContent = summaryText;
      card.appendChild(h);
      card.appendChild(meta);
      if (summaryText) card.appendChild(summary);
      var actions = document.createElement('p');
      actions.className = 'actions';
      var editBtn = document.createElement('button');
      editBtn.type = 'button';
      editBtn.className = 'secondary';
      editBtn.textContent = isEditing ? '正在编辑' : '编辑';
      editBtn.disabled = !!isEditing;
      editBtn.addEventListener('click', function () { startEdit(article.id); });
      actions.appendChild(editBtn);
      card.appendChild(actions);
      var li = document.createElement('li');
      li.appendChild(card);
      listEl.appendChild(li);
    });
    setEmpty(sorted.length === 0 ? '还没有保存的草稿。' : '');
  }
  function load() {
    return fetch('/api/articles', { headers: { accept: 'application/json' } })
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

  function setSaving(on, label) {
    saving = on;
    saveBtn.disabled = on;
    saveBtn.textContent = label || (editing ? '保存修改' : '保存草稿');
    cancelBtn.disabled = on;
  }

  function enterCreateMode(message, kind) {
    editing = null;
    form.reset();
    form.classList.remove('editing');
    editBanner.hidden = true;
    editBanner.textContent = '';
    hideConflict();
    cancelBtn.hidden = true;
    saveBtn.textContent = '保存草稿';
    setStatus(message || '', kind || '');
    render();
  }

  function buildEditBanner(article) {
    editBanner.innerHTML = '';
    var line1 = document.createElement('div');
    line1.textContent = '正在编辑草稿：';
    var code = document.createElement('code');
    code.textContent = String(article.title);
    line1.appendChild(code);
    var line2 = document.createElement('div');
    line2.className = 'meta';
    line2.textContent = '创建时间：' + formatTime(article.createdAt) + ' · 标识：' + String(article.id);
    editBanner.appendChild(line1);
    editBanner.appendChild(line2);
    editBanner.hidden = false;
  }

  function applyEditingState(article) {
    editing = {
      id: article.id,
      title: article.title,
      summary: article.summary,
      body: article.body,
      version: article.version
    };
    titleInput.value = article.title == null ? '' : String(article.title);
    summaryInput.value = article.summary == null ? '' : String(article.summary);
    bodyInput.value = article.body == null ? '' : String(article.body);
    form.classList.add('editing');
    buildEditBanner(article);
    cancelBtn.hidden = false;
    saveBtn.textContent = '保存修改';
    hideConflict();
    setStatus('已载入该草稿当前保存的内容，可以开始修改。', null);
    render();
    titleInput.focus();
  }

  function isDirty() {
    if (!editing) {
      return titleInput.value !== '' || summaryInput.value !== '' || bodyInput.value !== '';
    }
    return titleInput.value !== editing.title
      || summaryInput.value !== editing.summary
      || bodyInput.value !== editing.body;
  }

  function startEdit(id) {
    if (saving) return;
    if (isDirty() && !window.confirm('当前表单有未保存的修改，打开另一篇草稿将放弃这些输入。确定继续吗？')) {
      return;
    }
    setSaving(true, '读取中…');
    setStatus('正在读取草稿内容…', null);
    fetch('/api/articles/' + encodeURIComponent(id), { headers: { accept: 'application/json' } })
      .then(function (r) {
        return r.json().catch(function () { return null; }).then(function (data) {
          if (!r.ok || !data || !data.article) {
            throw new Error(data && data.error ? data.error : ('HTTP ' + r.status));
          }
          return data.article;
        });
      })
      .then(function (article) {
        setSaving(false);
        // 正文必须来自单篇接口，绝不使用列表里的摘要充当正文
        applyEditingState(article);
      })
      .catch(function (err) {
        setSaving(false);
        saveBtn.textContent = editing ? '保存修改' : '保存草稿';
        setStatus('打开草稿失败：' + (err && err.message ? err.message : err) + '，可稍后重试。', 'err');
      });
  }

  cancelBtn.addEventListener('click', function () {
    if (saving || !editing) return;
    // 取消不提交任何修改，也不删除原文章，直接回到新建草稿状态
    if (isDirty() && !window.confirm('取消将放弃当前未保存的修改，原草稿不会被删除。确定取消编辑吗？')) {
      return;
    }
    enterCreateMode('已取消编辑，原草稿未改动。可以新建草稿。', null);
  });

  function fieldValue(text, isEmpty) {
    var dd = document.createElement('dd');
    if (isEmpty) {
      dd.className = 'empty-field';
      dd.textContent = '（空）';
    } else {
      dd.textContent = text;
    }
    return dd;
  }

  function showConflict(saved) {
    // 只展示对照，绝不重置表单、绝不自动重试覆盖
    conflictBox.textContent = '';
    var h = document.createElement('h3');
    h.textContent = '检测到较新的已保存内容';
    var p = document.createElement('p');
    p.textContent = '这篇草稿在其他页面已被修改并保存。为避免覆盖新版本，本次保存已被拒绝。你输入的内容仍保留在上方表单中，可对照右侧最新版本；只有明确放弃当前输入后，才会载入最新内容继续编辑。';
    conflictBox.appendChild(h);
    conflictBox.appendChild(p);

    var grid = document.createElement('div');
    grid.className = 'conflict-grid';

    var mine = document.createElement('div');
    mine.className = 'ver mine';
    var mineTitle = document.createElement('h4');
    mineTitle.textContent = '你正在编辑的内容（未保存）';
    mine.appendChild(mineTitle);
    var dl1 = document.createElement('dl');
    [['标题', titleInput.value, titleInput.value.trim() === ''],
     ['摘要', summaryInput.value, summaryInput.value === ''],
     ['正文', bodyInput.value, bodyInput.value === '']].forEach(function (pair) {
      var dt = document.createElement('dt');
      dt.textContent = pair[0];
      dl1.appendChild(dt);
      dl1.appendChild(fieldValue(pair[1], pair[2]));
    });
    mine.appendChild(dl1);

    var theirs = document.createElement('div');
    theirs.className = 'ver saved';
    var tTitle = document.createElement('h4');
    tTitle.textContent = '最新已保存内容';
    theirs.appendChild(tTitle);
    var dl2 = document.createElement('dl');
    [['标题', saved.title, saved.title === ''],
     ['摘要', saved.summary, saved.summary === ''],
     ['正文', saved.body, saved.body === '']].forEach(function (pair) {
      var dt = document.createElement('dt');
      dt.textContent = pair[0];
      dl2.appendChild(dt);
      dl2.appendChild(fieldValue(pair[1], pair[2]));
    });
    theirs.appendChild(dl2);

    grid.appendChild(mine);
    grid.appendChild(theirs);
    conflictBox.appendChild(grid);

    var actions = document.createElement('p');
    actions.className = 'actions';
    var discard = document.createElement('button');
    discard.type = 'button';
    discard.className = 'danger';
    discard.textContent = '放弃当前输入并载入最新内容';
    discard.addEventListener('click', function () {
      applyEditingState(saved);
    });
    var keep = document.createElement('button');
    keep.type = 'button';
    keep.className = 'secondary';
    keep.textContent = '保留我的输入，继续修改';
    keep.addEventListener('click', function () {
      hideConflict();
      setStatus('已保留你的输入，请对照后手动调整再保存。', null);
      titleInput.focus();
    });
    var tip = document.createElement('span');
    tip.className = 'tip';
    tip.textContent = '不会自动覆盖或重置。';
    actions.appendChild(discard);
    actions.appendChild(keep);
    actions.appendChild(tip);
    conflictBox.appendChild(actions);
    conflictBox.hidden = false;
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
    hideConflict();
    var isEdit = !!editing;
    // 记录本次实际提交的原始字段值，响应返回时据此判断等待期间哪些字段又被编辑过。
    var submitted = {
      title: titleInput.value,
      summary: summaryInput.value,
      body: bodyInput.value
    };
    var payload = {
      title: titleInput.value,
      summary: summaryInput.value,
      body: bodyInput.value
    };
    var url = '/api/articles';
    if (isEdit) {
      url += '/' + encodeURIComponent(editing.id);
      payload.version = editing.version;
    }
    setSaving(true, '保存中…');
    setStatus(isEdit ? '正在保存修改…' : '正在保存…', null);
    fetch(url, {
      method: isEdit ? 'PUT' : 'POST',
      headers: { 'content-type': 'application/json', accept: 'application/json' },
      body: JSON.stringify(payload)
    })
      .then(function (r) {
        return r.json().catch(function () { return null; }).then(function (data) {
          return { status: r.status, data: data };
        });
      })
      .then(function (result) {
        var data = result.data;
        if (result.status === 409 && data && data.article) {
          // 版本冲突：保留表单全部输入，展示最新已保存内容供对照。
          // 绝不更新本地基准版本——只有用户显式确认载入最新内容后才允许覆盖。
          setSaving(false);
          saveBtn.textContent = editing ? '保存修改' : '保存草稿';
          setStatus('保存被拒绝：存在较新的已保存版本（409）。请对照后选择保留输入或载入最新内容。', 'err');
          showConflict(data.article);
          return;
        }
        if (result.status < 200 || result.status >= 300 || !data || !data.article) {
          throw new Error(data && data.error ? data.error : ('HTTP ' + result.status));
        }
        return data.article;
      })
      .then(function (article) {
        if (!article) return; // 冲突分支已处理
        if (isEdit) {
          var idx = -1;
          for (var i = 0; i < articles.length; i++) {
            if (articles[i] && articles[i].id === article.id) { idx = i; break; }
          }
          if (idx >= 0) articles[idx] = article; else articles.push(article);
        } else {
          // 新建：列表只记录本次请求实际保存的内容，等待期间补写的标题/摘要绝不提前进列表。
          articles.push(article);
        }
        render();
        setSaving(false);
        // 成功只确认本次提交的内容；无论新建还是编辑，都由同一逻辑合并响应与当前表单。
        mergeSavedState(article, submitted, isEdit);
      })
      .catch(function (err) {
        setSaving(false);
        saveBtn.textContent = editing ? '保存修改' : '保存草稿';
        var prefix = isEdit ? '保存修改失败：' : '保存失败：';
        setStatus(prefix + (err && err.message ? err.message : err) + '，输入已保留，可处理后再次操作。', 'err');
      });
  });

  // 保存成功后合并服务端响应与当前表单，新建与编辑共用。
  // 成功响应只能确认点击保存时提交的内容：服务端返回的已保存内容成为新的本地基准
  // （version 一并推进，再次保存不会误报冲突，也不会重复创建），但等待响应期间用户
  // 继续输入过的字段必须完整保留当前值（含换行、空行、多语言文字与类似 HTML 的文本，
  // 以及清空摘要或正文的操作），绝不能用刚保存的值回填或替换。
  function mergeSavedState(article, submitted, isEdit) {
    var savedFields = [
      { key: 'title', label: '标题', el: titleInput, value: article.title == null ? '' : String(article.title) },
      { key: 'summary', label: '摘要', el: summaryInput, value: article.summary == null ? '' : String(article.summary) },
      { key: 'body', label: '正文', el: bodyInput, value: article.body == null ? '' : String(article.body) }
    ];
    var pendingFields = [];
    savedFields.forEach(function (f) {
      if (f.el.value === submitted[f.key]) {
        // 等待期间未再改动：同步为服务端保存的规范值（例如去首尾空白后的标题）。
        if (f.el.value !== f.value) f.el.value = f.value;
      } else {
        // 等待期间又被修改（包括清空摘要或正文）：保留当前输入，不动一个字符。
        // 仅当保留下来的当前值确实与本次已保存值不同时，才算仍有未保存修改；
        // 改过又恢复为保存值的字段不再提示。
        if (f.el.value !== f.value) pendingFields.push(f.label);
      }
    });

    // 新建成功且当前表单与本次保存结果完全一致：沿用原行为——清空表单，回到可新建下一篇的状态。
    if (!isEdit && pendingFields.length === 0) {
      enterCreateMode('草稿已保存。可以继续新建草稿。', 'ok');
      return;
    }

    // 仍有未保存修改（或本就是编辑）：把表单关联到同一篇草稿，展示编辑状态。
    // 对新建而言这是首次进入编辑态——文章已创建一次，此后点“保存修改”只更新、不再创建。
    editing = {
      id: article.id,
      title: savedFields[0].value,
      summary: savedFields[1].value,
      body: savedFields[2].value,
      version: article.version
    };
    form.classList.add('editing');
    buildEditBanner(article);
    cancelBtn.hidden = false;
    saveBtn.textContent = '保存修改';
    hideConflict();
    render();
    if (pendingFields.length === 0) {
      setStatus(isEdit
        ? '修改已保存。文章标识、创建时间与草稿状态均未改变。'
        : '草稿已保存。文章标识、创建时间与草稿状态均未改变。', 'ok');
    } else {
      setStatus('本次提交的内容已保存（文章标识、创建时间与草稿状态不变）。但'
        + pendingFields.join('、')
        + '在保存期间又被修改，这些新输入尚未保存；再次点击“保存修改”将更新同一篇草稿，不会新建文章。', 'warn');
    }
  }

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
  version: number;
}
interface StoredArticle {
  id?: unknown;
  title?: unknown;
  summary?: unknown;
  body?: unknown;
  status?: unknown;
  createdAt?: unknown;
  version?: unknown;
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

// Normalize a stored record. Drafts saved by older versions have no `version`
// field; those are treated as version 0 and can be opened and edited directly.
function normalizeArticle(record: unknown): Article | null {
  if (typeof record !== 'object' || record === null || Array.isArray(record)) return null;
  const r = record as StoredArticle;
  if (typeof r.id !== 'string' || typeof r.title !== 'string' || typeof r.body !== 'string') return null;
  if (r.status !== 'draft' || typeof r.createdAt !== 'string') return null;
  const version = typeof r.version === 'number' && Number.isInteger(r.version) && r.version >= 0 ? r.version : 0;
  return {
    id: r.id,
    title: r.title,
    summary: typeof r.summary === 'string' ? r.summary : '',
    body: r.body,
    status: 'draft',
    createdAt: r.createdAt,
    version,
  };
}

function readArticles(): Article[] {
  let parsed: unknown;
  try {
    parsed = JSON.parse(readFileSync(dataFile, 'utf8'));
  } catch (error) {
    throw new Error(`unable to read articles: ${(error as Error).message}`);
  }
  if (!Array.isArray(parsed)) throw new Error('unable to read articles: stored data is not a list');
  const articles: Article[] = [];
  for (const record of parsed) {
    const article = normalizeArticle(record);
    if (article) articles.push(article);
  }
  return articles;
}

function writeArticles(records: Article[]): void {
  const tmpFile = join(dataDir, `.articles.${process.pid}.tmp`);
  try {
    writeFileSync(tmpFile, `${JSON.stringify(records, null, 2)}\n`);
    renameSync(tmpFile, dataFile);
  } catch (error) {
    try { rmSync(tmpFile, { force: true }); } catch { /* best effort cleanup */ }
    throw new Error(`unable to save articles: ${(error as Error).message}`);
  }
}

// Serialize read-modify-write so concurrent saves never lose or corrupt records.
let writeQueue: Promise<void> = Promise.resolve();
function enqueueWrite<T>(task: () => T): Promise<T> {
  const run = writeQueue.then(task);
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

function parseJsonObject(text: string): Record<string, unknown> {
  let payload: unknown;
  try {
    payload = JSON.parse(text);
  } catch (error) {
    throw new HttpError(400, `invalid JSON: ${(error as Error).message}`);
  }
  if (typeof payload !== 'object' || payload === null || Array.isArray(payload)) {
    throw new HttpError(400, 'request body must be a JSON object');
  }
  return payload as Record<string, unknown>;
}

function validateDraftInput(data: Record<string, unknown>): DraftInput {
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

// Updates carry the version the editor loaded; stale versions are rejected.
// As with creation, summary may be omitted (saved as empty string) but a
// present non-string summary is a 400.
function validateUpdateInput(data: Record<string, unknown>): DraftInput & { version: number } {
  const fields = validateDraftInput(data);
  if (typeof data.version !== 'number' || !Number.isInteger(data.version) || data.version < 0) {
    throw new HttpError(400, 'version must be a non-negative integer');
  }
  return { ...fields, version: data.version };
}

async function handle(req: IncomingMessage, res: ServerResponse): Promise<void> {
  let route: string;
  try { route = new URL(req.url ?? '/', 'http://localhost').pathname; }
  catch { respond(res, 400, { error: 'invalid request path' }); return; }

  // /api/articles/:id (match the raw path first; decode only the captured id)
  const itemMatch = /^\/api\/articles\/([^/]+)$/.exec(route);
  let itemId: string | null = null;
  if (itemMatch) {
    try { itemId = decodeURIComponent(itemMatch[1]); }
    catch { respond(res, 400, { error: 'invalid article id encoding' }); return; }
  }

  if (!['/', '/health', '/api/articles'].includes(route) && !itemMatch) {
    respond(res, 404, { error: 'not found' });
    return;
  }

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

  if (itemMatch && itemId !== null) {
    const id = itemId;
    if (req.method === 'GET') {
      const articles = readArticles();
      const article = articles.find((a) => a.id === id);
      if (!article) throw new HttpError(404, 'article not found');
      respond(res, 200, { article });
      return;
    }
    if (req.method === 'PUT') {
      const text = await readBody(req);
      const data = parseJsonObject(text);
      const fields = validateUpdateInput(data);
      const result = await enqueueWrite((): { status: number; article: Article } => {
        const records = readArticles();
        const index = records.findIndex((a) => a.id === id);
        if (index === -1) {
          throw new HttpError(404, 'article not found');
        }
        const current = records[index];
        if (fields.version !== current.version) {
          // 409 carries the currently saved article for side-by-side comparison.
          const conflict = new HttpError(409, 'a newer version of this article has been saved');
          (conflict as HttpError & { article?: Article }).article = current;
          throw conflict;
        }
        const updated: Article = {
          id: current.id,
          title: fields.title,
          summary: fields.summary,
          body: fields.body,
          status: current.status,
          createdAt: current.createdAt,
          version: current.version + 1,
        };
        records[index] = updated;
        writeArticles(records);
        return { status: 200, article: updated };
      });
      respond(res, result.status, { article: result.article });
      return;
    }
    methodNotAllowed(res, 'GET, PUT');
    return;
  }

  // /api/articles
  if (req.method === 'GET') {
    respond(res, 200, { [RESOURCE]: readArticles() });
    return;
  }
  if (req.method === 'POST') {
    const text = await readBody(req);
    const data = parseJsonObject(text);
    const fields = validateDraftInput(data);
    const article: Article = {
      id: randomUUID(),
      title: fields.title,
      summary: fields.summary,
      body: fields.body,
      status: 'draft',
      createdAt: new Date().toISOString(),
      version: 1,
    };
    await enqueueWrite((): void => {
      const records = readArticles();
      records.push(article);
      writeArticles(records);
    });
    respond(res, 201, { article });
    return;
  }
  methodNotAllowed(res, 'GET, POST');
}

const server = createServer((req: IncomingMessage, res: ServerResponse): void => {
  handle(req, res).catch((error: unknown) => {
    if (res.writableEnded) return;
    if (error instanceof HttpError) {
      const payload: { error: string; article?: Article } = { error: error.message };
      const withArticle = error as HttpError & { article?: Article };
      if (error.status === 409 && withArticle.article) payload.article = withArticle.article;
      respond(res, error.status, payload);
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
