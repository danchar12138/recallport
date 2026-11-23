/* The node page and the editor: the family tree that opens level by level, picking a node by searching (move,
   references), tags as chips, the date field, the character counter, file list. Every field also works as a plain
   form without this script. */
(() => {
  'use strict';
  const doc = document;
  const $ = (selector, scope = doc) => scope.querySelector(selector);
  const make = (tag, className, text) => {
    const el = doc.createElement(tag);
    if (className) el.className = className;
    if (text !== undefined) el.textContent = text;
    return el;
  };
  const icon = name => {
    const svg = doc.createElementNS('http://www.w3.org/2000/svg', 'svg');
    svg.setAttribute('class', 'icon icon-xs');
    svg.setAttribute('aria-hidden', 'true');
    const use = doc.createElementNS('http://www.w3.org/2000/svg', 'use');
    use.setAttribute('href', `#${name}`);
    svg.append(use);
    return svg;
  };
    const layerBadge = layer => {
    const badge = make('span', 'layer', `第 ${layer} 层`);
    badge.dataset.layer = String(Math.min(layer, 5));
    return badge;
  };

  /* ---------- the family tree: a node whose children were not sent asks for them when it is opened ---------- */
  const famItem = node => {
    const link = make('a', 'node-link');
    link.href = `/node/${encodeURIComponent(node.id)}`;
    link.append(layerBadge(node.layer), make('span', 'node-text', node.title || '（没有文字）'));
    const li = make('li');
    if (node.children && !node.children.length) { li.append(link); return li; }
    const fold = make('details');
    fold.dataset.lazy = node.id;
    const kids = make('ul');
    const summary = make('summary');
    summary.append(link, make('small', 'muted', `下面 ${node.size - 1} 条`));
    fold.append(summary, kids);
    li.append(fold);
    return li;
  };
  const lazyFold = async fold => {
    const kids = $('ul', fold);
    if (fold.dataset.loaded || kids.children.length) return;
    fold.dataset.loaded = '1';
    kids.replaceChildren(make('li', 'muted', '正在取……'));
    try {
      const response = await fetch(`/api/outline?root=${encodeURIComponent(fold.dataset.lazy)}&depth=1`);
      const list = await response.json();
      if (!response.ok) throw new Error();
      kids.replaceChildren(...((list[0] && list[0].children) || []).map(famItem));
    } catch {
      delete fold.dataset.loaded;
      kids.replaceChildren(make('li', 'muted', '没取到，再点一下试试'));
    }
  };
  const family = $('[data-family]');
  if (family) {
    // one listener for the folds that are there and the ones made later
    family.addEventListener('toggle', event => { if (event.target.dataset && event.target.dataset.lazy && event.target.open) lazyFold(event.target); }, true);
  }

  /* ---------- pick a node by typing a few words ---------- */
  // `input` is the search box, `list` the (hidden) results list; `onPick({id, text, layer})` gets the choice.
  const picker = (input, list, onPick, skip = () => false) => {
    let timer = 0;
    let serial = 0;
    const close = () => { list.hidden = true; input.setAttribute('aria-expanded', 'false'); };
    const note = text => {
      list.replaceChildren(make('li', 'pick-note', text));
      list.hidden = false;
      input.setAttribute('aria-expanded', 'true');
    };
    const show = hits => {
      const rows = hits.filter(hit => !skip(hit.id));
      if (!rows.length) { note('没有找到，换几个字试试'); return; }
      list.replaceChildren(...rows.map(hit => {
        const row = make('li');
        row.setAttribute('role', 'option');
        const btn = make('button', 'pick');
        btn.type = 'button';
        const text = hit.title;
        btn.append(layerBadge(hit.layer), make('span', 'node-text', text));
        btn.addEventListener('click', () => { close(); input.value = ''; onPick({id: hit.id, text, layer: hit.layer}); });
        row.append(btn);
        return row;
      }));
      list.hidden = false;
      input.setAttribute('aria-expanded', 'true');
    };
    const search = async () => {
      const q = input.value.trim();
      if (!q) { close(); return; }
      const mine = ++serial;
      try {
        const response = await fetch(`/api/search?k=8&q=${encodeURIComponent(q)}`);
        const body = await response.json();
        if (mine !== serial) return;
        if (!response.ok) note(body.error || '搜索失败');
        else show(body.hits);
      } catch {
        if (mine === serial) note('搜索失败，请稍后再试');
      }
    };
    input.addEventListener('input', () => { clearTimeout(timer); timer = setTimeout(search, 250); });
    input.addEventListener('keydown', event => {
      if (event.key === 'Escape') { close(); }
      else if (event.key === 'Enter') { event.preventDefault(); clearTimeout(timer); search(); }
      else if (event.key === 'ArrowDown') { $('.pick', list)?.focus(); event.preventDefault(); }
    });
    list.addEventListener('keydown', event => {
      const items = [...list.querySelectorAll('.pick')];
      const at = items.indexOf(doc.activeElement);
      if (event.key === 'ArrowDown') { items[Math.min(at + 1, items.length - 1)]?.focus(); event.preventDefault(); }
      else if (event.key === 'ArrowUp') { (at > 0 ? items[at - 1] : input).focus(); event.preventDefault(); }
      else if (event.key === 'Escape') { close(); input.focus(); }
    });
  };

  /* ---------- the move form on the node page ---------- */
  const move = $('form[data-move]');
  if (move) {
    const find = $('[data-move-find]', move);
    const search = $('[data-move-search]', move);
    const target = $('input[name=parent]', move);
    const picked = $('#move-picked', move);
    find.hidden = false;
    picker(search, $('#move-hits', move), choice => {
      target.value = choice.id;
      picked.textContent = `已选：${choice.text}`;
      picked.className = 'hint is-ok';
    }, id => id === move.dataset.self);
    target.addEventListener('input', () => { picked.textContent = ''; });
  }

  /* ---------- the editor ---------- */
  const form = $('form[data-editor]');
  if (!form) return;
  let dirty = false;
  let submitting = false;
  const touch = () => {
    dirty = true;
    const flag = $('[data-dirty]', form);
    if (flag) flag.hidden = false;
  };
  form.addEventListener('input', touch);
  form.addEventListener('change', touch);
  form.addEventListener('submit', () => { submitting = true; });
  window.addEventListener('beforeunload', event => { if (dirty && !submitting) { event.preventDefault(); event.returnValue = ''; } });
  form.addEventListener('keydown', event => {
    if (event.key === 'Enter' && (event.ctrlKey || event.metaKey)) { event.preventDefault(); form.requestSubmit(); }
  });

  // content: counter and a textarea that grows
  const content = $('textarea[name=content]', form);
  const counter = $('[data-counter]', form);
  const grow = () => { content.style.height = 'auto'; content.style.height = `${content.scrollHeight + 2}px`; };
  const count = () => {
    const n = [...content.value].length;
    counter.textContent = `${n} / 16000`;
    counter.classList.toggle('is-near', n > 3600 && n <= 4000);
    counter.classList.toggle('is-over', n > 4000);
  };
  content.addEventListener('input', () => { count(); grow(); });
  count();
  grow();

  // tags: chips; the original text field stays the value that is sent
  const tagField = $('input[data-tags]', form);
  if (tagField) {
    const parse = text => {
      const parts = /[,，、;；\n]/.test(text) ? text.split(/[,，、;；\n]/) : text.split(/\s+/);
      const out = [];
      for (const part of parts) {
        const tag = part.trim().replace(/^#+/, '').trim();
        if (tag && !out.includes(tag)) out.push(tag);
      }
      return out;
    };
    let tags = parse(tagField.value);
    const wrap = make('div', 'tag-input');
    const chips = make('span', 'tag-chips');
    const entry = make('input');
    entry.type = 'text';
    entry.id = tagField.id;
    entry.placeholder = tagField.placeholder;
    entry.autocomplete = 'off';
    entry.setAttribute('aria-describedby', tagField.getAttribute('aria-describedby'));
    tagField.removeAttribute('id');
    tagField.type = 'hidden';
    tagField.after(wrap);
    wrap.append(chips, entry);
    const sync = () => {
      tagField.value = tags.join(', ') + (tags.length === 1 && /\s/.test(tags[0]) ? ',' : '');
      chips.replaceChildren(...tags.map(tag => {
        const chip = make('span', 'chip');
        const remove = make('button', 'chip-x-btn');
        remove.type = 'button';
        remove.setAttribute('aria-label', `去掉标签 ${tag}`);
        remove.append(icon('x'));
        remove.addEventListener('click', () => { tags = tags.filter(t => t !== tag); sync(); touch(); entry.focus(); });
        chip.append(make('span', 'chip-text', tag), remove);
        return chip;
      }));
    };
    const commit = () => {
      const added = parse(entry.value.replace(/[,，、;；]+$/, ''));
      entry.value = '';
      if (!added.length) return;
      for (const tag of added) if (!tags.includes(tag)) tags.push(tag);
      sync();
      touch();
    };
    entry.addEventListener('keydown', event => {
      if (event.isComposing) return;
      if (event.key === 'Enter' || event.key === ',' || event.key === '，') { event.preventDefault(); event.stopPropagation(); commit(); }
      else if (event.key === 'Backspace' && !entry.value && tags.length) { tags.pop(); sync(); touch(); }
    });
    entry.addEventListener('blur', commit);
    wrap.addEventListener('click', event => { if (event.target === wrap) entry.focus(); });
    form.addEventListener('submit', commit, true);
    sync();
  }

  // occurred: precision buttons, today, clear, and a native date picker
  const date = $('input[data-date]', form);
  if (date) {
    const hint = $('[data-date-hint]', form);
    const original = hint.textContent;
    const levels = [['年', 4], ['月', 7], ['日', 10]];
    const tools = make('span', 'date-tools');
    const precision = make('span', 'segmented');
    precision.setAttribute('role', 'group');
    precision.setAttribute('aria-label', '精确到');
    const buttons = levels.map(([label, length]) => {
      const btn = make('button', '', label);
      btn.type = 'button';
      btn.title = `精确到${label}`;
      btn.addEventListener('click', () => { date.value = date.value.trim().slice(0, length); date.dispatchEvent(new Event('input', {bubbles: true})); });
      precision.append(btn);
      return btn;
    });
    const today = make('button', 'btn btn-sm', '今天');
    today.type = 'button';
    const clear = make('button', 'btn btn-sm btn-ghost', '清空');
    clear.type = 'button';
    const pick = make('button', 'btn btn-sm btn-icon', '');
    pick.type = 'button';
    pick.title = '用日历选择';
    pick.setAttribute('aria-label', '用日历选择日期');
    pick.append(icon('calendar'));
    const ghost = make('input', 'date-ghost');
    ghost.type = 'date';
    ghost.tabIndex = -1;
    ghost.setAttribute('aria-hidden', 'true');
    tools.append(precision, today, clear, pick, ghost);
    date.after(tools);
    const check = () => {
      const value = date.value.trim();
      const ok = value === '' || /^\d{4}(-\d{2}(-\d{2}([T ].*)?)?)?$/.test(value);
      date.setAttribute('aria-invalid', String(!ok));
      hint.textContent = ok ? original : '要写成 2024-09-12，或只写 2024-09、2024';
      hint.classList.toggle('is-error', !ok);
      levels.forEach(([, length], i) => {
        buttons[i].disabled = !ok || value.length < length;
        buttons[i].setAttribute('aria-pressed', String(ok && value.length >= length && (value.length === length || (i === 2 && value.length > 10))));
      });
    };
    date.addEventListener('input', check);
    today.addEventListener('click', () => {
      const now = new Date();
      const pad = n => String(n).padStart(2, '0');
      date.value = `${now.getFullYear()}-${pad(now.getMonth() + 1)}-${pad(now.getDate())}`;
      date.dispatchEvent(new Event('input', {bubbles: true}));
    });
    clear.addEventListener('click', () => { date.value = ''; date.dispatchEvent(new Event('input', {bubbles: true})); date.focus(); });
    pick.addEventListener('click', () => {
      ghost.value = /^\d{4}-\d{2}-\d{2}/.test(date.value) ? date.value.slice(0, 10) : '';
      try { ghost.showPicker(); } catch { ghost.focus(); }
    });
    ghost.addEventListener('change', () => { date.value = ghost.value; date.dispatchEvent(new Event('input', {bubbles: true})); });
    check();
  }

  // references: chips with a hidden input each, and a search to add more
  const refs = $('[data-refs]', form);
  if (refs) {
    const chips = $('[data-ref-chips]', refs);
    const search = $('[data-ref-search]', refs);
    const have = () => [...chips.querySelectorAll('input[name=refs]')].map(input => input.value);
    search.hidden = false;
    chips.addEventListener('click', event => {
      const btn = event.target.closest('[data-ref-remove]');
      if (!btn) return;
      btn.closest('li').remove();
      touch();
      search.focus();
    });
    picker(search, $('#f-refs-hits', refs), choice => {
      const chip = make('li', 'chip');
      const hidden = make('input');
      hidden.type = 'hidden';
      hidden.name = 'refs';
      hidden.value = choice.id;
      const remove = make('button', 'chip-x-btn');
      remove.type = 'button';
      remove.dataset.refRemove = '';
      remove.setAttribute('aria-label', `取消引用：${choice.text}`);
      remove.append(icon('x'));
      chip.append(layerBadge(choice.layer), make('span', 'chip-text', choice.text), hidden, remove);
      chips.append(chip);
      touch();
    }, id => id === form.dataset.self || have().includes(id));
  }

  // files: what was chosen, and a warning for the ones that are too big
  const files = $('input[data-files]', form);
  if (files) {
    const list = $('[data-file-list]', form);
    const MAX = 64 * 1024 * 1024;
    const size = bytes => bytes < 1048576 ? `${Math.max(1, Math.round(bytes / 1024))} KB` : `${(bytes / 1048576).toFixed(1)} MB`;
    files.addEventListener('change', () => {
      list.replaceChildren(...[...files.files].map(file => {
        const big = file.size > MAX;
        const row = make('li', big ? 'is-big' : '', big ? `${file.name}（${size(file.size)}）超过 64 MB，不会上传` : `${file.name}（${size(file.size)}）`);
        return row;
      }));
    });
    form.addEventListener('submit', event => {
      if ([...files.files].some(file => file.size > MAX)) {
        event.preventDefault();
        files.focus();
      }
    });
  }
})();
