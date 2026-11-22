/* The whole memory tree as one zoomable picture (a squarified treemap on a canvas).
   Every node is a rectangle sized by how many nodes sit at or under it. A rectangle shows its children only when
   it is big enough on screen, so zooming out leaves the top layers as large labelled tiles and zooming in opens
   them level by level down to the memories, whose full text is fetched when their tile is big enough to read. */
(() => {
  'use strict';
  const doc = document;
  const $ = id => doc.getElementById(id);
  const app = $('tree-app');
  if (!app) return;
  const canvas = $('tree-canvas');
  const stage = $('tree-stage');
  const ctx = canvas.getContext('2d', {alpha: false});
  const reduced = matchMedia('(prefers-reduced-motion: reduce)');

  const WORLD_H = 1000;          // the layout lives in a world of this height; the width follows the window
  const OPEN_W = 210;            // a tile shows its children from this many screen pixels on
  const OPEN_H = 118;
  const MIN_TILE = 3;            // smaller tiles are not drawn
  const FULL_W = 190;            // a memory tile this big asks for its full text
  const FULL_H = 70;
  const TEXT_LIMIT = 3000;

  /* ---------- colours: CSS custom properties, read again when the theme changes ---------- */
  let colors = {};
  const readColors = () => {
    const css = getComputedStyle(doc.documentElement);
    const get = name => css.getPropertyValue(name).trim();
    colors = {
      fill: [0, 1, 2, 3, 4, 5].map(i => get(`--layer-${i}`)),
      ink: [0, 1, 2, 3, 4, 5].map(i => get(`--layer-ink-${i}`)),
      gap: get('--tile-gap'),
      accent: get('--accent'),
      hit: get('--hit'),
      text: get('--text'),
      font: getComputedStyle(doc.body).fontFamily,
    };
  };

  /* ---------- the data ---------- */
  let root = null;
  let topLayer = 0;
  const byId = new Map();
  let WW = 1600;
  let WH = WORLD_H;
  let layoutAspect = 1.6;

  const prepare = list => {
    root = {id: '', text: '全部', layer: -1, size: 0, children: list, parent: null, virtual: true, laid: false};
    byId.clear();
    topLayer = 0;
    const stack = [root];
    while (stack.length) {
      const n = stack.pop();
      n.children.sort((a, b) => b.size - a.size);
      for (const c of n.children) {
        c.parent = n;
        c.laid = false;
        c.hitBelow = 0;
        byId.set(c.id, c);
        if (c.layer > topLayer) topLayer = c.layer;
        stack.push(c);
      }
    }
    root.size = list.reduce((sum, c) => sum + c.size, 0);
  };

  /* ---------- layout: squarified treemap, children of a tile only when first needed ---------- */
  const setRect = (n, x, y, w, h) => {
    n.x = x; n.y = y; n.w = w; n.h = h;
    n.laid = false;
    if (n.children.length) {
      n.head = Math.min(h * 0.13, w * 0.09);
      n.pad = n.head * 0.3;
    }
  };
  const squarify = (kids, x, y, w, h) => {
    let total = 0;
    for (const c of kids) total += c.size;
    const scale = (w * h) / total;
    let i = 0;
    while (i < kids.length) {
      const side = Math.min(w, h);
      if (!(side > 1e-9)) { for (; i < kids.length; i++) setRect(kids[i], x, y, 0, 0); break; }
      let sum = 0, max = 0, min = Infinity, worst = Infinity, j = i;
      for (; j < kids.length; j++) {
        const area = kids[j].size * scale;
        const s2 = sum + area, mx = Math.max(max, area), mn = Math.min(min, area);
        const ratio = Math.max((side * side * mx) / (s2 * s2), (s2 * s2) / (side * side * mn));
        if (j > i && ratio > worst) break;
        worst = ratio; sum = s2; max = mx; min = mn;
      }
      const thick = sum / side;
      if (w >= h) {
        let at = y;
        for (let k = i; k < j; k++) { const len = (kids[k].size * scale) / thick; setRect(kids[k], x, at, thick, len); at += len; }
        x += thick; w -= thick;
      } else {
        let at = x;
        for (let k = i; k < j; k++) { const len = (kids[k].size * scale) / thick; setRect(kids[k], at, y, len, thick); at += len; }
        y += thick; h -= thick;
      }
      i = j;
    }
  };
  const layoutChildren = n => {
    if (n.laid) return;
    n.laid = true;
    if (!n.children.length) return;
    if (n.virtual) squarify(n.children, 0, 0, WW, WH);
    else squarify(n.children, n.x + n.pad, n.y + n.head, Math.max(0, n.w - 2 * n.pad), Math.max(0, n.h - n.head - n.pad));
  };
  const ensureLaid = n => {
    const chain = [];
    for (let p = n.parent; p; p = p.parent) chain.push(p);
    for (let i = chain.length - 1; i >= 0; i--) layoutChildren(chain[i]);
  };
  const relayout = () => {
    for (const n of byId.values()) n.laid = false;
    root.laid = false;
  };

  /* ---------- the view: world centre and scale, eased towards a goal ---------- */
  let vw = 0, vh = 0, dpr = 1;
  const view = {cx: 0, cy: 0, k: 1};
  const goal = {cx: 0, cy: 0, k: 1};
  let kFit = 1;
  const clampGoal = g => {
    g.k = Math.min(Math.max(g.k, kFit * 0.5), kFit * 4000);
    g.cx = Math.min(Math.max(g.cx, 0), WW);
    g.cy = Math.min(Math.max(g.cy, 0), WH);
  };
  const panelWidth = () => (!$('tree-panel').hidden && vw > 767 ? Math.min(380, vw - 24) + 24 : 0);
  const go = (g, instant) => {
    Object.assign(goal, g);
    clampGoal(goal);
    if (instant || reduced.matches) Object.assign(view, goal);
    request();
  };
  const fitAll = () => {
    kFit = Math.min(vw / WW, vh / WH) * 0.97;
    go({cx: WW / 2, cy: WH / 2, k: kFit});
  };
  const zoomTo = n => {
    if (n.virtual) { fitAll(); return; }
    ensureLaid(n);
    const free = vw - panelWidth();
    const k = Math.min(free / (n.w * 1.12), vh / (n.h * 1.12));
    go({cx: n.x + n.w / 2 + (panelWidth() / 2) / k, cy: n.y + n.h / 2, k});
  };
  const zoomBy = (factor, px = vw / 2, py = vh / 2) => {
    const k = Math.min(Math.max(goal.k * factor, kFit * 0.5), kFit * 4000);
    const wx = goal.cx + (px - vw / 2) / goal.k;
    const wy = goal.cy + (py - vh / 2) / goal.k;
    go({cx: wx - (px - vw / 2) / k, cy: wy - (py - vh / 2) / k, k});
  };

  /* ---------- text ---------- */
  const measure = doc.createElement('canvas').getContext('2d');
  const widths = [new Map(), new Map()];
  const charW = (ch, bold) => {
    const cache = widths[bold ? 1 : 0];
    let w = cache.get(ch);
    if (w === undefined) {
      measure.font = `${bold ? 600 : 400} 100px ${colors.font}`;
      w = measure.measureText(ch).width;
      cache.set(ch, w);
    }
    return w;
  };
  const textW = (text, fs, bold) => {
    let w = 0;
    for (const ch of text) w += charW(ch, bold);
    return (w * fs) / 100;
  };
  const clip = (text, fs, maxW, bold) => {
    if (textW(text, fs, bold) <= maxW) return text;
    const chars = [...text];
    let w = textW('…', fs, bold);
    let out = '';
    for (const ch of chars) {
      w += (charW(ch, bold) * fs) / 100;
      if (w > maxW) break;
      out += ch;
    }
    return `${out}…`;
  };
  const wrapped = new Map();
  const wrap = (key, text, fs, maxW, maxLines) => {
    const hit = wrapped.get(key);
    if (hit) return hit;
    const lines = [];
    let line = '', w = 0, lastSpace = -1;
    const chars = [...text];
    let i = 0;
    for (; i < chars.length && lines.length < maxLines; i++) {
      const ch = chars[i];
      if (ch === '\n') { lines.push(line); line = ''; w = 0; lastSpace = -1; continue; }
      const cw = (charW(ch, false) * fs) / 100;
      if (w + cw > maxW && line) {
        if (lastSpace > line.length * 0.5) {
          const cut = line.slice(lastSpace + 1);
          lines.push(line.slice(0, lastSpace));
          line = cut; w = textW(cut, fs, false);
        } else { lines.push(line); line = ''; w = 0; }
        lastSpace = -1;
        if (lines.length >= maxLines) break;
      }
      if (ch === ' ') lastSpace = line.length;
      line += ch; w += cw;
    }
    if (lines.length < maxLines && line) lines.push(line);
    if (i < chars.length && lines.length) lines[lines.length - 1] = clip(`${lines[lines.length - 1]}…`, fs, maxW, false);
    if (wrapped.size > TEXT_LIMIT) wrapped.clear();
    wrapped.set(key, lines);
    return lines;
  };

  /* ---------- drawing ---------- */
  let ox = 0, oy = 0, k = 1, frameNo = 0, seenMin = 0;
  const wanted = [];
  let selected = null, hovered = null, hitNodes = [];

  const isOpen = (n, sw, sh) => n.children.length > 0 && sw >= OPEN_W && sh >= OPEN_H;
  const label = (n, x, y, w, h, alpha, center) => {
    const fs = Math.min(Math.max(Math.min(w, h) * 0.12, 10), 18);
    const lines = wrap(`${n.id}|${fs | 0}|${w | 0}|${h | 0}`, n.text, fs, w - 12, Math.max(1, Math.floor((h - 10) / (fs * 1.35))));
    const lh = fs * 1.35;
    ctx.globalAlpha = alpha;
    ctx.fillStyle = colors.ink[Math.min(n.layer, 5)];
    ctx.font = `${fs}px ${colors.font}`;
    ctx.textBaseline = 'top';
    ctx.textAlign = center ? 'center' : 'left';
    const top = center ? y + (h - lines.length * lh) / 2 : y + 6;
    lines.forEach((line, i) => ctx.fillText(line, center ? x + w / 2 : x + 6, top + i * lh + (lh - fs) / 2));
  };
  const leafText = (n, x, y, w, h, alpha) => {
    const big = w >= FULL_W && h >= FULL_H;
    if (big && n.full === undefined && !n.loading) wanted.push(n);
    const fs = w < 90 ? 11 : Math.min(Math.max(w / 48, 13.5), 22);
    const text = big && n.full !== undefined ? n.full : n.text;
    const lh = fs * 1.5;
    const lines = wrap(`${n.id}|${fs}|${w | 0}|${h | 0}|${big && n.full !== undefined ? 'f' : 's'}`, text, fs, w - 14, Math.max(1, Math.floor((h - 10) / lh)));
    ctx.globalAlpha = alpha;
    ctx.fillStyle = colors.ink[0];
    ctx.font = `${fs}px ${colors.font}`;
    ctx.textBaseline = 'top';
    ctx.textAlign = 'left';
    lines.forEach((line, i) => ctx.fillText(line, x + 7, y + 6 + i * lh + (lh - fs) / 2));
  };
  // The title strip of an open tile; it stays inside the window when the tile is much bigger than the window.
  const header = (n, x, y, w, alpha) => {
    const hh = n.head * k;
    const top = Math.max(y, 0);
    const room = y + hh - top;
    const fs = Math.min(room * 0.6, 22);
    if (fs < 8 || w < 50) return;
    ctx.globalAlpha = alpha;
    ctx.fillStyle = colors.ink[Math.min(n.layer, 5)];
    ctx.font = `600 ${fs}px ${colors.font}`;
    ctx.textBaseline = 'middle';
    ctx.textAlign = 'left';
    const pad = Math.min(n.pad * k + 4, 16);
    ctx.fillText(clip(n.text, fs, w - 2 * pad, true), x + pad, top + room * 0.5);
  };
  const ring = (x, y, w, h, color, width, glow) => {
    ctx.save();
    ctx.globalAlpha = 1;
    ctx.strokeStyle = color;
    ctx.lineWidth = width;
    if (glow) { ctx.shadowColor = color; ctx.shadowBlur = 14; }
    if (w < 10 || h < 10) {
      ctx.beginPath();
      ctx.arc(x + w / 2, y + h / 2, 8, 0, Math.PI * 2);
      ctx.stroke();
    } else {
      ctx.strokeRect(x + width / 2, y + width / 2, w - width, h - width);
    }
    ctx.restore();
  };

  const drawNode = (n, alpha) => {
    const sx = ox + n.x * k, sy = oy + n.y * k, sw = n.w * k, sh = n.h * k;
    if (sx > vw || sy > vh || sx + sw < 0 || sy + sh < 0 || sw < MIN_TILE || sh < MIN_TILE) return;
    n.shown = frameNo;
    // the part of the tile inside the window: text is laid out there, so it stays readable at any zoom
    const vx = Math.max(sx, 0), vy = Math.max(sy, 0);
    const vW = Math.min(sx + sw, vw) - vx, vH = Math.min(sy + sh, vh) - vy;
    const gap = sw < 8 || sh < 8 ? 0.4 : 1;
    ctx.globalAlpha = alpha;
    ctx.fillStyle = colors.fill[Math.min(n.layer, 5)];
    ctx.fillRect(sx + gap, sy + gap, sw - 2 * gap, sh - 2 * gap);
    if (!n.children.length) {
      if (vW >= 44 && vH >= 22) leafText(n, vx, vy, vW, vH, alpha);
      if (sw * sh >= 500 && n.layer < seenMin) seenMin = n.layer;
      return;
    }
    if (!isOpen(n, sw, sh)) {
      if (vW >= 44 && vH >= 22) label(n, vx, vy, vW, vH, alpha, true);
      if (n.hitBelow) ring(sx, sy, sw, sh, colors.hit, 2.5, true);
      if (sw * sh >= 500 && n.layer < seenMin) seenMin = n.layer;
      return;
    }
    const fade = Math.min(1, (Math.min(sw / OPEN_W, sh / OPEN_H) - 1) / 0.12);
    if (fade < 1 && vW >= 44 && vH >= 22) label(n, vx, vy, vW, vH, alpha * (1 - fade), true);
    header(n, vx, sy, vW, alpha);
    layoutChildren(n);
    for (const c of n.children) drawNode(c, alpha * fade);
  };

  // The node itself when its tile is on screen, else the nearest ancestor that is.
  const shownOf = n => {
    for (let p = n; p && !p.virtual; p = p.parent) if (p.shown === frameNo) return p;
    return null;
  };
  const overlay = (n, color, width, glow) => {
    ensureLaid(n);
    const tile = shownOf(n);
    if (!tile) return;
    const sx = ox + tile.x * k, sy = oy + tile.y * k, sw = tile.w * k, sh = tile.h * k;
    if (sx > vw || sy > vh || sx + sw < 0 || sy + sh < 0) return;
    ring(sx, sy, sw, sh, color, width, glow);
  };

  const draw = () => {
    frameNo++;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.globalAlpha = 1;
    ctx.fillStyle = colors.gap;
    ctx.fillRect(0, 0, vw, vh);
    k = view.k;
    ox = vw / 2 - view.cx * k;
    oy = vh / 2 - view.cy * k;
    seenMin = Infinity;
    wanted.length = 0;
    layoutChildren(root);
    for (const c of root.children) drawNode(c, 1);
    for (const n of hitNodes) overlay(n, colors.hit, 3, true);
    if (hovered && hovered !== selected) overlay(hovered, colors.text, 1.5, false);
    if (selected) overlay(selected, colors.accent, 3, false);
    ctx.globalAlpha = 1;
  };

  /* ---------- hit testing: the tile the picture shows under a point ---------- */
  const pickAt = (px, py) => {
    const wx = (px - ox) / k, wy = (py - oy) / k;
    let n = root, found = null;
    for (;;) {
      if (!n.virtual && !isOpen(n, n.w * k, n.h * k)) break;
      layoutChildren(n);
      const next = n.children.find(c => wx >= c.x && wx < c.x + c.w && wy >= c.y && wy < c.y + c.h);
      if (!next || next.w * k < MIN_TILE || next.h * k < MIN_TILE) break;
      found = next; n = next;
    }
    return found;
  };
  // The deepest tile that still fills a good part of the window: "where we are".
  const focusNode = () => {
    let n = root, found = root;
    for (;;) {
      if (!n.virtual && !isOpen(n, n.w * k, n.h * k)) break;
      layoutChildren(n);
      const next = n.children.find(c => view.cx >= c.x && view.cx < c.x + c.w && view.cy >= c.y && view.cy < c.y + c.h);
      if (!next || (next.w * k < vw * 0.45 && next.h * k < vh * 0.45)) break;
      found = next; n = next;
    }
    return found;
  };

  /* ---------- the frame loop: redraws only while something changes ---------- */
  let raf = 0, last = 0, crumbId = null, statusText = '', statusTimer = 0;
  const request = () => { if (!raf) raf = requestAnimationFrame(frame); };
  const settled = () => Math.abs(Math.log(goal.k / view.k)) < 0.0005 && Math.abs(goal.cx - view.cx) * view.k < 0.3 && Math.abs(goal.cy - view.cy) * view.k < 0.3;
  const frame = now => {
    raf = 0;
    const dt = Math.min(0.064, (now - last) / 1000 || 0.016);
    last = now;
    if (!settled()) {
      const a = reduced.matches ? 1 : 1 - Math.exp(-dt / 0.085);
      view.k = Math.exp(Math.log(view.k) + (Math.log(goal.k) - Math.log(view.k)) * a);
      view.cx += (goal.cx - view.cx) * a;
      view.cy += (goal.cy - view.cy) * a;
      if (settled()) Object.assign(view, goal);
    }
    draw();
    updateCrumbs();
    updateStatus();
    fetchFull();
    if (!settled()) request();
  };

  const updateStatus = () => {
    const text = seenMin === Infinity ? '' : seenMin === 0 ? '现在看到第 0 层（记忆本身）' : `现在看到第 ${seenMin} 层`;
    if (text === statusText) return;
    statusText = text;
    clearTimeout(statusTimer);
    statusTimer = setTimeout(() => { $('tree-status').textContent = statusText; }, 350);
  };

  /* ---------- breadcrumb of the focused tile ---------- */
  const updateCrumbs = () => {
    const focus = focusNode();
    if (focus.id === crumbId) return;
    crumbId = focus.id;
    const chain = [];
    for (let p = focus; p; p = p.parent) chain.unshift(p);
    const bar = $('tree-crumbs');
    bar.replaceChildren();
    chain.forEach((n, i) => {
      if (i) { const sep = doc.createElement('span'); sep.className = 'sep'; sep.textContent = '›'; sep.setAttribute('aria-hidden', 'true'); bar.append(sep); }
      const btn = doc.createElement('button');
      btn.type = 'button';
      btn.textContent = n.text;
      btn.title = n.text;
      if (i === chain.length - 1) btn.setAttribute('aria-current', 'location');
      btn.addEventListener('click', () => zoomTo(n));
      bar.append(btn);
    });
    bar.scrollLeft = bar.scrollWidth;
  };

  /* ---------- full text of the memories that are big enough to read ---------- */
  let inflight = 0;
  const fetchFull = () => {
    for (const n of wanted) {
      if (inflight >= 3) return;
      if (n.loading || n.full !== undefined) continue;
      n.loading = true;
      inflight++;
      fetch(`/api/node/${encodeURIComponent(n.id)}`)
        .then(r => (r.ok ? r.json() : Promise.reject()))
        .then(data => { n.full = data.content; })
        .catch(() => { n.full = n.text; })
        .finally(() => { n.loading = false; inflight--; request(); });
    }
  };

  /* ---------- the side panel ---------- */
  const panel = $('tree-panel');
  const panelBody = $('tree-panel-body');
  const el = (tag, className, text) => {
    const node = doc.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined) node.textContent = text;
    return node;
  };
  const badge = layer => {
    const b = el('span', 'layer', `第 ${layer} 层`);
    b.dataset.layer = String(Math.min(layer, 5));
    return b;
  };
  let selectSerial = 0;
  const showPanel = (n, data) => {
    const pane = el('div', 'pane');
    const meta = el('div', 'pane-meta');
    meta.append(badge(n.layer));
    if (data) {
      meta.append(el('span', '', `第 ${data.version} 版`));
      if (data.occurred) meta.append(el('span', '', `发生于 ${data.occurred}`));
      if (data.descendants) meta.append(el('span', '', `下面共 ${data.descendants} 条`));
    }
    pane.append(meta);
    if (data && data.path.length) {
      const path = el('nav', 'path');
      path.setAttribute('aria-label', '它在树里的位置');
      data.path.forEach((p, i) => {
        if (i) path.append(el('span', 'sep', '›'));
        const target = byId.get(p.id);
        const btn = el('button', '', p.text);
        btn.type = 'button';
        btn.title = p.text;
        if (target) btn.addEventListener('click', () => select(target, true));
        path.append(btn);
      });
      pane.append(path);
    }
    pane.append(el('p', 'pane-text', data ? data.content : n.text));
    if (data && data.tags.length) {
      const chips = el('span', 'chips');
      for (const tag of data.tags) {
        const a = el('a', 'chip', tag);
        a.href = `/memories?tag=${encodeURIComponent(tag)}`;
        chips.append(a);
      }
      pane.append(chips);
    }
    const actions = el('div', 'btns');
    const open = el('a', 'btn btn-primary btn-sm', '打开节点页');
    open.href = `/node/${encodeURIComponent(n.id)}`;
    const focus = el('button', 'btn btn-sm', '聚焦到这里');
    focus.type = 'button';
    focus.addEventListener('click', () => zoomTo(n));
    actions.append(open, focus);
    pane.append(actions);
    panelBody.replaceChildren(pane);
  };
  const select = (n, zoom) => {
    selected = n;
    panel.hidden = false;
    showPanel(n, null);
    const mine = ++selectSerial;
    fetch(`/api/node/${encodeURIComponent(n.id)}`)
      .then(r => (r.ok ? r.json() : Promise.reject()))
      .then(data => { if (mine === selectSerial) showPanel(n, data); })
      .catch(() => {});
    if (zoom) zoomTo(n);
    request();
  };
  const deselect = () => {
    selected = null;
    panel.hidden = true;
    selectSerial++;
    request();
  };
  $('tree-panel-close').addEventListener('click', deselect);

  /* ---------- the tooltip ---------- */
  const tip = $('tree-tip');
  const showTip = (n, x, y) => {
    if (!n) { tip.hidden = true; return; }
    tip.replaceChildren(badge(n.layer), el('span', '', n.text));
    tip.hidden = false;
    const w = tip.offsetWidth, h = tip.offsetHeight;
    tip.style.transform = `translate(${Math.max(8, Math.min(x + 14, vw - w - 8))}px, ${Math.max(8, Math.min(y + 16, vh - h - 8))}px)`;
  };

  /* ---------- pointer, wheel, keyboard ---------- */
  const pointers = new Map();
  let drag = null, pinch = null, lastTap = {t: 0, x: 0, y: 0};
  const local = event => {
    const rect = canvas.getBoundingClientRect();
    return {x: event.clientX - rect.left, y: event.clientY - rect.top};
  };
  const stopMotion = () => { Object.assign(goal, view); };

  canvas.addEventListener('pointerdown', event => {
    canvas.focus({preventScroll: true});
    canvas.setPointerCapture(event.pointerId);
    const p = local(event);
    pointers.set(event.pointerId, p);
    if (pointers.size === 1) {
      stopMotion();
      drag = {x: p.x, y: p.y, moved: 0};
    } else if (pointers.size === 2) {
      const [a, b] = [...pointers.values()];
      pinch = {d: Math.hypot(a.x - b.x, a.y - b.y) || 1};
      drag = null;
    }
  });
  canvas.addEventListener('pointermove', event => {
    const p = local(event);
    if (pointers.has(event.pointerId)) pointers.set(event.pointerId, p);
    if (pinch && pointers.size >= 2) {
      const [a, b] = [...pointers.values()];
      const d = Math.hypot(a.x - b.x, a.y - b.y) || 1;
      const mx = (a.x + b.x) / 2, my = (a.y + b.y) / 2;
      const factor = d / pinch.d;
      pinch.d = d;
      const kk = Math.min(Math.max(view.k * factor, kFit * 0.5), kFit * 4000);
      const wx = view.cx + (mx - vw / 2) / view.k, wy = view.cy + (my - vh / 2) / view.k;
      view.k = kk;
      view.cx = wx - (mx - vw / 2) / kk;
      view.cy = wy - (my - vh / 2) / kk;
      clampGoal(view);
      Object.assign(goal, view);
      request();
      return;
    }
    if (drag && pointers.has(event.pointerId)) {
      const dx = p.x - drag.x, dy = p.y - drag.y;
      drag.x = p.x; drag.y = p.y;
      drag.moved += Math.abs(dx) + Math.abs(dy);
      if (drag.moved > 5) {
        canvas.classList.add('is-dragging');
        tip.hidden = true;
        view.cx -= dx / view.k;
        view.cy -= dy / view.k;
        clampGoal(view);
        Object.assign(goal, view);
        request();
      }
      return;
    }
    if (event.pointerType === 'mouse') {
      const n = pickAt(p.x, p.y);
      canvas.classList.toggle('is-pointer', !!n);
      if (n !== hovered) { hovered = n; request(); }
      showTip(n, p.x, p.y);
    }
  });
  const release = event => {
    const p = local(event);
    const wasDrag = drag;
    pointers.delete(event.pointerId);
    canvas.classList.remove('is-dragging');
    if (pointers.size < 2) pinch = null;
    if (!wasDrag || event.type === 'pointercancel') { drag = pointers.size ? drag : null; return; }
    drag = null;
    if (wasDrag.moved > 5) return;
    // a tap or click: a second one close by is a double click
    const n = pickAt(p.x, p.y);
    const now = performance.now();
    const twice = now - lastTap.t < 380 && Math.hypot(p.x - lastTap.x, p.y - lastTap.y) < 24;
    lastTap = {t: twice ? 0 : now, x: p.x, y: p.y};
    if (!n) { deselect(); return; }
    if (twice) { select(n, false); zoomTo(n); } else select(n, false);
  };
  canvas.addEventListener('pointerup', release);
  canvas.addEventListener('pointercancel', release);
  canvas.addEventListener('pointerleave', event => {
    if (event.pointerType === 'mouse') { tip.hidden = true; if (hovered) { hovered = null; request(); } }
  });
  canvas.addEventListener('wheel', event => {
    event.preventDefault();
    const p = local(event);
    const unit = event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? vh : 1;
    const speed = event.ctrlKey ? 0.01 : 0.0016;
    zoomBy(Math.exp(-event.deltaY * unit * speed), p.x, p.y);
    tip.hidden = true;
  }, {passive: false});
  canvas.addEventListener('keydown', event => {
    if (event.ctrlKey || event.metaKey || event.altKey) return;
    const step = 0.15;
    const pan = (dx, dy) => go({cx: goal.cx + (dx * vw * step) / goal.k, cy: goal.cy + (dy * vh * step) / goal.k, k: goal.k});
    switch (event.key) {
      case '+': case '=': zoomBy(1.5); break;
      case '-': case '_': zoomBy(1 / 1.5); break;
      case '0': case 'Home': fitAll(); break;
      case 'ArrowLeft': pan(-1, 0); break;
      case 'ArrowRight': pan(1, 0); break;
      case 'ArrowUp': pan(0, -1); break;
      case 'ArrowDown': pan(0, 1); break;
      case 'Escape': deselect(); break;
      case 'Enter': if (selected) { location.href = `/node/${encodeURIComponent(selected.id)}`; } break;
      default: return;
    }
    event.preventDefault();
  });
  $('tree-in').addEventListener('click', () => zoomBy(1.6));
  $('tree-out').addEventListener('click', () => zoomBy(1 / 1.6));
  $('tree-fit').addEventListener('click', fitAll);

  /* ---------- search ---------- */
  const hitList = $('tree-hits');
  const searchBox = $('tree-q');
  const markHits = ids => {
    for (const n of hitNodes) { n.isHit = false; for (let p = n.parent; p; p = p.parent) p.hitBelow = 0; }
    hitNodes = ids.map(id => byId.get(id)).filter(Boolean);
    for (const n of hitNodes) { n.isHit = true; for (let p = n.parent; p; p = p.parent) p.hitBelow = 1; }
    request();
  };
  const jump = n => {
    hitList.hidden = true;
    select(n, false);
    zoomTo(n.children.length || !n.parent || n.parent.virtual ? n : n.parent);
  };
  const say = text => { $('tree-status').textContent = text; statusText = ''; };
  const search = async () => {
    const q = searchBox.value.trim();
    if (!q) { markHits([]); hitList.hidden = true; return; }
    say('正在搜索……');
    try {
      const response = await fetch(`/api/search?k=10&q=${encodeURIComponent(q)}`);
      const body = await response.json();
      if (!response.ok) throw new Error(body.error || '搜索失败');
      const hits = body.hits.filter(h => byId.has(h.id));
      markHits(hits.map(h => h.id));
      if (!hits.length) { hitList.hidden = true; say('没有找到'); return; }
      hitList.replaceChildren(...hits.map(h => {
        const li = el('li');
        const btn = el('button', 'pick');
        btn.type = 'button';
        btn.append(badge(h.layer), el('span', 'node-text', (h.content.split('\n').find(l => l.trim()) || '').trim().slice(0, 100)));
        btn.addEventListener('click', () => jump(byId.get(h.id)));
        li.append(btn);
        return li;
      }));
      hitList.hidden = false;
      say(`找到 ${hits.length} 条${body.no_strong_match ? '（没有明确匹配，只是线索）' : ''}，已跳到第一条`);
      jump(byId.get(hits[0].id));
    } catch (error) {
      say(error.message || '搜索失败');
    }
  };
  $('tree-search').addEventListener('submit', event => { event.preventDefault(); search(); });
  searchBox.addEventListener('keydown', event => {
    if (event.key === 'Escape') { searchBox.value = ''; markHits([]); hitList.hidden = true; }
  });
  searchBox.addEventListener('focus', () => { if (hitList.children.length && hitNodes.length) hitList.hidden = false; });
  searchBox.addEventListener('input', () => { if (!searchBox.value.trim()) { markHits([]); hitList.hidden = true; } });

  /* ---------- legend and a text version of the top layer ---------- */
  const buildLegend = () => {
    const legend = $('tree-legend');
    const shown = Math.min(topLayer, 5);
    for (let layer = 0; layer <= shown; layer++) {
      const item = el('li');
      const sw = el('span', 'layer-sw');
      sw.dataset.layer = String(layer);
      sw.setAttribute('aria-hidden', 'true');
      item.append(sw, el('span', '', layer === 5 && topLayer > 5 ? '第 5 层及以上' : layer === 0 ? '第 0 层 记忆' : `第 ${layer} 层`));
      legend.append(item);
    }
  };
  const buildOutline = () => {
    const list = $('tree-outline');
    for (const n of root.children.slice(0, 40)) {
      const li = el('li');
      const a = el('a', '', `第 ${n.layer} 层：${n.text}`);
      a.href = `/node/${encodeURIComponent(n.id)}`;
      li.append(a);
      list.append(li);
    }
  };

  /* ---------- size, theme, start ---------- */
  const resize = () => {
    const rect = stage.getBoundingClientRect();
    if (!rect.width || !rect.height) return;
    vw = rect.width; vh = rect.height;
    dpr = window.devicePixelRatio || 1;
    canvas.width = Math.round(vw * dpr);
    canvas.height = Math.round(vh * dpr);
    if (!root) return;
    const aspect = Math.min(Math.max(vw / vh, 0.7), 2.4);
    if (Math.abs(aspect / layoutAspect - 1) > 0.3) {
      layoutAspect = aspect;
      WW = WORLD_H * aspect;
      relayout();
      fitAll();
      Object.assign(view, goal);
    }
    kFit = Math.min(vw / WW, vh / WH) * 0.97;
    request();
  };
  const theme = () => { readColors(); wrapped.clear(); request(); };
  window.addEventListener('rp-theme', theme);
  matchMedia('(prefers-color-scheme: dark)').addEventListener('change', theme);
  reduced.addEventListener('change', request);

  const start = async () => {
    readColors();
    const message = $('tree-msg');
    try {
      const response = await fetch('/api/outline');
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      const list = await response.json();
      if (!list.length) { message.textContent = '树还是空的。'; return; }
      prepare(list);
      const rect = stage.getBoundingClientRect();
      vw = rect.width; vh = rect.height;
      layoutAspect = Math.min(Math.max(vw / vh, 0.7), 2.4);
      WW = WORLD_H * layoutAspect;
      resize();
      fitAll();
      Object.assign(view, goal);
      buildLegend();
      buildOutline();
      message.hidden = true;
      new ResizeObserver(resize).observe(stage);
      request();
    } catch (error) {
      message.replaceChildren(el('span', '', `没能画出这棵树：${error.message}。刷新页面再试一次。`));
    }
  };
  start();
})();
