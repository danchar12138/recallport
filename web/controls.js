/* 页面专属交互。依赖 ui.js 提供的 window.RP。 */
(() => {
  'use strict';
  const {$, $$, h, toast, confirmDialog, progressDialog, formatBytes, rebuilds} = window.RP;
  const pad = value => String(value).padStart(2, '0');
  const ymd = date => `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())}`;

  for (const btn of $$('dialog [data-close]')) btn.addEventListener('click', () => btn.closest('dialog').close('cancel'));

  /* ---------- 日期输入：自由输入 + 日历 + 即时校验 ---------- */
  const zh = {
    firstDayOfWeek: 1,
    weekdays: {shorthand: ['日', '一', '二', '三', '四', '五', '六'], longhand: ['星期日', '星期一', '星期二', '星期三', '星期四', '星期五', '星期六']},
    months: {shorthand: Array.from({length: 12}, (_, i) => `${i + 1}月`), longhand: Array.from({length: 12}, (_, i) => `${i + 1}月`)},
  };
  const normalizeDate = raw => {
    const text = raw.trim().replace(/日/g, '').replace(/[年月/.\s]+/g, '-').replace(/^-+|-+$/g, '');
    const m = /^(\d{4})(?:-(\d{1,2})(?:-(\d{1,2}))?)?$/.exec(text);
    if (!m) return raw.trim();
    return [m[1], m[2] && m[2].padStart(2, '0'), m[3] && m[3].padStart(2, '0')].filter(Boolean).join('-');
  };
  const checkDate = value => {
    if (!value) return {ok: true, text: ''};
    const m = /^(\d{4})(?:-(\d{2})(?:-(\d{2}))?)?$/.exec(value);
    if (!m || Number(m[1]) < 1) return {ok: false, text: '格式应为 2024、2024-09 或 2024-09-12'};
    const [year, month, day] = [Number(m[1]), Number(m[2] || 1), Number(m[3] || 1)];
    if (month < 1 || month > 12) return {ok: false, text: '月份应在 1–12 之间'};
    const probe = new Date(2000, 0, 1);
    probe.setFullYear(year, month - 1, day);
    if (probe.getMonth() !== month - 1 || probe.getDate() !== day) return {ok: false, text: '这一天不存在'};
    if (m[3]) return {ok: true, text: `将记为 ${year} 年 ${month} 月 ${day} 日`};
    if (m[2]) return {ok: true, text: `将记为 ${year} 年 ${month} 月（仅年月）`};
    return {ok: true, text: `将记为 ${year} 年（仅年份）`};
  };
  for (const box of $$('[data-date-field]')) {
    const input = $('[data-date-input]', box), pick = $('[data-date-pick]', box), hint = $('[data-date-hint]', box);
    const fallbackHint = hint?.textContent || '';
    const validate = () => {
      const result = checkDate(input.value.trim());
      input.setCustomValidity(result.ok ? '' : result.text);
      input.setAttribute('aria-invalid', String(!result.ok));
      if (hint) {
        hint.textContent = result.text || fallbackHint;
        hint.classList.toggle('is-error', !result.ok);
        hint.classList.toggle('is-ok', result.ok && Boolean(result.text));
      }
      return result.ok;
    };
    input.addEventListener('input', validate);
    input.addEventListener('blur', () => { input.value = normalizeDate(input.value); validate(); });
    for (const quick of $$('[data-date-set]', box)) quick.addEventListener('click', () => {
      const date = new Date();
      if (quick.dataset.dateSet === 'yesterday') date.setDate(date.getDate() - 1);
      input.value = quick.dataset.dateSet === '' ? '' : ymd(date);
      validate();
      input.dispatchEvent(new Event('input', {bubbles: true}));
      input.focus();
    });
    if (window.flatpickr) {
      const ghost = h('input', {type: 'text', class: 'date-ghost', tabindex: '-1', 'aria-hidden': 'true'});
      box.querySelector('.datefield').append(ghost);
      const picker = window.flatpickr(ghost, {
        dateFormat: 'Y-m-d', locale: zh, disableMobile: true, clickOpens: false, positionElement: input,
        onChange: (_, value) => {
          if (!value) return;
          input.value = value;
          validate();
          input.dispatchEvent(new Event('input', {bubbles: true}));
        },
      });
      pick.addEventListener('click', () => {
        if (/^\d{4}-\d{2}-\d{2}$/.test(input.value) && checkDate(input.value).ok) picker.setDate(input.value, false);
        picker.open();
      });
    } else pick.hidden = true;
    validate();
  }

  /* ---------- 时间线：筛选弹层、芯片、检索方式、清空 ---------- */
  const searchForm = $('[data-search-form]');
  if (searchForm) {
    const q = $('[data-search]', searchForm), clear = $('[data-clear-search]', searchForm);
    const sync = () => { clear.hidden = !q.value; };
    q.addEventListener('input', sync);
    clear.addEventListener('click', () => { q.value = ''; sync(); q.focus(); });
    sync();
    for (const radio of $$('[data-strategy] input', searchForm)) radio.addEventListener('change', () => {
      if (q.value.trim()) searchForm.requestSubmit();
      else toast('已切换检索方式，输入关键词后生效', {tone: 'info', timeout: 2400});
    });
    for (const chip of $$('[data-remove-filter]', searchForm)) chip.addEventListener('click', () => {
      const kind = chip.dataset.removeFilter;
      if (kind === 'range') { searchForm.elements.from.value = ''; searchForm.elements.through.value = ''; }
      else searchForm.querySelector(`input[type=hidden][name=${kind}]`)?.remove();
      searchForm.requestSubmit();
    });
    const popover = $('[data-popover]', searchForm);
    if (popover) {
      closeOnOutside(popover);
      for (const btn of $$('[data-range]', popover)) btn.addEventListener('click', () => {
        const now = new Date(), year = now.getFullYear();
        const range = {
          '30d': () => { const from = new Date(); from.setDate(from.getDate() - 30); return [ymd(from), ymd(now)]; },
          year: () => [String(year), String(year)],
          lastyear: () => [String(year - 1), String(year - 1)],
          clear: () => ['', ''],
        }[btn.dataset.range]();
        searchForm.elements.from.value = range[0];
        searchForm.elements.through.value = range[1];
        searchForm.requestSubmit();
      });
    }
  }
  function closeOnOutside(details) {
    document.addEventListener('click', event => {
      if (details.open && !details.contains(event.target) && !event.target.closest('.flatpickr-calendar')) details.open = false;
    });
    details.addEventListener('keydown', event => {
      if (event.key === 'Escape' && details.open) { details.open = false; details.querySelector('summary').focus(); }
    });
    details.addEventListener('toggle', () => { if (details.open) $('input', details)?.focus(); });
  }

  /* ---------- 概览：按数量给月份着色 ---------- */
  for (const month of $$('.month[data-count]')) {
    const ratio = Number(month.dataset.count) / Math.max(1, Number(month.dataset.max));
    month.dataset.level = ratio >= .75 ? 4 : ratio >= .5 ? 3 : ratio >= .25 ? 2 : 1;
  }

  /* ---------- 编辑器：自动增高、计数、未保存提示 ---------- */
  const editor = $('[data-editor]');
  if (editor) {
    const isNew = editor.getAttribute('action').endsWith('/new');
    const text = editor.elements.text, save = $('[data-save]', editor), state = $('[data-dirty-state]', editor);
    const watched = [text, editor.elements.context, editor.elements.occurred];
    const snapshot = () => watched.map(field => field.value).join('\u0000');
    const initial = snapshot(), forcedDirty = editor.hasAttribute('data-initial-dirty');
    let submitting = false, dirty = false;
    const fit = area => { area.style.height = 'auto'; area.style.height = `${area.scrollHeight + 2}px`; };
    const count = area => {
      const [kind, limitText] = area.dataset.counter.split(':'), limit = Number(limitText);
      const used = kind === 'bytes' ? new TextEncoder().encode(area.value).length : [...area.value].length;
      const out = document.getElementById(area.dataset.counterId);
      out.textContent = kind === 'bytes' ? `${used} / ${limit} 字节` : `${used} / ${limit}`;
      out.classList.toggle('is-near', used > limit * .9 && used <= limit);
      out.classList.toggle('is-over', used > limit);
      area.setCustomValidity(used > limit ? '内容过长，请精简后再保存' : '');
    };
    const refresh = () => {
      dirty = forcedDirty || snapshot() !== initial;
      if (isNew) { dirty = Boolean(text.value.trim()); save.disabled = !dirty; }
      else {
        save.disabled = !dirty;
        state.textContent = dirty ? '有未保存的更改' : '无未保存的更改';
        state.classList.toggle('is-dirty', dirty);
      }
    };
    for (const area of $$('textarea[data-autosize]', editor)) {
      const run = () => { fit(area); count(area); refresh(); };
      area.addEventListener('input', run);
      run();
    }
    editor.elements.occurred.addEventListener('input', refresh);
    editor.addEventListener('submit', () => { submitting = true; });
    window.addEventListener('beforeunload', event => { if (dirty && !submitting) { event.preventDefault(); event.returnValue = ''; } });
    window.addEventListener('pageshow', () => { submitting = false; refresh(); });
    refresh();
  }

  /* ---------- 添加引用：实时搜索选择 ---------- */
  const linkDialog = $('#link-dialog');
  if (linkDialog) {
    const self = linkDialog.dataset.self;
    const linked = new Set(linkDialog.dataset.linked.split(/\s+/).filter(Boolean));
    const search = $('[data-link-search]', linkDialog), results = $('[data-link-results]', linkDialog);
    const reasonBox = $('[data-link-reason]', linkDialog), target = $('[data-link-target]', linkDialog), submit = $('[data-link-submit]', linkDialog);
    let epoch = 0, controller, timer;
    const note = (message, retry) => {
      const node = h('div', {class: 'pick-note', text: message});
      if (retry) { node.append(' ', h('button', {type: 'button', class: 'link-btn', text: '重试'})); node.lastChild.addEventListener('click', retry); }
      return node;
    };
    const choose = (id, row) => {
      target.value = id;
      for (const other of $$('.pick', results)) other.setAttribute('aria-selected', String(other === row));
      reasonBox.hidden = false;
      submit.disabled = false;
      $('input', reasonBox).focus();
    };
    const render = nodes => {
      const rows = nodes.filter(node => node.id !== self).map(node => {
        const done = linked.has(node.id);
        const row = h('button', {type: 'button', class: 'pick', role: 'option', 'aria-selected': 'false', disabled: done},
          h('time', {text: node.occurred || '日期未记'}), h('span', {text: node.text}),
          h('small', {text: done ? `${node.source || ''} · 已引用` : node.source || ''}));
        row.addEventListener('click', () => choose(node.id, row));
        return row;
      });
      results.replaceChildren(...(rows.length ? rows : [note(search.value.trim() ? '没有找到匹配的记忆' : '还没有其他记忆可以引用')]));
    };
    const load = async () => {
      const mark = ++epoch;
      controller?.abort();
      controller = new AbortController();
      results.replaceChildren(...Array.from({length: 4}, () => h('div', {class: 'skeleton'})));
      const q = search.value.trim();
      try {
        const response = await fetch(`/graph-data?${new URLSearchParams(q ? {q} : {page: '1'})}`, {signal: controller.signal});
        if (!response.ok) throw new Error('search failed');
        const data = await response.json();
        if (mark === epoch) render(data.nodes);
      } catch (error) {
        if (error.name !== 'AbortError' && mark === epoch) results.replaceChildren(note('搜索失败。', load));
      }
    };
    search.addEventListener('input', () => { clearTimeout(timer); timer = setTimeout(load, 260); target.value = ''; reasonBox.hidden = true; submit.disabled = true; });
    search.addEventListener('keydown', event => { if (event.key === 'Enter') { event.preventDefault(); clearTimeout(timer); load(); } });
    const open = () => {
      search.value = ''; target.value = ''; reasonBox.hidden = true; submit.disabled = true;
      $('input', reasonBox).value = '';
      linkDialog.showModal();
      search.focus();
      load();
    };
    for (const btn of $$('[data-link-open]')) btn.addEventListener('click', open);
    linkDialog.addEventListener('click', event => { if (event.target === linkDialog) linkDialog.close('cancel'); });
    if (location.hash === '#add-link') open();
  }

  /* ---------- 接入：标签页与连接测试 ---------- */
  for (const root of $$('[data-tabs]')) {
    const tabs = $$('[role=tab]', root), panels = $$('[role=tabpanel]', root);
    const select = tab => {
      for (const item of tabs) {
        const on = item === tab;
        item.setAttribute('aria-selected', String(on));
        item.tabIndex = on ? 0 : -1;
        document.getElementById(item.getAttribute('aria-controls')).hidden = !on;
      }
    };
    for (const tab of tabs) {
      tab.addEventListener('click', () => select(tab));
      tab.addEventListener('keydown', event => {
        const step = event.key === 'ArrowRight' ? 1 : event.key === 'ArrowLeft' ? -1 : 0;
        if (!step) return;
        const next = tabs[(tabs.indexOf(tab) + step + tabs.length) % tabs.length];
        select(next); next.focus();
      });
    }
    select(tabs[0]);
    void panels;
  }

  const testMCP = $('#test-mcp');
  if (testMCP) {
    const status = $('#mcp-status'), list = $('#mcp-checks'), step = $('#verify');
    const mark = (name, value) => { $(`[data-check=${name}]`, list).dataset.state = value; };
    testMCP.hidden = false;
    testMCP.addEventListener('click', async () => {
      testMCP.disabled = true;
      list.hidden = false;
      step.classList.remove('is-done');
      status.textContent = '';
      for (const item of $$('[data-check]', list)) item.dataset.state = 'idle';
      let current = 'health';
      try {
        const signal = AbortSignal.timeout(6000);
        const send = async (body, protocol) => {
          const headers = {'Content-Type': 'application/json', Accept: 'application/json, text/event-stream'};
          if (protocol) headers['MCP-Protocol-Version'] = protocol;
          const response = await fetch('/mcp', {method: 'POST', headers, signal, body: JSON.stringify({jsonrpc: '2.0', ...body})});
          if (!response.ok) throw new Error(`HTTP ${response.status}`);
          if (body.id === undefined) return null;
          const reply = await response.json();
          if (!reply.result || reply.error || reply.id !== body.id) throw new Error('Invalid MCP response');
          return reply.result;
        };
        mark('health', 'run');
        if (!(await fetch('/health', {signal})).ok) throw new Error('health');
        mark('health', 'ok'); current = 'init'; mark('init', 'run');
        const init = await send({id: 1, method: 'initialize', params: {protocolVersion: '2025-11-25', capabilities: {}, clientInfo: {name: 'RecallPort Web', version: '1'}}});
        await send({method: 'notifications/initialized'}, init.protocolVersion);
        mark('init', 'ok'); current = 'tools'; mark('tools', 'run');
        const result = await send({id: 2, method: 'tools/list', params: {}}, init.protocolVersion);
        const names = new Set(result.tools.map(tool => tool.name));
        if (!['recall', 'read', 'remember', 'link'].every(name => names.has(name))) throw new Error('Missing tools');
        mark('tools', 'ok');
        status.textContent = '连接正常，4 个工具可用。现在可以去客户端里添加这个地址了。';
        step.classList.add('is-done');
        toast('MCP 连接正常');
      } catch {
        mark(current, 'fail');
        status.textContent = current === 'health' ? '本机服务没有响应，请确认 RecallPort 仍在运行。' : current === 'init' ? 'MCP 握手失败，请重启 RecallPort 后再试。' : '没有读取到完整的工具列表。';
        toast('连接测试未通过', {tone: 'error'});
      } finally {
        testMCP.disabled = false;
      }
    });
    const openTool = () => {
      const tool = document.getElementById(location.hash.slice(1));
      if (tool?.matches('.tool-doc')) tool.open = true;
    };
    openTool();
    window.addEventListener('hashchange', openTool);
  }

  /* ---------- 数据：索引状态、重建、备份、恢复 ---------- */
  const indexPanel = $('[data-index-panel]');
  if (indexPanel) {
    const labels = {ready: '正常', queued: '排队中', working: '处理中', degraded: '部分可用', error: '待修复', unknown: '待检查', disabled: '未启用'};
    const names = {keyword: '关键词索引', semantic: '语义向量', all: '全部索引'};
    const lanes = $$('[data-index-lane]', indexPanel);
    const pending = new Set(), seen = {}, watching = new Set(rebuilds.read().map(item => item.kind));
    let timer, firstPoll = true;
    const setState = (badge, state) => { badge.dataset.state = state; badge.textContent = labels[state] ?? labels.unknown; };
    const stamp = (value, empty) => {
      if (!value) return empty;
      const d = new Date(value);
      return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
    };
    for (const lane of lanes) { const badge = $('[data-index-state]', lane); setState(badge, badge.dataset.state); seen[lane.dataset.indexLane] = badge.dataset.state; }
    const poll = async () => {
      clearTimeout(timer);
      let next = 5000;
      try {
        const response = await fetch('/index-status');
        if (!response.ok) throw new Error('status unavailable');
        const s = await response.json();
        let busy = false;
        for (const lane of lanes) {
          const kind = lane.dataset.indexLane, health = s[kind];
          setState($('[data-index-state]', lane), health.state);
          $('[data-index-checked]', lane).textContent = stamp(health.checked_at, '未检查');
          $('[data-index-repaired]', lane).textContent = stamp(health.repaired_at, '无');
          $('[data-index-problem]', lane).hidden = !health.problem;
          const working = health.state === 'working' || health.state === 'queued';
          $('[data-index-rebuild]', lane).disabled = working || health.state === 'disabled' || pending.has(kind) || pending.has('all');
          const bar = $('[data-index-bar]', lane);
          if (bar) bar.hidden = !working;
          if (working) watching.add(kind);
          else if (watching.has(kind) && !firstPoll) {
            watching.delete(kind);
            rebuilds.remove(kind);
            if (health.state === 'ready') toast(`${names[kind]}已重建完成`);
            else if (health.state === 'error' || health.state === 'degraded') toast(`${names[kind]}重建后仍有问题，请稍后重试`, {tone: 'error'});
          }
          busy ||= working;
        }
        $('[data-index-rebuild="all"]', indexPanel).disabled = busy || pending.size > 0;
        document.getElementById('index-coverage').textContent = `${s.indexed} / ${s.total}`;
        document.getElementById('index-engine').textContent = s.model ? `LanceDB · ${s.dimensions} 维 · ${s.ann ? 'IVF-HNSW-Flat' : '精确扫描'}` : '语义模型未启用';
        const progress = document.getElementById('index-progress');
        progress.max = Math.max(s.total, 1); progress.value = s.indexed;
        const skipped = document.getElementById('index-skipped');
        skipped.hidden = !s.skipped; skipped.textContent = `跳过 ${s.skipped} 条`;
        if (busy) next = 750;
      } catch {
        for (const lane of lanes) $('[data-index-problem]', lane).hidden = false;
      }
      firstPoll = false;
      timer = setTimeout(poll, next);
    };
    for (const button of $$('[data-index-rebuild]', indexPanel)) {
      const form = button.form, kind = button.dataset.indexRebuild;
      form.addEventListener('submit', async event => {
        event.preventDefault();
        if (pending.has(kind)) return;
        pending.add(kind);
        button.classList.add('is-busy');
        try {
          const response = await fetch(form.action, {method: 'POST', headers: {Accept: 'application/json'}, body: new URLSearchParams({kind})});
          if (response.ok) {
            toast(`已开始重建${names[kind]}。完成后会在页面右下角提示，你可以先去做别的事。`, {timeout: 6000});
            rebuilds.add(kind === 'all' ? ['keyword', 'semantic'] : [kind]);
            for (const lane of lanes) if (kind === 'all' || lane.dataset.indexLane === kind) { setState($('[data-index-state]', lane), 'queued'); watching.add(lane.dataset.indexLane); }
          } else if (response.status === 409) toast('索引正在维护，或语义模型未启用，请稍后再试', {tone: 'info'});
          else toast('无法提交重建任务', {tone: 'error'});
        } catch {
          toast('无法连接本地服务', {tone: 'error'});
        } finally {
          pending.delete(kind);
          button.classList.remove('is-busy');
          poll();
        }
      });
    }
    poll();
  }

  const backupForm = $('#backup-form');
  if (backupForm) {
    const button = $('button', backupForm);
    backupForm.addEventListener('submit', async event => {
      event.preventDefault();
      button.classList.add('is-busy');
      try {
        const response = await fetch(backupForm.action, {method: 'POST'});
        if (!response.ok) throw new Error('backup failed');
        const blob = await response.blob();
        const name = /filename="?([^";]+)/.exec(response.headers.get('Content-Disposition') || '')?.[1] || 'recallport.recallport.json';
        const url = URL.createObjectURL(blob);
        const link = h('a', {href: url, download: name});
        document.body.append(link);
        link.click();
        link.remove();
        setTimeout(() => URL.revokeObjectURL(url), 15000);
        toast(`备份已生成：${name}（${formatBytes(blob.size)}），请在浏览器的下载列表中查看`, {timeout: 5000});
      } catch {
        toast('生成备份失败，请重试', {tone: 'error'});
      } finally {
        button.classList.remove('is-busy');
      }
    });
  }

  /** 带上传进度的恢复请求。成功后服务端重定向到 /data?done=restore。 */
  const runRestore = (url, body, title) => new Promise(resolve => {
    const dialog = progressDialog({title, message: '正在提交…'});
    const xhr = new XMLHttpRequest();
    xhr.open('POST', url);
    xhr.upload.onprogress = event => { if (event.lengthComputable) dialog.set(event.loaded / event.total, `正在上传 ${formatBytes(event.loaded)} / ${formatBytes(event.total)}`); };
    xhr.upload.onload = () => dialog.set(null, '正在替换记忆并重建索引。数据较多时可能需要几分钟，请勿关闭页面。');
    const fail = message => { dialog.close(); toast(message, {tone: 'error', timeout: 10000}); resolve(false); };
    xhr.onerror = () => fail('无法连接本地服务，恢复未完成');
    xhr.ontimeout = () => fail('恢复超时，请在「数据」页确认当前状态');
    xhr.onload = () => {
      let done = false;
      try { done = new URL(xhr.responseURL).searchParams.get('done') === 'restore'; } catch { /* 忽略 */ }
      if (done) {
        try { sessionStorage.setItem('rp-flash', '备份已恢复，索引正在重建'); } catch { /* 忽略 */ }
        dialog.close();
        location.href = '/data';
        resolve(true);
        return;
      }
      const page = new DOMParser().parseFromString(xhr.responseText, 'text/html');
      fail(page.querySelector('.banner-error')?.textContent.trim() || (xhr.status === 413 ? '备份文件过大' : '恢复失败，当前记忆未被改动'));
    };
    xhr.send(body);
  });

  const restoreForm = $('#restore-form');
  if (restoreForm) {
    const file = restoreForm.elements.backup, zone = $('[data-dropzone]', restoreForm);
    const nameEl = $('[data-file-name]', restoreForm), metaEl = $('[data-file-meta]', restoreForm), submit = $('[data-restore-submit]', restoreForm);
    restoreForm.elements.confirm.required = false;
    $('[data-fallback-confirm]', restoreForm).hidden = true;
    file.required = false;
    submit.disabled = true;
    const show = () => {
      const chosen = file.files[0];
      zone.classList.toggle('has-file', Boolean(chosen));
      nameEl.textContent = chosen ? chosen.name : '选择或拖入备份文件';
      metaEl.textContent = chosen ? formatBytes(chosen.size) : '.recallport.json';
      submit.disabled = !chosen;
    };
    file.addEventListener('change', show);
    for (const type of ['dragenter', 'dragover']) zone.addEventListener(type, event => { event.preventDefault(); zone.classList.add('is-over'); });
    for (const type of ['dragleave', 'drop']) zone.addEventListener(type, () => zone.classList.remove('is-over'));
    zone.addEventListener('drop', event => {
      event.preventDefault();
      if (event.dataTransfer.files.length) { file.files = event.dataTransfer.files; show(); }
    });
    restoreForm.addEventListener('submit', async event => {
      event.preventDefault();
      const chosen = file.files[0];
      if (!chosen) return;
      const total = Number(restoreForm.dataset.total) || 0;
      const ok = await confirmDialog({
        title: '用此备份替换当前记忆？',
        body: [`文件：${chosen.name}（${formatBytes(chosen.size)}）`, `当前的 ${total} 条记忆将被替换，不会合并。替换前会自动保留一份回退快照，之后仍可恢复回来。`],
        ok: '确认恢复', tone: 'danger',
      });
      if (!ok) return;
      const body = new FormData();
      body.append('confirm', 'yes');
      body.append('backup', chosen);
      runRestore(restoreForm.action, body, '正在恢复备份');
    });
  }

  const rollbackForm = $('#rollback-form');
  if (rollbackForm) {
    rollbackForm.addEventListener('submit', event => {
      event.preventDefault();
      runRestore(rollbackForm.action, new URLSearchParams(new FormData(rollbackForm)), '正在恢复到上一份快照');
    });
  }
})();
