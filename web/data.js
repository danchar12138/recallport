/* The two administration pages: 数据 (#adm-data) and 接入 (#adm-connect).
   Every action is a JSON call to /api/…; the long ones are background jobs that this file polls. After something
   changed on the server the page reloads, and the message to show afterwards (a toast) waits in sessionStorage. Copying,
   the confirmation dialog and the toast (data-copy, data-confirm, rp.toast) belong to ui.js. */
(() => {
  'use strict';
  const doc = document;
  const $ = (selector, root = doc) => root.querySelector(selector);
  const $$ = (selector, root = doc) => [...root.querySelectorAll(selector)];

  const {h, api: call, toast} = window.rp;

  /** A JSON call that throws on failure, carrying the server's own explanation in `message`. */
  const api = async (url, {method = 'POST', body} = {}) => {
    const r = await call(method, url, body);
    if (!r.ok) {
      throw Object.assign(new Error(r.status === 0 ? r.data.error : (r.data?.error || `操作没有成功（HTTP ${r.status}）。`)), {offline: r.status === 0, status: r.status, data: r.data});
    }
    return r.data;
  };

  const size = bytes => {
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    let value = bytes, unit = 0;
    while (value >= 1024 && unit < units.length - 1) { value /= 1024; unit++; }
    return unit === 0 ? `${bytes} B` : `${value.toFixed(1)} ${units[unit]}`;
  };

  const FLASH = 'rp-flash';
  const keepForNextPage = message => { try { sessionStorage.setItem(FLASH, JSON.stringify(message)); } catch { /* the page just reloads without a note */ } };
  const takeFromLastPage = () => {
    try {
      const raw = sessionStorage.getItem(FLASH);
      sessionStorage.removeItem(FLASH);
      return raw ? JSON.parse(raw) : null;
    } catch { return null; }
  };

  /* ---------------------------------------------------------------- 数据 */

  const initData = root => {
    const panel = $('[data-job]', root);
    const title = $('[data-job-title]', panel), pct = $('[data-job-pct]', panel), bar = $('[data-job-bar]', panel);
    const phase = $('[data-job-phase]', panel), result = $('[data-job-result]', panel);
    const form = $('[data-upload]', root), input = $('[data-file]', form), drop = $('[data-drop]', form);
    const uploadBox = $('[data-upload-progress]', form), uploadBar = $('[data-upload-bar]', form), uploadPct = $('[data-upload-pct]', form);
    const fileName = $('[data-file-name]', form), fileMeta = $('[data-file-meta]', form);
    const serverBusy = root.hasAttribute('data-busy');
    let running = false, upload = null;

    const refreshButtons = () => {
      for (const button of $$('[data-job-action]', root)) {
        button.disabled = serverBusy || running || (button.dataset.act === 'import' && !input.files.length);
      }
      input.disabled = running;
    };

    const show = (heading, {fraction = null, note = ''} = {}) => {
      panel.hidden = false;
      delete panel.dataset.tone;
      result.replaceChildren();
      title.textContent = heading;
      phase.textContent = note;
      bar.hidden = false;
      progress(fraction);
    };
    const progress = fraction => {
      if (fraction === null || fraction === undefined) {
        bar.removeAttribute('value');
        pct.textContent = '';
      } else {
        bar.value = fraction;
        pct.textContent = `${Math.round(fraction * 100)}%`;
      }
    };
    const finish = (heading, tone, ...lines) => {
      panel.hidden = false;
      title.textContent = heading;
      pct.textContent = '';
      phase.textContent = '';
      bar.hidden = true;
      panel.dataset.tone = tone;
      if (tone === 'ok') toast(heading);
      result.replaceChildren(...lines, h('div', {class: 'btns'}, h('button', {type: 'button', class: 'btn btn-sm', 'data-dismiss': '', text: tone === 'error' ? '关闭' : '知道了'})));
      result.focus({preventScroll: true});
      panel.scrollIntoView({block: 'nearest', behavior: 'smooth'});
    };

    const describe = (kind, r) => {
      const lines = [];
      if (kind === 'backup') {
        lines.push(h('p', {text: `已备份为 ${r.name}（${r.size}）。它在下面的列表里，可以下载保存到别处。`}));
        return lines;
      }
      const from = r.source || '备份';
      if (r.mode === 'replace') {
        lines.push(h('p', {text: `已把记忆库替换成 ${from}：共 ${r.nodes} 个节点。`}));
        if (r.kept_as) lines.push(h('p', {text: `原来的记忆库已另存为 ${r.kept_as}，可以在下面的备份列表里恢复它。`}));
      } else {
        const m = r.merge || {added: 0, updated: 0, unchanged: 0};
        lines.push(h('p', {text: `已把 ${from} 合并进来（它里面有 ${r.nodes} 个节点）：新增 ${m.added} 个，更新 ${m.updated} 个，没有变化的 ${m.unchanged} 个。`}));
      }
      if (r.legacy) lines.push(h('p', {text: '这是旧版（0.x）的备份，已转换：被更正过的记忆成了同一个节点的历史版本，汇总成了上一层的父节点，实体成了标签。'}));
      if (r.missing_files > 0) {
        lines.push(h('p', {class: 'warn-text', text: `有 ${r.missing_files} 个附件文件在备份里找不到（自动备份不带附件文件，或者备份不完整）。对应的节点都在，只是那些文件打不开。`}));
      }
      return lines;
    };
    const headings = {backup: '备份完成', restore: '恢复完成', import: '导入完成'};

    /** Wait for a job to end, showing what it reports. */
    const watch = id => new Promise((resolve, reject) => {
      let failures = 0;
      const tick = async () => {
        try {
          const state = await api(`/api/data/jobs/${id}`, {method: 'GET'});
          failures = 0;
          phase.textContent = state.phase || '';
          progress(state.fraction);
          if (state.finished) return resolve(state);
        } catch (error) {
          if (!error.offline || ++failures > 15) return reject(error);
        }
        setTimeout(tick, 400);
      };
      tick();
    });

    /** `early`: show the job's panel at once; an upload shows its own progress first. */
    const run = async (kind, heading, start, early = true) => {
      running = true;
      refreshButtons();
      if (early) show(heading);
      try {
        const {id} = await start();
        if (!early) show(heading);
        const state = await watch(id);
        if (state.error) {
          finish(`${headings[kind].replace('完成', '失败')}`, 'error', h('p', {text: state.error}));
        } else {
          keepForNextPage({kind, result: state.result});
          location.reload();
          return;
        }
      } catch (error) {
        if (error.cancelled) panel.hidden = true;
        else finish('没有完成', 'error', h('p', {text: error.message}));
      }
      running = false;
      refreshButtons();
    };

    const sendFile = (file, mode) => new Promise((resolve, reject) => {
      const xhr = new XMLHttpRequest();
      upload = xhr;
      xhr.open('POST', '/api/data/import');
      xhr.upload.onprogress = event => {
        if (!event.lengthComputable) return;
        uploadBar.value = event.loaded / event.total;
        uploadPct.textContent = `正在上传 ${Math.round(100 * event.loaded / event.total)}%（${size(event.loaded)} / ${size(event.total)}）`;
      };
      xhr.onload = () => {
        const data = (() => { try { return JSON.parse(xhr.responseText); } catch { return null; } })();
        if (xhr.status === 202 && data?.id) resolve(data);
        else reject(new Error(data?.error || `导入没有成功（HTTP ${xhr.status}）。`));
      };
      xhr.onerror = () => reject(new Error('上传被中断了。可能是另一项操作正在进行，或者本机的服务已经停止。'));
      xhr.onabort = () => reject(Object.assign(new Error('已取消上传。'), {cancelled: true}));
      const body = new FormData();
      body.append('mode', mode);
      body.append('file', file, file.name);
      xhr.send(body);
    });

    const importFile = async mode => {
      const file = input.files[0];
      if (!file) return;
      panel.hidden = true;
      uploadBox.hidden = false;
      uploadBar.value = 0;
      uploadPct.textContent = '正在上传 0%';
      const cancel = h('button', {type: 'button', class: 'btn btn-sm', text: '取消上传'});
      cancel.addEventListener('click', () => upload?.abort());
      uploadBox.append(cancel);
      try {
        await run('import', '正在导入', async () => {
          const started = await sendFile(file, mode);
          uploadBox.hidden = true;
          return started;
        }, false);
      } finally {
        cancel.remove();
        uploadBox.hidden = true;
        upload = null;
      }
    };

    const setFile = file => {
      fileName.textContent = file ? file.name : '选择或拖入一个 .zip 文件';
      fileMeta.textContent = file ? `${size(file.size)}。选好了，决定合并还是替换。` : '选好文件后，再决定合并还是替换。';
      drop.classList.toggle('has-file', Boolean(file));
      refreshButtons();
    };

    input.addEventListener('change', () => setFile(input.files[0]));
    for (const name of ['dragenter', 'dragover']) {
      drop.addEventListener(name, event => { event.preventDefault(); if (!running) drop.classList.add('is-over'); });
    }
    for (const name of ['dragleave', 'drop']) drop.addEventListener(name, () => drop.classList.remove('is-over'));
    drop.addEventListener('drop', event => {
      event.preventDefault();
      if (running || !event.dataTransfer.files.length) return;
      input.files = event.dataTransfer.files;
      setFile(input.files[0]);
    });
    form.addEventListener('submit', event => {
      event.preventDefault();
      if (running || serverBusy) return;
      importFile(event.submitter?.dataset.mode === 'replace' ? 'replace' : 'merge');
    });

    root.addEventListener('click', async event => {
      const button = event.target.closest('button');
      if (!button || !root.contains(button)) return;
      if (button.hasAttribute('data-reload')) return location.reload();
      if (button.hasAttribute('data-dismiss')) { panel.hidden = true; return; }
      if (button.type === 'submit' || !button.hasAttribute('data-job-action') || running) return;
      const {act, name, mode} = button.dataset;
      if (act === 'backup') {
        run('backup', '正在备份', () => api('/api/data/backup'));
      } else if (act === 'restore') {
        const word = mode === 'replace' ? '恢复' : '合并';
        run('restore', `正在${word} ${name}`, () => api('/api/data/restore', {body: {name, mode}}));
      } else if (act === 'delete') {
        button.disabled = true;
        try {
          await api(`/api/data/backups/${encodeURIComponent(name)}`, {method: 'DELETE'});
          keepForNextPage({kind: 'deleted', name});
          location.reload();
        } catch (error) {
          button.disabled = false;
          finish('没有删除', 'error', h('p', {text: error.message}));
        }
      }
    });

    for (const toggle of $$('[data-toggle]', root)) {
      toggle.addEventListener('change', async () => {
        const wanted = toggle.checked, note = $('[data-toggle-error]', root);
        note.hidden = true;
        toggle.disabled = true;
        try {
          const answer = await api(`/api/data/${toggle.dataset.toggle}`, {body: {enabled: wanted}});
          toggle.checked = answer.enabled;
          if (answer.enabled !== wanted) throw new Error('系统没有接受这个设置，请稍后再试。');
        } catch (error) {
          toggle.checked = !wanted;
          note.textContent = `没有改成功：${error.message}`;
          note.hidden = false;
        } finally {
          toggle.disabled = false;
        }
      });
    }

    const last = takeFromLastPage();
    if (last?.kind === 'deleted') {
      finish('已删除', 'ok', h('p', {text: `${last.name} 已删除。`}));
    } else if (last?.result) {
      finish(headings[last.kind] || '完成', 'ok', ...describe(last.kind, last.result));
    }
    refreshButtons();
  };

  /* ---------------------------------------------------------------- 接入 */

  const initConnect = root => {
    const say = (text, tone = 'ok') => toast(text, tone);

    const verdict = (card, check) => {
      const out = $('[data-verify]', card);
      const name = card.querySelector('h3').textContent;
      const state = card.dataset.state;
      const lines = [];
      if (check.ok) {
        lines.push(h('p', {class: 'verify-line', 'data-tone': 'ok', text: `本机服务正常：${check.server || 'RecallPort'}，${check.tools.length} 个工具可用（应有 ${check.expected} 个）。`}));
      } else {
        lines.push(h('p', {class: 'verify-line', 'data-tone': 'error', text: `检查没有通过：${check.error}`}));
      }
      if (check.tools.length) {
        lines.push(h('ul', {class: 'tools-found', 'aria-label': '服务提供的工具'}, ...check.tools.map(tool => h('li', {class: check.missing.includes(tool) ? 'is-missing' : ''}, h('code', {text: tool})))));
      }
      if (check.ok) {
        const seen = Boolean($('[data-state="seen"]', card));
        const tone = state === 'connected' && seen ? 'ok' : 'info';
        const text = state === 'connected'
          ? (seen ? `${name} 的配置正确，而且它连过本服务。` : `${name} 的配置正确，但还没收到它的连接：请重启 ${name}，它读到新配置后才会连上来。`)
          : state === 'conflict' ? `${name} 的配置指向别的地址，它连不到这个服务。选“替换为当前地址”。`
          : `${name} 还没有接入，上面只检查了本机服务。`;
        lines.push(h('p', {class: 'verify-line', 'data-tone': tone, text}));
      }
      out.replaceChildren(...lines);
    };

    const acts = {
      async connect(card, button) {
        const name = card.querySelector('h3').textContent;
        const answer = await api(`/api/clients/${card.dataset.client}/connect`, {body: {replace: button.dataset.replace === '1'}});
        if (answer.already) return {text: `${name} 已经接入了。`};
        return {text: `已接入 ${name}${answer.backup ? `（原配置已备份为 ${answer.backup}）` : ''}。请重启 ${name}，它才会读到新配置。`};
      },
      async disconnect(card) {
        const name = card.querySelector('h3').textContent;
        const answer = await api(`/api/clients/${card.dataset.client}/disconnect`, {body: {}});
        if (!answer.removed) return {text: `${name} 里本来就没有 recallport 这一项。`};
        return {text: `已断开 ${name}${answer.backup ? `（原配置已备份为 ${answer.backup}）` : ''}。记忆不受影响，随时可以重新接入。`};
      },
      async 'rules-on'(card) {
        const name = card.querySelector('h3').textContent;
        const answer = await api(`/api/clients/${card.dataset.client}/rules`, {body: {enabled: true}});
        if (!answer.changed) return {text: `${name} 的使用规则已经是最新的。`};
        return {text: `已写入 ${name} 的使用规则${answer.backup ? `（原文件已备份为 ${answer.backup}）` : ''}。请重启 ${name}，它才会读到。`};
      },
      async 'rules-off'(card) {
        const name = card.querySelector('h3').textContent;
        const answer = await api(`/api/clients/${card.dataset.client}/rules`, {body: {enabled: false}});
        return {text: answer.changed ? `已从 ${name} 的指令文件里移除使用规则。` : `${name} 的指令文件里本来就没有使用规则。`};
      },
    };

    root.addEventListener('click', async event => {
      const button = event.target.closest('[data-client-act]');
      if (!button) return;
      const card = button.closest('[data-client]');
      const act = button.dataset.clientAct;
      button.disabled = true;
      if (act === 'verify') {
        const out = $('[data-verify]', card);
        out.replaceChildren(h('p', {class: 'verify-line', 'data-tone': 'info', text: '正在检查本机服务……'}));
        try {
          verdict(card, await api('/api/clients/verify'));
        } catch (error) {
          out.replaceChildren(h('p', {class: 'verify-line', 'data-tone': 'error', text: error.message}));
        }
        button.disabled = false;
        return;
      }
      try {
        const done = await acts[act](card, button);
        keepForNextPage({text: done.text, tone: 'ok'});
        location.reload();
      } catch (error) {
        button.disabled = false;
        // a conflict means the file is not what the card showed: look at it again
        if (error.status === 409) {
          keepForNextPage({text: error.message, tone: 'error'});
          location.reload();
        } else say(error.message, 'error');
      }
    });

    /* The manual, as plain text for pasting into an AI. */
    const manual = $('[data-manual-copy]', root);
    const text = node => node.textContent.replace(/\s+/g, ' ').trim();
    const manualText = () => {
      const lines = ['# RecallPort 工具手册', '', `MCP 地址：${text($('#adm-mcp-url', root))}`, '', '## 记忆怎么组织与维护'];
      for (const item of $$('.adm-model li', root)) lines.push(`- ${text(item)}`);
      for (const tool of $$('[data-tool]', root)) {
        lines.push('', `## ${tool.dataset.tool} · ${text($('h3 span', tool))}`, text($('.adm-when', tool)), '参数：');
        for (const row of $$('.adm-args > div', tool)) {
          const names = $$('dt code', row).map(code => code.textContent).join(' / ') || text($('dt', row));
          const type = $('dt small', row);
          lines.push(`- ${names}${type ? `（${text(type)}）` : ''}：${text($('dd', row))}`);
        }
        lines.push(text($('.adm-returns', tool)), '示例：');
        for (const example of $$('pre', tool)) lines.push(example.textContent.trim());
      }
      return lines.join('\n');
    };
    manual.dataset.copy = manualText();

    const last = takeFromLastPage();
    if (last?.text) say(last.text, last.tone);
  };

  const data = $('#adm-data'), connect = $('#adm-connect');
  if (data) initData(data);
  if (connect) initConnect(connect);
})();
