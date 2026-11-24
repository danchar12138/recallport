/* The behaviour every page shares, and the one library of interface parts (window.rp) that the page scripts build on.
   Markup contract:
     <button data-theme-set="auto|light|dark">         the theme switch
     <button data-nav-toggle>                           the navigation: an icon rail that opens to full width (a drawer on a phone)
     <button data-copy="text"> / data-copy-target="#id"  copy; the button says 已复制
     <button|form data-confirm="question" data-confirm-text="detail" data-confirm-ok="label">   a dialog asks first
     <button data-dialog-open="id"> / <button data-dialog-close>   open and close a <dialog class="dialog">
     <input data-search> with <button data-search-clear>   the search box that `/` focuses
   rp: h(), icon(), api(), toast(), ask(), picker(), layerBadge(), parseTags() */
(() => {
  'use strict';
  const doc = document;
  const root = doc.documentElement;
  const $ = (selector, scope = doc) => scope.querySelector(selector);
  const $$ = (selector, scope = doc) => [...scope.querySelectorAll(selector)];

  /* ---------- parts the page scripts share ---------- */
  // h('div', {class: 'x', onclick: fn, text: 'hi'}, child, ...): a small element builder; text always goes through textContent.
  const h = (tag, attrs, ...kids) => {
    const node = doc.createElement(tag);
    for (const [key, value] of Object.entries(attrs || {})) {
      if (value === undefined || value === null || value === false) continue;
      if (key === 'class') node.className = value;
      else if (key === 'text') node.textContent = value;
      else if (key === 'value') node.value = value;
      else if (key.startsWith('on')) node.addEventListener(key.slice(2), value);
      else node.setAttribute(key, value === true ? '' : value);
    }
    for (const kid of kids.flat()) if (kid) node.append(kid);
    return node;
  };
  const icon = name => {
    const svg = doc.createElementNS('http://www.w3.org/2000/svg', 'svg');
    svg.setAttribute('class', 'icon');
    svg.setAttribute('aria-hidden', 'true');
    const use = doc.createElementNS('http://www.w3.org/2000/svg', 'use');
    use.setAttribute('href', `#${name}`);
    svg.append(use);
    return svg;
  };
  // A JSON call: {ok, status, data}; status 0 means the local service could not be reached.
  const api = async (method, url, body) => {
    try {
      const response = await fetch(url, {method, headers: body ? {'Content-Type': 'application/json'} : undefined, body: body ? JSON.stringify(body) : undefined});
      const data = await response.json().catch(() => ({}));
      return {ok: response.ok, status: response.status, data};
    } catch {
      return {ok: false, status: 0, data: {error: '连不上本机的 RecallPort 服务，请确认它还在运行。'}};
    }
  };
  const layerBadge = layer => {
    const badge = h('span', {class: 'layer', text: `第 ${layer} 层`});
    badge.dataset.layer = String(Math.min(layer, 5));
    return badge;
  };
  const parseTags = text => {
    const parts = /[,，、;；\n]/.test(text) ? text.split(/[,，、;；\n]/) : text.split(/\s+/);
    const out = [];
    for (const part of parts) {
      const tag = part.trim().replace(/^#+/, '').trim();
      if (tag && !out.includes(tag)) out.push(tag);
    }
    return out;
  };

  /* ---------- toast: the one place a finished action says so ---------- */
  const TOAST_ICON = {ok: 'circle-check', error: 'alert', info: 'info'};
  const dismiss = box => {
    if (!box.isConnected) return;
    box.classList.add('is-leaving');
    setTimeout(() => box.remove(), 180);
  };
  const keep = (box, ms) => setTimeout(() => dismiss(box), ms);
  const toast = (text, tone = 'ok') => {
    const region = $('#toasts');
    if (!region) return;
    const box = h('div', {class: 'toast', role: tone === 'error' ? 'alert' : 'status', 'data-tone': tone}, icon(TOAST_ICON[tone] || TOAST_ICON.ok), h('span', {text}));
    region.append(box);
    keep(box, tone === 'error' ? 6000 : 3200);
  };
  for (const box of $$('#toasts .toast[data-auto]')) keep(box, 3200);

  /* ---------- the dialog: native <dialog>, so focus stays inside, Esc closes and focus goes back to the opener ---------- */
  doc.addEventListener('click', event => {
    const opener = event.target.closest?.('[data-dialog-open]');
    if (opener) { $(`#${opener.dataset.dialogOpen}`)?.showModal(); return; }
    const closer = event.target.closest?.('[data-dialog-close]');
    if (closer) { closer.closest('dialog')?.close(); return; }
    if (event.target instanceof HTMLDialogElement && event.target.classList.contains('dialog')) event.target.close();
  });
  // ask({title, text, ok, danger, fields, check, extra}) -> Promise<boolean>. `fields` is an element shown in the body (a reason,
  // a picker). `check()` (may be async) does the work or validates: it returns '' to close the dialog with a yes, or a message
  // that keeps the dialog open. `extra` is more answers: [{text, check}].
  const ask = ({title, text = '', ok = '确认', danger = false, fields = null, check = null, extra = []}) => new Promise(resolve => {
    const error = h('p', {class: 'field-error', role: 'alert', hidden: true});
    const yes = h('button', {type: 'submit', class: `btn ${danger ? 'btn-danger' : 'btn-primary'}`, text: ok});
    const others = extra.map(item => h('button', {type: 'button', class: 'btn', text: item.text, onclick: () => attempt(item.check)}));
    const no = h('button', {type: 'button', class: 'btn', text: '取消', 'data-dialog-close': ''});
    const form = h('form', {class: 'dialog-form', method: 'dialog'},
      h('div', {class: 'dialog-head'}, h('h2', {id: 'ask-title', text: title})),
      h('div', {class: 'dialog-body'}, text && h('p', {text}), fields, error),
      h('div', {class: 'dialog-foot'}, no, ...others, yes));
    const dlg = h('dialog', {class: 'dialog', 'aria-labelledby': 'ask-title'}, form);
    let answer = false;
    const attempt = async work => {
      const buttons = [no, ...others, yes];
      buttons.forEach(b => { b.disabled = true; });
      const problem = work ? await work() : '';
      buttons.forEach(b => { b.disabled = false; });
      if (problem) { error.textContent = problem; error.hidden = false; return; }
      answer = true;
      dlg.close();
    };
    form.addEventListener('submit', event => { event.preventDefault(); attempt(check); });
    dlg.addEventListener('close', () => { dlg.remove(); resolve(answer); });
    doc.body.append(dlg);
    dlg.showModal();
  });

  /* ---------- pick a node by typing a few words ---------- */
  // `input` is the search box, `list` the (hidden) results menu; `onPick({id, text, layer})` gets the choice.
  const picker = (input, list, onPick, skip = () => false) => {
    let timer = 0;
    let serial = 0;
    const close = () => { list.hidden = true; input.setAttribute('aria-expanded', 'false'); };
    const note = text => {
      list.replaceChildren(h('li', {class: 'pick-note', text}));
      list.hidden = false;
      input.setAttribute('aria-expanded', 'true');
    };
    const show = hits => {
      const rows = hits.filter(hit => !skip(hit.id));
      if (!rows.length) { note('没有找到，换几个字试试'); return; }
      list.replaceChildren(...rows.map(hit => h('li', {role: 'option'},
        h('button', {type: 'button', class: 'pick', onclick: () => { close(); input.value = ''; onPick({id: hit.id, text: hit.title, layer: hit.layer}); }},
          layerBadge(hit.layer), h('span', {class: 'node-text', text: hit.title})))));
      list.hidden = false;
      input.setAttribute('aria-expanded', 'true');
    };
    const search = async () => {
      const q = input.value.trim();
      if (!q) { close(); return; }
      const mine = ++serial;
      const r = await api('GET', `/api/search?k=8&q=${encodeURIComponent(q)}`);
      if (mine !== serial) return;
      if (r.ok) show(r.data.hits); else note(r.data.error || '搜索失败');
    };
    input.addEventListener('input', () => { clearTimeout(timer); timer = setTimeout(search, 250); });
    input.addEventListener('keydown', event => {
      if (event.key === 'Escape' && !list.hidden) { close(); event.stopPropagation(); }
      else if (event.key === 'Enter') { event.preventDefault(); clearTimeout(timer); search(); }
      else if (event.key === 'ArrowDown') { $('.pick', list)?.focus(); event.preventDefault(); }
    });
    list.addEventListener('keydown', event => {
      const items = $$('.pick', list);
      const at = items.indexOf(doc.activeElement);
      if (event.key === 'ArrowDown') { items[Math.min(at + 1, items.length - 1)]?.focus(); event.preventDefault(); }
      else if (event.key === 'ArrowUp') { (at > 0 ? items[at - 1] : input).focus(); event.preventDefault(); }
      else if (event.key === 'Escape') { close(); input.focus(); event.stopPropagation(); }
    });
  };
  window.rp = {h, icon, api, toast, ask, picker, layerBadge, parseTags};

  /* ---------- theme: auto / light / dark ---------- */
  const markTheme = () => {
    for (const btn of $$('[data-theme-set]')) {
      btn.setAttribute('aria-pressed', String(btn.dataset.themeSet === (root.dataset.theme || 'auto')));
    }
  };
  doc.addEventListener('click', event => {
    const btn = event.target.closest?.('[data-theme-set]');
    if (!btn) return;
    const value = btn.dataset.themeSet;
    if (value === 'auto') delete root.dataset.theme; else root.dataset.theme = value;
    try {
      if (value === 'auto') localStorage.removeItem('rp-theme'); else localStorage.setItem('rp-theme', value);
    } catch { /* private mode: the choice just lasts for this page */ }
    markTheme();
    window.dispatchEvent(new Event('rp-theme'));
  });
  markTheme();

  /* ---------- the navigation: an icon rail that a button opens to full width ----------
     On the tree page it opens as a drawer over the picture, on a phone as a drawer from the left (both with a scrim, never
     pushing the content), so neither is remembered; on the other wide pages it pushes the content aside and is remembered. */
  const phone = matchMedia('(max-width: 767px)');
  const treePage = doc.body.dataset.view === 'tree';
  const navOpen = () => root.dataset.nav === 'open';
  const drawer = () => treePage || phone.matches;
  const markNav = () => {
    for (const btn of $$('[data-nav-toggle]')) {
      const label = navOpen() ? '收起导航' : '展开导航';
      btn.setAttribute('aria-expanded', String(navOpen()));
      btn.setAttribute('aria-label', label);
      btn.title = label;
    }
  };
  const setNav = open => {
    if (open) root.dataset.nav = 'open'; else root.removeAttribute('data-nav');
    if (!drawer()) {
      try {
        if (open) localStorage.setItem('rp-nav', 'open'); else localStorage.removeItem('rp-nav');
      } catch { /* private mode: the choice just lasts for this page */ }
    }
    markNav();
  };
  doc.addEventListener('click', event => {
    if (event.target.closest?.('[data-nav-toggle]')) {
      setNav(!navOpen());
      if (navOpen() && phone.matches) $('.tabs a')?.focus();
    } else if (navOpen() && event.target.closest?.('[data-nav-scrim]')) setNav(false);
  });
  doc.addEventListener('keydown', event => {
    if (!navOpen() || !drawer() || event.defaultPrevented) return;
    const toggle = $('[data-nav-toggle]');
    if (event.key === 'Escape') {
      event.preventDefault();
      setNav(false);
      toggle?.focus();
    } else if (event.key === 'Tab' && phone.matches) {
      // a drawer holds the focus until it is closed
      const stops = $$('.tabs a, [data-theme-set], [data-nav-toggle]').filter(el => el.offsetParent !== null || el === toggle);
      const first = stops[0], last = stops[stops.length - 1];
      if (event.shiftKey && doc.activeElement === first) { event.preventDefault(); last.focus(); }
      else if (!event.shiftKey && doc.activeElement === last) { event.preventDefault(); first.focus(); }
    }
  });
  phone.addEventListener('change', () => { if (navOpen() && phone.matches) setNav(false); });
  markNav();

  /* ---------- copy ---------- */
  const copyText = async text => {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      const area = h('textarea', {class: 'sr-only', readonly: true, value: text});
      doc.body.append(area);
      area.select();
      let ok = false;
      try { ok = doc.execCommand('copy'); } catch { /* not available */ }
      area.remove();
      return ok;
    }
  };
  doc.addEventListener('click', async event => {
    const btn = event.target.closest?.('[data-copy], [data-copy-target]');
    if (!btn) return;
    let text = btn.dataset.copy;
    if (text === undefined) {
      const source = $(btn.dataset.copyTarget);
      text = source ? (source.value ?? source.textContent) : '';
    }
    btn.querySelector('.copy-flag')?.remove();
    const flag = h('span', {class: 'copy-flag', role: 'status', text: (await copyText(text)) ? '已复制' : '复制失败，请手动选中'});
    btn.append(flag);
    clearTimeout(btn.copyTimer);
    btn.copyTimer = setTimeout(() => flag.remove(), 1600);
  });

  /* ---------- a dialog asks before anything destructive happens (never the browser's own confirm) ---------- */
  const question = el => ({title: el.dataset.confirm, text: el.dataset.confirmText || '', ok: el.dataset.confirmOk || '确认', danger: /\bbtn-danger\b/.test(el.className)});
  // In the capture phase and without letting the click go on: a page's own handler for the button must not run
  // until the question has been answered with yes (then the click is replayed).
  doc.addEventListener('click', async event => {
    const btn = event.target.closest?.('button[data-confirm], a[data-confirm]');
    if (!btn || btn.dataset.confirmed) return;
    event.preventDefault();
    event.stopPropagation();
    if (await ask(question(btn))) {
      btn.dataset.confirmed = '1';
      btn.click();
      delete btn.dataset.confirmed;
    }
  }, true);
  doc.addEventListener('submit', async event => {
    const form = event.target;
    if (!(form instanceof HTMLFormElement) || !form.dataset.confirm || form.dataset.confirmed) return;
    event.preventDefault();
    event.stopPropagation();
    const submitter = event.submitter || $('button[type=submit], button:not([type])', form);
    if (!submitter) return;
    if (await ask({...question(form), danger: /\bbtn-danger\b/.test(submitter.className)})) {
      form.dataset.confirmed = '1';
      form.requestSubmit(submitter);
    }
  }, true);

  /* ---------- a submitted form cannot be submitted twice ---------- */
  doc.addEventListener('submit', event => {
    const form = event.target;
    if (!(form instanceof HTMLFormElement) || event.defaultPrevented || form.method === 'dialog') return;
    if (form.dataset.busy) { event.preventDefault(); return; }
    form.dataset.busy = '1';
    (event.submitter || $('button[type=submit]', form))?.classList.add('is-busy');
  });
  window.addEventListener('pageshow', () => {
    for (const form of $$('form')) { delete form.dataset.busy; delete form.dataset.confirmed; }
    for (const btn of $$('.is-busy')) btn.classList.remove('is-busy');
  });

  /* ---------- search: the same box, the same clear button, the same `/` everywhere ---------- */
  doc.addEventListener('click', event => {
    const clear = event.target.closest?.('[data-search-clear]');
    if (!clear) return;
    const input = $('input', clear.parentElement);
    input.value = '';
    input.dispatchEvent(new Event('input', {bubbles: true}));
    input.focus();
    if (input.defaultValue && input.form) input.form.requestSubmit();   // a search that is showing: show the full list again
  });
  const searchBox = $('[data-search]');
  if (searchBox && location.hash === '#q') {
    searchBox.focus();
    history.replaceState(null, '', location.pathname + location.search);
  }

  /* ---------- keys that mean the same on every page: / searches, ? explains, Esc closes what is open ---------- */
  doc.addEventListener('keydown', event => {
    if (event.ctrlKey || event.metaKey || event.altKey || event.defaultPrevented) return;
    if (event.target.closest?.('input, textarea, select, [contenteditable]') || $('dialog[open]')) return;
    if (event.key === '?') {
      event.preventDefault();
      $('#help-dialog')?.showModal();
    } else if (event.key === '/' && !treePage) {   // on the tree page the picture owns `/`
      event.preventDefault();
      if (searchBox) { searchBox.focus(); searchBox.select(); } else location.href = '/memories#q';
    }
  });
})();
