/* Generic behaviour every page can use, no page-specific logic:
   theme buttons, copy buttons, two-step confirmation, double-submit guard.
   Markup contract:
     <button data-theme-set="auto|light|dark">
     <button data-copy="text"> or <button data-copy-target="#id">    (shows 已复制)
     <button data-confirm="question"> / <form data-confirm="question">  (inline two-step confirmation) */
(() => {
  'use strict';
  const doc = document;
  const root = doc.documentElement;
  const $$ = (selector, scope = doc) => [...scope.querySelectorAll(selector)];

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

  /* ---------- copy ---------- */
  const copyText = async text => {
    try {
      await navigator.clipboard.writeText(text);
      return true;
    } catch {
      const area = doc.createElement('textarea');
      area.value = text;
      area.setAttribute('readonly', '');
      area.className = 'sr-only';
      doc.body.append(area);
      area.select();
      let ok = false;
      try { ok = doc.execCommand('copy'); } catch { /* not available */ }
      area.remove();
      return ok;
    }
  };
  const flag = (btn, text) => {
    btn.querySelector('.copy-flag')?.remove();
    const tip = doc.createElement('span');
    tip.className = 'copy-flag';
    tip.setAttribute('role', 'status');
    tip.textContent = text;
    btn.append(tip);
    clearTimeout(btn.copyTimer);
    btn.copyTimer = setTimeout(() => tip.remove(), 1600);
  };
  doc.addEventListener('click', async event => {
    const btn = event.target.closest?.('[data-copy], [data-copy-target]');
    if (!btn) return;
    let text = btn.dataset.copy;
    if (text === undefined) {
      const source = doc.querySelector(btn.dataset.copyTarget);
      text = source ? (source.value ?? source.textContent) : '';
    }
    flag(btn, (await copyText(text)) ? '已复制' : '复制失败，请手动选中');
  });

  /* ---------- two-step confirmation, inline (no browser dialogs) ---------- */
  const ask = (trigger, question) => new Promise(resolve => {
    const box = doc.createElement('span');
    box.className = 'confirm';
    box.setAttribute('role', 'alert');
    const text = doc.createElement('span');
    text.className = 'confirm-q';
    text.textContent = question;
    const yes = doc.createElement('button');
    yes.type = 'button';
    yes.className = 'btn btn-sm btn-danger';
    yes.textContent = '确认';
    const no = doc.createElement('button');
    no.type = 'button';
    no.className = 'btn btn-sm';
    no.textContent = '取消';
    box.append(text, yes, no);
    const finish = answer => {
      box.remove();
      trigger.hidden = false;
      if (!answer) trigger.focus();
      resolve(answer);
    };
    yes.addEventListener('click', () => finish(true));
    no.addEventListener('click', () => finish(false));
    box.addEventListener('keydown', event => { if (event.key === 'Escape') { event.stopPropagation(); finish(false); } });
    trigger.after(box);
    trigger.hidden = true;
    no.focus();
  });

  doc.addEventListener('click', async event => {
    const btn = event.target.closest?.('button[data-confirm], a[data-confirm]');
    if (!btn || btn.dataset.confirmed) return;
    event.preventDefault();
    if (await ask(btn, btn.dataset.confirm)) {
      btn.dataset.confirmed = '1';
      btn.click();
      delete btn.dataset.confirmed;
    }
  });
  doc.addEventListener('submit', async event => {
    const form = event.target;
    if (!(form instanceof HTMLFormElement) || !form.dataset.confirm || form.dataset.confirmed) return;
    event.preventDefault();
    const submitter = event.submitter || form.querySelector('button[type=submit], button:not([type])');
    if (!submitter) return;
    if (await ask(submitter, form.dataset.confirm)) {
      form.dataset.confirmed = '1';
      form.requestSubmit(submitter);
    }
  }, true);

  /* ---------- a submitted form cannot be submitted twice ---------- */
  doc.addEventListener('submit', event => {
    const form = event.target;
    if (!(form instanceof HTMLFormElement) || event.defaultPrevented) return;
    if (form.dataset.busy) { event.preventDefault(); return; }
    form.dataset.busy = '1';
    (event.submitter || form.querySelector('button[type=submit]'))?.classList.add('is-busy');
  });
  window.addEventListener('pageshow', () => {
    for (const form of $$('form')) { delete form.dataset.busy; delete form.dataset.confirmed; }
    for (const btn of $$('.is-busy')) btn.classList.remove('is-busy');
  });
})();
