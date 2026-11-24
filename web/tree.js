/* The memory tree page: the whole tree as one zoomable picture (a treemap on a canvas) with small tools, an outline
   and a detail panel floating on it. Everything about a node can be read and done here: the picture, the outline
   and the panel never leave the page.
   Every node is a card sized by how much sits under it (a power of the count, so small branches stay visible).
   A card shows its children only when it is big enough on screen for them to carry a title and a few lines of text,
   so zooming in opens the tree level by level down to the memories themselves. Whatever room a card has is filled
   with its own words: a title, the whole text at the biggest size that fits, a line of facts. How a card sets its
   words depends on the card's own size and nothing else: what the window crops or a floating panel covers does not
   change a single line. Children are fetched when a card is about to open. The memories that sit under no summary
   are gathered in one grey block, "未汇总的记忆". */
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

  const WORLD_H = 1000;                  // the top level is laid out in a world of this height; the width follows the window
  const CHILD_W = 112, CHILD_H = 76;     // the least a child gets when its parent opens: a title and two lines of text
  const GAP = 5;                         // space between neighbouring cards
  const TOP_GAP = 16;                    // ... and between the cards of the top level, so the board shows between them
  const PAD = 6;                         // space between a card's edge and its children
  const SUM_H = 16;                      // the one-line summary under the title of an open card
  const MIN_TILE = 3;                    // smaller tiles are not drawn
  const TILE_AREA = CHILD_W * CHILD_H;   // a child that would get less than this many pixels when its parent just opened is merged into "other"
  const FULL_AREA = 300 * 170;           // a memory this big asks for its whole text
  const MAX_HEAD = 12;                   // most tiles a card shows by name next to its "other" tile
  const FANOUT = 6;                      // how many "other" tiles a crowd is split into
  const POWER = 0.7;                     // area grows with size to this power
  const TITLE_PX = [13, 14, 15, 16, 17]; // title size of an open card by layer
  const BODY_MIN = 12, BODY_MAX = 40, TITLE_MAX = 44;  // the sizes a card's text ranges over (its title grows with it, to 44px at most); a huge card may go up to 72px
  const FILL_W = 200, FILL_H = 120;      // a card smaller than this keeps the sizes its width gives it
  const MIN_RATIO = 0.5, MAX_RATIO = 20000;  // the range of the zoom, as multiples of the fitted picture
  const VIEW_HISTORY = 20;               // how many earlier views Esc can go back through
  const OL_PAGE = 200;
  const LOOSE = 'loose';                 // the id of the block of memories under no summary
  const MARGIN = {x: 22, top: 62, bottom: 56, phoneTop: 104, phoneBottom: 52};
  const SIDE_BREAKPOINT = 900;           // below this the outline is a drawer over the picture, not a column beside it
  const HUES = [212, 28, 150, 320, 50, 265, 185, 0];
  const NEUTRAL = HUES.length;
  const NO_START = '，。、；：！？）」』》”’,.;:!?)]}';  // closing marks never start a line
  const clamp = (v, lo, hi) => Math.min(Math.max(v, lo), hi);
  const half = v => Math.round(v * 2) / 2;

  /* ---------- small helpers ---------- */
  // The element builder, the icon, the JSON call, the toast and the layer badge are the interface parts every page shares (ui.js).
  const {h, icon, api, parseTags, layerBadge: badge} = window.rp;
  const toast = (text, error = false) => window.rp.toast(text, error ? 'error' : 'ok');
  const store = {
    get(key) { try { return localStorage.getItem(key); } catch { return null; } },
    set(key, value) { try { localStorage.setItem(key, value); } catch { /* private mode: the choice lasts for this page */ } },
  };
  const fmtTime = iso => {
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return iso || '';
    const p = v => String(v).padStart(2, '0');
    return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}`;
  };

  /* ---------- colours: CSS custom properties, read again when the theme changes ---------- */
  let pal = [];
  let colors = {};
  const readColors = () => {
    const css = getComputedStyle(doc.documentElement);
    const text = name => css.getPropertyValue(name).trim();
    const num = name => parseFloat(text(name));
    const sat = num('--tree-sat'), l0 = num('--tree-l0'), step = num('--tree-lstep'), edge = num('--tree-edge');
    const ink = num('--tree-ink-l'), bar = num('--tree-bar-l'), grey = num('--tree-grey-sat');
    pal = [...HUES, 210].map((hue, t) => {
      const s = t === NEUTRAL ? grey : sat;
      const hsl = (sa, l) => `hsl(${hue} ${sa}% ${l}%)`;
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
      bgA: text('--tree-bg-a'), bgB: text('--tree-bg-b'), dot: text('--tree-dot'), shadow: text('--tree-shadow'), hi: text('--tree-hi'),
      font: getComputedStyle(doc.body).fontFamily,
    };
    bgKey = '';
  };

  /* ---------- the data ---------- */
  let root = null;
  let generation = 0;                // counts the times the tree was built again: an answer for an older one is dropped
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
  // A topic is a branch near the top: the first level with at least two summaries, looking into a summary that holds
  // most of the tree (an all-embracing summary on top would otherwise paint everything in one colour). What sits
  // above stays grey; the small summaries beside it are topics of their own.
  let topicCount = 0;
  const assignTopics = () => {
    const open = list => list.filter(n => n.children === null || (n.children && n.children.length));
    let level = open(root.children);
    const beside = [];
    for (let i = 0; i < 4 && level.length; i++) {
      const total = level.reduce((sum, n) => sum + n.size, 0);
      const big = level.reduce((a, b) => (b.size > a.size ? b : a), level[0]);
      if (!(level.length === 1 || big.size / total >= 0.6) || !big.children) break;
      const next = open(big.children);
      if (!next.length) break;
      beside.push(...level.filter(n => n !== big));
      level = next;
    }
    const paint = (n, t) => { n.topic = t; if (n.children) n.children.forEach(c => paint(c, t)); };
    [...level, ...beside].forEach((n, i) => paint(n, i % HUES.length));
    topicCount = level.length + beside.length;
  };
  // The memories that have no parent and nothing under them are not top-level cards: they are gathered, newest
  // first, in one grey block that sits beside the tree.
  const gather = list => {
    const loose = list.filter(n => n.layer === 0).sort((a, b) => (b.to || '').localeCompare(a.to || '') || a.id.localeCompare(b.id));
    const dated = loose.filter(n => n.to), undated = loose.filter(n => !n.to);
    return {real: list.filter(n => n.layer > 0), loose: [...dated, ...undated]};
  };
  const makeLoose = list => {
    const dates = list.map(n => n.to).filter(Boolean).sort();
    const block = {
      id: LOOSE, title: `未汇总的记忆 · ${list.length} 条`, snippet: '', layer: 1, size: list.length, from: dates[0] || '', to: dates[dates.length - 1] || '',
      loose: true, parent: root, depth: 0, topic: NEUTRAL, weight: Math.pow(list.length, POWER), items: null,
    };
    adopt(block, list);
    byId.set(LOOSE, block);
    root.gathered = block;
  };
  const buildTree = list => {
    generation++;
    byId.clear();
    root = {id: '', title: '全部', layer: -1, size: 0, virtual: true, topic: NEUTRAL, weight: 1, depth: -1, gathered: null};
    const {real, loose} = gather(list);
    adopt(root, real);
    if (loose.length) makeLoose(loose);
    root.size = list.reduce((sum, c) => sum + c.size, 0);
    assignTopics();
  };

  /* ---------- layout: squarified treemap in fractions of the parent's inner area ---------- */
  const fontOf = n => TITLE_PX[Math.min(n.layer, 4)];
  const headOf = n => fontOf(n) + 17 + (n.loose ? 0 : SUM_H);
  const edge = it => (it.depth < 1 ? TOP_GAP : GAP) / 2;
  // the part of a cell where its children go
  const inner = (c, x, y, w, h) => {
    if (c.virtual || c.group) return [x, y, w, h];
    const g = edge(c), head = headOf(c);
    return [x + g + PAD, y + g + head, w - 2 * g - 2 * PAD, h - 2 * g - head - PAD];
  };
  // How big a card has to be on screen to show its children: room for two children side by side, each with a title
  // and two lines of text (the block of loose memories, whose children are all alike, opens for a single column).
  const openSize = it => {
    const g = edge(it), across = it.loose ? 1 : 2, down = it.loose ? 1 : 2;
    if (it.group) return [2 * CHILD_W + GAP, 2 * CHILD_H + GAP];
    return [across * CHILD_W + (across - 1) * GAP + 2 * g + 2 * PAD, down * CHILD_H + (down - 1) * GAP + 2 * g + headOf(it) + PAD];
  };
  // How many pixels a card's inner area has at least when it shows its children: the least it opens at, or what it
  // gets when the picture is fitted.
  const openArea = c => {
    if (c.virtual) return kFit * kFit * WW * WH;
    const least = (2 * CHILD_W + GAP) * (2 * CHILD_H + GAP);
    const [, , w, h] = cellRect(c, kFit);
    const [, , iw, ih] = inner(c, 0, 0, w, h);
    return Math.max(least, iw * ih);
  };
  const squarify = (parts, asp) => {
    let total = 0;
    for (const p of parts) total += p.weight;
    const set = (p, x, y, w, h) => { p.nx = x / asp; p.nw = w / asp; p.ny = y; p.nh = h; };
    const scale = asp / total;
    let x = 0, y = 0, w = asp, hh = 1, i = 0;
    while (i < parts.length) {
      const side = Math.min(w, hh);
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
      let at = w >= hh ? y : x;
      for (let k = i; k < j; k++) {
        const len = (parts[k].weight * scale) / thick;
        if (w >= hh) set(parts[k], x, at, thick, len); else set(parts[k], at, y, len, thick);
        at += len;
      }
      if (w >= hh) { x += thick; w -= thick; } else { y += thick; hh -= thick; }
      i = j;
    }
  };
  const makeGroup = (owner, lo, hi) => {
    let weight = 0, size = 0;
    for (let i = lo; i < hi; i++) { weight += owner.sorted[i].weight; size += owner.sorted[i].size; }
    return {group: true, owner, lo, hi, weight, size, count: hi - lo, depth: owner.depth + 1, topic: owner.topic, layer: Math.max(owner.layer - 1, 0), items: null};
  };
  let miniDirty = true;
  // The loose memories are all alike: a grid of cards of at least the least size, newest first; what does not fit is
  // one "other" tile in the last place.
  const layoutGrid = (c, owner, list, base) => {
    const [, , w, hh] = cellRect(c, kFit);
    const [, , iw0, ih0] = c.group ? [0, 0, w, hh] : inner(c, 0, 0, w, hh);
    // measured at least as big as the card is when it opens, so an "other" tile always holds fewer than its parent
    const iw = Math.max(iw0, c.group ? 2 * CHILD_W + GAP : CHILD_W), ih = Math.max(ih0, 2 * CHILD_H + GAP);
    const cols = Math.max(1, Math.floor((iw + GAP) / (CHILD_W + GAP)));
    const cap = Math.max(2, cols * Math.floor((ih + GAP) / (CHILD_H + GAP)));
    const parts = list.length <= cap ? list.slice() : [...list.slice(0, cap - 1), makeGroup(owner, base + cap - 1, base + list.length)];
    const rowH = Math.min(ih / Math.ceil(parts.length / cols), CHILD_H * 2.4);
    const asp = clamp(c.asp || 1.6, 0.25, 5);
    parts.forEach((p, i) => {
      p.nx = (i % cols) / cols; p.nw = 1 / cols;
      p.ny = (Math.floor(i / cols) * rowH) / ih; p.nh = rowH / ih;
      p.lp = c;
      p.asp = (p.nw * asp) / p.nh;
    });
    c.items = parts;
    miniDirty = true;
  };
  // What a card (or an "other" tile) shows in its inner area: the big children by name and the rest as "other" tiles.
  const layout = c => {
    if (c.items) return;
    const owner = c.group ? c.owner : c;
    const base = c.group ? c.lo : 0;
    const list = c.group ? owner.sorted.slice(c.lo, c.hi) : owner.sorted;
    let total = 0;
    for (const n of list) total += n.weight;
    if (owner.loose) { layoutGrid(c, owner, list, base); return; }
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
    // the block of loose memories takes a band of its own beside the tree: a column when the picture is wide, else a row
    const band = c === root ? root.gathered : null;
    const share = !band ? 0 : parts.length ? clamp(band.weight / (total + band.weight), 0.14, 0.3) : 1;
    if (parts.length) {
      const wide = asp >= 1;
      squarify(parts, wide ? asp * (1 - share) : asp / (1 - share));
      for (const p of parts) {
        if (wide) { p.nx *= 1 - share; p.nw *= 1 - share; } else { p.ny *= 1 - share; p.nh *= 1 - share; }
      }
      if (band) {
        if (wide) Object.assign(band, {nx: 1 - share, ny: 0, nw: share, nh: 1});
        else Object.assign(band, {nx: 0, ny: 1 - share, nw: 1, nh: share});
        parts.push(band);
      }
    } else if (band) {
      Object.assign(band, {nx: 0, ny: 0, nw: 1, nh: 1});
      parts.push(band);
    }
    for (const p of parts) {
      p.lp = c;
      p.asp = p.nh > 0 ? p.nw * asp / p.nh : 1;
    }
    c.items = parts;
    miniDirty = true;
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
    miniDirty = true;
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
    const born = generation;
    n.pending = fetch(`/api/outline?root=${encodeURIComponent(n.id)}&depth=2`)
      .then(r => (r.ok ? r.json() : Promise.reject(new Error(String(r.status)))))
      .then(list => {
        if (born !== generation) return;
        adopt(n, (list[0] && list[0].children) || []);
        n.items = null;
      })
      .catch(() => { n.failed = true; })
      .finally(() => { n.pending = null; inflight--; renderOutline(); miniDirty = true; request(); })
      .then(() => n);
    return n.pending;
  };
  const want = n => { if (!n.pending && !n.failed && inflight < 3) loadChildren(n); };
  // Make sure every node on a path from the top (ids, top first) is loaded; the last one, or null when it is not there.
  const ensure = async ids => {
    for (let i = 0; i < ids.length; i++) {
      if (byId.has(ids[i])) continue;
      const above = i ? byId.get(ids[i - 1]) : root;
      if (!above) return null;
      await loadChildren(above);
    }
    return byId.get(ids[ids.length - 1]) || null;
  };

  /* ---------- the floating layers: what takes room from the picture ---------- */
  const panel = $('tree-panel');
  const side = $('tree-side');
  const foot = $('tree-foot');
  const mini = $('tree-mini');
  let vw = 0, vh = 0, dpr = 1;
  let detailPinned = store.get('rp-tree-detail-pin') === '1';
  const phone = () => vw <= 767;
  const panelOpen = () => panel.classList.contains('is-open');
  const outlineOpen = () => side.classList.contains('is-open');
  // The part of the window the picture is fitted into: the margins, the open outline and the open panel left out.
  const margins = withPanel => {
    const m = {l: MARGIN.x, r: MARGIN.x, t: phone() ? MARGIN.phoneTop : MARGIN.top, b: phone() ? MARGIN.phoneBottom : MARGIN.bottom};
    if (outlineOpen() && !phone() && innerWidth >= SIDE_BREAKPOINT) m.l += side.offsetWidth + 14;
    if (withPanel ? panelOpen() : (detailPinned && panelOpen())) {
      if (phone()) m.b += panel.offsetHeight + 8;
      else m.r += panel.offsetWidth + 14;
    }
    return m;
  };
  const usable = withPanel => {
    const m = margins(withPanel);
    return {x: m.l, y: m.t, w: Math.max(120, vw - m.l - m.r), h: Math.max(120, vh - m.t - m.b)};
  };

  /* ---------- the view: world centre and scale, eased towards a goal or flown along a path ---------- */
  const view = {cx: 0, cy: 0, k: 1};
  const goal = {cx: 0, cy: 0, k: 1};
  let tween = null;
  let kFit = 1;
  let atFit = true;                  // the whole tree is in view, as fitted: a new window size fits it again
  const views = [];                  // the views Esc goes back through, the latest last
  const calcFit = () => {
    const u = usable(true);
    kFit = Math.min(u.w / WW, u.h / WH);
  };
  const clampView = g => {
    g.k = clamp(g.k, kFit * MIN_RATIO, kFit * MAX_RATIO);
    g.cx = clamp(g.cx, 0, WW);
    g.cy = clamp(g.cy, 0, WH);
  };
  const go = (g, instant, fitting = false) => {
    tween = null;
    if (!fitting) atFit = false;
    Object.assign(goal, g);
    clampView(goal);
    if (instant || reduced.matches) Object.assign(view, goal);
    request();
  };
  // Smooth zoom: the point that stays put on screen is the fixed point of the move, so a big zoom does not swing.
  const fly = (g, fitting = false) => {
    if (!fitting) atFit = false;
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
  const fitView = () => {
    const u = usable(true);
    return {cx: WW / 2 - (u.x + u.w / 2 - vw / 2) / kFit, cy: WH / 2 - (u.y + u.h / 2 - vh / 2) / kFit, k: kFit};
  };
  const sameView = (a, b) => Math.abs(Math.log(a.k / b.k)) < 0.01 && Math.hypot(a.cx - b.cx, a.cy - b.cy) * b.k < 4;
  // Remember where the view was before a jump, so that Esc can come back to it.
  const pushView = () => {
    const now = {cx: goal.cx, cy: goal.cy, k: goal.k, fit: atFit};
    if (views.length && sameView(views[views.length - 1], now)) return;
    views.push(now);
    if (views.length > VIEW_HISTORY) views.shift();
  };
  // Back to the view before the last jump; a view that was the whole tree comes back as the whole tree, fitted into
  // the room there is now (a panel may have opened since).
  const goBack = () => {
    const to = views.pop();
    if (!to) return false;
    if (to.fit) fitAll(true, false);
    else { fly(to); atFit = false; }
    return true;
  };
  const fitAll = (animate = true, record = true) => {
    calcFit();
    if (record && animate && !atFit) pushView();
    crumbKey = null;
    if (animate) fly(fitView(), true); else go(fitView(), true, true);
    atFit = true;
  };
  // The scale and centre that put a cell in the middle of the part of the canvas the panel leaves free.
  const viewFor = (it, margin) => {
    reveal(it);
    const u = usable(true);
    const fw = u.w - 2 * margin, fh = u.h - 2 * margin;
    const a = cellRect(it, 1), b = cellRect(it, 2);
    const dw = b[2] - a[2], dh = b[3] - a[3];
    const k = clamp(Math.min((fw - (a[2] - dw)) / dw, (fh - (a[3] - dh)) / dh), kFit * MIN_RATIO, kFit * MAX_RATIO);
    const r = cellRect(it, k);
    const ox = u.x + u.w / 2 - (r[0] + r[2] / 2), oy = u.y + u.h / 2 - (r[1] + r[3] / 2);
    return {k, cx: (vw / 2 - ox) / k, cy: (vh / 2 - oy) / k};
  };
  // Zoom smoothly to the scale at which a card just fits the free part of the window.
  const zoomTo = it => {
    const {k, cx, cy} = viewFor(it, 14);
    if (!sameView({k, cx, cy}, goal)) pushView();
    fly({cx, cy, k});
  };
  // To show a node: its parent when the node is readable there, else the node itself.
  const flyTo = n => {
    reveal(n);
    let target = n;
    if (n.parent && !n.parent.virtual && !n.parent.loose && (!n.children || !n.children.length)) {
      const v = viewFor(n.parent, 14);
      const r = cellRect(n, v.k);
      if (r[2] >= 200 && r[3] >= 100) target = n.parent;
    }
    zoomTo(target);
  };
  const zoomBy = (factor, px = vw / 2, py = vh / 2) => {
    const k = clamp(goal.k * factor, kFit * MIN_RATIO, kFit * MAX_RATIO);
    const wx = goal.cx + (px - vw / 2) / goal.k;
    const wy = goal.cy + (py - vh / 2) / goal.k;
    go({cx: wx - (px - vw / 2) / k, cy: wy - (py - vh / 2) / k, k});
  };
  const stopMotion = () => { tween = null; Object.assign(goal, view); };
  // After a card is selected the panel may cover it: slide the picture just far enough to show it.
  const keepInSight = n => {
    const tile = shownOf(n);
    if (!tile || !panelOpen() || tween) return;
    let dx = 0, dy = 0;
    if (phone()) {
      const limit = vh - panel.offsetHeight - 16;
      if (tile.sy + tile.sh > limit) dy = Math.max(limit - (tile.sy + tile.sh), MARGIN.phoneTop - tile.sy);
    } else {
      const limit = vw - panel.offsetWidth - 28;
      if (tile.sx + tile.sw > limit) dx = Math.max(limit - (tile.sx + tile.sw), 16 - tile.sx);
    }
    if (dx || dy) go({cx: goal.cx - dx / goal.k, cy: goal.cy - dy / goal.k, k: goal.k});
  };

  /* ---------- text ---------- */
  const measure = doc.createElement('canvas').getContext('2d');
  const tables = new Map();          // weight -> (character -> width at 100px)
  const tableOf = weight => {
    let table = tables.get(weight);
    if (!table) { table = new Map(); tables.set(weight, table); }
    return table;
  };
  const charW = (ch, weight, table = tableOf(weight)) => {
    let w = table.get(ch);
    if (w === undefined) {
      measure.font = `${weight} 100px ${colors.font}`;
      w = measure.measureText(ch).width;
      table.set(ch, w);
    }
    return w;
  };
  const textW = (text, fs, weight) => {
    const table = tableOf(weight);
    let w = 0;
    for (const ch of text) w += charW(ch, weight, table);
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
  // Wrap text into at most `maxLines` lines, a Chinese character or an English word at a time; the last line ends
  // in … when text was left over.
  const inWord = ch => /[\w.\-/+#@%]/.test(ch);
  const wrapRaw = (text, fs, weight, maxW, maxLines) => {
    const table = tableOf(weight);
    const lines = [];
    let line = '', w = 0, brk = -1;
    for (const ch of text) {
      if (lines.length > maxLines) break;
      if (ch === '\n') { lines.push(line); line = ''; w = 0; brk = -1; continue; }
      if (ch === ' ' && !line) continue;
      const cw = (charW(ch, weight, table) * fs) / 100;
      if (w + cw > maxW && line && !NO_START.includes(ch)) {
        // a Latin word that does not fit moves to the next line whole; anything else breaks where it is
        let from = line.length;
        if (inWord(ch)) while (from > 0 && inWord(line[from - 1])) from--;
        if (inWord(ch) && from > 0 && from < line.length) { lines.push(line.slice(0, from).trimEnd()); line = line.slice(from); w = textW(line, fs, weight); }
        else if (brk > line.length * 0.5) { lines.push(line.slice(0, brk)); line = line.slice(brk + 1); w = textW(line, fs, weight); }
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
  // What a node says besides its title: all of its text. Old data without a title of its own is called by the first
  // words of its text, and those are not said twice.
  const restOf = (n, text) => {
    if (!n.untitled || !text) return text || '';
    const head = n.title.replace(/…$/, '');
    return head && text.startsWith(head) ? text.slice(head.length).replace(/^[\s，,。.；;：:、—-]+/, '') : text;
  };
  // What a card says besides its title: the rest of its text; on a small card all on one flow, on a big one with its lines.
  const wordsOf = (n, source, lines) => {
    const memo = n.words;
    if (memo && memo.source === source && memo.lines === lines) return memo.value;
    const rest = restOf(n, source);
    const value = lines ? rest : rest.replace(/\s*\n+\s*/g, ' ');
    n.words = {source, lines, value};
    return value;
  };
  const titleSize = w => half(clamp(12 + (w - 110) * (8 / 370), 12, 20));
  const bodySize = w => half(clamp(12 + (w - 150) * (8 / 1050), 12, 20));

  // How a card says its words, worked out from the card's own size (whole, not as much of it as the window shows).
  // The body gets the biggest size, 12 to 40px (up to 72px on a card whose short side is over 480px), at which all of it fits between the title and the line of facts, and
  // the title grows with it (to 44px at most); what cannot fit even at 12px is cut short with … (a card smaller than
  // FILL_W x FILL_H keeps the size its width gives it). Remembered by text and size, so panning measures nothing.
  let layGen = 0;
  const placeCard = (n, source, w, hh, sum, roomy) => {
    const g = Math.min(edge(n), w / 6, hh / 6);
    const left = g + (sum ? 14 : 11), tw = w - g - 10 - left, top = g + 8;
    const metaH = w - 2 * g >= 120 && hh - 2 * g >= 74 ? 17 : 0;
    const limit = hh - g - 8 - metaH;
    const weight = sum ? 700 : 600;
    const baseT = titleSize(w), baseB = bodySize(w);
    if (tw < 28 || hh - 2 * g < Math.round(baseT * 1.32) + 12 || Math.floor((limit - top) / Math.round(baseT * 1.32)) < 1) return null;
    const text = wordsOf(n, source, roomy);
    const bmax = half(clamp(Math.min(w, hh) / 12, BODY_MAX, 72)), tmax = TITLE_MAX * bmax / BODY_MAX;   // a card much bigger than a screen's worth lets its words grow with it
    // everything at one body size: the title's lines, the body's lines, and whether all of the body fits
    const put = bfs => {
      const tfs = half(clamp(baseT * (bfs / baseB), BODY_MIN, tmax));
      const lh = Math.round(tfs * 1.32), lh2 = Math.round(bfs * 1.5);
      const rows = Math.floor((limit - top) / lh);
      const titleLines = wrapRaw(n.title, tfs, weight, tw, text ? Math.min(rows, w >= 380 ? 3 : 2) : Math.min(rows, 5));
      const bodyTop = top + titleLines.length * lh + 3;
      const room = Math.floor((limit - bodyTop) / lh2);
      let bodyLines = [], fit = true;
      if (text && room >= 1) {
        bodyLines = wrapRaw(text, bfs, 400, tw, room + 1);
        fit = bodyLines.length <= room;
        if (!fit) bodyLines = wrapRaw(text, bfs, 400, tw, room);
      } else if (text) fit = false;
      return {tfs, lh, titleLines, bfs, lh2, bodyTop, bodyLines, fit, room, hasText: !!text};
    };
    const fills = !!text && w >= FILL_W && hh >= FILL_H;
    let best;
    if (!fills) best = put(baseB);
    else {
      // The biggest size at which it fits is looked for by halving, starting from the size the area asks for (the
      // text's width at 100px says how much room it needs), so that only a few sizes are tried.
      const ideal = Math.sqrt((tw * Math.max(1, limit - top - 2 * Math.round(baseT * 1.32)) * 100) / (1.5 * Math.max(1, textW(text, 100, 400))));
      const guess = half(clamp(ideal * 0.8, BODY_MIN, bmax));
      let lo, hi;
      const first = put(guess);
      if (first.fit) {
        best = first; lo = guess; hi = bmax + 0.5;
        const up = half(clamp(ideal * 1.1, guess, bmax));
        if (up > guess) { const r = put(up); if (r.fit) { best = r; lo = up; } else hi = up; }
        if (lo >= bmax) hi = lo;
        else if (hi > bmax) { const r = put(bmax); if (r.fit) { best = r; lo = hi = bmax; } else hi = bmax; }
      } else {
        best = put(BODY_MIN); lo = BODY_MIN;      // when not even the smallest size fits, that one is cut short with …
        hi = best.fit ? guess : lo;
      }
      for (;;) {
        const mid = half((lo + hi) / 2);
        if (mid <= lo || mid >= hi) break;
        const tried = put(mid);
        if (tried.fit) { lo = mid; best = tried; } else hi = mid;
      }
    }
    return {...best, left, tw, top, metaH, weight, g};
  };
  const layoutCard = (n, source, w, hh, sum, roomy) => {
    const rw = Math.round(w), rh = Math.round(hh);
    const memo = n.lay;
    if (memo && memo.w === rw && memo.h === rh && memo.source === source && memo.title === n.title && memo.sum === sum && memo.gen === layGen) return memo.value;
    const value = placeCard(n, source, rw, rh, sum, roomy);
    n.lay = {w: rw, h: rh, source, title: n.title, sum, gen: layGen, value};
    return value;
  };

  /* ---------- drawing ---------- */
  let frameNo = 0;
  let selected = null, hovered = null, hitNodes = [];
  const needFull = [];
  const stat = {cards: 0, bodies: 0, unfilled: 0, full: 0};

  const roundRect = (x, y, w, hh, r) => { ctx.beginPath(); ctx.roundRect(x, y, w, hh, r); };
  // The board: a soft light in the middle and a faint grid of dots that moves with the picture.
  let bgKey = '', bgGrad = null, dots = null;
  const background = () => {
    const key = `${vw}|${vh}|${dpr}|${colors.bgA}`;
    if (key !== bgKey) {
      bgKey = key;
      bgGrad = ctx.createRadialGradient(vw / 2, vh * 0.45, Math.min(vw, vh) * 0.15, vw / 2, vh / 2, Math.hypot(vw, vh) * 0.6);
      bgGrad.addColorStop(0, colors.bgA);
      bgGrad.addColorStop(1, colors.bgB);
      const tile = doc.createElement('canvas');
      tile.width = tile.height = Math.round(24 * dpr);
      const t = tile.getContext('2d');
      t.fillStyle = colors.dot;
      t.beginPath();
      t.arc(tile.width / 2, tile.height / 2, 1.1 * dpr, 0, Math.PI * 2);
      t.fill();
      dots = ctx.createPattern(tile, 'repeat');
      dots.setTransform(new DOMMatrix().scale(1 / dpr));
    }
    ctx.globalAlpha = 1;
    ctx.fillStyle = bgGrad;
    ctx.fillRect(0, 0, vw, vh);
    ctx.save();
    ctx.translate((vw / 2 - view.cx * view.k) % 24, (vh / 2 - view.cy * view.k) % 24);
    ctx.fillStyle = dots;
    ctx.fillRect(-24, -24, vw + 48, vh + 48);
    ctx.restore();
  };
  const card = (it, x, y, w, hh, a) => {
    const g = Math.min(edge(it), w / 6, hh / 6);
    const x0 = Math.max(x + g, -40), y0 = Math.max(y + g, -40), x1 = Math.min(x + w - g, vw + 40), y1 = Math.min(y + hh - g, vh + 40);
    const rw = x1 - x0, rh = y1 - y0;
    if (rw <= 0 || rh <= 0) return;
    const p = pal[it.topic], li = Math.min(it.layer, 5);
    const r = Math.min(7, rw / 4, rh / 4);
    if (rw >= 28 && rh >= 28 && !it.group) {          // the card lifts off the board: a thin shadow under it and a light on its top edge
      ctx.globalAlpha = a;
      ctx.fillStyle = colors.shadow;
      roundRect(x0, y0 + 2, rw, rh, r);
      ctx.fill();
    }
    ctx.globalAlpha = a;
    ctx.fillStyle = p.fill[li];
    if (it.group) { ctx.globalAlpha = a * 0.7; ctx.fillStyle = p.fill[Math.min(li + 1, 5)]; }
    roundRect(x0, y0, rw, rh, r);
    ctx.fill();
    if (rw >= 40 && rh >= 40 && !it.group && x >= -40 && y >= -40) {
      ctx.globalAlpha = a;
      ctx.strokeStyle = colors.hi;
      ctx.lineWidth = 1;
      ctx.beginPath();
      ctx.moveTo(x0 + r, y0 + 1.5);
      ctx.lineTo(x0 + rw - r, y0 + 1.5);
      ctx.stroke();
    }
    if (rw >= 12 && rh >= 12) {
      ctx.globalAlpha = a;
      ctx.strokeStyle = p.edge[li];
      ctx.lineWidth = 1;
      if (it.group) ctx.setLineDash([4, 3]);
      roundRect(x0 + 0.5, y0 + 0.5, rw - 1, rh - 1, r);
      ctx.stroke();
      if (it.group) ctx.setLineDash([]);
    }
    if (!it.group && !it.loose && it.children !== undefined && (it.children === null || it.children.length) && rw >= 40 && x + g >= -40) {
      ctx.fillStyle = p.bar;
      ctx.beginPath();
      ctx.roundRect(x0, y0, 4, rh, [r, 0, 0, r]);
      ctx.fill();
    }
  };

  // A line of facts: layer, count and dates, as much of it as fits.
  const meta = (n, maxW) => {
    if (n.loose) return n.to ? (n.from === n.to ? n.from : `${n.from} – ${n.to}`) : '';
    const dates = n.from ? (n.from === n.to || !n.to ? n.from : `${n.from} – ${n.to}`) : '';
    const facts = n.children === null || n.children.length ? [`L${n.layer}`, `${n.size - 1} 条`] : [];
    for (const parts of [[...facts, dates], facts, [dates]]) {
      const text = parts.filter(Boolean).join(' · ');
      if (text && textW(text, 11, 500) <= maxW) return text;
    }
    return '';
  };
  // What a card that does not show its children says, from the top down: its title, then its text at the biggest
  // size that fits, then a line of facts. Nothing is left blank that words can fill. Where the lines go is worked
  // out for the whole card; the window only cuts off what lies outside it.
  const closedBody = (n, x, y, w, hh, ta) => {
    const sum = n.children === null || (n.children && n.children.length > 0);
    const roomy = !sum && w * hh >= FULL_AREA;
    const full = roomy && n.snippet.endsWith('…');
    if (full && n.full === undefined && !n.fetching) needFull.push(n);
    const source = full && n.full !== undefined ? n.full : n.snippet;
    const L = layoutCard(n, source, w, hh, sum, roomy);
    if (!L) return;
    stat.cards++;
    const left = x + L.left, p = pal[n.topic];
    ctx.globalAlpha = ta;
    ctx.fillStyle = p.ink;
    ctx.textBaseline = 'middle';
    ctx.textAlign = 'left';
    ctx.font = `${L.weight} ${L.tfs}px ${colors.font}`;
    L.titleLines.forEach((line, i) => ctx.fillText(line, left, y + L.top + L.lh / 2 + i * L.lh));
    if (L.bodyLines.length) {
      ctx.globalAlpha = ta * 0.9;
      ctx.font = `400 ${L.bfs}px ${colors.font}`;
      ctx.textBaseline = 'top';
      L.bodyLines.forEach((line, i) => ctx.fillText(line, left, y + L.bodyTop + i * L.lh2));
      stat.bodies++;
      if (full && n.full !== undefined) stat.full++;
    } else if (L.hasText && L.room >= 1) stat.unfilled++;     // there were words and room for a line, yet nothing was drawn
    if (L.metaH) {
      ctx.font = `500 11px ${colors.font}`;
      const line = meta(n, L.tw);
      if (line) {
        ctx.globalAlpha = ta * 0.88;
        ctx.textBaseline = 'top';
        ctx.fillText(line, left, y + hh - L.g - 8 - 12);
      }
    }
  };
  // An "other" tile names what it holds: its members' titles, as many as fit.
  const groupBody = (it, x, y, w, hh, ta) => {
    if (w < 64 || hh < 26) return;
    const g = Math.min(edge(it), w / 6, hh / 6);
    const p = pal[it.topic];
    const left = x + g + 10, tw = w - 2 * g - 20;
    ctx.globalAlpha = ta * 0.9;
    ctx.fillStyle = p.ink;
    ctx.textAlign = 'left';
    ctx.textBaseline = 'middle';
    ctx.font = `650 12.5px ${colors.font}`;
    ctx.fillText(clip(`其他 ${it.count} 条`, 12.5, 650, tw), left, y + g + 15);
    const hint = w >= 110 && hh >= 64;
    const rows = Math.floor((hh - 2 * g - 30 - (hint ? 18 : 0)) / 17);
    if (rows >= 1 && tw >= 40) {
      ctx.font = `400 12px ${colors.font}`;
      ctx.globalAlpha = ta * 0.8;
      const members = it.owner.sorted.slice(it.lo, Math.min(it.hi, it.lo + rows));
      members.forEach((m, i) => ctx.fillText(clip(`· ${m.title}`, 12, 400, tw), left, y + g + 33 + i * 17));
    }
    if (hint) {
      ctx.globalAlpha = ta * 0.7;
      ctx.font = `400 11px ${colors.font}`;
      ctx.fillText('点一下展开', left, y + hh - g - 11);
    }
  };
  // The strip of an open card: its title with the small layer tag at the right end, and its own words in one line.
  const header = (n, x, y, w, ta) => {
    const g = edge(n), top = y + g, fs = fontOf(n), line1 = fs + 17;
    if (top + headOf(n) < 0 || top > vh) return;
    const p = pal[n.topic];
    const left = x + g + 14, right = x + w - g - 8;
    if (right - left < 60) return;
    ctx.textBaseline = 'middle';
    let tagW = 0;
    if (!n.loose) {
      const tag = `L${n.layer}`;
      ctx.font = `700 11px ${colors.font}`;
      tagW = textW(tag, 11, 700) + 10;
      ctx.globalAlpha = ta * 0.14;
      ctx.fillStyle = p.ink;
      roundRect(right - tagW, top + line1 / 2 - 8, tagW, 16, 8);
      ctx.fill();
      ctx.globalAlpha = ta;
      ctx.textAlign = 'center';
      ctx.fillText(tag, right - tagW / 2, top + line1 / 2 + 0.5);
    }
    ctx.globalAlpha = ta;
    ctx.fillStyle = p.ink;
    ctx.textAlign = 'left';
    ctx.font = `700 ${fs}px ${colors.font}`;
    ctx.fillText(clip(n.title, fs, 700, right - tagW - 8 - left), left, top + line1 / 2 + 0.5);
    if (n.loose) return;
    const words = wordsOf(n, n.snippet, false);
    if (words) {
      ctx.globalAlpha = ta * 0.82;
      ctx.font = `400 11.5px ${colors.font}`;
      ctx.fillText(clip(words, 11.5, 400, right - left), left, top + line1 + SUM_H / 2 - 3);
    }
  };
  const spinner = (x, y, a, now) => {
    ctx.globalAlpha = a * 0.7;
    ctx.strokeStyle = colors.text;
    ctx.lineWidth = 2;
    ctx.beginPath();
    ctx.arc(x, y, 5, now / 180, now / 180 + 4.2);
    ctx.stroke();
  };
  let clock = 0;
  const drawItems = (c, x, y, w, hh, fade) => {
    layout(c);
    for (const it of c.items) drawItem(it, x + it.nx * w, y + it.ny * hh, it.nw * w, it.nh * hh, fade);
  };
  const drawItem = (it, x, y, w, hh, fade) => {
    if (x > vw || y > vh || x + w < 0 || y + hh < 0 || w < MIN_TILE || hh < MIN_TILE) return;
    it.shown = frameNo; it.sx = x; it.sy = y; it.sw = w; it.sh = hh; it.opened = 0;
    const a = fade, ta = fade;
    const [ow, oh] = openSize(it);
    const canOpen = it.group || ((it.children === null || it.children.length > 0) && !it.failed);
    const open = canOpen && w >= ow && hh >= oh;
    const t = open ? (moving ? Math.min((Math.min(w / ow, hh / oh) - 1) / 0.12, 1) : 1) : 0;
    if (!it.group && it.children === null && w >= ow * 0.55 && hh >= oh * 0.55) want(it);
    if (t > 0) it.opened = frameNo;
    if (!it.group || t < 1) card(it, x, y, w, hh, it.group ? a * (1 - t) : a);
    const rest = 1 - Math.min(1, t * 2);       // the closed text is gone by the time half the children are in
    if (rest > 0) {
      if (it.group) groupBody(it, x, y, w, hh, ta * rest);
      else closedBody(it, x, y, w, hh, ta * rest);
    }
    if (it.pending && w >= 60 && hh >= 40) spinner(x + w - (t > 0 ? 60 : 20), y + 4 + (t > 0 ? headOf(it) / 2 : 12), a, clock);
    if (t === 0) return;
    if (!it.group) header(it, x, y, w, ta * t);
    if (it.children === null) {
      ctx.globalAlpha = ta * t * 0.7;
      ctx.fillStyle = pal[it.topic].ink;
      ctx.textAlign = 'center';
      ctx.textBaseline = 'middle';
      ctx.font = `400 12px ${colors.font}`;
      ctx.fillText('正在取下面的内容…', x + w / 2, y + hh / 2);
    } else {
      const [ix, iy, iw, ih] = inner(it, x, y, w, hh);
      drawItems(it, ix, iy, iw, ih, fade * t);
    }
  };

  const ring = (it, color, width, glow) => {
    const {sx: x, sy: y, sw: w, sh: hh} = it;
    ctx.save();
    ctx.globalAlpha = 1;
    ctx.strokeStyle = color;
    ctx.lineWidth = width;
    if (glow) { ctx.shadowColor = color; ctx.shadowBlur = glow; }
    if (w < 10 || hh < 10) {
      ctx.beginPath();
      ctx.arc(x + w / 2, y + hh / 2, 8, 0, Math.PI * 2);
      ctx.stroke();
    } else {
      const g = Math.min(edge(it), w / 6, hh / 6);
      roundRect(x + g + width / 2, y + g + width / 2, w - 2 * g - width, hh - 2 * g - width, 6);
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
    if (tile) ring(tile, color, width, glow);
  };

  const draw = now => {
    frameNo++;
    clock = now;
    stat.cards = stat.bodies = stat.unfilled = stat.full = 0;
    ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    background();
    const ox = vw / 2 - view.cx * view.k, oy = vh / 2 - view.cy * view.k;
    needFull.length = 0;
    drawItems(root, ox, oy, WW * view.k, WH * view.k, 1);
    for (const n of hitNodes) overlay(n, colors.hit, 3, 14);
    if (hovered && hovered !== selected) overlay(hovered, colors.text, 1.5, 0);
    if (selected) overlay(selected, colors.accent, 3, 18);
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

  /* ---------- the little map of the whole tree ---------- */
  const mctx = mini.getContext('2d');
  const MINI_W = 150;
  let miniBase = null, miniAt = 0, miniH = 94;
  const miniShown = () => !app.classList.contains('is-immersive') && !phone() && vh >= 520 && view.k > kFit * 1.15;
  // The outline of the tree as far as it is laid out: the top-level cards and, inside them, the cards they show.
  const paintMini = () => {
    miniH = clamp(Math.round(MINI_W * WH / WW), 64, 110);
    mini.style.height = `${miniH}px`;
    mini.width = Math.round(MINI_W * dpr);
    mini.height = Math.round(miniH * dpr);
    miniBase = doc.createElement('canvas');
    miniBase.width = mini.width; miniBase.height = mini.height;
    const m = miniBase.getContext('2d');
    m.scale(dpr * MINI_W / WW, dpr * miniH / WH);
    const walk = (c, depth) => {
      if (!c.items) return;
      for (const it of c.items) {
        const [x, y, w, hh] = cellRect(it, 1);
        const g = Math.min(edge(it), w / 6, hh / 6);
        m.globalAlpha = it.group ? 0.5 : 1;
        m.fillStyle = pal[it.topic].fill[Math.min(it.layer, 5)];
        m.strokeStyle = pal[it.topic].edge[Math.min(it.layer, 5)];
        m.lineWidth = 1.2 * WW / MINI_W;
        m.beginPath();
        m.rect(x + g, y + g, w - 2 * g, hh - 2 * g);
        m.fill();
        m.stroke();
        if (depth < 2 && !it.group) walk(it, depth + 1);
      }
    };
    walk(root, 0);
    miniDirty = false;
    miniAt = performance.now();
  };
  const drawMini = now => {
    const on = miniShown();
    mini.classList.toggle('is-on', on);
    if (!on) return;
    if (!miniBase || (miniDirty && now - miniAt > 250)) paintMini();
    mctx.setTransform(1, 0, 0, 1, 0, 0);
    mctx.clearRect(0, 0, mini.width, mini.height);
    mctx.drawImage(miniBase, 0, 0);
    const s = (MINI_W * dpr) / WW;
    const x0 = (view.cx - vw / 2 / view.k) * s, y0 = (view.cy - vh / 2 / view.k) * s;
    const rw = (vw / view.k) * s, rh = (vh / view.k) * s;
    mctx.fillStyle = 'rgb(0 0 0 / .22)';
    mctx.fillRect(0, 0, mini.width, mini.height);
    mctx.clearRect(x0, y0, rw, rh);
    mctx.drawImage(miniBase, x0, y0, rw, rh, x0, y0, rw, rh);
    mctx.strokeStyle = colors.accent;
    mctx.lineWidth = 2 * dpr;
    mctx.strokeRect(x0, y0, rw, rh);
  };
  let miniDrag = false;
  const miniGo = event => {
    const rect = mini.getBoundingClientRect();
    stopMotion();
    go({cx: clamp(((event.clientX - rect.left) / rect.width) * WW, 0, WW), cy: clamp(((event.clientY - rect.top) / rect.height) * WH, 0, WH), k: goal.k});
  };
  mini.addEventListener('pointerdown', event => { mini.setPointerCapture(event.pointerId); miniDrag = true; miniGo(event); });
  mini.addEventListener('pointermove', event => { if (miniDrag) miniGo(event); });
  mini.addEventListener('pointerup', () => { miniDrag = false; });
  mini.addEventListener('pointercancel', () => { miniDrag = false; });

  /* ---------- the frame loop: redraws only while something changes ---------- */
  let raf = 0, last = 0, crumbKey = null, statusText = '', statusTimer = 0;
  let peakMs = 0, sumMs = 0, frames = 0;   // how long frames took while the picture moved (average/peak, in ms)
  let moving = false;                // the picture is moving: cards cross-fade as they open; at rest they are either open or closed
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
    const began = performance.now();
    moving = !!tween || !settled();
    draw(now);
    drawMini(now);
    updateSlider();
    updateCrumbs();
    updateStatus();
    fetchFull();
    if (moving) { peakMs = Math.max(peakMs, performance.now() - began); sumMs += performance.now() - began; frames++; }
    const busy = !settled() || inflight > 0;
    // the numbers a test can read: where the tree is, how much was drawn in the last frame and how long drawing took
    canvas.dataset.settled = busy ? '0' : '1';
    if (!busy) {
      Object.assign(canvas.dataset, {cards: stat.cards, bodies: stat.bodies, unfilled: stat.unfilled, full: stat.full});
      canvas.dataset.drawMs = (performance.now() - began).toFixed(2);
      if (frames) { canvas.dataset.moveMs = `${(sumMs / frames).toFixed(1)}/${peakMs.toFixed(1)}`; peakMs = sumMs = frames = 0; }
      canvas.dataset.tree = [vw / 2 - view.cx * view.k, vh / 2 - view.cy * view.k, WW * view.k, WH * view.k].map(Math.round).join(',');
    }
    if (busy) request();
  };

  // What the tests read: how a card was last set (its lines and sizes) and where it was drawn.
  const cardInfo = n => ({id: n.id, x: n.sx, y: n.sy, w: n.sw, h: n.sh, lay: n.lay ? n.lay.value : null});
  canvas.rpCard = id => { const n = byId.get(id); return n && n.shown === frameNo ? cardInfo(n) : null; };
  canvas.rpCards = () => [...byId.values()].filter(n => n.shown === frameNo && n.opened !== frameNo && n.lay && n.lay.value).map(cardInfo);

  const updateStatus = () => {
    const mid = pickAt(vw / 2, vh / 2);
    const text = !mid ? '' : mid.loose ? '未汇总的记忆' : mid.layer === 0 ? '第 0 层（记忆本身）' : `第 ${mid.layer} 层`;
    if (text === statusText) return;
    statusText = text;
    clearTimeout(statusTimer);
    statusTimer = setTimeout(() => { $('tree-status').textContent = statusText ? `现在看到${statusText}` : ''; }, 350);
  };

  /* ---------- the capsule's crumbs: where we are ---------- */
  const crumbs = $('tree-crumbs');
  const updateCrumbs = () => {
    const at = selected || viewTile();
    const key = `${at.id}|${at.title}`;
    if (key === crumbKey) return;
    crumbKey = key;
    const chain = [];
    for (let p = at; p && !p.virtual; p = p.parent) chain.unshift(p);
    crumbs.replaceChildren();
    const add = (text, current, onClick) => {
      if (crumbs.lastChild) crumbs.append(h('span', {class: 'sep', 'aria-hidden': 'true', text: '›'}));
      crumbs.append(h('button', {type: 'button', title: text, text, 'aria-current': current ? 'location' : false, onclick: onClick}));
    };
    add('全部', !chain.length, () => fitAll());
    chain.forEach((n, i) => add(n.title, i === chain.length - 1, () => { hitList.hidden = true; zoomTo(n); }));
    crumbs.scrollLeft = crumbs.scrollWidth;
  };

  /* ---------- the zoom slider: a log scale from the fitted picture to the deepest zoom ---------- */
  const slider = $('tree-slider'), pct = $('tree-pct');
  const LOG_MAX = Math.log(MAX_RATIO);
  const ratioText = r => (r < 100 ? `${Math.round(r * 100)}%` : `×${Math.round(r)}`);
  let sliderKey = '';
  const updateSlider = () => {
    const r = view.k / kFit, t = clamp(Math.log(Math.max(r, 1)) / LOG_MAX, 0, 1);
    const key = `${ratioText(r)}|${t.toFixed(4)}`;
    if (key === sliderKey) return;
    sliderKey = key;
    slider.style.setProperty('--t', t.toFixed(4));
    slider.setAttribute('aria-valuenow', String(Math.round(t * 100)));
    slider.setAttribute('aria-valuetext', `放大到 ${ratioText(r)}`);
    pct.textContent = ratioText(r);
  };
  // Zoom to a multiple of the fitted picture, about the middle of the window.
  const zoomRatio = r => {
    stopMotion();
    if (r <= 1.005) { fitAll(); return; }
    zoomBy(clamp(kFit * r, kFit * MIN_RATIO, kFit * MAX_RATIO) / goal.k);
  };
  const stepRatio = t => zoomRatio(Math.exp(clamp(Math.log(Math.max(goal.k / kFit, 1)) / LOG_MAX + t, 0, 1) * LOG_MAX));
  let sliding = false;
  const slideTo = event => {
    const rect = slider.getBoundingClientRect();
    zoomRatio(Math.exp(clamp((event.clientX - rect.left - 8) / (rect.width - 16), 0, 1) * LOG_MAX));
  };
  slider.addEventListener('pointerdown', event => { slider.setPointerCapture(event.pointerId); slider.focus(); sliding = true; slideTo(event); event.preventDefault(); });
  slider.addEventListener('pointermove', event => { if (sliding) slideTo(event); });
  slider.addEventListener('pointerup', () => { sliding = false; });
  slider.addEventListener('pointercancel', () => { sliding = false; });
  slider.addEventListener('keydown', event => {
    const steps = {ArrowRight: 0.03, ArrowUp: 0.03, ArrowLeft: -0.03, ArrowDown: -0.03, PageUp: 0.15, PageDown: -0.15};
    if (event.key === 'Home') zoomRatio(1);
    else if (event.key === 'End') zoomRatio(MAX_RATIO);
    else if (event.key in steps) stepRatio(steps[event.key]);
    else return;
    event.preventDefault();
    event.stopPropagation();
  });

  /* ---------- full text of the memories that are big enough to read ---------- */
  let fetching = 0;
  const fetchFull = () => {
    for (const n of needFull) {
      if (fetching >= 3) return;
      if (n.fetching || n.full !== undefined) continue;
      n.fetching = true;
      fetching++;
      api('GET', `/api/node/${encodeURIComponent(n.id)}`)
        .then(r => { n.full = r.ok ? r.data.content : n.snippet; })
        .finally(() => { n.fetching = false; fetching--; request(); });
    }
  };

  /* ---------- the detail panel: read and do everything about one node ---------- */
  let detail = null;                 // what /api/node said about the selected node
  let act = null;                    // the form that is open: edit, child, move, delete
  let editTitle = false;
  let selectSerial = 0;
  let kidsShown = 6;
  const fold = {relations: true, history: false};
  const span = it => (it.from ? (it.to && it.to !== it.from ? `${it.from} – ${it.to}` : it.from) : '');
  const snippetOf = text => {
    const t = text.trim();
    return [...t].length > 600 ? `${[...t].slice(0, 600).join('')}…` : t;
  };
  const setPanel = open => {
    panel.classList.toggle('is-open', open);
    app.classList.toggle('has-panel', open);
    panel.setAttribute('aria-hidden', open ? 'false' : 'true');
    if (open) panel.removeAttribute('inert'); else panel.setAttribute('inert', '');
  };
  // A node on the page: itself when it is loaded, else the path to it is fetched first.
  const nodeById = async id => {
    const known = byId.get(id);
    if (known) return known;
    const r = await api('GET', `/api/node/${encodeURIComponent(id)}`);
    if (!r.ok || r.data.deleted) return null;
    return ensure([...r.data.path.map(p => p.id), id]);
  };
  const goTo = async id => {
    const n = await nodeById(id);
    if (!n) { toast('没找到这个节点，它可能已经被删除', true); return null; }
    select(n, {fly: true, focusPanel: true});
    return n;
  };
  const tagSearch = tag => {
    searchBox.value = `#${tag}`;
    search();
  };

  const field = (label, control, hint, required = false) => h('div', {class: 'field'}, h('label', {for: control.id}, label, required && h('span', {class: 'req', text: '必填'})), control, hint && h('p', {class: 'hint', text: hint}));
  const formError = (box, r, retry) => {
    box.replaceChildren(icon('alert'), h('p', {text: r.data.error || '没有成功'}));
    box.hidden = false;
    if (r.data.conflict) box.append(h('button', {type: 'button', class: 'btn btn-sm', text: '重新载入最新内容', onclick: retry}));
  };
  const errorBox = () => h('div', {class: 'banner banner-error', role: 'alert', hidden: true});
  const cancelButton = () => h('button', {type: 'button', class: 'btn', text: '取消', onclick: () => { act = null; renderPanel(); focusPanel(); }});
  const ctrlEnter = (area, form) => area.addEventListener('keydown', event => { if (event.key === 'Enter' && (event.ctrlKey || event.metaKey)) { event.preventDefault(); form.requestSubmit(); } });

  // Run a change from a form (or any element that holds the buttons and one error box): `done(data)` when it
  // worked, else the reason shows in the box, with a way to load the newest version when someone else changed it.
  const submit = async (scope, request_, done) => {
    const box = scope.querySelector('.banner');
    const buttons = [...scope.querySelectorAll('button')];
    buttons.forEach(b => { b.disabled = true; });
    const r = await request_();
    buttons.forEach(b => { b.disabled = false; });
    if (r.ok) { box.hidden = true; await done(r.data); }
    else formError(box, r, () => reload());
  };
  const reload = async () => {
    if (!selected) return;
    act = null;
    const r = await api('GET', `/api/node/${encodeURIComponent(selected.id)}`);
    if (r.ok && selected) { apply(selected, r.data); renderPanel(); }
  };
  // What the picture and the outline show of a node, after it was changed.
  const apply = (n, data) => {
    detail = data;
    n.title = data.label;
    n.untitled = !data.title;
    n.snippet = snippetOf(data.content);
    n.full = data.content;
    n.words = null;
    if (!n.children || !n.children.length) { const day = (data.occurred || '').slice(0, 10); n.from = day; n.to = day; }
    crumbKey = null;
    renderOutline();
    miniDirty = true;
    request();
  };
  const saveChange = async fields => {
    const r = await api('POST', `/api/node/${encodeURIComponent(selected.id)}`, {version: detail.version, ...fields});
    if (r.ok) { act = null; editTitle = false; }
    return r;
  };
  const needTitle = form => formError(form.querySelector('.banner'), {data: {error: '标题必填：给它起一个 16 个字以内的名称'}});

  const editForm = d => {
    const title = h('input', {type: 'text', id: 'f-title', value: d.title, maxlength: '40', required: true, placeholder: '16 个字以内的名称', autocomplete: 'off'});
    const content = h('textarea', {id: 'f-content', rows: '7', text: d.content});
    const tags = h('input', {type: 'text', id: 'f-tags', value: d.tags.join(', '), autocomplete: 'off'});
    const occurred = h('input', {type: 'text', id: 'f-occurred', value: d.occurred, placeholder: '2024-09-12，或只写 2024-09、2024', autocomplete: 'off'});
    const form = h('form', {class: 'pane-form', 'aria-label': '编辑这个节点'},
      h('h3', {text: '编辑'}), errorBox(),
      field('标题', title, '一个名称，不是一句话；结论写在内容里', true),
      field('内容', content), field('标签（用逗号分开）', tags), field('发生时间', occurred),
      h('div', {class: 'form-actions'}, h('span', {class: 'hint', text: 'Ctrl+Enter 保存'}), cancelButton(), h('button', {type: 'submit', class: 'btn btn-primary', text: '保存'})));
    form.addEventListener('submit', event => {
      event.preventDefault();
      if (!title.value.trim()) { needTitle(form); return; }
      submit(form, () => saveChange({title: title.value, content: content.value, tags: parseTags(tags.value), occurred: occurred.value}),
        async data => { apply(selected, data); renderPanel(); toast('已保存'); });
    });
    ctrlEnter(content, form);
    return form;
  };
  const childForm = d => {
    const title = h('input', {type: 'text', id: 'f-title', maxlength: '40', required: true, placeholder: '16 个字以内的名称', autocomplete: 'off'});
    const content = h('textarea', {id: 'f-content', rows: '6', placeholder: '写下这一条记忆'});
    const tags = h('input', {type: 'text', id: 'f-tags', autocomplete: 'off'});
    const form = h('form', {class: 'pane-form', 'aria-label': '新建子节点'},
      h('h3', {text: `在「${d.label}」下面新建`}), errorBox(),
      field('标题', title, '一个名称，不是一句话；结论写在内容里', true), field('内容', content), field('标签（用逗号分开）', tags),
      h('div', {class: 'form-actions'}, h('span', {class: 'hint', text: 'Ctrl+Enter 保存'}), cancelButton(), h('button', {type: 'submit', class: 'btn btn-primary', text: '新建'})));
    form.addEventListener('submit', event => {
      event.preventDefault();
      if (!title.value.trim()) { needTitle(form); return; }
      if (!content.value.trim()) { formError(form.querySelector('.banner'), {data: {error: '内容不能是空的'}}); return; }
      submit(form, () => api('POST', '/api/node', {parent: d.id, title: title.value, content: content.value, tags: parseTags(tags.value)}),
        async data => { act = null; await rebuild(data.id); toast('已新建，它已经长在树里了'); });
    });
    ctrlEnter(content, form);
    return form;
  };
  // Move and delete ask in a dialog, the same one the node page uses: the dialog stays open and says why when the change fails.
  const moveDialog = d => {
    let chosen = null;
    const q = h('input', {type: 'text', id: 'f-move', placeholder: '输入几个字，找一条汇总或节点', autocomplete: 'off', 'aria-label': '搜索新的上级'});
    const list = h('ul', {class: 'menu', hidden: true});
    const picked = h('p', {class: 'hint', role: 'status'});
    const run = async parent => {
      const r = await api('POST', `/api/node/${encodeURIComponent(d.id)}/move`, {parent});
      if (!r.ok) return r.data.error || '没有移成功';
      await rebuild(d.id);
      toast(parent ? '已移动' : '已移到最上层');
      return '';
    };
    let serial = 0, timer = 0;
    const find = async () => {
      const text = q.value.trim();
      const mine = ++serial;
      if (!text) { list.hidden = true; return; }
      const r = await api('GET', `/api/search?k=8&q=${encodeURIComponent(text)}`);
      if (mine !== serial) return;
      const rows = r.ok ? r.data.hits.filter(hit => hit.id !== d.id) : [];
      list.replaceChildren(...(rows.length ? rows.map(hit => h('li', {}, h('button', {type: 'button', class: 'pick', onclick: () => {
        chosen = hit.id;
        picked.textContent = `已选：${hit.title}`;
        picked.className = 'hint is-ok';
        list.hidden = true;
      }}, badge(hit.layer), h('span', {class: 'node-text', text: hit.title})))) : [h('li', {class: 'pick-note', text: r.ok ? '没有找到，换几个字试试' : r.data.error})]));
      list.hidden = false;
    };
    q.addEventListener('input', () => { clearTimeout(timer); timer = setTimeout(find, 250); });
    q.addEventListener('keydown', event => { if (event.key === 'Enter') { event.preventDefault(); clearTimeout(timer); find(); } });
    window.rp.ask({
      title: `移动「${d.label}」`,
      text: `一个节点只有一个位置：放在哪一条汇总下面。它下面的节点会跟着一起搬走。${d.parent ? `现在在「${d.path[d.path.length - 1].title}」下面。` : '现在是顶层节点。'}`,
      ok: '移到这里',
      fields: h('div', {class: 'stack'}, field('新的上级', q), list, picked),
      check: () => (chosen ? run(chosen) : '先搜索并选一个新的上级'),
      extra: d.parent ? [{text: '移到最上层', check: () => run(null)}] : [],
    });
  };
  const deleteDialog = d => {
    const reason = h('input', {type: 'text', id: 'f-reason', value: '不再需要了', maxlength: '200', autocomplete: 'off', required: true});
    const where = d.parent ? `「${d.path[d.path.length - 1].title}」` : '最上层';
    window.rp.ask({
      title: `删除「${d.label}」？`,
      text: `删除后会进回收站，30 天内可恢复。${d.children.length ? `它下面的 ${d.children.length} 个节点不会跟着删，会上移一层，挂到${where}下面。` : ''}`,
      ok: '删除',
      danger: true,
      fields: field('为什么删除', reason),
      check: async () => {
        if (!reason.value.trim()) return '要写明为什么删除';
        const r = await api('POST', `/api/node/${encodeURIComponent(d.id)}/delete`, {version: d.version, reason: reason.value});
        if (!r.ok) return r.data.error || '没有删除成功';
        await rebuild(r.data.parent);
        toast(r.data.moved ? `已移到回收站，它下面的 ${r.data.moved} 个节点上移了一层` : '已移到回收站');
        return '';
      },
    });
  };
  const open_ = name => {
    if (!selected || !detail) return;
    if (name === 'move') { moveDialog(detail); return; }
    if (name === 'delete') { deleteDialog(detail); return; }
    act = act === name ? null : name;
    renderPanel();
    const first = panel.querySelector('.pane-form input:not([type=hidden]), .pane-form textarea');
    if (first) { first.focus(); first.scrollIntoView({block: 'nearest'}); }
  };
  const focusPanel = () => panel.focus({preventScroll: true});

  const relItem = (b, lead) => h('button', {type: 'button', class: 'rel-item', onclick: () => goTo(b.id)}, badge(b.layer), h('span', {class: 'rel-title', text: b.title || '（没有文字）'}), lead && h('span', {class: 'rel-lead', text: lead}));
  const section = (key, title, count, ...body) => {
    const node = h('details', {class: 'fold is-flat pane-fold', open: fold[key]}, h('summary', {}, title, count !== undefined && h('span', {class: 'count', text: count})), h('div', {class: 'pane-fold-body'}, ...body));
    node.addEventListener('toggle', () => { fold[key] = node.open; });
    return node;
  };
  const relations = (n, d) => {
    const parts = [];
    const path = h('nav', {class: 'pane-path', 'aria-label': '它在树里的位置'});
    d.path.forEach((p, i) => { if (i) path.append(h('span', {class: 'sep', text: '›'})); path.append(h('button', {type: 'button', title: p.title, text: p.title, onclick: () => goTo(p.id)})); });
    if (d.path.length) path.append(h('span', {class: 'sep', text: '›'}));
    path.append(h('span', {class: 'here', text: d.label}));
    parts.push(h('div', {class: 'pane-sec'}, h('h3', {text: d.path.length ? '路径' : '位置'}), d.path.length ? path : h('p', {class: 'pane-note', text: d.layer === 0 && !d.children.length ? '还没汇总到任何一条汇总下面。' : '在最上层。'})));
    if (d.children.length) {
      const shown = d.children.slice(0, kidsShown);
      parts.push(h('div', {class: 'pane-sec'}, h('h3', {text: `子节点 · ${d.children.length} 条${d.descendants > d.children.length ? `，下面一共 ${d.descendants} 条` : ''}`}),
        h('div', {class: 'rel-list'}, shown.map(c => relItem(c, c.snippet))),
        d.children.length > shown.length && h('button', {type: 'button', class: 'btn btn-sm btn-ghost rel-more', text: `再显示 ${Math.min(10, d.children.length - shown.length)} 条`, onclick: () => { kidsShown += 10; renderPanel(); }})));
    }
    const s = d.siblings;
    if (s && s.count > 1) {
      const prev = s.before[s.before.length - 1], next = s.after[0];
      parts.push(h('div', {class: 'pane-sec'}, h('h3', {text: d.parent ? '同级' : '最上层的同级'}),
        h('div', {class: 'sib'}, h('span', {text: `共 ${s.count} 条，这是第 ${s.index + 1} 条`}),
          h('button', {type: 'button', class: 'btn btn-sm', disabled: !prev, title: prev ? prev.title : '', onclick: () => prev && goTo(prev.id)}, icon('left'), '上一个'),
          h('button', {type: 'button', class: 'btn btn-sm', disabled: !next, title: next ? next.title : '', onclick: () => next && goTo(next.id)}, '下一个', icon('right')))));
    }
    if (d.refs.length) parts.push(h('div', {class: 'pane-sec'}, h('h3', {text: '它引用的'}), h('div', {class: 'rel-list'}, d.refs.map(r => relItem(r)))));
    if (d.referenced_by.length) parts.push(h('div', {class: 'pane-sec'}, h('h3', {text: '引用了它的'}), h('div', {class: 'rel-list'}, d.referenced_by.map(r => relItem(r)))));
    return section('relations', '关系', undefined, ...parts);
  };
  const attachments = d => h('div', {class: 'pane-sec'}, h('h3', {text: `附件 · ${d.attachments.length}`}), h('ul', {class: 'pane-files'}, d.attachments.map(a => {
    if (!a.available) return h('li', {class: 'pane-file'}, icon('alert'), h('span', {text: `${a.name}（文件找不到了）`}));
    if (a.referenced) return h('li', {class: 'pane-file'}, icon('file'), h('span', {text: `${a.name}（只记了位置：${a.path}）`}));
    return h('li', {}, a.kind === 'image' && h('a', {href: `/file/${a.hash}`, target: '_blank', rel: 'noopener'}, h('img', {src: `/file/${a.hash}`, alt: a.caption || a.name, loading: 'lazy'})),
      h('div', {class: 'pane-file'}, icon(a.kind === 'image' ? 'image' : 'file'), h('a', {href: `/file/${a.hash}`, target: '_blank', rel: 'noopener', text: a.name}), h('span', {class: 'muted', text: a.size_text})));
  })));
  const content = (n, d) => {
    const body = [h('p', {class: 'pane-text', text: d ? d.content : n.snippet})];
    if (d && d.tags.length) body.push(h('span', {class: 'chips'}, d.tags.map(tag => h('button', {type: 'button', class: 'chip chip-btn', title: '在树里高亮同样标签的卡片', text: tag, onclick: () => tagSearch(tag)}))));
    if (d && d.sources.length) body.push(h('div', {class: 'pane-sec'}, h('h3', {text: '来源'}), h('ul', {class: 'sources'}, d.sources.map(s => h('li', {text: s})))));
    if (d && d.attachments.length) body.push(attachments(d));
    return h('div', {class: 'pane-sec'}, ...body);
  };
  const history = d => section('history', '历史', ` ${d.history.length} 个版本`, h('ol', {class: 'pane-hist'}, d.history.map(r => h('li', {},
    h('div', {class: 'rev-head'}, h('span', {class: `op op-${r.op}`, text: r.label}), h('b', {text: `第 ${r.version} 版`}), h('span', {class: 'muted', text: `${r.who} · ${fmtTime(r.at)}`})),
    r.reason && h('p', {class: 'reason', text: r.reason}),
    r.title_changed && h('p', {class: 'diff'}, h('span', {class: 'muted', text: '标题：'}), r.old_title && h('del', {text: r.old_title}), r.title ? h('ins', {text: r.title}) : h('span', {class: 'muted', text: '（去掉了标题）'})),
    r.removed !== null && r.removed !== undefined
      ? h('p', {class: 'diff'}, h('span', {class: 'muted', text: '改了：'}), r.removed && h('del', {text: r.removed}), r.added && h('ins', {text: r.added}))
      : (r.same && !r.title_changed && h('p', {class: 'muted', text: '文字没有变化，改的是标签、日期、引用或附件。'}))))));

  const startTitleEdit = () => {
    if (!detail || editTitle) return;
    editTitle = true;
    renderPanel();
    const input = panel.querySelector('.pane-title-edit input');
    if (input) { input.focus(); input.select(); }
  };
  const titleBlock = (n, d) => {
    if (editTitle && d) {
      const input = h('input', {type: 'text', value: d.title, maxlength: '40', required: true, 'aria-label': '标题', autocomplete: 'off', placeholder: '16 个字以内的名称'});
      const cancel = () => { editTitle = false; renderPanel(); focusPanel(); };
      const form = h('form', {class: 'inline-form pane-title-edit'}, input,
        h('button', {type: 'button', class: 'btn btn-sm', text: '取消', onclick: cancel}),
        h('button', {type: 'submit', class: 'btn btn-primary btn-sm', text: '保存'}));
      const wrap = h('div', {class: 'pane-title-box'}, form, errorBox());
      form.addEventListener('submit', event => {
        event.preventDefault();
        if (!input.value.trim()) { formError(wrap.querySelector('.banner'), {data: {error: '标题必填：给它起一个 16 个字以内的名称'}}); return; }
        submit(wrap, () => saveChange({title: input.value}), async data => { apply(selected, data); renderPanel(); toast('标题已保存'); });
      });
      input.addEventListener('keydown', event => { if (event.key === 'Escape') { event.stopPropagation(); cancel(); } });
      return [wrap];
    }
    const label = d ? d.label : n.title;
    return [h('button', {type: 'button', class: `inline-edit pane-title${d && !d.title ? ' is-empty' : ''}`, title: d ? '点一下改标题' : '', text: label || '（没有文字）', onclick: startTitleEdit})];
  };

  const actionButton = (name, label, ic, on, extra = {}) => h('button', {type: 'button', class: 'btn btn-sm btn-ghost', disabled: !detail, 'aria-pressed': act === name ? 'true' : false, onclick: on, ...extra}, icon(ic), label);
  const paneHead = (n, d) => {
    const pin = h('button', {type: 'button', class: 'btn btn-ghost btn-icon btn-sm', 'aria-pressed': String(detailPinned), title: '固定面板：点空白处不会收起', 'aria-label': '固定详情面板', onclick: () => setDetailPinned(!detailPinned)}, icon('pin'));
    return h('div', {class: 'pane-head', id: 'pane-head'},
      h('div', {class: 'pane-top glass-head'}, badge(n.layer), h('span', {class: 'grow', text: d ? `第 ${d.version} 版${d.occurred ? ` · 发生于 ${d.occurred}` : ''}` : ''}),
        h('button', {type: 'button', class: 'btn btn-ghost btn-icon btn-sm', title: '放大到这张卡片（Enter）', 'aria-label': '放大到这张卡片', 'aria-keyshortcuts': 'Enter', onclick: () => zoomTo(n)}, icon('zoom-in')),
        h('a', {class: 'btn btn-ghost btn-icon btn-sm pane-open', href: `/node/${encodeURIComponent(n.id)}`, title: '打开节点页（有完整的层级关系和历史）', 'aria-label': '打开节点页'}, icon('external')),
        pin,
        h('button', {type: 'button', class: 'btn btn-ghost btn-icon btn-sm', title: '关闭（Esc）', 'aria-label': '关闭详情面板', onclick: () => { closePanel(); canvas.focus(); }}, icon('x'))),
      ...titleBlock(n, d),
      h('div', {class: 'pane-acts'},
        actionButton('edit', '编辑', 'edit', () => open_('edit'), {'aria-keyshortcuts': 'E'}),
        actionButton('child', '新建子节点', 'plus', () => open_('child'), {'aria-keyshortcuts': 'N'}),
        actionButton('move', '移动', 'move', () => open_('move')),
        actionButton('delete', '删除', 'trash', () => open_('delete'))));
  };
  const renderPanel = () => {
    const keep = panel.querySelector('.pane-scroll')?.scrollTop || 0;
    if (!selected) {
      panel.replaceChildren(h('div', {class: 'pane-scroll'}, h('div', {class: 'pane-empty'}, h('b', {text: '详情面板'}), h('p', {text: '点一个卡片，就能在这里读全文、编辑、新建子节点、移动、删除，看它的上下级和历史。'}),
        h('p', {}, h('kbd', {text: 'E'}), ' 编辑　', h('kbd', {text: 'N'}), ' 新建子节点　', h('kbd', {text: 'Esc'}), ' 关闭'))));
      return;
    }
    const n = selected, d = detail && detail.id === n.id ? detail : null;
    const form = d && act ? { edit: editForm, child: childForm }[act](d) : null;
    const meta = d ? h('div', {class: 'pane-meta'},
      d.descendants ? h('span', {text: `下面共 ${d.descendants} 条`}) : null,
      h('span', {text: `记录于 ${fmtTime(d.created)}`}), h('span', {text: `更新于 ${fmtTime(d.updated)}`})) : h('div', {class: 'pane-meta'}, n.size > 1 && h('span', {text: `下面共 ${n.size - 1} 条`}), span(n) && h('span', {text: span(n)}), h('span', {class: 'loading'}, h('span', {class: 'spinner', 'aria-hidden': 'true'}), '正在取详细内容……'));
    panel.replaceChildren(paneHead(n, d), h('div', {class: 'pane-scroll'}, form, meta, content(n, d), d && relations(n, d), d && history(d)));
    const body = panel.querySelector('.pane-scroll');
    if (body) body.scrollTop = keep;
  };
  const closePanel = () => {
    selected = null;
    detail = null;
    act = null;
    editTitle = false;
    selectSerial++;
    setPanel(false);
    syncOutline(null);
    request();
  };
  // Another node, or none. With the panel pinned it stays open and says what it is for.
  const select = (n, {fly: doFly = false, focusPanel: moveFocus = false} = {}) => {
    if (n.loose) { zoomTo(n); return; }
    selected = n;
    detail = null;
    act = null;
    editTitle = false;
    kidsShown = 6;
    setPanel(true);
    renderPanel();
    const mine = ++selectSerial;
    api('GET', `/api/node/${encodeURIComponent(n.id)}`).then(r => {
      if (mine !== selectSerial) return;
      if (r.ok) { detail = r.data; if (r.data.label !== n.title) { n.title = r.data.label; crumbKey = null; } }
      renderPanel();
    });
    syncOutline(n);
    if (doFly) flyTo(n); else keepInSight(n);
    if (moveFocus) focusPanel();
    request();
  };
  const deselect = () => {
    if (!selected) return;
    if (detailPinned) { selected = null; detail = null; act = null; selectSerial++; renderPanel(); syncOutline(null); request(); }
    else closePanel();
  };
  const setDetailPinned = on => {
    detailPinned = on;
    store.set('rp-tree-detail-pin', on ? '1' : '0');
    renderPanel();
  };
  // The panel can be dragged away: to the right on a wide screen, down on a phone.
  let paneDrag = null;
  panel.addEventListener('pointerdown', event => {
    const head = event.target.closest('.pane-head');
    if (!head || event.target.closest('button, a, input, form') || event.button > 0) return;
    paneDrag = {x: event.clientX, y: event.clientY, id: event.pointerId};
    head.setPointerCapture(event.pointerId);
    head.classList.add('is-dragging');
  });
  panel.addEventListener('pointermove', event => {
    if (!paneDrag || paneDrag.id !== event.pointerId) return;
    const d = phone() ? event.clientY - paneDrag.y : event.clientX - paneDrag.x;
    panel.style.transition = 'none';
    panel.style.transform = `translate${phone() ? 'Y' : 'X'}(${Math.max(0, d)}px)`;
    paneDrag.d = d;
  });
  const dropPanel = () => {
    if (!paneDrag) return;
    const far = (paneDrag.d || 0) > 90;
    paneDrag = null;
    panel.style.transition = '';
    panel.style.transform = '';
    panel.querySelector('.pane-head')?.classList.remove('is-dragging');
    if (far) { closePanel(); canvas.focus(); }
  };
  panel.addEventListener('pointerup', dropPanel);
  panel.addEventListener('pointercancel', dropPanel);
  panel.addEventListener('keydown', event => {
    if (event.key !== 'Escape') return;
    event.stopPropagation();
    if (act) { act = null; renderPanel(); focusPanel(); return; }
    closePanel();
    canvas.focus();
  });

  // After the tree changed shape (a node written, moved or deleted): fetch it again and go to a node.
  const rebuild = async id => {
    const r = await api('GET', '/api/outline?depth=3');
    if (!r.ok) { toast('树没能刷新，请刷新页面', true); return; }
    markHits([]);
    hovered = null;
    closePanel();
    buildTree(r.data);
    resetLayouts();
    calcFit();
    views.length = 0;
    crumbKey = null;
    buildTopList();
    buildLegend();
    renderOutline();
    if (id) await goTo(id);
    else {
      if (detailPinned) { setPanel(true); renderPanel(); }
      if (atFit) fitAll(false);
    }
    request();
  };

  /* ---------- the tooltip ---------- */
  const tip = $('tree-tip');
  const showTip = (it, x, y) => {
    if (!it) { tip.hidden = true; return; }
    if (it.group) {
      tip.replaceChildren(h('strong', {class: 'tip-title', text: `其他 ${it.count} 条`}), h('span', {class: 'tip-meta', text: '太小放不下的兄弟，点一下展开'}));
    } else if (it.loose) {
      tip.replaceChildren(h('strong', {class: 'tip-title', text: it.title}), h('span', {class: 'tip-snip', text: '这些记忆还没有汇总到任何一条汇总下面。点一下放大到这里。'}));
    } else {
      const bits = [];
      if (it.size > 1) bits.push(`下面共 ${it.size - 1} 条`);
      if (span(it)) bits.push(span(it));
      const rest = restOf(it, it.snippet);
      tip.replaceChildren(h('span', {class: 'tip-head'}, badge(it.layer)), h('strong', {class: 'tip-title', text: it.title}), rest && h('span', {class: 'tip-snip', text: rest}), bits.length && h('span', {class: 'tip-meta', text: bits.join(' · ')}));
    }
    tip.hidden = false;
    const w = tip.offsetWidth, hh = tip.offsetHeight;
    tip.style.transform = `translate(${Math.max(8, Math.min(x + 14, vw - w - 8))}px, ${Math.max(8, Math.min(y + 16, vh - hh - 8))}px)`;
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
      const kk = clamp(view.k * factor, kFit * MIN_RATIO, kFit * MAX_RATIO);
      const wx = view.cx + (mx - vw / 2) / view.k, wy = view.cy + (my - vh / 2) / view.k;
      view.k = kk;
      view.cx = wx - (mx - vw / 2) / kk;
      view.cy = wy - (my - vh / 2) / kk;
      clampView(view);
      Object.assign(goal, view);
      atFit = false;
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
        atFit = false;
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
    if (it.group || it.loose) { zoomTo(it); return; }
    if (!twice || selected !== it) select(it, {focusPanel: !twice});
    if (twice) zoomTo(it);
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

  // The keys work wherever the focus is on this page, except while someone is typing, and the keys that move the
  // picture or press a card only when the focus is on the picture.
  const typing = target => target.closest('input, textarea, select, [contenteditable]');
  doc.addEventListener('keydown', event => {
    if (event.ctrlKey || event.metaKey || event.altKey || event.defaultPrevented) return;
    const target = event.target;
    if (typing(target) || doc.querySelector('dialog[open]')) return;   // a dialog owns the keys while it is open
    const onPicture = target === canvas || target === doc.body;
    const pan = (dx, dy) => { stopMotion(); go({cx: goal.cx + (dx * vw * 0.15) / goal.k, cy: goal.cy + (dy * vh * 0.15) / goal.k, k: goal.k}); };
    switch (event.key) {
      case '/': searchBox.focus(); searchBox.select(); break;
      case 'f': case 'F': setImmersive(!app.classList.contains('is-immersive')); break;
      case 'o': case 'O': setOutline(!outlineOpen()); break;
      case 'e': case 'E': if (selected) open_('edit'); else return; break;
      case 'n': case 'N': if (selected) open_('child'); else return; break;
      case '+': case '=': stopMotion(); zoomBy(1.5); break;
      case '-': case '_': stopMotion(); zoomBy(1 / 1.5); break;
      case '0': fitAll(); break;
      case 'Home': if (!onPicture) return; fitAll(); break;
      case 'ArrowLeft': if (!onPicture) return; pan(-1, 0); break;
      case 'ArrowRight': if (!onPicture) return; pan(1, 0); break;
      case 'ArrowUp': if (!onPicture) return; pan(0, -1); break;
      case 'ArrowDown': if (!onPicture) return; pan(0, 1); break;
      case 'Enter': if (!onPicture || !selected) return; zoomTo(selected); break;
      case 'Escape':
        if (!hitList.hidden) hitList.hidden = true;
        else if (goBack()) break;
        else if (!atFit) fitAll(true, false);
        else if (selected || panelOpen()) { closePanel(); canvas.focus(); }
        else if (app.classList.contains('is-immersive')) setImmersive(false);
        else if (outlineOpen()) setOutline(false);
        else return;
        break;
      default: return;
    }
    event.preventDefault();
  });
  $('tree-in').addEventListener('click', () => { stopMotion(); zoomBy(1.6); });
  $('tree-out').addEventListener('click', () => { stopMotion(); zoomBy(1 / 1.6); });
  $('tree-fit').addEventListener('click', () => fitAll());
  /* ---------- immersive: only the picture (the page covers the whole window; Esc has to reach the page, so the
     browser's own full screen is not used) ---------- */
  const immerseBtn = $('tree-immerse');
  function setImmersive(on) {
    app.classList.toggle('is-immersive', on);
    immerseBtn.setAttribute('aria-pressed', String(on));
    tip.hidden = true;
    hitList.hidden = true;
    if (on) canvas.focus({preventScroll: true});
    request();
  }
  immerseBtn.addEventListener('click', () => setImmersive(!app.classList.contains('is-immersive')));

  /* ---------- search ---------- */
  const hitList = $('tree-hits');
  const searchBox = $('tree-q');
  const markHits = nodes => {
    for (const n of hitNodes) n.isHit = false;
    hitNodes = nodes;
    for (const n of nodes) n.isHit = true;
    renderOutline();
    request();
  };
  // A hit may sit in a part of the tree that is not loaded yet: load down the path to it.
  const nodeFor = hit => ensure([...hit.path.map(p => p.id).reverse(), hit.id]);
  const say = text => { $('tree-status').textContent = text; statusText = ''; };
  let searchSerial = 0;
  // A query that starts with # looks for a tag: the cards that carry it.
  const search = async () => {
    const raw = searchBox.value.trim();
    if (!raw) { markHits([]); hitList.hidden = true; return; }
    const tag = raw.startsWith('#') ? raw.replace(/^#+/, '').trim() : '';
    const q = tag || raw;
    if (!q) return;
    const mine = ++searchSerial;
    say('正在搜索……');
    const r = await api('GET', `/api/search?k=${tag ? 50 : 8}&q=${encodeURIComponent(q)}${tag ? `&tag=${encodeURIComponent(tag)}` : ''}`);
    if (mine !== searchSerial) return;
    if (!r.ok) { say(r.data.error || '搜索失败'); return; }
    const found = await Promise.all(r.data.hits.map(async hit => ({hit, n: await nodeFor(hit)})));
    if (mine !== searchSerial) return;
    const hits = found.filter(f => f.n);
    markHits(hits.map(f => f.n));
    if (!hits.length) { hitList.hidden = true; say(tag ? `没有带标签「${tag}」的节点` : '没有找到'); return; }
    hitList.replaceChildren(...hits.slice(0, 12).map(({hit, n}) => h('li', {}, h('button', {type: 'button', class: 'pick', onclick: () => { hitList.hidden = true; select(n, {fly: true, focusPanel: true}); }},
      badge(hit.layer), h('span', {class: 'tree-hit-text'}, h('span', {class: 'node-text', text: hit.title}), hit.path.length && h('span', {class: 'tree-hit-path', text: hit.path.map(p => p.title).reverse().join(' › ')}))))));
    hitList.hidden = false;
    say(`找到 ${hits.length} 条${r.data.no_strong_match && !tag ? '（没有明确匹配，只是线索）' : ''}${tag ? `带标签「${tag}」的节点，已高亮` : '，已飞到第一条'}`);
    if (!tag) { select(hits[0].n, {fly: true}); if (phone()) hitList.hidden = true; }
    else if (!atFit) fitAll();
  };
  $('tree-search').addEventListener('submit', event => { event.preventDefault(); search(); });
  const clearSearch = () => { searchSerial++; markHits([]); hitList.hidden = true; };
  searchBox.addEventListener('keydown', event => {
    if (event.key === 'Escape') { event.stopPropagation(); searchBox.value = ''; clearSearch(); canvas.focus(); }
    else if (event.key === 'ArrowDown') { hitList.querySelector('.pick')?.focus(); event.preventDefault(); }
  });
  hitList.addEventListener('keydown', event => {
    const items = [...hitList.querySelectorAll('.pick')];
    const at = items.indexOf(doc.activeElement);
    if (event.key === 'ArrowDown') { items[Math.min(at + 1, items.length - 1)]?.focus(); event.preventDefault(); }
    else if (event.key === 'ArrowUp') { (at > 0 ? items[at - 1] : searchBox).focus(); event.preventDefault(); }
    else if (event.key === 'Escape') { hitList.hidden = true; searchBox.focus(); event.stopPropagation(); }
  });
  // the list of hits goes away as soon as a click lands anywhere else
  doc.addEventListener('pointerdown', event => { if (!hitList.hidden && !event.target.closest('#tree-hits, #tree-search')) hitList.hidden = true; });
  searchBox.addEventListener('focus', () => { if (hitList.children.length && hitNodes.length) hitList.hidden = false; });
  searchBox.addEventListener('input', () => { if (!searchBox.value.trim()) clearSearch(); });

  /* ---------- the outline: a real tree, with the loose memories apart at the bottom ---------- */
  const ol = $('tree-ol');
  const olToggle = $('tree-ol-toggle');
  const olOpen = new Set();       // ids of expanded rows
  const olMore = new Map();       // id -> how many of its children are listed
  const olRows = new Map();       // id -> the row's element
  let olSel = null;
  let olDirty = true;
  const hasKids = n => n.children === null || (n.children && n.children.length > 0);

  const buildRows = (parentEl, list, level, owner) => {
    const shown = olMore.get(owner.id) || OL_PAGE;
    for (const n of list.slice(0, shown)) parentEl.append(buildRow(n, level));
    if (list.length > shown) {
      parentEl.append(h('li', {class: 'more', role: 'treeitem', tabindex: '-1', text: `还有 ${list.length - shown} 条，点这里再显示一些`, 'data-more': owner.id}));
    }
  };
  const buildRow = (n, level) => {
    const open = olOpen.has(n.id);
    const kids = hasKids(n);
    const row = h('div', {class: 'ol-row', title: n.title},
      h('span', {class: kids ? 'tog' : 'tog none', 'aria-hidden': 'true'}));
    if (!n.loose && n.layer > 0) {
      const dot = h('span', {class: 'dot'});
      dot.style.background = pal[n.topic].bar;
      row.append(dot);
    }
    row.append(h('span', {class: 't', text: n.title}));
    if (kids && n.size > 1 && !n.loose) row.append(h('span', {class: 'n', text: String(n.size - 1)}));
    const li = h('li', {role: 'treeitem', 'aria-level': String(level), tabindex: '-1', 'data-id': n.id,
      'aria-expanded': kids ? String(open) : false, 'aria-selected': String(n.id === olSel), class: `${n.loose ? 'ol-loose' : ''}${n.isHit ? ' is-hit' : ''}`.trim() || false}, row);
    olRows.set(n.id, li);
    if (open && kids) {
      const group = h('ul', {role: 'group'});
      if (n.children === null) group.append(h('li', {class: 'more', role: 'none', text: n.failed ? '没取到，稍后再试' : '正在取……'}));
      else buildRows(group, n.children, level + 1, n);
      li.append(group);
    }
    return li;
  };
  function renderOutline() {
    if (!root || !outlineOpen()) { olDirty = true; return; }
    olDirty = false;
    const active = doc.activeElement && ol.contains(doc.activeElement) ? doc.activeElement.dataset.id || doc.activeElement.dataset.more : null;
    olRows.clear();
    const frag = doc.createDocumentFragment();
    buildRows(frag, root.children, 1, root);
    if (root.gathered) {
      frag.append(buildRow(root.gathered, 1));
    }
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
    if (row && outlineOpen()) row.scrollIntoView({block: 'nearest'});
  }
  const toggleRow = n => {
    if (olOpen.has(n.id)) olOpen.delete(n.id);
    else { olOpen.add(n.id); if (n.children === null) loadChildren(n); }
    renderOutline();
  };
  const activateRow = n => {
    if (n.loose) { toggleRow(n); zoomTo(n); return; }
    select(n, {fly: true});
    if (innerWidth < SIDE_BREAKPOINT) setOutline(false);
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
      case 'Escape': setOutline(false); canvas.focus(); break;
      default: return;
    }
    event.preventDefault();
    event.stopPropagation();
  });
  // Open and close: the handle on the left edge and the button in the tools are the only things that do it, and
  // nothing else opens or closes with it. Whether it is open is remembered.
  const handle = $('tree-handle');
  function setOutline(open, remember = true) {
    side.classList.toggle('is-open', open);
    app.classList.toggle('has-outline', open);
    for (const button of [olToggle, handle]) button.setAttribute('aria-pressed', String(open));
    if (open) side.removeAttribute('inert'); else side.setAttribute('inert', '');
    if (open && olDirty) renderOutline();
    if (open) { const row = olRows.get(olSel); if (row) row.scrollIntoView({block: 'nearest'}); }
    if (remember) store.set('rp-tree-ol', open ? '1' : '0');
    refit();
  }
  for (const button of [olToggle, handle]) button.addEventListener('click', () => setOutline(!outlineOpen()));
  $('tree-side-close').addEventListener('click', () => { setOutline(false); olToggle.focus(); });

  /* ---------- legend and a text version of the top layer ---------- */
  const buildLegend = () => {
    const legend = $('tree-legend');
    legend.replaceChildren();
    const dots = h('span', {class: 'swatches', 'aria-hidden': 'true'});
    for (let t = 0; t < Math.min(topicCount, HUES.length); t++) {
      const dot = h('span', {class: 'sw'});
      dot.style.background = pal[t].bar;
      dots.append(dot);
    }
    legend.append(h('li', {}, dots, h('span', {text: '颜色 = 话题'})));
    const sw = h('span', {class: 'swatches', 'aria-hidden': 'true'});
    for (const i of [0, 2, 4]) {
      const box = h('span', {class: 'sw box'});
      box.style.background = pal[0].fill[i];
      box.style.borderColor = pal[0].edge[i];
      sw.append(box);
    }
    legend.append(h('li', {}, sw, h('span', {text: '亮度 = 层'}), h('span', {class: 'opt', text: '（越往上越深）'})));
    if (root.gathered || [...byId.values()].some(n => n.topic === NEUTRAL && n.layer > 0)) {
      const box = h('span', {class: 'sw box'});
      box.style.background = pal[NEUTRAL].fill[0];
      box.style.borderColor = pal[NEUTRAL].edge[0];
      legend.append(h('li', {class: 'opt'}, box, h('span', {text: '灰 = 总览和未汇总的记忆'})));
    }
  };
  const buildTopList = () => {
    const list = $('tree-outline');
    list.replaceChildren();
    for (const n of root.children.slice(0, 40)) {
      list.append(h('li', {}, h('a', {href: `/node/${encodeURIComponent(n.id)}`, text: `第 ${n.layer} 层：${n.title}`})));
    }
  };
  const setLegend = open => {
    foot.classList.toggle('is-collapsed', !open);
    $('tree-foot-toggle').setAttribute('aria-expanded', String(open));
    store.set('rp-tree-legend', open ? '1' : '0');
  };
  $('tree-foot-toggle').addEventListener('click', () => setLegend(foot.classList.contains('is-collapsed')));

  /* ---------- size, theme, start ---------- */
  // The picture is fitted again when the window, the open outline or the open panel changes what is free.
  const refit = () => {
    if (!root || !loaded) return;
    calcFit();
    if (atFit) fitAll(true);
    else request();
  };
  const resize = () => {
    const rect = stage.getBoundingClientRect();
    if (!rect.width || !rect.height) return;
    vw = rect.width; vh = rect.height;
    dpr = window.devicePixelRatio || 1;
    canvas.width = Math.round(vw * dpr);
    canvas.height = Math.round(vh * dpr);
    bgKey = '';
    if (!root) return;
    const u = usable(false);
    const aspect = clamp(u.w / u.h, 0.7, 2.4);
    if (!loaded || Math.abs(aspect / layoutAspect - 1) > 0.3) {
      layoutAspect = aspect;
      WW = WORLD_H * aspect;
      calcFit();
      resetLayouts();
      views.length = 0;
      fitAll(false);
      loaded = true;
    }
    calcFit();
    if (atFit) fitAll(false);
    request();
  };
  const theme = () => { readColors(); tables.clear(); layGen++; miniBase = null; renderOutline(); buildLegend(); request(); };
  window.addEventListener('rp-theme', theme);
  matchMedia('(prefers-color-scheme: dark)').addEventListener('change', theme);
  reduced.addEventListener('change', request);

  const start = async () => {
    readColors();
    setLegend(store.get('rp-tree-legend') === null ? innerWidth >= 900 : store.get('rp-tree-legend') === '1');
    setPanel(false);
    setOutline(store.get('rp-tree-ol') === '1' && innerWidth >= SIDE_BREAKPOINT, false);
    const message = $('tree-msg');
    try {
      const response = await fetch('/api/outline?depth=3');
      if (!response.ok) throw new Error(`HTTP ${response.status}`);
      const list = await response.json();
      if (!list.length) { message.textContent = '树还是空的。'; return; }
      buildTree(list);
      const rect = stage.getBoundingClientRect();
      vw = rect.width; vh = rect.height;
      layoutAspect = clamp(usable(false).w / usable(false).h, 0.7, 2.4);
      WW = WORLD_H * layoutAspect;
      root.asp = WW / WH;
      buildLegend();
      resize();
      buildTopList();
      for (const n of root.children.slice(0, 60)) if (hasKids(n)) olOpen.add(n.id);
      renderOutline();
      if (detailPinned) { setPanel(true); renderPanel(); refit(); }
      message.hidden = true;
      new ResizeObserver(resize).observe(stage);
      request();
      const wanted = new URLSearchParams(location.search).get('focus');
      if (wanted) {
        const n = await nodeById(wanted);
        if (n) { select(n); zoomTo(n); }
        else toast('链接指向的节点已经不在树里了，这里是整棵树');
      }
    } catch (error) {
      message.replaceChildren(h('span', {text: `没能画出这棵树：${error.message}。刷新页面再试一次。`}));
    }
  };
  start();
})();
