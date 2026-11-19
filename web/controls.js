/* 页面专属交互。依赖 ui.js 提供的 window.RP。 */
(() => {
  'use strict';
  const {$, $$, h, toast, openDialog, confirmDialog, progressDialog, formatBytes, rebuilds} = window.RP;
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

  /* ---------- 接入：检测、接入、取消、验证 ---------- */
  const clientList = $('[data-clients]');
  if (clientList) {
    const ago = iso => {
      const seconds = Math.max(0, (Date.now() - new Date(iso).getTime()) / 1000);
      if (seconds < 60) return '刚刚';
      if (seconds < 3600) return `${Math.floor(seconds / 60)} 分钟前`;
      if (seconds < 86400) return `${Math.floor(seconds / 3600)} 小时前`;
      return new Date(iso).toLocaleDateString();
    };
    const states = {connected: ['已接入', 'ready'], none: ['未接入', ''], conflict: ['地址不同', 'degraded'], error: ['无法读取', 'error']};
    let latest = null;
    const post = async (url, body) => {
      const response = await fetch(url, {method: 'POST', headers: {'Content-Type': 'application/x-www-form-urlencoded'}, body: new URLSearchParams(body)});
      return {status: response.status, json: await response.json().catch(() => ({}))};
    };
    const detail = c => [
      h('p', {text: `范围：${c.scope}`}), h('p', {class: 'muted', text: `配置文件：${c.path}`}),
      h('p', {text: '将写入这一项（其余内容保持不变）：'}), h('pre', {class: 'quote', text: c.preview}),
    ];
    const refresh = async () => {
      try {
        const response = await fetch('/clients-status');
        if (!response.ok) throw new Error('status');
        latest = await response.json();
        clientList.replaceChildren(...latest.clients.map(card));
      } catch { /* 下次再试 */ }
    };
    const act = async (c, kind) => {
      if (kind === 'connect' || kind === 'replace') {
        const conflict = kind === 'replace';
        const ok = await confirmDialog({
          title: conflict ? `替换 ${c.name} 里的同名配置？` : `接入 ${c.name}？`,
          body: conflict ? [`${c.name} 里已经有一个叫 recallport 的配置，指向 ${c.existing}。替换后它会改为指向当前地址，原文件会先备份。`, ...detail(c)] : detail(c),
          ok: conflict ? '替换为当前地址' : '接入', tone: conflict ? 'danger' : undefined,
        });
        if (!ok) return;
        const result = await post(`/clients/${c.id}/connect`, {replace: conflict ? '1' : '0'});
        if (result.status === 409) { toast(`${c.name} 里已有同名配置（指向 ${result.json.existing}），请在卡片上选择「替换」`, {tone: 'info', timeout: 8000}); }
        else if (result.json.ok) toast(result.json.already ? `${c.name} 已经接入` : `已接入 ${c.name}。请重启 ${c.name} 使其生效。`, {timeout: 6000});
        else toast(result.json.error || '接入未完成', {tone: 'error', timeout: 9000});
      } else if (kind === 'disconnect') {
        const ok = await confirmDialog({title: `取消接入 ${c.name}？`, body: [`只会从 ${c.name} 的配置里移除 recallport 这一项（原文件先备份）。你的记忆不会被删除，随时可以重新接入。`, h('p', {class: 'muted', text: `配置文件：${c.path}`})], ok: '取消接入', tone: 'danger'});
        if (!ok) return;
        const result = await post(`/clients/${c.id}/disconnect`, {});
        if (result.json.ok) toast(result.json.removed ? `已取消接入 ${c.name}` : `${c.name} 本来就没有接入`, {tone: result.json.removed ? 'success' : 'info'});
        else toast(result.json.error || '操作未完成', {tone: 'error'});
      } else if (kind === 'verify') {
        await refresh();
        const now = latest.clients.find(x => x.id === c.id);
        if (now.state !== 'connected') toast(`${c.name} 的配置还不正确，请先接入`, {tone: 'error'});
        else if (now.last_seen) toast(`配置正确，${now.last_seen.client} ${ago(now.last_seen.at)}连接过`);
        else toast(`配置正确，但还没有收到 ${c.name} 的连接。请重启 ${c.name} 后再点「验证」`, {tone: 'info', timeout: 7000});
        return;
      }
      await refresh();
    };
    const card = c => {
      const [label, tone] = states[c.state] || states.none;
      const buttons = [];
      const button = (text, kind, primary) => { const b = h('button', {type: 'button', class: `btn btn-sm${primary ? ' btn-primary' : ''}`, text}); b.addEventListener('click', () => { b.classList.add('is-busy'); act(c, kind).finally(() => b.classList.remove('is-busy')); }); return b; };
      if (c.state === 'none') buttons.push(button('接入', 'connect', true));
      if (c.state === 'conflict') buttons.push(button('替换为当前地址…', 'replace', true));
      if (c.state === 'connected') buttons.push(button('验证', 'verify', true), button('取消接入', 'disconnect'));
      const note = c.state === 'error' ? c.error : c.state === 'conflict' ? `配置里已有同名项，指向 ${c.existing}。` : c.state === 'connected'
        ? (c.last_seen ? `最近连接：${c.last_seen.client} · ${ago(c.last_seen.at)}` : `还没有收到它的连接。重启 ${c.name} 后点「验证」。`) : '';
      return h('div', {class: 'client-card'},
        h('div', {class: 'client-head'}, h('b', {text: c.name}), h('span', {class: 'badge', 'data-state': tone, text: label}), c.installed ? null : h('span', {class: 'muted', text: '未检测到安装，仍可接入'})),
        h('p', {class: 'muted client-path', text: `${c.scope} · ${c.path}`}), note ? h('p', {class: 'client-note', text: note}) : null,
        h('div', {class: 'client-actions'}, ...buttons));
    };
    refresh();
    setInterval(() => { if (!document.hidden) refresh(); }, 4000);
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
        if (!['recall', 'read', 'remember', 'link', 'entity'].every(name => names.has(name))) throw new Error('Missing tools');
        mark('tools', 'ok');
        status.textContent = '连接正常，5 个工具可用。现在可以去客户端里添加这个地址了。';
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
        const rank = {error: 4, degraded: 3, working: 2, queued: 2, ready: 1};
        const overall = $('[data-index-overall]', indexPanel);
        if (overall) setState(overall, ['keyword', 'semantic'].map(k => s[k].state).sort((a, b) => (rank[b] || 0) - (rank[a] || 0))[0]);
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

  /* ---------- 数据：后台任务（导出、导入、合并、回退）与进度 ---------- */
  const pollJob = (id, dialog) => new Promise((resolve, reject) => {
    const tick = async () => {
      try {
        const response = await fetch(`/jobs/${id}`);
        if (!response.ok) throw new Error('无法读取任务进度');
        const state = await response.json();
        dialog.set(state.fraction, state.phase);
        if (!state.finished) { setTimeout(tick, 500); return; }
        if (state.error) reject(new Error(state.error)); else resolve(state);
      } catch (error) { reject(error); }
    };
    tick();
  });
  const startJob = async (url, options = {method: 'POST'}) => {
    const response = await fetch(url, options);
    const body = await response.json().catch(() => ({}));
    if (!response.ok) throw new Error(body.error || '操作未能开始');
    return body.id;
  };
  const ask = btn => confirmDialog({title: btn.dataset.confirmTitle, body: btn.dataset.confirmBody, ok: btn.dataset.confirmOk || '确认', tone: btn.dataset.confirmTone});
  const download = (id, file) => {
    const link = h('a', {href: `/download/${id}`, download: file});
    document.body.append(link); link.click(); link.remove();
  };

  for (const btn of $$('[data-export], [data-recovery-export]')) btn.addEventListener('click', async () => {
    const recovery = btn.hasAttribute('data-recovery-export');
    const dialog = progressDialog({title: recovery ? '正在导出回退快照' : '正在导出备份', message: '正在准备…'});
    btn.classList.add('is-busy');
    try {
      const id = await startJob(recovery ? '/recovery/export' : '/backup');
      const state = await pollJob(id, dialog);
      dialog.close();
      download(id, state.result.file);
      const warnings = state.result.info?.warnings || [];
      toast(`已生成 ${state.result.file}（${state.result.size}），已开始下载`, {timeout: 6000});
      if (warnings.length) toast(warnings.slice(0, 3).join('；'), {tone: 'info', timeout: 10000});
    } catch (error) {
      dialog.close();
      toast(error.message, {tone: 'error', timeout: 10000});
    } finally {
      btn.classList.remove('is-busy');
    }
  });

  const finishRestored = (state) => {
    const mode = state.result?.mode;
    try { sessionStorage.setItem('rp-flash', mode === 'rollback' ? '已回到导入之前的状态' : '备份已导入，检索索引已重建'); } catch { /* 忽略 */ }
    location.href = '/data';
  };
  const showMerge = report => openDialog({
    title: '合并完成', icon: 'circle-check',
    body: [
      h('p', {text: `新增 ${report.added} 条记忆，合并了 ${report.merged} 条已存在的记忆，${report.unchanged} 条没有变化。新增 ${report.entities_added} 个实体、${report.attachments_added} 个附件。`}),
      ...(report.same_name_entities.length ? [h('p', {text: `有 ${report.same_name_entities.length} 个名称现在对应不止一个实体，系统没有擅自合并，请到「实体」页确认：${report.same_name_entities.slice(0, 8).join('、')}${report.same_name_entities.length > 8 ? ' 等' : ''}`})] : []),
      h('p', {class: 'muted', text: '已自动保留合并之前的回退快照。'}),
    ],
    actions: [{label: '知道了', value: 'ok', primary: true}],
  }).then(() => { location.href = '/data'; });

  const uploadBackup = (file, mode, dialog) => new Promise((resolve, reject) => {
    const body = new FormData();
    body.append('mode', mode);
    body.append('backup', file);
    const xhr = new XMLHttpRequest();
    xhr.open('POST', '/restore');
    xhr.upload.onprogress = event => { if (event.lengthComputable) dialog.set(event.loaded / event.total, `正在上传 ${formatBytes(event.loaded)} / ${formatBytes(event.total)}`); };
    xhr.upload.onload = () => dialog.set(null, '上传完成，正在校验…');
    xhr.onerror = () => reject(new Error('无法连接本地服务，导入未完成'));
    xhr.onload = () => {
      let json = {};
      try { json = JSON.parse(xhr.responseText); } catch { /* 忽略 */ }
      if (xhr.status === 202 && json.id) resolve(json.id);
      else reject(new Error(json.error || (xhr.status === 413 ? '备份文件过大' : '导入未能开始，当前记忆未被改动')));
    };
    xhr.send(body);
  });

  const restoreForm = $('#restore-form');
  if (restoreForm) {
    const file = restoreForm.elements.backup, zone = $('[data-dropzone]', restoreForm);
    const nameEl = $('[data-file-name]', restoreForm), metaEl = $('[data-file-meta]', restoreForm), submit = $('[data-restore-submit]', restoreForm);
    const show = () => {
      const chosen = file.files[0];
      zone.classList.toggle('has-file', Boolean(chosen));
      nameEl.textContent = chosen ? chosen.name : '选择或拖入备份文件';
      metaEl.textContent = chosen ? formatBytes(chosen.size) : '.recallport.zip';
      submit.disabled = !chosen;
    };
    file.addEventListener('change', show);
    for (const type of ['dragenter', 'dragover']) zone.addEventListener(type, event => { event.preventDefault(); zone.classList.add('is-over'); });
    for (const type of ['dragleave', 'drop']) zone.addEventListener(type, () => zone.classList.remove('is-over'));
    zone.addEventListener('drop', event => {
      event.preventDefault();
      if (event.dataTransfer.files.length) { file.files = event.dataTransfer.files; show(); }
    });
    submit.addEventListener('click', async () => {
      const chosen = file.files[0];
      if (!chosen) return;
      const merge = restoreForm.elements.mode.value === 'merge';
      const total = Number(restoreForm.dataset.total) || 0;
      const ok = await confirmDialog({
        title: merge ? '把此备份合并进当前记忆库？' : '用此备份替换当前记忆库？',
        body: merge
          ? [`文件：${chosen.name}（${formatBytes(chosen.size)}）`, '备份里的记忆会并入现在的库，两边都保留。同名实体不会被自动合并，相同的附件按内容去重。合并前会自动保留回退快照。']
          : [`文件：${chosen.name}（${formatBytes(chosen.size)}）`, `当前的 ${total} 条记忆将被替换，不会合并。替换前会自动保留一份回退快照，之后仍可回退。`],
        ok: merge ? '开始合并' : '确认替换', tone: merge ? undefined : 'danger',
      });
      if (!ok) return;
      const dialog = progressDialog({title: merge ? '正在合并备份' : '正在导入备份', message: '正在上传…'});
      try {
        const id = await uploadBackup(chosen, merge ? 'merge' : 'restore', dialog);
        const state = await pollJob(id, dialog);
        dialog.close();
        if (merge) showMerge(state.result.report); else finishRestored(state);
      } catch (error) {
        dialog.close();
        toast(error.message, {tone: 'error', timeout: 12000});
      }
    });
  }

  const rollback = $('[data-rollback]');
  if (rollback) rollback.addEventListener('click', async () => {
    if (!(await ask(rollback))) return;
    const dialog = progressDialog({title: '正在回退', message: '正在准备…'});
    try {
      const id = await startJob('/recovery/rollback');
      finishRestored(await pollJob(id, dialog));
    } catch (error) {
      dialog.close();
      toast(error.message, {tone: 'error', timeout: 10000});
    }
  });

  const cleanup = $('[data-cleanup]');
  if (cleanup) cleanup.addEventListener('click', async () => {
    if (!(await ask(cleanup))) return;
    cleanup.classList.add('is-busy');
    try {
      const response = await fetch('/files/cleanup', {method: 'POST'});
      if (!response.ok) throw new Error('清理未完成');
      const result = await response.json();
      toast(result.files ? `已清理 ${result.files} 个文件，释放 ${result.size}` : '没有需要清理的文件', {tone: result.files ? 'success' : 'info'});
      if (result.files) setTimeout(() => location.reload(), 1200);
    } catch (error) {
      toast(error.message, {tone: 'error'});
    } finally {
      cleanup.classList.remove('is-busy');
    }
  });

  /* ---------- 设置：自启与退出（托盘版本提供） ---------- */
  const autostart = $('[data-autostart]');
  if (autostart) autostart.addEventListener('change', async () => {
    const want = autostart.checked;
    try {
      const response = await fetch('/settings/autostart', {method: 'POST', headers: {'Content-Type': 'application/x-www-form-urlencoded'}, body: new URLSearchParams({enabled: want ? '1' : '0'})});
      if (!response.ok) throw new Error('设置未能保存');
      toast(want ? '已开启：登录 Windows 后会自动在后台运行' : '已关闭自动启动');
    } catch (error) {
      autostart.checked = !want;
      toast(error.message, {tone: 'error'});
    }
  });
  const quit = $('[data-quit]');
  if (quit) quit.addEventListener('click', async () => {
    if (!(await ask(quit))) return;
    try { await fetch('/settings/quit', {method: 'POST'}); } catch { /* 服务已退出 */ }
    document.body.replaceChildren(h('main', {class: 'shell'}, h('div', {class: 'empty'}, h('h3', {text: 'RecallPort 已退出'}), h('p', {text: '需要时再双击 recallport.exe 启动。'}))));
  });

  /* ---------- 通用：对话框、自动提交 ---------- */
  for (const btn of $$('[data-dialog-open]')) btn.addEventListener('click', () => {
    const dialog = document.getElementById(btn.dataset.dialogOpen);
    dialog.showModal();
    $('input[type=text], input[type=search], textarea', dialog)?.focus();
  });
  for (const dialog of $$('dialog.dialog')) dialog.addEventListener('click', event => { if (event.target === dialog) dialog.close('cancel'); });
  for (const group of $$('[data-autosubmit]')) group.addEventListener('change', () => group.closest('form').requestSubmit());

  /* ---------- 实体选择（详情页添加、实体页合并） ---------- */
  const pickList = (container, items, choose, selectedId) => {
    container.replaceChildren(...items.map(item => {
      const row = h('button', {type: 'button', class: 'pick', role: 'option', 'aria-selected': String(item.id === selectedId)},
        h('time', {text: item.count || ''}), h('span', {text: item.name + (item.hint ? ` · ${item.hint}` : '')}), h('small', {text: item.note || ''}));
      row.addEventListener('click', () => choose(item, row));
      return row;
    }));
  };
  const entityApi = async q => {
    const response = await fetch(`/entity-search?${new URLSearchParams({q})}`);
    if (!response.ok) throw new Error('search failed');
    return (await response.json()).entities;
  };
  const mergeDialog = $('#entity-merge');
  if (mergeDialog) {
    const input = $('[data-entity-search]', mergeDialog), results = $('[data-entity-results]', mergeDialog);
    const target = $('[data-entity-target]', mergeDialog), submit = $('[data-entity-submit]', mergeDialog);
    let timer, epoch = 0;
    const load = async () => {
      const mark = ++epoch;
      try {
        const found = (await entityApi(input.value.trim())).filter(e => e.ID !== mergeDialog.dataset.self);
        if (mark !== epoch) return;
        if (!found.length) { results.replaceChildren(h('div', {class: 'pick-note', text: '没有可合并的实体'})); return; }
        pickList(results, found.map(e => ({id: e.ID, name: e.Name, hint: e.Hint, count: `${e.Mentions} 条`, note: e.Aliases.join('、')})), (item, row) => {
          target.value = item.id; submit.disabled = false;
          for (const other of $$('.pick', results)) other.setAttribute('aria-selected', String(other === row));
        }, target.value);
      } catch { results.replaceChildren(h('div', {class: 'pick-note', text: '搜索失败，请重试'})); }
    };
    input.addEventListener('input', () => { clearTimeout(timer); target.value = ''; submit.disabled = true; timer = setTimeout(load, 220); });
    mergeDialog.addEventListener('toggle', () => { if (mergeDialog.open) load(); });
    new MutationObserver(() => { if (mergeDialog.open) load(); }).observe(mergeDialog, {attributes: true, attributeFilter: ['open']});
  }

  const entityDialog = $('#entity-dialog');
  if (entityDialog) {
    const name = $('[data-entity-search]', entityDialog), results = $('[data-entity-results]', entityDialog);
    const target = $('[data-entity-target]', entityDialog), fresh = $('[data-entity-new]', entityDialog), hintBox = $('[data-entity-hint]', entityDialog);
    const submit = $('[data-entity-submit]', entityDialog);
    let timer, epoch = 0, matches = [];
    const same = (a, b) => a.normalize('NFKC').toLowerCase().trim() === b.normalize('NFKC').toLowerCase().trim();
    const render = () => {
      const typed = name.value.trim();
      const rows = matches.map(e => ({id: e.ID, name: e.Name, hint: e.Hint, count: `${e.Mentions} 条`, note: e.Aliases.join('、')}));
      const exact = matches.some(e => same(e.Name, typed) || e.Aliases.some(a => same(a, typed)));
      if (typed && exact) rows.push({id: '', name: `另一个叫「${typed}」的新事物`, hint: '', count: '新建', note: '与上面的同名但不是同一个，需要填区分说明'});
      pickList(results, rows, (item, row) => {
        target.value = item.id; fresh.value = item.id ? '' : '1';
        hintBox.hidden = Boolean(item.id);
        if (item.id) name.value = matches.find(e => e.ID === item.id).Name;
        for (const other of $$('.pick', results)) other.setAttribute('aria-selected', String(other === row));
      }, target.value);
      if (!rows.length) results.replaceChildren(h('div', {class: 'pick-note', text: typed ? `没有同名的实体，将新建「${typed}」` : '输入名称，或从已有实体中选择'}));
    };
    const load = async () => {
      const mark = ++epoch;
      try { const found = await entityApi(name.value.trim()); if (mark === epoch) { matches = found; render(); } } catch { /* 忽略 */ }
    };
    name.addEventListener('input', () => { clearTimeout(timer); target.value = ''; fresh.value = ''; hintBox.hidden = true; timer = setTimeout(load, 200); });
    entityDialog.querySelector('form').addEventListener('submit', event => {
      const typed = name.value.trim();
      const exact = matches.some(e => same(e.Name, typed) || e.Aliases.some(a => same(a, typed)));
      if (exact && !target.value && !fresh.value) {
        event.preventDefault(); event.stopImmediatePropagation();
        toast('已经有同名的实体：请选择其中一个，或选择「另一个同名的新事物」', {tone: 'info', timeout: 5000});
      }
    }, true);
    for (const btn of $$('[data-entity-open]')) btn.addEventListener('click', () => {
      name.value = btn.dataset.entityName || '';
      target.value = ''; fresh.value = ''; hintBox.hidden = true;
      entityDialog.showModal(); name.focus(); load();
    });
    void submit;
  }

  /* ---------- 附件预览 ---------- */
  const preview = $('#preview-dialog');
  if (preview) {
    const body = $('[data-preview-body]', preview);
    for (const btn of $$('[data-preview]')) btn.addEventListener('click', async () => {
      const {kind, url, name} = btn.dataset;
      $('[data-preview-title]', preview).textContent = name;
      body.replaceChildren();
      const usable = Boolean(btn.dataset.available);
      $('[data-preview-open]', preview).hidden = !usable;
      $('[data-preview-download]', preview).hidden = !usable;
      if (usable) { $('[data-preview-open]', preview).href = url; $('[data-preview-download]', preview).href = `${url}?download=1`; }
      if (!usable) body.append(h('p', {class: 'muted', text: '这个文件没有入库，或已经找不到了，无法预览。'}));
      else if (kind === 'image') body.append(h('img', {src: url, alt: name}));
      else if (kind === 'audio') body.append(h('audio', {src: url, controls: true}));
      else if (kind === 'video') body.append(h('video', {src: url, controls: true}));
      else if (kind === 'pdf') body.append(h('p', {class: 'muted', text: 'PDF 请在新标签页中打开查看。'}));
      else if (kind === 'text') {
        body.append(h('p', {class: 'muted', text: '正在读取…'}));
        try {
          const response = await fetch(`${url}/text`);
          if (!response.ok) throw new Error('unsupported');
          body.replaceChildren(h('pre', {text: await response.text()}));
        } catch { body.replaceChildren(h('p', {class: 'muted', text: '无法预览这个文件，可以下载后查看。'})); }
      } else body.append(h('p', {class: 'muted', text: '这类文件不支持在线预览，可以下载后查看。'}));
      preview.showModal();
    });
    preview.addEventListener('close', () => body.replaceChildren());
  }
})();
