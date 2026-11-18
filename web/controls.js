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
  let timer;
  const pendingRebuilds = new Set();
  const feedback = document.getElementById('data-feedback');
  const labels = {ready:'正常', queued:'排队中', working:'处理中', degraded:'部分可用', error:'待修复', unknown:'待检查',disabled:'未启用'};
  const byId = name => document.getElementById('index-'+name);
  const label = (item,state) => {item.dataset.state=state;item.textContent=labels[state] ?? labels.unknown;};
  const lanes = [...indexPanel.querySelectorAll('[data-index-lane]')];
  for (const lane of lanes) {const badge=lane.querySelector('[data-index-state]');label(badge,badge.dataset.state);}
  const stamp = (value, empty) => {
    if (!value) return empty;
    const d = new Date(value), pad = v => String(v).padStart(2,'0');
    return `${d.getFullYear()}-${pad(d.getMonth()+1)}-${pad(d.getDate())} ${pad(d.getHours())}:${pad(d.getMinutes())}:${pad(d.getSeconds())}`;
  };
  const poll = async () => {
    window.clearTimeout(timer);
    let next = 5000;
    try {
      const response = await fetch('/index-status');
      if (!response.ok) throw new Error('status unavailable');
      const s = await response.json();
      let busy = false;
      for (const lane of lanes) {
        const kind=lane.dataset.indexLane,h=s[kind];
        label(lane.querySelector('[data-index-state]'),h.state);
        lane.querySelector('[data-index-checked]').textContent=stamp(h.checked_at,'未检查');
        lane.querySelector('[data-index-repaired]').textContent=stamp(h.repaired_at,'无');
        lane.querySelector('[data-index-problem]').hidden=!h.problem;
        const pending=h.state==='working'||h.state==='queued';
        lane.querySelector('[data-index-rebuild]').disabled=pending||h.state==='disabled'||pendingRebuilds.has(kind)||pendingRebuilds.has('all');
        busy ||= pending;
      }
      indexPanel.querySelector('[data-index-rebuild="all"]').disabled=busy||pendingRebuilds.size>0;
      byId('coverage').textContent = `${s.indexed} / ${s.total}`;
      byId('engine').textContent = s.model ? `LanceDB · ${s.dimensions} 维 · ${s.ann ? 'IVF-HNSW-Flat' : '精确扫描'}` : '语义模型未启用';
      byId('progress').max = Math.max(s.total,1); byId('progress').value = s.indexed;
      byId('skipped').hidden = !s.skipped; byId('skipped').textContent = `跳过 ${s.skipped} 条`;
      if (busy) next=750;
    } catch { for (const lane of lanes) lane.querySelector('[data-index-problem]').hidden=false; }
    timer=window.setTimeout(poll,next);
  };
  for (const button of indexPanel.querySelectorAll('[data-index-rebuild]')) {
    const form=button.form;
    form.addEventListener('submit',async event=>{
      event.preventDefault();
      const kind=button.dataset.indexRebuild;
      if (pendingRebuilds.has(kind)) return;
      pendingRebuilds.add(kind);button.disabled=true;
      try {
        const res=await fetch(form.action,{method:'POST',headers:{Accept:'application/json'},body:new URLSearchParams({kind})});
        feedback.textContent=res.ok ? '重建任务已提交' : res.status===409 ? '索引正在维护或未启用' : '无法提交重建任务';
        feedback.dataset.error=String(!res.ok);
      } catch {feedback.textContent='无法连接本地服务';feedback.dataset.error='true';}
      finally {feedback.hidden=false;pendingRebuilds.delete(kind);poll();}
    });
  }
  poll();
}

const restoreForm = document.getElementById('restore-form');
const restoreDialog = document.getElementById('restore-confirm');
if (restoreForm && restoreDialog?.showModal) {
  const file=restoreForm.elements.backup, confirm=restoreForm.elements.confirm;
  confirm.required=false;
  restoreForm.querySelector('[data-fallback-confirm]').hidden=true;
  file.addEventListener('change',()=>{
    confirm.checked=false;
    restoreForm.querySelector('[data-snapshot-name]').textContent=file.files[0]?.name || '选择备份快照';
  });
  restoreForm.addEventListener('submit',event=>{
    if (confirm.checked) return;
    event.preventDefault();
    restoreDialog.returnValue='cancel';
    restoreDialog.querySelector('[data-selected-snapshot]').textContent=file.files[0].name;
    restoreDialog.showModal();
  });
  restoreDialog.addEventListener('close',()=>{
    if (restoreDialog.returnValue==='confirm') {confirm.checked=true;restoreForm.requestSubmit();}
  });
  window.addEventListener('pageshow',()=>{confirm.checked=false;});
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
  form.addEventListener('submit', event => {
    if (event.defaultPrevented) return;
    button.disabled = true;
    button.textContent = form.dataset.busy;
  });
  window.addEventListener('pageshow', () => {
    button.disabled = false;
    button.innerHTML = initial;
  });
}
