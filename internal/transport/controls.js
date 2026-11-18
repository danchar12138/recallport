for (const copy of document.querySelectorAll('[data-copy]')) {
  copy.hidden = false;
  const endpoint = document.getElementById(copy.dataset.copy);
  const status = copy.parentElement.querySelector('.copy-status');
  let restore;
  copy.addEventListener('click', async () => {
    try {
      await navigator.clipboard.writeText(endpoint.value ?? endpoint.textContent);
      status.textContent = '已复制';
    } catch {
      if (typeof endpoint.select === 'function') {
        endpoint.focus();
        endpoint.select();
      } else {
        const selection = window.getSelection();
        const range = document.createRange();
        range.selectNodeContents(endpoint);
        selection.removeAllRanges();
        selection.addRange(range);
      }
      status.textContent = '已选中';
    }
    copy.dataset.copied = 'true';
    copy.title = status.textContent;
    clearTimeout(restore);
    restore = setTimeout(() => {
      delete copy.dataset.copied;
      copy.title = '复制';
      status.textContent = '';
    }, 1600);
  });
}

for (const field of document.querySelectorAll('[data-calendar]')) {
  if (window.flatpickr) window.flatpickr(field, {dateFormat:'Y-m-d', allowInput:true, disableMobile:true, monthSelectorType:'static'});
}

const testMCP = document.getElementById('test-mcp');
if (testMCP) {
  const status = document.getElementById('mcp-status');
  testMCP.hidden = false;
  testMCP.addEventListener('click', async () => {
    testMCP.disabled = true;
    status.textContent = '检查中…';
    status.className = 'small';
    try {
      const signal = AbortSignal.timeout(4000);
      const send = async (body, protocol) => {
        const headers = {'Content-Type':'application/json', Accept:'application/json, text/event-stream'};
        if (protocol) headers['MCP-Protocol-Version'] = protocol;
        const response = await fetch('/mcp', {method:'POST', headers, signal, body:JSON.stringify({jsonrpc:'2.0', ...body})});
        if (!response.ok) throw new Error(`HTTP ${response.status}`);
        if (body.id === undefined) return;
        const reply = await response.json();
        if (!reply.result || reply.error || reply.id !== body.id) throw new Error('Invalid MCP response');
        return reply.result;
      };
      const init = await send({id:1, method:'initialize', params:{protocolVersion:'2025-11-25', capabilities:{}, clientInfo:{name:'RecallPort Web', version:'1'}}});
      await send({method:'notifications/initialized'}, init.protocolVersion);
      const result = await send({id:2, method:'tools/list', params:{}}, init.protocolVersion);
      const names = new Set(result.tools.map(tool => tool.name));
      if (!['recall','read','remember'].every(name => names.has(name))) throw new Error('Missing tools');
      status.textContent = '连接正常 · 3 个工具可用';
      status.className = 'small text-success';
    } catch {
      status.textContent = '连接失败，未能读取工具列表';
      status.className = 'small text-danger';
    } finally {
      testMCP.disabled = false;
    }
  });
  const openTool = () => {
    const tool = document.getElementById(location.hash.slice(1));
    if (tool?.matches('.tool-doc')) tool.open = true;
  };
  openTool();
  window.addEventListener('hashchange',openTool);
}

const indexPanel = document.getElementById('search-index');
if (indexPanel) {
  const labels = {ready:'正常', queued:'等待检查', working:'检查 / 重建中', degraded:'部分可用', error:'待修复', unknown:'待检查'};
  const byId = name => document.getElementById('index-'+name);
  const label = (name, state) => {const item=byId(name);item.dataset.state=state;item.textContent=labels[state] ?? labels.unknown;};
  label('state',byId('state').dataset.state);
  label('keyword',byId('keyword').dataset.keyword);
  const stamp = value => {
    if (!value) return '—';
    const d = new Date(value), pad = v => String(v).padStart(2,'0');
    return `${d.getFullYear()}-${pad(d.getMonth()+1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
  };
  const poll = async () => {
    let next = 5000;
    try {
      const response = await fetch('/index-status');
      if (!response.ok) throw new Error('status unavailable');
      const s = await response.json();
      label('state',s.state); label('keyword',s.keyword);
      byId('problem').hidden = !s.problem;
      byId('rebuild').disabled = s.state === 'working' || s.state === 'queued';
      byId('coverage').textContent = s.model ? `${s.indexed} / ${s.total}` : '未启用';
      byId('progress').max = Math.max(s.total,1); byId('progress').value = s.indexed; byId('progress').hidden = !s.model;
      byId('skipped').hidden = !s.skipped; byId('skipped').textContent = `跳过 ${s.skipped} 条`;
      byId('checked').textContent = stamp(s.checked_at); byId('repaired').textContent = stamp(s.repaired_at);
      byId('model').textContent = s.model || '关键词模式';
      if (s.state === 'working' || s.state === 'queued') next=750;
    } catch { byId('problem').hidden=false; }
    window.setTimeout(poll,next);
  };
  poll();
}

for (const field of document.querySelectorAll('[data-date-control]')) {
  const precision = field.querySelector('select');
  const units = field.querySelectorAll('[data-unit]');
  const update = () => {
    for (const unit of units) {
      const selected = unit.dataset.unit === precision.value;
      const input = unit.querySelector('input');
      unit.hidden = !selected;
      input.disabled = !selected;
      input.required = selected;
    }
  };
  update();
  precision.addEventListener('change', update);
}

const linkPicker = document.getElementById('add-link');
if (linkPicker) {
  const reveal = () => { if (location.hash === '#add-link') linkPicker.open = true; };
  reveal();
  window.addEventListener('hashchange', reveal);
}

for (const form of document.querySelectorAll('form[data-busy]')) {
  const button = form.querySelector('button');
  const initial = button.innerHTML;
  form.addEventListener('submit', () => {
    button.disabled = true;
    button.textContent = form.dataset.busy;
  });
  window.addEventListener('pageshow', () => {
    button.disabled = false;
    button.innerHTML = initial;
  });
}
