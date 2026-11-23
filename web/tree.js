/* The whole memory tree as one zoomable picture (a treemap on a canvas) with an outline beside it.
   Every node is a card sized by how much sits under it (a power of the count, so small branches stay visible).
   A card shows its children only when it is big enough on screen, so zooming in opens the tree level by level down
   to the memories themselves. Children are fetched when a card is about to open; text is drawn only when it can
   be read, otherwise a tile stays a plain block of colour. */
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

  const WORLD_H = 1000;            // the top level is laid out in a world of this height; the width follows the window
  const OPEN_W = 260, OPEN_H = 150;      // a card shows its children from this many screen pixels on
  const GROUP_W = 190, GROUP_H = 110;    // an "other" tile shows what it holds from here on
  const SNIP_W = 220, SNIP_H = 110;      // a closed tile also shows a piece of its text
  const FULL_W = 300, FULL_H = 170;      // a memory this big asks for its whole text
  const GAP = 5;                         // space between neighbouring cards
  const PAD = 6;                         // space between a card's edge and its children
  const MIN_TILE = 3;                    // smaller tiles are not drawn
  const MIN_FONT = 11;                   // text is never drawn smaller than this
  const TILE_AREA = 3200;                // a child that would get less than this many pixels when its parent just opened is merged into "other"
  const MAX_HEAD = 12;                   // most tiles a card shows by name next to its "other" tile
  const FANOUT = 6;                      // how many "other" tiles a crowd is split into
  const POWER = 0.7;                     // area grows with size to this power
  const DIM = 0.35;
  const OPEN_ZOOM = 1.12;                // below the top layer, cards open only once zoomed in this far past "fit"; so fit shows two layers
  const TITLE_PX = [12, 13, 14, 15, 16]; // title size by layer, never by area
  const OL_PAGE = 200;
  const HUES = [212, 28, 150, 320, 50, 265, 185, 0];
  const NEUTRAL = HUES.length;
  const clamp = (v, lo, hi) => Math.min(Math.max(v, lo), hi);

  /* ---------- colours: CSS custom properties, read again when the theme changes ---------- */
  let pal = [];
  let colors = {};
  const readColors = () => {
    const css = getComputedStyle(doc.documentElement);
    const text = name => css.getPropertyValue(name).trim();
    const num = name => parseFloat(text(name));
    const sat = num('--tree-sat'), l0 = num('--tree-l0'), step = num('--tree-lstep'), edge = num('--tree-edge');
    const ink = num('--tree-ink-l'), bar = num('--tree-bar-l'), grey = num('--tree-grey-sat');
    pal = [...HUES, 210].map((h, t) => {
      const s = t === NEUTRAL ? grey : sat;
      const hsl = (sa, l) => `hsl(${h} ${sa}% ${l}%)`;
      const layers = [0, 1, 2, 3, 4, 5];
      return {
        fill: layers.map(i => hsl(s, l0 + step * i)),
        edge: layers.map(i => hsl(s, l0 + step * i + edge)),
        bar: hsl(t === NEUTRAL ? grey + 6 : 56, bar),
        ink: hsl(t === NEUTRAL ? grey : Math.min(s + 12, 50), ink),
      };
    });
    colors = {
      gap: text('--tile-gap'), accent: text('--accent'), hit: text('--hit'), text: text('--text'),
      font: getComputedStyle(doc.body).fontFamily,
    };
  };

  /* ---------- the data ---------- */
  let root = null;
  const byId = new Map();
  let WW = 1600;
  let WH = WORLD_H;
  let layoutAspect = 1.6;
  let loaded = false;

  // Give a node what the picture needs; `kids` are outline nodes from the server.
  const adopt = (parent, kids) => {
    parent.children = kids;
    kids.forEach(c => {
      c.parent = parent;
      c.depth = parent.depth + 1;
      c.topic = parent.topic;
      c.weight = Math.pow(c.size, POWER);
      c.items = null;
      byId.set(c.id, c);
      if (c.children) adopt(c, c.children);
    });
    parent.sorted = kids.slice().sort((a, b) => b.weight - a.weight);
    parent.sorted.forEach((c, i) => { c.rank = i; });
    parent.items = null;
  };
  // A topic is a branch near the top: the first level with at least two summaries (a single all-embracing
  // summary on top would otherwise paint everything in one colour). What sits above stays grey.
  let topicCount = 0;
  const assignTopics = () => {
    const open = list => list.filter(n => n.children === null || (n.children && n.children.length));
    let level = open(root.children);
    for (let i = 0; i < 4 && level.length === 1 && level[0].children; i++) {
      const next = open(level[0].children);
      if (!next.length) break;
      level = next;
    }
    const paint = (n, t) => { n.topic = t; if (n.children) n.children.forEach(c => paint(c, t)); };
    level.forEach((n, i) => paint(n, i % HUES.length));
    topicCount = level.length;
  };

  /* ---------- layout: squarified treemap in fractions of the parent's inner area ---------- */
  const fontOf = n => TITLE_PX[Math.min(n.layer, 4)];
  const headOf = n => fontOf(n) + 17;
  // the part of a cell where its children go
  const inner = (c, x, y, w, h) => {
    if (c.virtual || c.group) return [x, y, w, h];
    const g = GAP / 2, head = headOf(c);
    return [x + g + PAD, y + g + head, w - GAP - 2 * PAD, h - GAP - head - PAD];
  };
  // How many pixels a card's inner area has at least when it shows its children: the least it opens at, or what it
  // gets when the picture is fitted (top layer) or just zoomed far enough to open it (below).
  const openArea = c => {
    if (c.virtual) return kFit * kFit * WW * WH;
    const least = c.group ? GROUP_W * GROUP_H : (OPEN_W - GAP - 2 * PAD) * (OPEN_H - GAP - headOf(c) - PAD);
    const [, , w, h] = cellRect(c, kFit * (c.depth < 1 ? 1 : OPEN_ZOOM));
    const [, , iw, ih] = inner(c, 0, 0, w, h);
    return Math.max(least, iw * ih);
  };
  const squarify = (parts, asp) => {
    let total = 0;
    for (const p of parts) total += p.weight;
    const set = (p, x, y, w, h) => { p.nx = x / asp; p.nw = w / asp; p.ny = y; p.nh = h; };
    const scale = asp / total;
    let x = 0, y = 0, w = asp, h = 1, i = 0;
    while (i < parts.length) {
      const side = Math.min(w, h);
      if (!(side > 1e-9)) { for (; i < parts.length; i++) set(parts[i], x, y, 0, 0); break; }
      let sum = 0, max = 0, min = Infinity, worst = Infinity, j = i;
      for (; j < parts.length; j++) {
        const area = parts[j].weight * scale;
        const s2 = sum + area, mx = Math.max(max, area), mn = Math.min(min, area);
        const ratio = Math.max((side * side * mx) / (s2 * s2), (s2 * s2) / (side * side * mn));
        if (j > i && ratio > worst) break;
        worst = ratio; sum = s2; max = mx; min = mn;
      }
      const thick = sum / side;
      let at = w >= h ? y : x;
      for (let k = i; k < j; k++) {
        const len = (parts[k].weight * scale) / thick;
        if (w >= h) set(parts[k], x, at, thick, len); else set(parts[k], at, y, len, thick);
        at += len;
      }
      if (w >= h) { x += thick; w -= thick; } else { y += thick; h -= thick; }
      i = j;
    }
  };
  const makeGroup = (owner, lo, hi) => {
    let weight = 0, size = 0;
    for (let i = lo; i < hi; i++) { weight += owner.sorted[i].weight; size += owner.sorted[i].size; }
    return {group: true, owner, lo, hi, weight, size, count: hi - lo, depth: owner.depth + 1, topic: owner.topic, layer: Math.max(owner.layer - 1, 0), items: null};
  };
  // What a card (or an "other" tile) shows in its inner area: the big children by name and the rest as "other" tiles.
  const layout = c => {
    if (c.items) return;
    const owner = c.group ? c.owner : c;
    const base = c.group ? c.lo : 0;
    const list = c.group ? owner.sorted.slice(c.lo, c.hi) : owner.sorted;
    let total = 0;
    for (const n of list) total += n.weight;
    const fmin = TILE_AREA / openArea(c);
    let head = 0;
    while (head < list.length && head < MAX_HEAD && list[head].weight / total >= fmin) head++;
    if (head === 0 && list.length <= MAX_HEAD) head = list.length;     // a few small ones are better shown than hidden
    const parts = [];
    if (head === 0) {
      const per = Math.ceil(list.length / clamp(Math.ceil(list.length / MAX_HEAD), 2, FANOUT));
      for (let s = 0; s < list.length; s += per) {
        parts.push(Math.min(s + per, list.length) - s === 1 ? list[s] : makeGroup(owner, base + s, base + Math.min(s + per, list.length)));
      }
    } else {
      parts.push(...list.slice(0, head));
      const rest = list.length - head;
      if (rest === 1) parts.push(list[head]);
      else if (rest > 1) parts.push(makeGroup(owner, base + head, base + list.length));
    }
    parts.sort((a, b) => b.weight - a.weight);
    const asp = clamp(c.asp || 1.6, 0.25, 5);
    squarify(parts, asp);
    for (const p of parts) {
      p.lp = c;
      p.asp = p.nh > 0 ? p.nw * asp / p.nh : 1;
    }
    c.items = parts;
  };
  // Lay out everything above `n` so that its place in its parent is known.
  const reveal = n => {
    const chain = [];
    for (let p = n.parent; p; p = p.parent) chain.unshift(p);
    chain.forEach((owner, i) => {
      const target = chain[i + 1] || n;
      let c = owner;
      layout(c);
      for (;;) {
        const g = c.items.find(it => it.group && it.owner === owner && target.rank >= it.lo && target.rank < it.hi);
        if (!g) break;
        layout(g);
        c = g;
      }
    });
  };
  const resetLayouts = () => {
    for (const n of byId.values()) n.items = null;
    root.items = null;
    root.asp = WW / WH;
  };
  // The cell of an item at scale k with the root's corner at the origin. Every size is affine in k, which lets
  // zoomTo solve for the scale that fits an item without iterating.
  const cellRect = (it, k) => {
    if (it === root) return [0, 0, WW * k, WH * k];
    const [px, py, pw, ph] = cellRect(it.lp, k);
    const [ix, iy, iw, ih] = inner(it.lp, px, py, pw, ph);
    return [ix + it.nx * iw, iy + it.ny * ih, it.nw * iw, it.nh * ih];
  };

  /* ---------- children that are not loaded yet ---------- */
  let inflight = 0;
  const loadChildren = n => {
    if (n.children || n.failed) return Promise.resolve(n);
    if (n.pending) return n.pending;
    inflight++;
    n.pending = fetch(`/api/outline?root=${encodeURIComponent(n.id)}&depth=2`)
      .then(r => (r.ok ? r.json() : Promise.reject(new Error(String(r.status)))))
      .then(list => {
        adopt(n, (list[0] && list[0].children) || []);
        n.items = null;
      })
      .catch(() => { n.failed = true; })
      .finally(() => { n.pending = null; inflight--; renderOutline(); request(); })
      .then(() => n);
    return n.pending;
  };
  const want = n => { if (!n.pending && !n.failed && inflight < 3) loadChildren(n); };

  /* ---------- the view: world centre and scale, eased towards a goal or flown along a path ---------- */
  let vw = 0, vh = 0, dpr = 1;
  const view = {cx: 0, cy: 0, k: 1};
  const goal = {cx: 0, cy: 0, k: 1};
  let tween = null;
  let kFit = 1;
  const foot = $('tree-foot');
  const footH = () => foot.offsetHeight + 16;          // the legend sits over the bottom of the picture: leave room for it
  const calcFit = () => { kFit = Math.min(vw / WW, (vh - footH()) / WH) * 0.97; };
  const clampView = g => {
    g.k = clamp(g.k, kFit * 0.5, kFit * 1e5);
    g.cx = clamp(g.cx, 0, WW);
    g.cy = clamp(g.cy, 0, WH);
  };
  const go = (g, instant) => {
    tween = null;
    Object.assign(goal, g);
    clampView(goal);
    if (instant || reduced.matches) Object.assign(view, goal);
    request();
  };
  // Smooth zoom: the point that stays put on screen is the fixed point of the move, so a big zoom does not swing.
  const fly = g => {
    Object.assign(goal, g);
    clampView(goal);
    if (reduced.matches) { tween = null; Object.assign(view, goal); request(); return; }
    const from = {cx: view.cx, cy: view.cy, k: view.k};
    const ratio = Math.abs(Math.log(goal.k / from.k));
    const pivot = (c0, c1) => (Math.abs(goal.k - from.k) < 1e-9 ? null : (c1 * goal.k - c0 * from.k) / (goal.k - from.k));
    tween = {
      from, start: performance.now(), dur: clamp(350 + 170 * ratio, 400, 1100),
      px: pivot(from.cx, goal.cx), py: pivot(from.cy, goal.cy),
    };
    request();
  };
  const fitAll = (animate = true) => {
    calcFit();
    focus = null;
    const g = {cx: WW / 2, cy: WH / 2 + footH() / 2 / kFit, k: kFit};
    if (animate) fly(g); else go(g, true);
  };
  const panelInset = () => {
    const open = !$('tree-panel').hidden;
    if (!open) return [0, 0];
    return vw > 767 ? [Math.min(380, vw - 24) + 24, 0] : [0, $('tree-panel').offsetHeight + 8];
  };
  // The scale and centre that put a cell in the middle of the part of the canvas the panel leaves free.
  const viewFor = (it, margin) => {
    reveal(it);
    const [pr, pb] = panelInset();
    const fx = margin, fy = margin, fw = vw - pr - 2 * margin, fh = vh - pb - footH() - 2 * margin;
    const a = cellRect(it, 1), b = cellRect(it, 2);
    const dw = b[2] - a[2], dh = b[3] - a[3];
    const k = clamp(Math.min((fw - (a[2] - dw)) / dw, (fh - (a[3] - dh)) / dh), kFit * 0.5, kFit * 1e5);
    const r = cellRect(it, k);
    const ox = fx + fw / 2 - (r[0] + r[2] / 2), oy = fy + fh / 2 - (r[1] + r[3] / 2);
    return {k, cx: (vw / 2 - ox) / k, cy: (vh / 2 - oy) / k};
  };
  const zoomTo = it => {
    const {k, cx, cy} = viewFor(it, 20);
    fly({cx, cy, k});
  };
  // To show a node: its parent when the node is readable there, else the node itself.
  const flyTo = n => {
    reveal(n);
    let target = n;
    if (n.parent && !n.parent.virtual && (!n.children || !n.children.length)) {
      const v = viewFor(n.parent, 20);
      const r = cellRect(n, v.k);
      if (r[2] >= 200 && r[3] >= 100) target = n.parent;
    }
    zoomTo(target);
  };
  const zoomBy = (factor, px = vw / 2, py = vh / 2) => {
    const k = clamp(goal.k * factor, kFit * 0.5, kFit * 1e5);
    const wx = goal.cx + (px - vw / 2) / goal.k;
    const wy = goal.cy + (py - vh / 2) / goal.k;
    go({cx: wx - (px - vw / 2) / k, cy: wy - (py - vh / 2) / k, k});
  };
  const stopMotion = () => { tween = null; Object.assign(goal, view); };

  /* ---------- text ---------- */
  const measure = doc.createElement('canvas').getContext('2d');
  const widths = new Map();
  const charW = (ch, weight) => {
    const key = `${weight}${ch}`;
    let w = widths.get(key);
    if (w === undefined) {
      measure.font = `${weight} 100px ${colors.font}`;
      w = measure.measureText(ch).width;
      widths.set(key, w);
    }
    return w;
  };
  const textW = (text, fs, weight) => {
    let w = 0;
    for (const ch of text) w += charW(ch, weight);
    return (w * fs) / 100;
  };
  const ellipsize = (line, fs, weight, maxW) => {
    if (textW(`${line}…`, fs, weight) <= maxW) return `${line}…`;
    let out = '', w = textW('…', fs, weight);
    for (const ch of line) {
      w += (charW(ch, weight) * fs) / 100;
      if (w > maxW) break;
      out += ch;
    }
    return `${out}…`;
  };
  const clip = (text, fs, weight, maxW) => (textW(text, fs, weight) <= maxW ? text : ellipsize(text, fs, weight, maxW).replace(/\s+…$/, '…'));
  // Wrap text into at most `maxLines` lines; the last one ends in … when text was left over.
  const wrap = (text, fs, weight, maxW, maxLines) => {
    const lines = [];
    let line = '', w = 0, brk = -1;
    for (const ch of text) {
      if (lines.length > maxLines) break;
      if (ch === '\n') { lines.push(line); line = ''; w = 0; brk = -1; continue; }
      if (ch === ' ' && !line) continue;
      const cw = (charW(ch, weight) * fs) / 100;
      if (w + cw > maxW && line) {
        if (brk > line.length * 0.5) { lines.push(line.slice(0, brk)); line = line.slice(brk + 1); w = textW(line, fs, weight); }
        else { lines.push(line); line = ''; w = 0; }
        brk = -1;
      }
      if (ch === ' ') brk = line.length;
      line += ch; w += cw;
    }
    if (line) lines.push(line);
    if (lines.length > maxLines) {
      lines.length = maxLines;
      lines[maxLines - 1] = ellipsize(lines[maxLines - 1], fs, weight, maxW);
    }
    return lines;
  };
  // the text of a memory after its title when the title is only its first words
  const restOf = (n, text) => {
    const head = n.title.replace(/…$/, '');
    if (!text || !head) return text || '';
    return text.startsWith(head) ? text.slice(head.length).replace(/^[\s，,。.；;：:、—-]+/, '') : text;
  };

  /* ---------- drawing ---------- */
  let frameNo = 0;
  let selected = null, hovered = null, focus = null, hitNodes = [], hitAnc = [];
  const needFull = [];

  const roundRect = (x, y, w, h, r) => { ctx.beginPath(); ctx.roundRect(x, y, w, h, r); };
  const card = (it, x, y, w, h, a) => {
    const g = Math.min(GAP / 2, w / 6, h / 6);
    const x0 = Math.max(x + g, -40), y0 = Math.max(y + g, -40), x1 = Math.min(x + w - g, vw + 40), y1 = Math.min(y + h - g, vh + 40);
    const rw = x1 - x0, rh = y1 - y0;
    if (rw <= 0 || rh <= 0) return;
    const p = pal[it.topic], li = Math.min(it.layer, 5);
    const r = Math.min(7, rw / 4, rh / 4);
    ctx.globalAlpha = a;
    ctx.fillStyle = p.fill[li];
    if (it.group) { ctx.globalAlpha = a * 0.7; ctx.fillStyle = p.fill[Math.min(li + 1, 5)]; }
    roundRect(x0, y0, rw, rh, r);
    ctx.fill();
    if (rw >= 12 && rh >= 12) {
      ctx.globalAlpha = a;
      ctx.strokeStyle = p.edge[li];
      ctx.lineWidth = 1;
      if (it.group) ctx.setLineDash([4, 3]);
      roundRect(x0 + 0.5, y0 + 0.5, rw - 1, rh - 1, r);
      ctx.stroke();
      if (it.group) ctx.setLineDash([]);
    }
    if (!it.group && it.children !== undefined && (it.children === null || it.children.length) && rw >= 40 && x + g >= -40) {
      ctx.fillStyle = p.bar;
      ctx.beginPath();
      ctx.roundRect(x0, y0, 4, rh, [r, 0, 0, r]);
      ctx.fill();
    }
  };

  // A line of facts: layer, count and dates, as much of it as fits.
  const meta = (n, maxW) => {
    const dates = n.from ? (n.from === n.to || !n.to ? n.from : `${n.from} – ${n.to}`) : '';
    const facts = n.children === null || n.children.length ? [`L${n.layer}`, `${n.size - 1} 条`] : [];
    for (const parts of [[...facts, dates], facts, [dates]]) {
      const text = parts.filter(Boolean).join(' · ');
      if (text && textW(text, MIN_FONT, 500) <= maxW) return text;
    }
    return '';
  };
  // What a tile that does not show its children says: its title, then a piece of text, then a line of facts.
  const closedBody = (n, x, y, w, h, a) => {
    const g = Math.min(GAP / 2, w / 6, h / 6);
    const x0 = Math.max(x + g, 0), y0 = Math.max(y + g, 0), x1 = Math.min(x + w - g, vw), y1 = Math.min(y + h - g, vh);
    const bw = x1 - x0, bh = y1 - y0;
    const kids = n.children === null || n.children.length > 0;
    const fs = fontOf(n), lh = fs * 1.35, weight = kids ? 600 : 500;
    const left = x0 + (kids ? 14 : 10), tw = x1 - 9 - left;
    if (tw < fs * 4 || bh < lh + 12) return;
    const showMeta = bw >= 130 && bh >= 70;
    const metaH = showMeta ? 17 : 0;
    const first = y0 + headOf(n) / 2 + 0.5;       // the title's first line sits where an open card's header text does
    const avail = y1 - 8 - metaH - (first - lh / 2);
    const bodyOn = bw >= SNIP_W && bh >= SNIP_H;
    const fullOn = !kids && bw >= FULL_W && bh >= FULL_H;
    if (fullOn && n.full === undefined && !n.fetching) needFull.push(n);
    const source = fullOn && n.full !== undefined ? n.full : n.snippet;
    // a title that is only the cut-off start of the text gives way to the text itself
    const cut = bodyOn && n.title.endsWith('…') && source.startsWith(n.title.slice(0, -1));
    let maxT = Math.floor(avail / lh);
    if (bw < 90 || bodyOn) maxT = Math.min(maxT, 2);
    if (maxT < 1) return;
    const p = pal[n.topic];
    ctx.globalAlpha = a;
    ctx.fillStyle = p.ink;
    ctx.textBaseline = 'middle';
    ctx.textAlign = 'left';
    ctx.font = `${weight} ${fs}px ${colors.font}`;
    const lines = cut ? [] : wrap(n.title, fs, weight, tw, maxT);
    lines.forEach((line, i) => ctx.fillText(line, left, first + i * lh));
    if (bodyOn) {
      const text = cut ? source : restOf(n, source);
      const size = fullOn ? (bw >= 420 && bh >= 260 ? 14 : 13) : 12, lh2 = Math.round(size * 1.55);
      const top = lines.length ? first + (lines.length - 1) * lh + lh / 2 + 5 : first - lh / 2;
      const room = Math.floor((y1 - 8 - metaH - top) / lh2);
      if (text && room >= 1) {
        ctx.globalAlpha = a * 0.82;
        ctx.font = `400 ${size}px ${colors.font}`;
        ctx.textBaseline = 'top';
        wrap(text, size, 400, tw, room).forEach((line, i) => ctx.fillText(line, left, top + i * lh2));
      }
    }
    if (showMeta) {
      ctx.font = `500 ${MIN_FONT}px ${colors.font}`;
      const text = meta(n, tw);
      if (text) {
        ctx.globalAlpha = a * 0.62;
        ctx.textBaseline = 'top';
        ctx.fillText(text, left, y1 - 8 - MIN_FONT - 1);
      }
    }
  };
  const groupBody = (it, x, y, w, h, a) => {
    if (w < 64 || h < 26) return;
    const p = pal[it.topic];
    ctx.globalAlpha = a * 0.85;
    ctx.fillStyle = p.ink;
    ctx.textAlign = 'center';
    ctx.textBaseline = 'middle';
    const hint = w >= 110 && h >= 56;
    ctx.font = `600 12px ${colors.font}`;
    ctx.fillText(clip(`其他 ${it.count} 条`, 12, 600, w - 16), x + w / 2, y + h / 2 - (hint ? 8 : 0));
    if (hint) {
      ctx.globalAlpha = a * 0.6;
      ctx.font = `400 ${MIN_FONT}px ${colors.font}`;
      ctx.fillText('点一下展开', x + w / 2, y + h / 2 + 9);
    }
  };
  // The title strip of an open card, with the small layer tag at its right end.
  const header = (n, x, y, w, a) => {
    const g = GAP / 2, hh = headOf(n), top = y + g;
    if (top + hh < 0 || top > vh) return;
    const p = pal[n.topic], fs = fontOf(n);
    const left = x + g + 14, right = x + w - g - 8;
    if (right - left < 60) return;
    const tag = `L${n.layer}`;
    ctx.font = `700 ${MIN_FONT}px ${colors.font}`;
    const tagW = textW(tag, MIN_FONT, 700) + 10;
    ctx.globalAlpha = a * 0.14;
    ctx.fillStyle = p.ink;
    roundRect(right - tagW, top + hh / 2 - 8, tagW, 16, 8);
    ctx.fill();
    ctx.globalAlpha = a;
    ctx.textBaseline = 'middle';
    ctx.textAlign = 'center';
    ctx.fillText(tag, right - tagW / 2, top + hh / 2 + 0.5);
    ctx.textAlign = 'left';
    ctx.font = `600 ${fs}px ${colors.font}`;
    ctx.fillText(clip(n.title, fs, 600, right - tagW - 8 - left), left, top + hh / 2 + 0.5);
  };
  const spinner = (x, y, a, now) => {
    ctx.globalAlpha = a * 0.7;
    ctx.strokeStyle = colors.text;
    ctx.lineWidth = 2;
    ctx.beginPath();
    ctx.arc(x, y, 5, now / 180, now / 180 + 4.2);
    ctx.stroke();
  };
  const hasHit = it => {
    if (it.group) return hitAnc.some(h => h.parent === it.owner && h.rank >= it.lo && h.rank < it.hi);
    return it.hitBelow || it.isHit;
  };

  let clock = 0;
  const drawItems = (c, x, y, w, h, fade, inFocus, inHit) => {
    layout(c);
    for (const it of c.items) drawItem(it, x + it.nx * w, y + it.ny * h, it.nw * w, it.nh * h, fade, inFocus, inHit);
  };
  const drawItem = (it, x, y, w, h, fade, inFocus, inHit) => {
    if (x > vw || y > vh || x + w < 0 || y + h < 0 || w < MIN_TILE || h < MIN_TILE) return;
    it.shown = frameNo; it.sx = x; it.sy = y; it.sw = w; it.sh = h; it.opened = 0;
    const inF = inFocus || it === focus;
    const inH = inHit || !!it.isHit;
    const dim = (focus && !inF) || (hitNodes.length && !inH && !hasHit(it));
    const a = fade * (dim ? DIM : 1);
    const ow = it.group ? GROUP_W : OPEN_W, oh = it.group ? GROUP_H : OPEN_H;
    const canOpen = it.group || ((it.children === null || it.children.length > 0) && !it.failed);
    const zoomed = it.depth < 1 ? 1 : clamp((view.k / (kFit * OPEN_ZOOM) - 1) / 0.08, 0, 1);
    const open = canOpen && w >= ow && h >= oh && zoomed > 0;
    const t = open ? Math.min(zoomed, (Math.min(w / ow, h / oh) - 1) / 0.12, 1) : 0;
    if (!it.group && it.children === null && w >= ow * 0.55 && h >= oh * 0.55 && (it.depth < 1 || view.k >= kFit * OPEN_ZOOM * 0.8)) want(it);
    if (t > 0) it.opened = frameNo;
    if (!it.group || t < 1) card(it, x, y, w, h, it.group ? a * (1 - t) : a);
    const rest = 1 - Math.min(1, t * 2);       // the closed text is gone by the time half the children are in
    if (rest > 0) {
      if (it.group) groupBody(it, x, y, w, h, a * rest);
      else closedBody(it, x, y, w, h, a * rest);
    }
    if (it.pending && w >= 60 && h >= 40) spinner(x + w - (t > 0 ? 60 : 20), y + 4 + (t > 0 ? headOf(it) / 2 : 12), a, clock);
    if (t === 0) return;
    if (!it.group) header(it, x, y, w, a * t);
    if (it.children === null) {
      ctx.globalAlpha = a * t * 0.7;
      ctx.fillStyle = pal[it.topic].ink;
      ctx.textAlign = 'center';
      ctx.textBaseline = 'middle';
      ctx.font = `400 12px ${colors.font}`;
      ctx.fillText('正在取下面的内容…', x + w / 2, y + h / 2);
    } else {
      const [ix, iy, iw, ih] = inner(it, x, y, w, h);
      drawItems(it, ix, iy, iw, ih, fade * t, inF, inH);
    }
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
      const g = Math.min(GAP / 2, w / 6, h / 6);
      roundRect(x + g + width / 2, y + g + width / 2, w - 2 * g - width, h - 2 * g - width, 6);
      ctx.stroke();
    }
    ctx.restore();
  };
  // The tile the picture shows for a node: itself, or the closed tile it sits in.
  const shownOf = n => {
    for (let p = n; p && !p.virtual; p = p.lp || p.parent) {
      if (p.shown === frameNo) return p.opened === frameNo && p !== n ? null : p;
    }
    return null;
  };
  const overlay = (n, color, width, glow) => {
    const tile = shownOf(n);
    if (tile) ring(tile.sx, tile.sy, tile.sw, tile.sh, color, width, glow);
  };

  const draw = now => {
    frameNo++;
    clock = now;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    ctx.globalAlpha = 1;
    ctx.fillStyle = colors.gap;
    ctx.fillRect(0, 0, vw, vh);
    const ox = vw / 2 - view.cx * view.k, oy = vh / 2 - view.cy * view.k;
    needFull.length = 0;
    drawItems(root, ox, oy, WW * view.k, WH * view.k, 1, false, false);
    for (const n of hitNodes) overlay(n, colors.hit, 3, true);
    if (hovered && hovered !== selected) overlay(hovered, colors.text, 1.5, false);
    if (selected) overlay(selected, colors.accent, 3, false);
    ctx.globalAlpha = 1;
  };

  /* ---------- hit testing: the tile the last frame shows under a point ---------- */
  const pickAt = (px, py) => {
    let found = null, c = root;
    while (c.items) {
      const next = c.items.find(it => it.shown === frameNo && px >= it.sx && px < it.sx + it.sw && py >= it.sy && py < it.sy + it.sh);
      if (!next) break;
      if (next.opened !== frameNo) { found = next; break; }
      if (!next.group) found = next;
      c = next;
    }
    return found;
  };
  // The deepest card that still fills a good part of the window: "where we are".
  const viewTile = () => {
    let found = root, c = root;
    while (c.items) {
      const next = c.items.find(it => it.shown === frameNo && vw / 2 >= it.sx && vw / 2 < it.sx + it.sw && vh / 2 >= it.sy && vh / 2 < it.sy + it.sh);
      if (!next) break;
      if (!next.group) {
        if (next.sw < vw * 0.45 && next.sh < vh * 0.45) break;
        found = next;
      }
      if (next.opened !== frameNo) break;
      c = next;
    }
    return found;
  };

  /* ---------- the frame loop: redraws only while something changes ---------- */
  let raf = 0, last = 0, crumbKey = null, statusText = '', statusTimer = 0;
  const request = () => { if (!raf) raf = requestAnimationFrame(frame); };
  const settled = () => !tween && Math.abs(Math.log(goal.k / view.k)) < 0.0005 && Math.abs(goal.cx - view.cx) * view.k < 0.3 && Math.abs(goal.cy - view.cy) * view.k < 0.3;
  const frame = now => {
    raf = 0;
    if (!root || !loaded) return;
    const dt = Math.min(0.064, (now - last) / 1000 || 0.016);
    last = now;
    if (tween) {
      const s = clamp((now - tween.start) / tween.dur, 0, 1);
      const e = s < 0.5 ? 4 * s * s * s : 1 - Math.pow(-2 * s + 2, 3) / 2;
      if (s >= 1) { Object.assign(view, goal); tween = null; }
      else {
        const k = tween.from.k * Math.pow(goal.k / tween.from.k, e);
        view.k = k;
        view.cx = tween.px === null ? tween.from.cx + (goal.cx - tween.from.cx) * e : tween.px + (tween.from.cx - tween.px) * tween.from.k / k;
        view.cy = tween.py === null ? tween.from.cy + (goal.cy - tween.from.cy) * e : tween.py + (tween.from.cy - tween.py) * tween.from.k / k;
      }
    } else if (!settled()) {
      const a = reduced.matches ? 1 : 1 - Math.exp(-dt / 0.085);
      view.k = Math.exp(Math.log(view.k) + (Math.log(goal.k) - Math.log(view.k)) * a);
      view.cx += (goal.cx - view.cx) * a;
      view.cy += (goal.cy - view.cy) * a;
      if (settled()) Object.assign(view, goal);
    }
    draw(now);
    updateCrumbs();
    updateStatus();
    fetchFull();
    if (!settled() || inflight > 0) request();
  };

  const updateStatus = () => {
    const mid = pickAt(vw / 2, vh / 2);
    const text = !mid ? '' : mid.layer === 0 ? '现在看到第 0 层（记忆本身）' : `现在看到第 ${mid.layer} 层`;
    if (text === statusText) return;
    statusText = text;
    clearTimeout(statusTimer);
    statusTimer = setTimeout(() => { $('tree-status').textContent = statusText; }, 350);
  };

  /* ---------- the bar of where we are ---------- */
  const el = (tag, className, text) => {
    const node = doc.createElement(tag);
    if (className) node.className = className;
    if (text !== undefined) node.textContent = text;
    return node;
  };
  const updateCrumbs = () => {
    const at = focus || selected || viewTile();
    const key = `${at.id}|${focus ? 'f' : ''}`;
    if (key === crumbKey) return;
    crumbKey = key;
    const chain = [];
    for (let p = at; p && !p.virtual; p = p.parent) chain.unshift(p);
    const bar = $('tree-crumbs');
    bar.replaceChildren();
    if (focus) bar.append(el('span', 'tag', '聚焦'));
    const add = (text, current, onClick) => {
      if (bar.lastChild && !bar.lastChild.classList.contains('tag')) {
        const sep = el('span', 'sep', '›');
        sep.setAttribute('aria-hidden', 'true');
        bar.append(sep);
      }
      const btn = el('button', '', text);
      btn.type = 'button';
      btn.title = text;
      if (current) btn.setAttribute('aria-current', 'location');
      btn.addEventListener('click', onClick);
      bar.append(btn);
    };
    add('全部', !chain.length, () => { fitAll(); });
    chain.forEach((n, i) => add(n.title, i === chain.length - 1, () => focusOn(n)));
    bar.scrollLeft = bar.scrollWidth;
  };
  const focusOn = n => {
    focus = n;
    crumbKey = null;
    zoomTo(n);
  };
  const focusUp = () => {
    if (!focus) return false;
    if (!focus.parent || focus.parent.virtual) { fitAll(); return true; }
    focusOn(focus.parent);
    return true;
  };

  /* ---------- full text of the memories that are big enough to read ---------- */
  let fetching = 0;
  const fetchFull = () => {
    for (const n of needFull) {
      if (fetching >= 3) return;
      if (n.fetching || n.full !== undefined) continue;
      n.fetching = true;
      fetching++;
      fetch(`/api/node/${encodeURIComponent(n.id)}`)
        .then(r => (r.ok ? r.json() : Promise.reject()))
        .then(data => { n.full = data.content; })
        .catch(() => { n.full = n.snippet; })
        .finally(() => { n.fetching = false; fetching--; request(); });
    }
  };

  /* ---------- the side panel ---------- */
  const panel = $('tree-panel');
  const panelBody = $('tree-panel-body');
  const badge = layer => {
    const b = el('span', 'layer', layer === 0 ? '第 0 层 · 记忆' : `第 ${layer} 层 · 汇总`);
    b.dataset.layer = String(Math.min(layer, 5));
    return b;
  };
  const span = it => (it.from ? (it.to && it.to !== it.from ? `${it.from} – ${it.to}` : it.from) : '');
  let selectSerial = 0;
  const showPanel = (n, data) => {
    const pane = el('div', 'pane');
    if (data && data.path.length) {
      const path = el('nav', 'path');
      path.setAttribute('aria-label', '它在树里的位置');
      data.path.forEach((p, i) => {
        if (i) path.append(el('span', 'sep', '›'));
        const target = byId.get(p.id);
        const btn = el('button', '', p.title);
        btn.type = 'button';
        btn.title = p.title;
        if (target) btn.addEventListener('click', () => select(target, true));
        path.append(btn);
      });
      pane.append(path);
    }
    pane.append(el('h2', 'pane-title', data ? data.title || data.label : n.title));
    const facts = el('div', 'pane-meta');
    facts.append(badge(n.layer));
    if (data) {
      facts.append(el('span', '', `第 ${data.version} 版`));
      if (data.occurred) facts.append(el('span', '', `发生于 ${data.occurred}`));
      if (data.descendants) facts.append(el('span', '', `下面共 ${data.descendants} 条`));
    } else {
      if (n.size > 1) facts.append(el('span', '', `下面共 ${n.size - 1} 条`));
      if (span(n)) facts.append(el('span', '', span(n)));
    }
    pane.append(facts);
    pane.append(el('p', 'pane-text', data ? data.content : n.snippet));
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
    const here = el('button', 'btn btn-sm', '聚焦到这里');
    here.type = 'button';
    here.addEventListener('click', () => focusOn(n));
    actions.append(open, here);
    pane.append(actions);
    panelBody.replaceChildren(pane);
  };
  const select = (n, fly_) => {
    selected = n;
    panel.hidden = false;
    showPanel(n, null);
    const mine = ++selectSerial;
    fetch(`/api/node/${encodeURIComponent(n.id)}`)
      .then(r => (r.ok ? r.json() : Promise.reject()))
      .then(data => { if (mine === selectSerial) showPanel(n, data); })
      .catch(() => {});
    syncOutline(n);
    if (fly_) flyTo(n);
    request();
  };
  const deselect = () => {
    selected = null;
    panel.hidden = true;
    selectSerial++;
    syncOutline(null);
    request();
  };
  $('tree-panel-close').addEventListener('click', deselect);

  /* ---------- the tooltip ---------- */
  const tip = $('tree-tip');
  const showTip = (it, x, y) => {
    if (!it) { tip.hidden = true; return; }
    if (it.group) {
      tip.replaceChildren(el('strong', 'tip-title', `其他 ${it.count} 条`), el('span', 'tip-meta', '太小放不下的兄弟，点一下展开'));
    } else {
      const bits = [];
      if (it.size > 1) bits.push(`下面共 ${it.size - 1} 条`);
      if (span(it)) bits.push(span(it));
      const rows = [el('span', 'tip-head'), el('strong', 'tip-title', it.title)];
      rows[0].append(badge(it.layer));
      const rest = restOf(it, it.snippet);
      if (rest) rows.push(el('span', 'tip-snip', rest));
      if (bits.length) rows.push(el('span', 'tip-meta', bits.join(' · ')));
      tip.replaceChildren(...rows);
    }
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

  canvas.addEventListener('pointerdown', event => {
    if (event.pointerType !== 'touch') canvas.focus({preventScroll: true});
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
      const kk = clamp(view.k * factor, kFit * 0.5, kFit * 1e5);
      const wx = view.cx + (mx - vw / 2) / view.k, wy = view.cy + (my - vh / 2) / view.k;
      view.k = kk;
      view.cx = wx - (mx - vw / 2) / kk;
      view.cy = wy - (my - vh / 2) / kk;
      clampView(view);
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
        clampView(view);
        Object.assign(goal, view);
        request();
      }
      return;
    }
    if (event.pointerType === 'mouse') {
      const it = pickAt(p.x, p.y);
      canvas.classList.toggle('is-pointer', !!it);
      const over = it && !it.group ? it : null;
      if (over !== hovered) { hovered = over; request(); }
      showTip(it, p.x, p.y);
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
    const it = pickAt(p.x, p.y);
    const now = performance.now();
    const twice = now - lastTap.t < 380 && Math.hypot(p.x - lastTap.x, p.y - lastTap.y) < 24;
    lastTap = {t: twice ? 0 : now, x: p.x, y: p.y};
    if (!it) { deselect(); return; }
    if (it.group) { zoomTo(it); return; }
    select(it, false);
    if (twice) focusOn(it);
  };
  canvas.addEventListener('pointerup', release);
  canvas.addEventListener('pointercancel', release);
  canvas.addEventListener('pointerleave', event => {
    if (event.pointerType === 'mouse') { tip.hidden = true; if (hovered) { hovered = null; request(); } }
  });
  canvas.addEventListener('wheel', event => {
    event.preventDefault();
    stopMotion();
    const p = local(event);
    const unit = event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? vh : 1;
    const speed = event.ctrlKey ? 0.01 : 0.0016;
    zoomBy(Math.exp(-event.deltaY * unit * speed), p.x, p.y);
    tip.hidden = true;
  }, {passive: false});
  canvas.addEventListener('keydown', event => {
    if (event.ctrlKey || event.metaKey || event.altKey) return;
    const step = 0.15;
    const pan = (dx, dy) => { stopMotion(); go({cx: goal.cx + (dx * vw * step) / goal.k, cy: goal.cy + (dy * vh * step) / goal.k, k: goal.k}); };
    switch (event.key) {
      case '+': case '=': stopMotion(); zoomBy(1.5); break;
      case '-': case '_': stopMotion(); zoomBy(1 / 1.5); break;
      case '0': case 'Home': fitAll(); break;
      case 'ArrowLeft': pan(-1, 0); break;
      case 'ArrowRight': pan(1, 0); break;
      case 'ArrowUp': pan(0, -1); break;
      case 'ArrowDown': pan(0, 1); break;
      case 'Escape': if (!focusUp()) deselect(); break;
      case 'Enter': if (selected) focusOn(selected); break;
      default: return;
    }
    event.preventDefault();
  });
  $('tree-in').addEventListener('click', () => { stopMotion(); zoomBy(1.6); });
  $('tree-out').addEventListener('click', () => { stopMotion(); zoomBy(1 / 1.6); });
  $('tree-fit').addEventListener('click', () => fitAll());

  /* ---------- search ---------- */
  const hitList = $('tree-hits');
  const searchBox = $('tree-q');
  const markHits = nodes => {
    for (const n of hitNodes) { n.isHit = false; for (let p = n.parent; p; p = p.parent) p.hitBelow = false; }
    hitNodes = nodes;
    hitAnc = [];
    for (const n of nodes) {
      n.isHit = true;
      for (let p = n; p.parent; p = p.parent) { hitAnc.push({parent: p.parent, rank: p.rank}); p.parent.hitBelow = true; }
    }
    request();
  };
  // A hit may sit in a part of the tree that is not loaded yet: load down the path to it.
  const nodeFor = async hit => {
    const ids = [...hit.path.map(p => p.id).reverse(), hit.id];
    for (let i = 0; i < ids.length; i++) {
      if (byId.has(ids[i])) continue;
      const above = i ? byId.get(ids[i - 1]) : root;
      if (!above) return null;
      await loadChildren(above);
    }
    return byId.get(hit.id) || null;
  };
  const jump = n => {
    hitList.hidden = true;
    select(n, true);
  };
  const say = text => { $('tree-status').textContent = text; statusText = ''; };
  let searchSerial = 0;
  const search = async () => {
    const q = searchBox.value.trim();
    if (!q) { markHits([]); hitList.hidden = true; return; }
    const mine = ++searchSerial;
    say('正在搜索……');
    try {
      const response = await fetch(`/api/search?k=8&q=${encodeURIComponent(q)}`);
      const body = await response.json();
      if (!response.ok) throw new Error(body.error || '搜索失败');
      const found = await Promise.all(body.hits.map(async h => ({h, n: await nodeFor(h)})));
      if (mine !== searchSerial) return;
      const hits = found.filter(f => f.n);
      markHits(hits.map(f => f.n));
      if (!hits.length) { hitList.hidden = true; say('没有找到'); return; }
      hitList.replaceChildren(...hits.map(({h, n}) => {
        const li = el('li');
        const btn = el('button', 'pick');
        btn.type = 'button';
        const text = el('span', 'tree-hit-text');
        text.append(el('span', 'node-text', h.title));
        if (h.path.length) text.append(el('span', 'tree-hit-path', h.path.map(p => p.title).reverse().join(' › ')));
        btn.append(badge(h.layer), text);
        btn.addEventListener('click', () => jump(n));
        li.append(btn);
        return li;
      }));
      hitList.hidden = false;
      say(`找到 ${hits.length} 条${body.no_strong_match ? '（没有明确匹配，只是线索）' : ''}，已飞到第一条`);
      jump(hits[0].n);
    } catch (error) {
      say(error.message || '搜索失败');
    }
  };
  $('tree-search').addEventListener('submit', event => { event.preventDefault(); search(); });
  const clearSearch = () => { searchSerial++; markHits([]); hitList.hidden = true; };
  searchBox.addEventListener('keydown', event => {
    if (event.key === 'Escape') { searchBox.value = ''; clearSearch(); }
  });
  searchBox.addEventListener('focus', () => { if (hitList.children.length && hitNodes.length) hitList.hidden = false; });
  searchBox.addEventListener('input', () => { if (!searchBox.value.trim()) clearSearch(); });

  /* ---------- the outline beside the picture ---------- */
  const side = $('tree-side');
  const ol = $('tree-ol');
  const olToggle = $('tree-ol-toggle');
  const olOpen = new Set();       // ids of expanded rows
  const olMore = new Map();       // id -> how many of its children are listed
  const olRows = new Map();       // id -> the row's element
  let olSel = null;
  const olKey = () => (innerWidth < 900 ? 'rp-tree-outline-narrow' : 'rp-tree-outline');
  const hasKids = n => n.children === null || (n.children && n.children.length > 0);

  const buildRows = (parentEl, list, level, owner) => {
    const shown = olMore.get(owner.id) || OL_PAGE;
    for (const n of list.slice(0, shown)) parentEl.append(buildRow(n, level));
    if (list.length > shown) {
      const more = el('li', 'more', `还有 ${list.length - shown} 条，点这里再显示一些`);
      more.setAttribute('role', 'treeitem');
      more.tabIndex = -1;
      more.dataset.more = owner.id;
      more.style.paddingLeft = `${26 + (level - 1) * 14}px`;
      parentEl.append(more);
    }
  };
  const buildRow = (n, level) => {
    const li = el('li');
    li.setAttribute('role', 'treeitem');
    li.setAttribute('aria-level', String(level));
    li.tabIndex = -1;
    li.dataset.id = n.id;
    const open = olOpen.has(n.id);
    if (hasKids(n)) li.setAttribute('aria-expanded', open ? 'true' : 'false');
    li.setAttribute('aria-selected', n.id === olSel ? 'true' : 'false');
    const row = el('div', 'ol-row');
    row.style.paddingLeft = `${6 + (level - 1) * 14}px`;
    row.title = n.title;
    const tog = el('span', hasKids(n) ? 'tog' : 'tog none');
    tog.setAttribute('aria-hidden', 'true');
    const dot = el('span', 'dot');
    dot.style.background = pal[n.topic].bar;
    row.append(tog, dot, el('span', 't', n.title));
    if (hasKids(n) && n.size > 1) row.append(el('span', 'n', String(n.size - 1)));
    li.append(row);
    olRows.set(n.id, li);
    if (open && hasKids(n)) {
      const group = el('ul');
      group.setAttribute('role', 'group');
      if (n.children === null) {
        const wait = el('li', 'more', n.failed ? '没取到，稍后再试' : '正在取……');
        wait.setAttribute('role', 'none');
        group.append(wait);
      } else buildRows(group, n.children, level + 1, n);
      li.append(group);
    }
    return li;
  };
  let olDirty = true;
  function renderOutline() {
    if (!root || side.hidden) { olDirty = true; return; }
    olDirty = false;
    const active = doc.activeElement && ol.contains(doc.activeElement) ? doc.activeElement.dataset.id || doc.activeElement.dataset.more : null;
    olRows.clear();
    const frag = doc.createDocumentFragment();
    buildRows(frag, root.children, 1, root);
    ol.replaceChildren(frag);
    const stop = olRows.get(olSel) || ol.querySelector('li');
    if (stop) stop.tabIndex = 0;
    if (active) {
      const back = olRows.get(active) || ol.querySelector(`li[data-more="${active}"]`);
      if (back) back.focus({preventScroll: true});
    }
  }
  function syncOutline(n) {
    olSel = n ? n.id : null;
    for (let p = n; p && p.parent; p = p.parent) {
      const owner = p.parent;
      if (!owner.virtual) olOpen.add(owner.id);
      const at = owner.children.indexOf(p);
      if (at >= (olMore.get(owner.id) || OL_PAGE)) olMore.set(owner.id, at + OL_PAGE);
    }
    renderOutline();
    const row = n && olRows.get(n.id);
    if (row && !side.hidden) row.scrollIntoView({block: 'nearest'});
  }
  const toggleRow = n => {
    if (olOpen.has(n.id)) olOpen.delete(n.id);
    else { olOpen.add(n.id); if (n.children === null) loadChildren(n); }
    renderOutline();
  };
  const activateRow = n => {
    select(n, true);
    if (innerWidth < 900) setOutline(false);
  };
  const rowOf = target => target.closest('li[role="treeitem"]');
  ol.addEventListener('click', event => {
    const li = rowOf(event.target);
    if (!li) return;
    if (li.dataset.more) { olMore.set(li.dataset.more, (olMore.get(li.dataset.more) || OL_PAGE) + OL_PAGE); renderOutline(); return; }
    const n = byId.get(li.dataset.id);
    if (!n) return;
    if (event.target.closest('.tog') && hasKids(n)) toggleRow(n);
    else activateRow(n);
  });
  ol.addEventListener('keydown', event => {
    const li = rowOf(event.target);
    if (!li || event.ctrlKey || event.metaKey || event.altKey) return;
    const rows = [...ol.querySelectorAll('li[role="treeitem"]')];
    const at = rows.indexOf(li);
    const n = byId.get(li.dataset.id);
    const to = row => { if (row) { for (const r of rows) r.tabIndex = -1; row.tabIndex = 0; row.focus(); } };
    switch (event.key) {
      case 'ArrowDown': to(rows[at + 1]); break;
      case 'ArrowUp': to(rows[at - 1]); break;
      case 'Home': to(rows[0]); break;
      case 'End': to(rows[rows.length - 1]); break;
      case 'ArrowRight':
        if (n && hasKids(n)) { if (!olOpen.has(n.id)) toggleRow(n); else to(li.querySelector('ul li[role="treeitem"]')); }
        break;
      case 'ArrowLeft':
        if (n && hasKids(n) && olOpen.has(n.id)) toggleRow(n);
        else to(li.parentElement.closest('li[role="treeitem"]'));
        break;
      case 'Enter': case ' ':
        if (li.dataset.more) li.click();
        else if (n) activateRow(n);
        break;
      case 'Escape': if (innerWidth < 900) { setOutline(false); canvas.focus(); } else return; break;
      default: return;
    }
    event.preventDefault();
  });
  function setOutline(open, remember = true) {
    side.hidden = !open;
    olToggle.setAttribute('aria-pressed', open ? 'true' : 'false');
    if (remember) { try { localStorage.setItem(olKey(), open ? '1' : '0'); } catch { /* private mode */ } }
    if (open && olDirty) renderOutline();
    if (open) { const row = olRows.get(olSel); if (row) row.scrollIntoView({block: 'nearest'}); }
    request();
  }
  olToggle.addEventListener('click', () => setOutline(side.hidden));
  $('tree-side-close').addEventListener('click', () => { setOutline(false); olToggle.focus(); });
  const initialOutline = () => {
    let saved = null;
    try { saved = localStorage.getItem(olKey()); } catch { /* private mode */ }
    setOutline(saved === null ? innerWidth >= 900 : saved === '1', false);
  };

  /* ---------- legend and a text version of the top layer ---------- */
  const buildLegend = () => {
    const legend = $('tree-legend');
    legend.replaceChildren();
    const topics = el('li');
    const dots = el('span', 'swatches');
    dots.setAttribute('aria-hidden', 'true');
    for (let t = 0; t < Math.min(topicCount, HUES.length); t++) {
      const dot = el('span', 'sw');
      dot.style.background = pal[t].bar;
      dots.append(dot);
    }
    topics.append(dots, el('span', '', '颜色 = 话题'));
    legend.append(topics);
    const layers = el('li');
    const sw = el('span', 'swatches');
    sw.setAttribute('aria-hidden', 'true');
    const grey = pal[NEUTRAL];
    for (const i of [0, 2, 4]) {
      const box = el('span', 'sw box');
      box.style.background = pal[0].fill[i];
      box.style.borderColor = pal[0].edge[i];
      sw.append(box);
    }
    const lightness = el('span', '', '亮度 = 层');
    lightness.append(el('span', 'opt', '（越往上越深）'));
    layers.append(sw, lightness);
    legend.append(layers);
    if (byId.size && [...byId.values()].some(n => n.topic === NEUTRAL)) {
      const single = el('li', 'opt');
      const box = el('span', 'sw box');
      box.style.background = grey.fill[0];
      box.style.borderColor = grey.edge[0];
      single.append(box, el('span', '', '灰 = 总览和零散的记忆'));
      legend.append(single);
    }
  };
  const buildTopList = () => {
    const list = $('tree-outline');
    for (const n of root.children.slice(0, 40)) {
      const li = el('li');
      const a = el('a', '', `第 ${n.layer} 层：${n.title}`);
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
    const aspect = clamp(vw / vh, 0.7, 2.4);
    if (!loaded || Math.abs(aspect / layoutAspect - 1) > 0.3) {
      layoutAspect = aspect;
      WW = WORLD_H * aspect;
      calcFit();
      resetLayouts();
      if (focus) go(viewFor(focus, 20), true);
      else fitAll(false);
      loaded = true;
    }
    calcFit();
    request();
  };
  const theme = () => { readColors(); widths.clear(); renderOutline(); buildLegend(); request(); };
  window.addEventListener('rp-theme', theme);
  matchMedia('(prefers-color-scheme: dark)').addEventListener('change', theme);
  reduced.addEventListener('change', request);

  const start = async () => {
    readColors();
    initialOutline();
    const message = $('tree-msg');
    try {
      const response = await fetch('/api/outline?depth=3');
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      const list = await response.json();
      if (!list.length) { message.textContent = '树还是空的。'; return; }
      root = {id: '', title: '全部', layer: -1, size: 0, virtual: true, topic: NEUTRAL, weight: 1, depth: -1};
      adopt(root, list);
      root.size = list.reduce((sum, c) => sum + c.size, 0);
      assignTopics();
      const rect = stage.getBoundingClientRect();
      vw = rect.width; vh = rect.height;
      layoutAspect = clamp(vw / vh, 0.7, 2.4);
      WW = WORLD_H * layoutAspect;
      root.asp = WW / WH;
      buildLegend();
      resize();
      buildTopList();
      for (const n of root.children.slice(0, 60)) if (hasKids(n)) olOpen.add(n.id);
      renderOutline();
      message.hidden = true;
      new ResizeObserver(resize).observe(stage);
      request();
    } catch (error) {
      message.replaceChildren(el('span', '', `没能画出这棵树：${error.message}。刷新页面再试一次。`));
    }
  };
  start();
})();
