/* 通用交互层：通知、确认/进度对话框、导航进度、复制、快捷键、文本高亮。
   页面专属逻辑在 controls.js。 */
(() => {
  'use strict';
  const doc = document;
  const $ = (selector, root = doc) => root.querySelector(selector);
  const $$ = (selector, root = doc) => [...root.querySelectorAll(selector)];
  const icon = name => `<svg class="icon" aria-hidden="true"><use href="#${name}"/></svg>`;

  /** 创建元素：h('div', {class:'x', text:'…'}, child…)。文本一律走 textContent，避免注入。 */
  const h = (tag, props = {}, ...children) => {
    const node = doc.createElement(tag);
    for (const [key, value] of Object.entries(props)) {
      if (value === undefined || value === null || value === false) continue;
      if (key === 'text') node.textContent = value;
      else if (key === 'html') node.innerHTML = value;
      else node.setAttribute(key, value === true ? '' : value);
    }
    node.append(...children.filter(Boolean));
    return node;
  };
  const formatBytes = bytes => bytes < 1024 ? `${bytes} B` : bytes < 1048576 ? `${(bytes / 1024).toFixed(1)} KB` : `${(bytes / 1048576).toFixed(1)} MB`;

  /* ---------- 通知 ---------- */
  let stack;
  const toast = (message, options = {}) => {
    const tone = options.tone || 'success';
    stack ||= doc.body.appendChild(h('div', {class: 'toasts', 'aria-live': 'polite'}));
    const el = h('div', {class: `toast toast-${tone}`, role: tone === 'error' ? 'alert' : 'status'});
    el.insertAdjacentHTML('afterbegin', icon(tone === 'error' ? 'alert' : tone === 'info' ? 'info' : 'circle-check'));
    el.append(h('span', {class: 'toast-text', text: message}));
    if (options.action) {
      const act = h('button', {type: 'button', class: 'toast-action', text: options.action.label});
      act.addEventListener('click', () => { options.action.run(); dismiss(); });
      el.append(act);
    }
    const close = h('button', {type: 'button', class: 'toast-close', 'aria-label': '关闭通知', html: icon('x')});
    el.append(close);
    stack.append(el);
    requestAnimationFrame(() => el.classList.add('in'));
    let timer;
    const dismiss = () => {
      clearTimeout(timer);
      el.classList.remove('in');
      setTimeout(() => el.remove(), 220);
    };
    const arm = () => {
      const ms = options.timeout ?? (tone === 'error' ? 8000 : 3800);
      if (ms > 0) timer = setTimeout(dismiss, ms);
    };
    close.addEventListener('click', dismiss);
    el.addEventListener('mouseenter', () => clearTimeout(timer));
    el.addEventListener('mouseleave', arm);
    arm();
    while (stack.children.length > 4) stack.firstElementChild.remove();
    return dismiss;
  };

  /* ---------- 对话框 ---------- */
  const openDialog = ({title, body, actions, icon: iconName, tone, size, dismissible = true, label}) => new Promise(resolve => {
    const opener = doc.activeElement;
    const dlg = h('dialog', {class: `dialog${size ? ` dialog-${size}` : ''}`, 'aria-label': label || title});
    const panel = h('div', {class: 'dialog-panel'});
    if (iconName) panel.append(h('div', {class: `dialog-icon${tone === 'danger' ? '' : ' is-info'}`, html: icon(iconName)}));
    panel.append(h('div', {class: 'dialog-head'}, h('h2', {text: title})));
    const content = h('div', {class: 'dialog-body'});
    for (const part of [].concat(body || [])) content.append(typeof part === 'string' ? h('p', {text: part}) : part);
    panel.append(content);
    if (actions?.length) {
      const foot = h('div', {class: 'dialog-foot'});
      for (const action of actions) {
        const btn = h('button', {type: 'button', class: `btn ${action.tone === 'danger' ? 'btn-danger' : action.primary ? 'btn-primary' : ''}`, text: action.label});
        btn.addEventListener('click', () => dlg.close(action.value));
        foot.append(btn);
      }
      panel.append(foot);
    }
    dlg.append(panel);
    doc.body.append(dlg);
    if (dismissible) dlg.addEventListener('click', event => { if (event.target === dlg) dlg.close('cancel'); });
    else dlg.addEventListener('cancel', event => event.preventDefault());
    dlg.addEventListener('close', () => {
      const value = dlg.returnValue || 'cancel';
      dlg.remove();
      if (opener?.isConnected) opener.focus({preventScroll: true});
      resolve(value);
    });
    dlg.showModal();
    (dlg.querySelector('.btn-danger, .btn-primary') || dlg.querySelector('.btn'))?.focus();
    dlg.api = {close: value => dlg.close(value), content, panel};
    openDialog.last = dlg;
  });

  const confirmDialog = async ({title, body, ok = '确认', cancel = '取消', tone, quote}) => {
    const parts = [].concat(body || []);
    if (quote) {
      const text = quote.length > 140 ? `${quote.slice(0, 140)}…` : quote;
      parts.push(h('blockquote', {class: 'quote', text}));
    }
    const value = await openDialog({
      title, body: parts, icon: tone === 'danger' ? 'alert' : 'info', tone,
      actions: [{label: cancel, value: 'cancel'}, {label: ok, value: 'ok', tone, primary: tone !== 'danger'}],
    });
    return value === 'ok';
  };

  /** 进度对话框：不可手动关闭，由调用者 set()/close() 控制。fraction 为 null 表示不确定。 */
  const progressDialog = ({title, message}) => {
    const bar = h('progress', {class: 'bar', max: '1'});
    const pct = h('span', {class: 'pct'});
    const note = h('p', {class: 'progress-note', text: message || ''});
    const body = [h('div', {class: 'progress-row'}, bar, pct), note];
    openDialog({title, body, dismissible: false, size: ''});
    const dlg = openDialog.last;
    const api = {
      set(fraction, text) {
        if (fraction === null || fraction === undefined) { bar.removeAttribute('value'); pct.textContent = ''; }
        else { bar.value = Math.max(0, Math.min(1, fraction)); pct.textContent = `${Math.round(fraction * 100)}%`; }
        if (text) note.textContent = text;
      },
      close() { dlg.close('done'); },
    };
    api.set(null, message);
    return api;
  };

  /* ---------- 页面切换进度条 ---------- */
  const navigating = on => doc.documentElement.classList.toggle('is-navigating', on);
  doc.addEventListener('click', event => {
    const a = event.target.closest?.('a[href]');
    if (!a || event.defaultPrevented || event.button !== 0 || event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
    if (a.target && a.target !== '_self') return;
    if (a.hasAttribute('download')) return;
    const url = new URL(a.href, location.href);
    if (url.origin !== location.origin) return;
    if (url.pathname === location.pathname && url.search === location.search) return;
    navigating(true);
  });
  window.addEventListener('pageshow', () => {
    navigating(false);
    for (const form of $$('form')) { delete form.dataset.confirmed; delete form.dataset.submitting; }
    for (const btn of $$('.is-busy')) btn.classList.remove('is-busy');
  });

  /* ---------- 表单：确认对话框、提交中状态、操作后的提示 ---------- */
  const rememberFlash = form => {
    const message = form.dataset.flashAfter;
    if (!message) return;
    try { sessionStorage.setItem('rp-flash', message); } catch { /* 隐私模式下忽略 */ }
  };
  doc.addEventListener('submit', async event => {
    const form = event.target;
    if (!(form instanceof HTMLFormElement) || !form.dataset.confirmTitle || form.dataset.confirmed) return;
    event.preventDefault();
    const submitter = event.submitter;
    const ok = await confirmDialog({
      title: form.dataset.confirmTitle,
      body: form.dataset.confirmBody,
      quote: form.dataset.confirmQuote,
      ok: form.dataset.confirmOk || '确认',
      tone: form.dataset.confirmTone,
    });
    if (!ok) return;
    form.dataset.confirmed = '1';
    form.requestSubmit(submitter && submitter.form === form ? submitter : undefined);
  }, true);
  doc.addEventListener('submit', event => {
    const form = event.target;
    if (!(form instanceof HTMLFormElement) || event.defaultPrevented) return;
    if (form.dataset.submitting) { event.preventDefault(); return; }
    form.dataset.submitting = '1';
    rememberFlash(form);
    const button = event.submitter || form.querySelector('button:not([type=button])');
    button?.classList.add('is-busy');
    navigating(true);
  });

  /* ---------- 复制 ---------- */
  const copyText = async source => {
    const text = source.value ?? source.textContent;
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      try {
        if (typeof source.select === 'function') { source.focus(); source.select(); }
        else { const range = doc.createRange(); range.selectNodeContents(source); const s = getSelection(); s.removeAllRanges(); s.addRange(range); }
        return doc.execCommand('copy');
      } catch { return false; }
    }
  };
  for (const btn of $$('[data-copy]')) {
    btn.hidden = false;
    let restore;
    btn.addEventListener('click', async () => {
      const source = doc.getElementById(btn.dataset.copy);
      const ok = await copyText(source);
      if (ok) toast('已复制到剪贴板');
      else toast('无法自动复制，已选中内容，请按 Ctrl+C', {tone: 'info'});
      btn.dataset.copied = 'true';
      btn.title = ok ? '已复制' : '已选中';
      clearTimeout(restore);
      restore = setTimeout(() => { delete btn.dataset.copied; btn.title = '复制'; }, 1600);
    });
  }
  for (const input of $$('[data-select-all]')) input.addEventListener('focus', () => input.select());

  /* ---------- 一次性提示：服务端渲染的 data-flash 与跨页 flash ---------- */
  (() => {
    let stored = '';
    try { stored = sessionStorage.getItem('rp-flash') || ''; sessionStorage.removeItem('rp-flash'); } catch { /* 忽略 */ }
    const nodes = $$('[data-flash]');
    if (stored) toast(stored);
    for (const node of nodes) {
      if (!stored) toast(node.textContent.trim(), {tone: node.dataset.flash === 'error' ? 'error' : 'success'});
      node.remove();
    }
    const url = new URL(location.href);
    let changed = false;
    for (const key of ['saved', 'done']) if (url.searchParams.has(key)) { url.searchParams.delete(key); changed = true; }
    if (changed) history.replaceState(history.state, '', url);
  })();

  /* ---------- 快捷键 ---------- */
  const showShortcuts = () => {
    const row = (label, keys) => h('div', {}, h('span', {text: label}), h('span', {html: keys.map(k => `<kbd>${k}</kbd>`).join(' ')}));
    openDialog({
      title: '键盘快捷键', icon: 'keyboard',
      body: [h('div', {class: 'shortcut-list'},
        row('聚焦搜索', ['/']), row('新建记忆', ['N']), row('保存正在编辑的记忆', ['Ctrl', 'Enter']),
        row('关闭对话框', ['Esc']), row('显示本帮助', ['?']),
        h('p', {class: 'muted', text: '关系图里'}), row('在相邻内容间移动', ['←', '↑', '→', '↓']), row('进入选中的领地', ['Enter']), row('缩放 / 适合窗口', ['+', '-', '0']))],
      actions: [{label: '知道了', value: 'ok', primary: true}],
    });
  };
  for (const btn of $$('[data-shortcuts]')) btn.addEventListener('click', showShortcuts);
  doc.addEventListener('keydown', event => {
    if (event.key === 'Enter' && (event.ctrlKey || event.metaKey)) {
      const form = event.target.closest?.('form[data-editor]');
      if (form) { event.preventDefault(); form.requestSubmit(); }
      return;
    }
    if (event.ctrlKey || event.metaKey || event.altKey || event.isComposing) return;
    if (event.target.closest?.('input, textarea, select, [contenteditable]') || $('dialog[open]')) return;
    if (event.key === '/') {
      const search = $('[data-search]');
      if (search) { event.preventDefault(); search.focus(); search.select(); }
      else if (doc.body.dataset.view === 'overview') { event.preventDefault(); navigating(true); location.href = '/memories'; }
    } else if (event.key === 'n' || event.key === 'N') {
      const link = $('[data-shortcut-new]');
      if (link) { navigating(true); location.href = link.href; }
    } else if (event.key === '?') {
      showShortcuts();
    }
  });

  /* ---------- 搜索词高亮 ---------- */
  const highlight = root => {
    const terms = (root.dataset.highlight || '').split(/\s+/).filter(Boolean).slice(0, 12);
    if (!terms.length) return;
    const pattern = new RegExp(terms.map(t => t.replace(/[.*+?^${}()|[\]\\]/g, '\\$&')).join('|'), 'gi');
    for (const el of $$('[data-hl]', root)) {
      const walker = doc.createTreeWalker(el, NodeFilter.SHOW_TEXT);
      const nodes = [];
      while (walker.nextNode()) nodes.push(walker.currentNode);
      for (const node of nodes) {
        const text = node.nodeValue;
        pattern.lastIndex = 0;
        if (!pattern.test(text)) continue;
        pattern.lastIndex = 0;
        const frag = doc.createDocumentFragment();
        let last = 0, match;
        while ((match = pattern.exec(text))) {
          if (!match[0]) { pattern.lastIndex++; continue; }
          frag.append(text.slice(last, match.index), h('mark', {text: match[0]}));
          last = match.index + match[0].length;
        }
        frag.append(text.slice(last));
        node.replaceWith(frag);
      }
    }
  };
  for (const root of $$('[data-highlight]')) highlight(root);

  /* ---------- 主题：自动 / 浅色 / 深色 ---------- */
  const root = doc.documentElement;
  const markTheme = () => {
    for (const btn of $$('[data-theme-set]')) btn.setAttribute('aria-pressed', String(btn.dataset.themeSet === (root.dataset.theme || 'auto')));
  };
  for (const btn of $$('[data-theme-set]')) btn.addEventListener('click', () => {
    const value = btn.dataset.themeSet;
    if (value === 'auto') delete root.dataset.theme; else root.dataset.theme = value;
    try { if (value === 'auto') localStorage.removeItem('rp-theme'); else localStorage.setItem('rp-theme', value); } catch { /* 忽略 */ }
    markTheme();
    window.dispatchEvent(new Event('rp-theme'));
  });
  markTheme();

  /* ---------- 索引重建的跨页面通知 ---------- */
  const rebuildNames = {keyword: '关键词索引', semantic: '语义向量'};
  const rebuilds = {
    read() { try { return JSON.parse(sessionStorage.getItem('rp-rebuild') || '[]'); } catch { return []; } },
    write(list) { try { if (list.length) sessionStorage.setItem('rp-rebuild', JSON.stringify(list)); else sessionStorage.removeItem('rp-rebuild'); } catch { /* 忽略 */ } },
    add(kinds) {
      const list = this.read().filter(item => !kinds.includes(item.kind));
      this.write(list.concat(kinds.map(kind => ({kind, since: Date.now(), seen: false}))));
    },
    remove(kind) { this.write(this.read().filter(item => item.kind !== kind)); },
  };
  // 数据页自己轮询；其他页面在有待完成的重建时，后台等待并在完成时提示。
  if (doc.body.dataset.view !== 'data' && rebuilds.read().length) {
    const tick = async () => {
      const pending = rebuilds.read();
      if (!pending.length) return;
      try {
        const response = await fetch('/index-status');
        if (!response.ok) throw new Error('status unavailable');
        const status = await response.json();
        const left = [];
        for (const item of pending) {
          const state = status[item.kind]?.state;
          const busy = state === 'working' || state === 'queued';
          if (busy) { left.push({...item, seen: true}); continue; }
          if (!item.seen && Date.now() - item.since < 8000) { left.push(item); continue; }
          const go = {label: '查看', run: () => { location.href = '/data'; }};
          if (state === 'ready') toast(`${rebuildNames[item.kind]}已重建完成`, {action: go, timeout: 8000});
          else toast(`${rebuildNames[item.kind]}重建后仍有问题，请到「数据」页查看`, {tone: 'error', action: go});
        }
        rebuilds.write(left);
        if (left.length) setTimeout(tick, 2000);
      } catch { setTimeout(tick, 5000); }
    };
    tick();
  }

  window.RP = {$, $$, h, icon, toast, highlight, openDialog, confirmDialog, progressDialog, formatBytes, navigating, rebuilds};
})();
