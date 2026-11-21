// 关系图：整个页面就是一块画板。缩小看每个实体的「领地」，放大进去才一条条出现记忆；
// 搜索、详情、定位都在画板上完成，不跳转页面。
const root = document.querySelector('[data-graph]');
if (root) mountGraph(root);

function mountGraph(root) {
  const $ = selector => root.querySelector(selector);
  const canvas = $('[data-graph-canvas]'), message = $('[data-graph-message]'), status = $('[data-graph-status]');
  const form = $('[data-graph-form]'), searchBox = form.querySelector('[data-search]');
  const mini = $('[data-graph-mini]'), lanesCanvas = $('[data-lanes-canvas]');
  const crumbs = $('[data-graph-crumbs]');
  const coFilter = $('[data-co-filter]'), coRange = $('[data-co-range]'), coValue = $('[data-co-value]'), legend = $('[data-graph-groups]');
  const LOOSE = '~loose';
  const reduce = matchMedia('(prefers-reduced-motion: reduce)').matches;
  const {short, when, memoryTip, entityTip} = RPGraphUI;
  if (typeof window.cytoscape !== 'function') { message.classList.remove('is-loading'); message.textContent = '关系图组件未加载'; return; }
  // 图里的文字只会在空格处换行，中文要自己断行：汉字算一格，字母数字约半格。
  const wrapLabel = (text, units) => {
    const lines = []; let line = '', used = 0;
    for (const ch of Array.from(text)) {
      const width = /[\u2e80-\u9fff\uff00-\uffef]/.test(ch) ? 1 : ch === ' ' ? 0.35 : 0.58;
      if (used + width > units && line) { lines.push(line); line = ''; used = 0; }
      line += ch; used += width;
    }
    if (line) lines.push(line);
    return lines.join('\n');
  };

  const LAND_CAP = 200; // 服务端每页给多少条；更早的在「更早的 N 条」里分页载入
  const NEAR = 0.15; // 缩得比这更小时，只看领地（实体），不画记忆
  const FAR = 0.62; // 缩得比这更小时，卡片退化成醒目的圆点；放大到能读清再变回卡片
  const FIRST = 170, STEP = 105; // 记忆围着实体排成一圈圈，起点和间距
  const reach = count => FIRST + STEP * Math.sqrt(Math.max(0, count - 1));
  const GOLDEN = 2.39996;
  const maxNodes = 5000;
  const KEY = 'rp-map-v3';
  const BELT = co => Math.min(900, Math.round(co * 7)); // 两个实体共享的记忆越多，它们之间留给「桥」的带子越宽
  const coPairs = new Map(); // 两个实体同时出现在几条记忆里（总览数据里的 co）
  const pairKey = (a, b) => a < b ? `${a}|${b}` : `${b}|${a}`;
  const beltBetween = (a, b) => BELT(coPairs.get(pairKey(a, b)) || 0);

  const cardText = node => wrapLabel(short((node.data('old') ? '已被取代 · ' : '') + node.data('text'), 52), 13);
  const theme = () => {
    const css = getComputedStyle(document.documentElement);
    const v = name => css.getPropertyValue(name).trim();
    const font = 'system-ui, "Microsoft YaHei UI", sans-serif';
    const fade = reduce ? {} : {'transition-property': 'opacity', 'transition-duration': '180ms'};
    return [
      {selector: 'node', style: {width: 24, height: 24, 'background-color': v('--g-node'), 'border-width': 2, 'border-color': v('--g-halo'), label: 'data(label)', 'font-size': 13, 'font-family': font, 'text-valign': 'bottom', 'text-margin-y': 6, 'text-wrap': 'ellipsis', 'text-max-width': '152px', color: v('--g-label'), 'text-background-color': v('--g-label-bg'), 'text-background-opacity': 0.92, 'text-background-padding': 2, ...fade}},
      {selector: 'node.isolated', style: {'background-color': v('--g-isolated')}},
      {selector: 'node:selected', style: {'background-color': v('--g-anchor'), 'border-width': 5, 'border-color': v('--g-suggest'), 'z-index': 5}},
      {selector: 'edge', style: {width: 1.6, 'curve-style': 'unbundled-bezier', 'control-point-distances': 'data(bend)', 'control-point-weights': 0.5, 'line-color': v('--g-edge'), 'target-arrow-shape': 'triangle', 'target-arrow-color': v('--g-edge'), 'arrow-scale': 0.78, opacity: 0.7, ...fade}},
      {selector: 'node.entity', style: {shape: 'diamond', width: 'mapData(count,1,20,28,54)', height: 'mapData(count,1,20,28,54)', 'background-color': v('--g-suggest'), 'border-color': v('--g-halo'), 'font-weight': 600}},
      {selector: 'node.entity:selected', style: {'border-color': v('--g-anchor'), 'border-width': 5}},
      ...[0, 1, 2, 3, 4, 5].map(n => ({selector: `node.g${n}`, style: {'background-color': v(`--g-c${n}`)}})),
      {selector: 'edge.co', style: {'curve-style': 'bezier', width: 'mapData(co,1,8,1.6,6)', 'line-color': v('--g-suggest'), 'target-arrow-shape': 'none', opacity: 0.75}},
      {selector: 'edge.corrects', style: {'line-color': v('--warn'), 'target-arrow-color': v('--warn'), width: 2.4, opacity: 0.95}},
      {selector: 'edge.summarizes', style: {'line-style': 'dashed', 'line-dash-pattern': [8, 3]}},
      {selector: 'edge:selected', style: {width: 3, 'line-color': v('--g-anchor'), 'target-arrow-color': v('--g-anchor'), opacity: 1}},
      ...cardStyles(v),
      ...farStyles(v),
      ...bridgeStyles(v),
      ...mapStyles(v),
      {selector: 'node.gone,edge.gone,.carried', style: {display: 'none'}},
      {selector: 'edge.minor', style: {display: 'none'}},
      {selector: 'edge.minor.show', style: {display: 'element'}},
      {selector: 'node.dim', style: {opacity: 0.4}},
      {selector: 'edge.dim', style: {opacity: 0.15}},
      {selector: 'node.faded', style: {opacity: 0.5}},
      {selector: 'edge.faded', style: {opacity: 0.3}},
      {selector: 'node.miss', style: {opacity: 0.25}},
      {selector: 'edge.miss', style: {opacity: 0.12}},
      {selector: 'node.entering,edge.entering', style: {opacity: 0}},
      {selector: 'node.entity.territory.miss', style: {opacity: 0.35, 'background-opacity': 0.04}},
      {selector: 'node.entity.territory.has-hit', style: {'background-opacity': 0.34, 'border-width': 9, 'border-style': 'solid', 'border-color': v('--g-anchor'), 'z-index': 1}},
      {selector: 'node.mem.hit', style: {'border-color': v('--g-anchor'), 'border-width': 4, 'background-color': v('--accent-soft'), 'z-index': 6}},
      {selector: 'node.mem.far.hit', style: {width: 52, height: 52, 'background-color': v('--g-anchor'), 'border-color': v('--g-suggest'), 'border-width': 6, 'z-index': 6}},
      // 缩得很小时只看领地：记忆和引用线淡出（不用 display，才能做渐隐）。
      {selector: 'node.mem.t1,node.more-tab.t1', style: {opacity: 0, events: 'no'}},
      {selector: 'edge.ref.t1,edge.thread.t1', style: {opacity: 0, events: 'no'}},
    ];
  };
  // 放大到能读清时，每条记忆是一张小卡片，直接看到内容；被更正/汇总掉的旧记忆叠在新记忆下面。
  function cardStyles(v) {
    return [
      {selector: 'node.mem', style: {shape: 'round-rectangle', width: 190, height: 'label', padding: '9px', 'background-color': v('--surface'), 'border-width': 1.5, 'border-color': v('--border-strong'), label: cardText, 'text-valign': 'center', 'text-halign': 'center', 'text-margin-y': 0, 'text-wrap': 'wrap', 'text-max-width': '210px', 'line-height': 1.38, 'font-size': 12.5, color: v('--text'), 'text-background-opacity': 0, 'z-index': 2}},
      {selector: 'node.mem.stacked', style: {'border-style': 'dashed', 'border-color': v('--warn'), opacity: 0.85, 'z-index': 1}},
      {selector: 'node.mem:selected', style: {'border-color': v('--g-suggest'), 'border-width': 3, 'background-color': v('--surface')}},
    ];
  }
  // 缩小看整体时：记忆是醒目的彩色圆点（不写字），领地是带名字的大圆。放大后自动换成卡片。
  function farStyles(v) {
    return [
      {selector: 'node.mem.far', style: {shape: 'ellipse', width: 30, height: 30, padding: 0, 'background-color': v('--g-node'), 'border-color': v('--g-halo'), 'border-width': 3, label: '', 'text-opacity': 0}},
      {selector: 'node.mem.far.stacked', style: {width: 22, height: 22, 'border-width': 2}},
      {selector: 'node.mem.far.isolated', style: {'background-color': v('--g-isolated')}},
      {selector: 'node.mem.far:selected', style: {'background-color': v('--g-anchor'), 'border-color': v('--g-suggest'), 'border-width': 6}},
      ...[0, 1, 2, 3, 4, 5].map(n => ({selector: `node.mem.far.g${n}`, style: {'background-color': v(`--g-c${n}`)}})),
      {selector: 'edge.far', style: {width: 3.4, 'arrow-scale': 1.3}},
      {selector: 'edge.co.far', style: {width: 'mapData(co,1,8,4,12)'}},
    ];
  }
  // 提到多个实体的记忆：六边形的「桥」，停在它们所属的领地之间，细线连着每个领地；选中时展开成卡片。
  function bridgeStyles(v) {
    return [
      {selector: 'node.mem.bridge', style: {shape: 'hexagon', width: 30, height: 30, padding: 0, 'background-color': v('--surface'), 'border-width': 4, 'border-color': v('--g-anchor'), label: '', 'text-opacity': 0, 'z-index': 3}},
      {selector: 'node.mem.bridge.far', style: {width: 44, height: 44}},
      {selector: 'node.mem.bridge:selected', style: {shape: 'round-rectangle', width: 190, height: 'label', padding: '9px', 'border-width': 3, 'border-color': v('--g-suggest'), label: cardText, 'text-opacity': 1, 'text-valign': 'center', 'text-halign': 'center', 'text-wrap': 'wrap', 'text-max-width': '210px', 'line-height': 1.38, 'font-size': 12.5, color: v('--text'), 'text-background-opacity': 0, 'z-index': 9}},
      {selector: 'node.mem.bridge.far:selected', style: {shape: 'hexagon', width: 56, height: 56, padding: 0, label: '', 'text-opacity': 0, 'border-width': 6}},
      {selector: 'edge.thread', style: {'curve-style': 'straight', width: 1, 'line-color': v('--g-edge'), 'target-arrow-shape': 'none', opacity: 0.35, 'z-index': 0}},
      {selector: 'edge.thread.show', style: {width: 1.8, opacity: 0.95}},
    ];
  }
  function mapStyles(v) {
    const T = 'node.entity.territory';
    const name = n => n.hasClass('nolabel') ? '' : `${short(n.data('text'), 14)}  ${n.data('count')}${n.data('hits') ? ` · 命中 ${n.data('hits')}` : ''}`;
    return [
      {selector: T, style: {shape: 'ellipse', width: 'data(d)', height: 'data(d)', 'background-opacity': 0.07, 'border-width': 4, 'border-style': 'dashed', label: name, 'text-valign': 'top', 'text-halign': 'center', 'text-margin-y': -16, 'font-size': 44, 'font-weight': 700, 'text-wrap': 'none', 'text-background-opacity': 0, 'z-index': 0}},
      ...[0, 1, 2, 3, 4, 5].map(n => ({selector: `${T}.g${n}`, style: {'background-color': v(`--g-c${n}`), 'border-color': v(`--g-c${n}`)}})),
      {selector: `${T}.loose`, style: {'background-color': v('--g-isolated'), 'border-color': v('--g-isolated')}},
      {selector: `${T}.far`, style: {'font-size': 64, 'text-margin-y': -20, 'border-width': 4}},
      {selector: `${T}.t1`, style: {'background-opacity': 0.36, 'border-style': 'solid', 'border-width': 6, 'text-valign': 'center', 'text-margin-y': 0, 'font-size': 'mapData(d,520,2600,120,300)', 'text-wrap': 'wrap', 'text-max-width': 'data(d)', label: n => n.hasClass('nolabel') ? '' : `${short(n.data('text'), 10)}\n${n.data('count')} 条${n.data('hits') ? `\n命中 ${n.data('hits')}` : ''}`}},
      {selector: `${T}.t2`, style: {events: 'no'}},
      {selector: 'node.mem.far.lazy', style: {width: 46, height: 46}},
      {selector: 'edge.co.t1', style: {opacity: 0.6, width: 'mapData(co,1,8,6,26)'}},
      {selector: 'edge.co.t2', style: {opacity: 0.1}},
      {selector: 'node.more-tab', style: {shape: 'round-rectangle', width: 150, height: 34, 'background-color': v('--accent-soft'), 'border-color': v('--accent'), 'border-width': 2, label: 'data(label)', 'text-valign': 'center', 'text-halign': 'center', 'text-margin-y': 0, 'font-size': 14, 'font-weight': 600, color: v('--accent-text'), 'text-wrap': 'none', 'text-background-opacity': 0, 'z-index': 3}},
      {selector: 'node.more-tab.far', style: {width: 180, height: 44, 'font-size': 19}},
    ];
  }
  const cy = window.cytoscape({container: canvas, minZoom: 0.02, maxZoom: 2, userZoomingEnabled: false, boxSelectionEnabled: false, style: theme(),
    ...(new URLSearchParams(location.search).get('webgl') === '1' ? {renderer: {name: 'canvas', webgl: true}} : {})});
  root.cy = cy; // 方便自动化检查
  const restyle = () => { cy.style(theme()); drawMini(); };
  matchMedia('(prefers-color-scheme: dark)').addEventListener('change', restyle);
  window.addEventListener('rp-theme', restyle);

  const say = (text, loading = false) => { message.hidden = !text; message.textContent = text; message.classList.toggle('is-loading', loading); };
  const ui = RPGraphUI.mount({panel: $('[data-ui-panel]'), tip: $('[data-ui-tip]'), chips: $('[data-graph-chips]'), crumbs, groups: legend, legend: $('[data-ui-legend]')}, {
    clearSearch: () => clearAllFilters(), clearAll: () => clearAllFilters(),
    crumbRoot: () => { touched = true; goTo(viewFor(fitBounds(), canvas.clientWidth < 500 ? 22 : 40), 420); },
    crumbHere: () => { const id = ui.state.crumb?.id; if (id) focusNode(id); },
    pickGroup: group => pickGroup(group),
  });
  // 同一份记忆用两种画法：地图（实体的领地）和泳道（时间）。共用搜索、详情浮窗和定位。
  const VIEW_KEY = 'rp-graph-view';
  let mode = 'map', lanesData = null;
  const lanes = RPLanes.mount({
    canvas: lanesCanvas, ui, area: () => area(), entityTipOf: id => entityTip(entityById.get(id), coLines(id)),
    onSelect: mid => { if (mid) selectInLanes(mid, false); else deselectLanes(); },
    onPickEntity: id => pickLane(id),
  });
  root.lanes = lanes; // 同样给自动化检查用
  const tier = {far: false, t1: false};
  const applyTier = els => { els.toggleClass('far', tier.far); els.toggleClass('t1', tier.t1); els.toggleClass('t2', !tier.t1); };
  let lastLabelZoom = 0;
  const syncFar = () => {
    const z = cy.zoom();
    const far = z < (tier.far ? FAR + 0.04 : FAR), t1 = z < (tier.t1 ? NEAR + 0.02 : NEAR);
    if (far !== tier.far || t1 !== tier.t1) { tier.far = far; tier.t1 = t1; applyTier(cy.elements()); lastLabelZoom = 0; }
    if (Math.abs(Math.log(z / (lastLabelZoom || 1e-9))) > 0.04) declutter();
  };

  // ---------- 视野：始终居中在没被浮层挡住的那部分 ----------
  let touched = false;
  const panelOn = () => root.classList.contains('panel-on');
  const area = () => {
    const w = canvas.clientWidth, hh = canvas.clientHeight, narrow = w < 760;
    const bar = form.getBoundingClientRect(), box = canvas.getBoundingClientRect();
    const top = Math.max(0, bar.bottom - box.top) + 10;
    const cover = panelOn() && !narrow ? Math.min(384, w * 0.42) : 0;
    const bottom = panelOn() && narrow ? hh * 0.5 : 14;
    // 泳道要排在图例卡片的上面（地图上图例浮在图上没关系）
    const dock = mode === 'lanes' ? $('.graph-bottom').getBoundingClientRect() : null, dockTop = dock && dock.height ? dock.top - box.top - 10 : hh;
    return {x0: 0, y0: Math.min(top, hh * 0.4), w: Math.max(120, w - cover), h: Math.max(120, hh - Math.min(top, hh * 0.4) - bottom), y1: Math.min(hh - bottom, dockTop)};
  };
  const viewFor = (box, pad, maxZoom = 1.1) => {
    const a = area();
    const zoom = Math.max(0.02, Math.min(maxZoom, (a.w - pad * 2) / Math.max(1, box.w), (a.h - pad * 2) / Math.max(1, box.h)));
    return {zoom, pan: {x: a.x0 + a.w / 2 - zoom * (box.x1 + box.w / 2), y: a.y0 + a.h / 2 - zoom * (box.y1 + box.h / 2)}};
  };
  const goTo = (view, ms = 360, done) => {
    if (!ms || reduce) { cy.zoom(view.zoom); cy.pan(view.pan); syncFar(); done?.(); return; }
    cy.animate({zoom: view.zoom, pan: view.pan}, {duration: ms, complete: () => { syncFar(); done?.(); }});
  };
  const fit = () => {
    if (!cy.nodes().length) return;
    const box = cy.elements(':visible').not('.more-tab').boundingBox();
    const pad = canvas.clientWidth < 500 ? 22 : 40;
    goTo(viewFor(box, pad), 0);
  };

  // ---------- 排版 ----------
  const saved = () => { try { return JSON.parse(localStorage.getItem(KEY) || '{}'); } catch { return {}; } };
  const remember = () => { try { localStorage.setItem(KEY, JSON.stringify(Object.fromEntries(cy.nodes('.territory').map(n => [n.id(), [Math.round(n.position('x')), Math.round(n.position('y'))]])))); } catch { /* 隐私模式下不记 */ } };
  // 每个实体按领地大小当成一个圆。排过一次的位置会记住：数据变了，已有的领地不动，只给新来的找位置，
  // 你记住的「哪里是什么」不会因为多了几条记忆而全部换位。点「重新整理」才会重排。
  function mapLayout(fresh) {
    const lands = cy.nodes('.territory').filter(n => n.id() !== LOOSE), shelf = cy.getElementById(LOOSE);
    if (lands.length) {
      const items = lands.map(n => ({n, x: 0, y: 0, r: n.data('d') / 2 + 90, m: 1}));
      const at = new Map(items.map((item, k) => [item.n.id(), k]));
      const links = lands.connectedEdges('.co').toArray().map(e => [at.get(e.source().id()), at.get(e.target().id()), e.data('co')]).filter(l => l[0] !== undefined && l[1] !== undefined);
      const belts = new Map(links.map(([a, b, w]) => [`${Math.min(a, b)}|${Math.max(a, b)}`, BELT(w)]));
      const beltOf = (a, b) => belts.get(`${Math.min(a, b)}|${Math.max(a, b)}`) || 0;
      const memory = fresh ? {} : saved();
      const known = items.filter(a => memory[a.n.id()]);
      const resolve = () => {
        let moved = false;
        for (let a = 0; a < items.length; a++) for (let b = a + 1; b < items.length; b++) {
          let dx = items[b].x - items[a].x, dy = items[b].y - items[a].y, dist = Math.hypot(dx, dy);
          const need = items[a].r + items[b].r + beltOf(a, b);
          if (dist >= need - 1.5) continue; // 差一两个单位肉眼看不出，也免得「存了再取」的取整把位置推来推去
          if (dist < 1) { dx = 1; dy = 0; dist = 1; }
          const push = (need - dist) + 0.5, wa = items[a].m, wb = items[b].m, sum = wa + wb;
          items[a].x -= dx / dist * push * wa / sum; items[a].y -= dy / dist * push * wa / sum;
          items[b].x += dx / dist * push * wb / sum; items[b].y += dy / dist * push * wb / sum;
          moved = true;
        }
        return moved;
      };
      if (known.length && known.length >= items.length * 0.5) {
        // 沿用上次的位置；新来的挂在和它同现最多的领地旁边，没有同现的放到外圈。
        for (const a of known) { [a.x, a.y] = memory[a.n.id()]; a.m = 0.12; }
        const box = {x1: Math.min(...known.map(a => a.x - a.r)), x2: Math.max(...known.map(a => a.x + a.r)), y1: Math.min(...known.map(a => a.y - a.r)), y2: Math.max(...known.map(a => a.y + a.r))};
        const placed = known.slice();
        for (const a of items.filter(item => !memory[item.n.id()])) {
          const mine = links.filter(l => items[l[0]] === a || items[l[1]] === a).map(l => ({o: items[l[0]] === a ? items[l[1]] : items[l[0]], w: l[2]})).filter(l => memory[l.o.n.id()]);
          const total = mine.reduce((t, l) => t + l.w, 0);
          const cx = mine.length ? mine.reduce((t, l) => t + l.o.x * l.w, 0) / total : (box.x1 + box.x2) / 2, cyy = mine.length ? mine.reduce((t, l) => t + l.o.y * l.w, 0) / total : (box.y1 + box.y2) / 2;
          // 从它的「朋友」附近开始，沿螺旋往外找第一个放得下的空位，不去推别的领地。
          let spot = null;
          for (let t = 0; t < 4000 && !spot; t++) {
            const x = cx + Math.cos(t * GOLDEN) * 70 * Math.sqrt(t), y = cyy + Math.sin(t * GOLDEN) * 70 * Math.sqrt(t);
            if (placed.every(o => Math.hypot(o.x - x, o.y - y) >= o.r + a.r + 40)) spot = {x, y};
          }
          a.x = spot ? spot.x : cx; a.y = spot ? spot.y : cyy;
          placed.push(a);
        }
        for (let round = 0; round < 800 && resolve(); round++);
      } else {
        // 固定步数的力导向：互相排斥，经常一起出现的拉近，起点按顺序铺成螺旋，所以同样的数据永远排出同样的图。
        items.forEach((a, k) => { const angle = k * GOLDEN, radius = 500 * Math.sqrt(k + 1); a.x = Math.cos(angle) * radius; a.y = Math.sin(angle) * radius; });
        const STEPS = 400;
        for (let step = 0; step < STEPS; step++) {
          const heat = 1 - step / STEPS, fx = new Array(items.length).fill(0), fy = new Array(items.length).fill(0);
          for (let a = 0; a < items.length; a++) for (let b = a + 1; b < items.length; b++) {
            const dx = items[b].x - items[a].x, dy = items[b].y - items[a].y, dist = Math.max(1, Math.hypot(dx, dy));
            const gap = items[a].r + items[b].r + 140 + beltOf(a, b);
            if (dist > gap * 3) continue;
            const push = 0.5 * gap * gap / dist;
            fx[a] -= dx / dist * push; fy[a] -= dy / dist * push; fx[b] += dx / dist * push; fy[b] += dy / dist * push;
          }
          for (const [a, b, w] of links) {
            const dx = items[b].x - items[a].x, dy = items[b].y - items[a].y, dist = Math.max(1, Math.hypot(dx, dy));
            const natural = items[a].r + items[b].r + 160 + beltOf(a, b), pull = 0.5 * (dist - natural) * Math.min(3, 1 + Math.log(w || 1)) * 0.2;
            fx[a] += dx / dist * pull; fy[a] += dy / dist * pull; fx[b] -= dx / dist * pull; fy[b] -= dy / dist * pull;
          }
          items.forEach((a, k) => {
            fx[k] -= a.x * 0.01; fy[k] -= a.y * 0.01;
            const move = Math.min(Math.hypot(fx[k], fy[k]) * 0.05, 150 * heat + 6), len = Math.max(1, Math.hypot(fx[k], fy[k]));
            a.x += fx[k] / len * move; a.y += fy[k] / len * move;
          });
        }
        // 拉成画布的形状，再向中心挤紧，挤的同时把重叠的圆推开，最后一定没有重叠。
        const xs = items.map(a => a.x), ys = items.map(a => a.y);
        const w = Math.max(1, Math.max(...xs) - Math.min(...xs)), hh = Math.max(1, Math.max(...ys) - Math.min(...ys));
        const ratio = canvas.clientWidth / Math.max(1, canvas.clientHeight), stretch = Math.max(1, Math.min(2, Math.sqrt(ratio * hh / w)));
        items.forEach(a => { a.x *= stretch; });
        const wide = Math.max(1, Math.min(2, ratio)), cx = items.reduce((t, a) => t + a.x, 0) / items.length, cy0 = items.reduce((t, a) => t + a.y, 0) / items.length;
        for (let round = 0; round < 500; round++) {
          for (const a of items) {
            const dx = cx - a.x, dy = cy0 - a.y;
            a.x += Math.sign(dx) * Math.min(Math.abs(dx), 30 / wide); a.y += Math.sign(dy) * Math.min(Math.abs(dy), 30);
          }
          for (let pass = 0; pass < 6 && resolve(); pass++);
        }
        for (let round = 0; round < 600 && resolve(); round++);
      }
      cy.batch(() => items.forEach(a => a.n.position({x: a.x, y: a.y})));
    }
    if (shelf.length) {
      const own = fresh ? null : saved()[LOOSE], box = lands.length ? lands.boundingBox() : {x1: 0, x2: 0, y2: 0};
      shelf.position(own ? {x: own[0], y: own[1]} : {x: (box.x1 + box.x2) / 2, y: box.y2 + shelf.data('d') / 2 + 260});
    }
    remember();
    fit();
  }
  const layout = fresh => {
    if (!cy.nodes().length) return;
    cy.elements().removeClass('far t1 t2');
    tier.far = false; tier.t1 = false;
    mapLayout(fresh);
    applyTier(cy.elements());
    syncFar();
  };

  // ---------- 记忆落座 ----------
  // 只属于一个实体的记忆住在它的领地里：离中心越近越新，同一圈里意思相近的坐在同一侧。
  function seat(items, home, offset) {
    const placed = new Map();
    const slots = items.map((_, k) => { const g = offset + k, a = g * GOLDEN; return {x: Math.cos(a) * (FIRST + STEP * Math.sqrt(g)), y: Math.sin(a) * (FIRST + STEP * Math.sqrt(g)), band: Math.floor(Math.sqrt(g) * 0.5), dir: ((a + Math.PI) % (2 * Math.PI)) - Math.PI}; });
    const bands = new Map();
    items.forEach((d, k) => { const b = slots[k].band; if (!bands.has(b)) bands.set(b, []); bands.get(b).push(k); });
    for (const ks of bands.values()) {
      const byMeaning = [...ks].sort((p, q) => (items[p].sa ?? 0) - (items[q].sa ?? 0) || p - q);
      const bySlot = [...ks].sort((p, q) => slots[p].dir - slots[q].dir || p - q);
      const useMeaning = ks.some(k => items[k].sa !== undefined);
      bySlot.forEach((slot, i) => placed.set(items[useMeaning ? byMeaning[i] : slot].id, {x: home.x + slots[slot].x, y: home.y + slots[slot].y}));
    }
    return placed;
  }
  // 提到多个实体的记忆画成「桥」，停在它们所属领地之间的空隙里：从这几个领地的中点出发，沿螺旋往外找第一个空位。
  const BRIDGE_SPACE = 62;
  function bridgeSpot(lands, taken) {
    // 领地本身，以及它上方写名字的那一块，桥都要躲开
    const all = cy.nodes('.territory').toArray().flatMap(n => [{x: n.position('x'), y: n.position('y'), r: n.data('d') / 2 + 34}, {x: n.position('x'), y: n.position('y') - n.data('d') / 2 - 40, r: 100}]);
    const cx = lands.reduce((t, n) => t + n.position('x'), 0) / lands.length, cyy = lands.reduce((t, n) => t + n.position('y'), 0) / lands.length;
    for (let t = 0; t < 3000; t++) {
      const x = cx + Math.cos(t * GOLDEN) * 34 * Math.sqrt(t), y = cyy + Math.sin(t * GOLDEN) * 34 * Math.sqrt(t);
      if (all.some(o => Math.hypot(o.x - x, o.y - y) < o.r) || taken.some(q => Math.hypot(q.x - x, q.y - y) < BRIDGE_SPACE)) continue;
      return {x, y};
    }
    return {x: cx, y: cyy};
  }
  // 被更正、被汇总掉的旧记忆叠在取代它的新记忆下面。
  const stackOffset = n => ({dx: 11 + 7 * Math.min(n, 5), dy: 13 + 7 * Math.min(n, 5)});

  const loaded = new Map(), loading = new Set(); // 领地 id → {page, total, more, drawn, inner}
  const bendOf = (a, b, id) => {
    const dist = Math.hypot(a.x - b.x, a.y - b.y);
    let sum = 0; for (const ch of id) sum = (sum * 31 + ch.charCodeAt(0)) >>> 0;
    return (sum % 2 ? 1 : -1) * Math.min(160, dist * 0.22);
  };
  // 记忆属于哪几个领地：它提到的、画出了领地的那些实体；一个都没有就是「未归类」。
  const landsOfData = d => { const ids = (d.entity_ids || []).filter(x => cy.getElementById(x).hasClass('territory')); return ids.length ? ids : [LOOSE]; };
  async function loadLand(id, page = 0) {
    const have = loaded.get(id);
    if ((page === 0 && have) || (have && page <= have.page) || loading.has(id + page)) return;
    const land = cy.getElementById(id);
    if (!land.length || (page > 0 && !have)) return;
    loading.add(id + page);
    status.textContent = `正在载入「${land.data('text')}」的记忆…`;
    try {
      const response = await fetch(`/graph-data?focus=${encodeURIComponent(id)}&page=${page}`, {signal: AbortSignal.timeout(12000)});
      if (!response.ok) throw new Error('land request failed');
      const data = await response.json();
      if (!cy.getElementById(id).length) return;
      const colour = [0, 1, 2, 3, 4, 5].map(n => `g${n}`).find(c => land.hasClass(c)) || '';
      const state = have || {inner: 0, drawn: 0};
      // 别的领地先载入过的记忆（桥）已经在图上了，不再画第二次
      const memories = data.nodes.filter(d => d.kind !== 'entity' && !cy.getElementById(d.id).length);
      const kept = new Set(memories.map(d => d.id)), parent = new Map();
      for (const e of data.edges) if ((e.rk === 'corrects' || e.rk === 'summarizes') && kept.has(e.from) && kept.has(e.to) && e.from !== e.to && !parent.has(e.to)) parent.set(e.to, e.from);
      const rootOf = key => { let cur = key, guard = 0; while (parent.has(cur) && guard++ < 20) cur = parent.get(cur); return cur; };
      const free = memories.filter(d => !parent.has(d.id)), inside = free.filter(d => landsOfData(d).length === 1), bridges = free.filter(d => landsOfData(d).length > 1);
      const insideIds = new Set(inside.map(d => d.id));
      const inner = state.inner + inside.length + memories.filter(d => parent.has(d.id) && insideIds.has(rootOf(d.id))).length;
      growLand(land, inner);
      const home = land.position(), seats = seat(inside, home, state.inner), taken = cy.nodes('.bridge').map(n => ({x: n.position('x'), y: n.position('y')})), spots = new Map();
      for (const d of inside) spots.set(d.id, {...seats.get(d.id), anchor: id});
      for (const d of bridges) { const spot = bridgeSpot(landsOfData(d).map(l => cy.getElementById(l)), taken); taken.push(spot); spots.set(d.id, {...spot, anchor: ''}); }
      const under = new Map();
      for (const d of memories) {
        if (!parent.has(d.id)) continue;
        const top = rootOf(d.id), base = spots.get(top), n = under.get(top) || 0, off = stackOffset(n);
        under.set(top, n + 1);
        spots.set(d.id, base ? {x: base.x + off.dx, y: base.y + off.dy, anchor: base.anchor, under: top, off: [off.dx, off.dy]} : {x: home.x, y: home.y, anchor: id});
      }
      const olds = new Set(data.edges.filter(e => e.rk === 'corrects' || e.rk === 'summarizes').map(e => e.to));
      let fresh = cy.collection();
      cy.batch(() => {
        for (const d of memories) {
          if (cy.nodes().length >= maxNodes) break;
          const spot = spots.get(d.id), lands = landsOfData(d), bridge = lands.length > 1 && !spot.under;
          const groups = lands.map(l => cy.getElementById(l).data('group')).filter(Number.isInteger);
          fresh = fresh.union(cy.add({group: 'nodes', data: {...d, label: short(d.text, 17), lands, groups, anchor: spot.anchor, under: spot.under || '', off: spot.off || null, old: olds.has(d.id) ? 1 : 0}, classes: ['mem', 'lazy', bridge ? 'bridge' : colour, spot.under && 'stacked'].filter(Boolean).join(' '), position: {x: spot.x, y: spot.y}}));
          if (bridge) for (const l of lands) fresh = fresh.union(cy.add({group: 'edges', data: {id: `th:${d.id}:${l}`, source: d.id, target: l, bend: 0}, classes: 'thread'}));
        }
        for (const e of data.edges.filter(x => !x.mention && !x.co && cy.getElementById(x.from).length && cy.getElementById(x.to).length)) {
          const edgeId = `r:${e.from}:${e.to}`;
          if (cy.getElementById(edgeId).length) continue;
          const stacked = cy.getElementById(e.to).hasClass('stacked');
          fresh = fresh.union(cy.add({group: 'edges', data: {id: edgeId, source: e.from, target: e.to, reason: e.reason, rk: e.rk, bend: stacked ? 0 : bendOf(cy.getElementById(e.from).position(), cy.getElementById(e.to).position(), edgeId)}, classes: ['ref', e.rk && e.rk !== 'relates' && e.rk].filter(Boolean).join(' ')}));
        }
      });
      loaded.set(id, {page, total: data.total, more: Boolean(data.more), drawn: state.drawn + data.nodes.length, inner});
      moreTab(land);
      applyTier(fresh.union(fresh.connectedEdges()));
      fresh.addClass('entering');
      requestAnimationFrame(() => requestAnimationFrame(() => fresh.removeClass('entering')));
      if (solo) fresh.nodes().forEach(n => n.toggleClass('gone', !inGroup(n)));
      if (searching) markHits(fresh.nodes());
    } catch {
      status.textContent = '未能载入这个实体的记忆，缩放一下再试';
    } finally {
      loading.delete(id + page);
      announce();
      refreshPane();
    }
  }
  // 领地里的记忆更多、或被拖动时，领地自己或它的邻居要移动：先移领地（连同里面的记忆），再把桥重新摆一遍。
  const familyMap = () => {
    const family = new Map(cy.nodes('.territory').map(n => [n.id(), cy.collection()]));
    cy.nodes().forEach(n => { const owner = n.hasClass('territory') ? n.id() : n.data('anchor'); if (family.has(owner)) family.set(owner, family.get(owner).union(n)); });
    return family;
  };
  // 领地挤到别的领地时，把邻居推开；anchor 这个领地自己不动。返回被推动过的领地。
  // carry 不为空时（正在拖动）只移动领地本身，每个领地被推了多远记在 carry 里，里面的记忆等松手时一次跟上。
  function relaxAround(anchor, carry = null) {
    const lands = cy.nodes('.territory').toArray(), family = carry ? null : familyMap(), touchedLands = new Set();
    const shift = carry
      ? (n, dx, dy) => { n.position({x: n.position('x') + dx, y: n.position('y') + dy}); const c = carry.get(n.id()) || {dx: 0, dy: 0}; carry.set(n.id(), {dx: c.dx + dx, dy: c.dy + dy}); }
      : (n, dx, dy) => cy.batch(() => family.get(n.id()).forEach(m => m.position({x: m.position('x') + dx, y: m.position('y') + dy})));
    for (let round = 0; round < 120; round++) {
      let moved = false;
      for (let i = 0; i < lands.length; i++) for (let j = i + 1; j < lands.length; j++) {
        const a = lands[i], b = lands[j], wa = a.id() === anchor ? 0 : 1, wb = b.id() === anchor ? 0 : 1;
        if (wa + wb === 0) continue;
        let dx = b.position('x') - a.position('x'), dy = b.position('y') - a.position('y'), dist = Math.hypot(dx, dy);
        const need = a.data('d') / 2 + b.data('d') / 2 + 120 + beltBetween(a.id(), b.id());
        if (dist >= need) continue;
        if (dist < 1) { dx = 1; dy = 0; dist = 1; }
        const push = need - dist;
        shift(a, -dx / dist * push * wa / (wa + wb), -dy / dist * push * wa / (wa + wb));
        shift(b, dx / dist * push * wb / (wa + wb), dy / dist * push * wb / (wa + wb));
        if (wa) touchedLands.add(a.id());
        if (wb) touchedLands.add(b.id());
        moved = true;
      }
      if (!moved) break;
    }
    return touchedLands;
  }
  // 跟着旧记忆的新记忆挪了位置，叠在它下面的旧记忆也跟过去。
  function refollow(parent) {
    cy.nodes().filter(n => n.data('under') === parent.id()).forEach(n => { const [dx, dy] = n.data('off'); n.position({x: parent.position('x') + dx, y: parent.position('y') + dy}); });
  }
  // 领地动了以后，和它们有关的桥重新找位置。
  function replaceBridges(ids) {
    const mine = cy.nodes('.bridge').filter(n => n.data('lands').some(l => ids.has(l)));
    if (!mine.length) return;
    const taken = cy.nodes('.bridge').not(mine).map(n => ({x: n.position('x'), y: n.position('y')}));
    cy.batch(() => mine.toArray().sort((a, b) => String(b.data('occurred') || '').localeCompare(String(a.data('occurred') || '')) || (a.id() < b.id() ? -1 : 1)).forEach(n => {
      const spot = bridgeSpot(n.data('lands').map(l => cy.getElementById(l)), taken);
      taken.push(spot); n.position(spot); refollow(n);
    }));
  }
  // 画出的记忆多于一页时，领地随之长大，挤到的邻居被推开。
  function growLand(land, inner) {
    const d = 2 * (reach(inner) + 90);
    if (d <= land.data('d')) return;
    land.data('d', d);
    replaceBridges(new Set([land.id(), ...relaxAround(land.id())]));
    remember();
  }
  // 还有更早的记忆没画出来时，在领地边缘放一块「更早的 N 条」。
  function moreTab(land) {
    const id = land.id(), state = loaded.get(id), tabId = `more@${id}`;
    cy.getElementById(tabId).remove();
    if (!state?.more) return;
    const left = state.total - state.drawn;
    cy.add({group: 'nodes', data: {id: tabId, kind: 'more', land: id, anchor: id, label: `更早的 ${left} 条 ▸`}, classes: 'more-tab', position: {x: land.position('x'), y: land.position('y') + land.data('d') / 2 - 60}});
    applyTier(cy.getElementById(tabId));
  }

  // 放大到能看见记忆时，把画面里（最靠近中心的）实体的记忆依次载入。
  let pumping = false, pumpTimer = 0;
  async function pump() {
    if (pumping || cy.zoom() < NEAR) return;
    pumping = true;
    try {
      for (let round = 0; round < 6; round++) {
        const ext = cy.extent(), cx = (ext.x1 + ext.x2) / 2, cyy = (ext.y1 + ext.y2) / 2;
        const todo = cy.nodes('.territory').filter(n => !loaded.has(n.id()) && !loading.has(n.id() + 0) && n.boundingBox().x1 < ext.x2 && n.boundingBox().x2 > ext.x1 && n.boundingBox().y1 < ext.y2 && n.boundingBox().y2 > ext.y1)
          .toArray().sort((a, b) => Math.hypot(a.position('x') - cx, a.position('y') - cyy) - Math.hypot(b.position('x') - cx, b.position('y') - cyy));
        if (!todo.length) break;
        for (const n of todo.slice(0, 3)) await loadLand(n.id());
      }
    } finally { pumping = false; }
  }
  cy.on('viewport', () => { clearTimeout(pumpTimer); pumpTimer = setTimeout(pump, 220); scheduleChrome(); });

  // ---------- 领地名字：只给放得下、不互相压住的领地写名字 ----------
  function declutter() {
    lastLabelZoom = cy.zoom();
    const z = cy.zoom(), t1 = z < NEAR;
    const lands = cy.nodes('.territory').toArray().sort((a, b) => b.data('count') - a.data('count') || (a.id() < b.id() ? -1 : 1));
    const boxes = [];
    cy.batch(() => {
      for (const n of lands) {
        const d = n.data('d'), p = n.position(), name = short(n.data('text'), t1 ? 10 : 14);
        const fs = t1 ? Math.max(120, Math.min(300, 120 + (d - 520) / (2600 - 520) * 180)) : (tier.far ? 64 : 44);
        const chars = Array.from(name).length;
        const w = t1 ? Math.min(d, chars * fs) : (chars + 4) * fs * 0.95, hh = t1 ? fs * 2.5 : fs * 1.25;
        const box = t1 ? {x1: p.x - w / 2, x2: p.x + w / 2, y1: p.y - hh / 2, y2: p.y + hh / 2} : {x1: p.x - w / 2, x2: p.x + w / 2, y1: p.y - d / 2 - 20 - hh, y2: p.y - d / 2 - 20};
        const fits = fs * z >= 8 && !boxes.some(o => box.x1 < o.x2 && box.x2 > o.x1 && box.y1 < o.y2 && box.y2 > o.y1);
        if (fits) boxes.push(box);
        n.toggleClass('nolabel', !fits);
      }
    });
  }

  const fitBounds = () => { const b = cy.elements(':visible').not('.more-tab').boundingBox(); return b; };

  // ---------- 加入数据 ----------
  let snapshot, epoch = 0, retries = 0, minCo = 1, solo = '';
  function put(data) {
    cy.batch(() => {
      for (const d of data.nodes) {
        if (cy.getElementById(d.id).length || cy.nodes().length >= maxNodes) continue;
        cy.add({group: 'nodes', data: {...d, label: short(d.text, 17), d: 2 * (reach(Math.min(d.own ?? d.count, LAND_CAP)) + 90)}, classes: `entity territory${d.id === LOOSE ? ' loose' : ''}${Number.isInteger(d.group) ? ` g${d.group}` : ''}`});
      }
      addCo(data.edges);
    });
  }
  function addCo(list) {
    for (const e of list) {
      if (!e.co || e.co < minCo || !cy.getElementById(e.from).length || !cy.getElementById(e.to).length) continue;
      const ends = [e.from, e.to].sort(), id = `c:${ends.join(':')}`;
      if (!cy.getElementById(id).length) cy.add({group: 'edges', data: {id, source: e.from, target: e.to, reason: e.reason, co: e.co, bend: 0}, classes: 'co'});
    }
    thin();
  }
  // 每个实体只把和它最紧密的 3 条连线常亮，其余的在点它、悬停它时才出现，免得缩小后一团乱线。
  function thin() {
    const ranked = new Map();
    for (const e of cy.edges('.co')) for (const end of [e.source().id(), e.target().id()]) {
      if (!ranked.has(end)) ranked.set(end, []);
      ranked.get(end).push(e);
    }
    const strong = new Set();
    for (const list of ranked.values()) list.sort((a, b) => b.data('co') - a.data('co') || (a.id() < b.id() ? -1 : 1)).slice(0, 3).forEach(e => strong.add(e.id()));
    cy.batch(() => cy.edges('.co').forEach(e => e.toggleClass('minor', !strong.has(e.id()))));
  }
  function setupCo(data) {
    minCo = data.suggested_min || 1;
    if (coFilter && coRange) {
      coRange.max = String(Math.max(1, data.max_co || 1));
      coRange.value = String(minCo);
      coValue.textContent = String(minCo);
      coFilter.hidden = (data.max_co || 1) < 2;
    }
    ui.set({groups: data.groups || [], solo: ''});
  }
  coRange?.addEventListener('input', () => {
    minCo = Number(coRange.value);
    coValue.textContent = String(minCo);
    cy.edges('.co').remove();
    addCo(snapshot?.edges ?? []);
    applyTier(cy.edges('.co'));
    markFilterGone();
  });
  // 点图例里的一组，只看这一组；位置不变，视野移过去。
  const inGroup = n => n.hasClass('bridge') ? (n.data('groups') || []).includes(Number(solo)) : n.hasClass(`g${solo}`);
  const markFilterGone = () => cy.batch(() => cy.nodes().forEach(n => n.toggleClass('gone', Boolean(solo) && !n.hasClass('more-tab') && !inGroup(n))));
  function pickGroup(group) {
    solo = solo === group ? '' : group;
    ui.set({solo});
    markFilterGone();
    lanes.setSolo(solo);
    if (mode === 'map') { touched = false; fit(); }
  }
  function announce(extra = '') {
    const parts = [];
    if (mode === 'lanes') parts.push(`${entityById.size} 条泳道`, `${snapshot?.memories ?? 0} 条记忆`, '滚轮缩放时间，拖动平移，Shift + 滚轮上下翻');
    else {
      const lands = cy.nodes('.territory').filter(n => n.id() !== LOOSE).length, shown = cy.nodes('.mem').length;
      parts.push(`${lands} 个实体`, `${snapshot?.memories ?? 0} 条记忆`, shown ? `已画出 ${shown} 条` : '放大进实体就能看到记忆');
    }
    if (searching) parts.push(`${lit.length} 条命中`);
    if (extra) parts.push(extra);
    status.textContent = parts.join(' · ');
  }

  // ---------- 选中：亮出相连的部分 ----------
  let active = '';
  // 选中时让它「弹」一下：边框先变粗再回来。
  function pulse(node) {
    if (reduce) return;
    const base = parseFloat(node.style('border-width')) || 2;
    node.stop(true).animate({style: {'border-width': base + 7}}, {duration: 140}).animate({style: {'border-width': base}}, {duration: 320, complete: () => node.removeStyle('border-width')});
  }
  // 选中的东西亮起来：相连的部分（一条桥连着的那几个领地和细线、一个领地里和它有关的记忆），其余淡下去。
  function paint(node, dim) {
    cy.elements().removeClass('dim');
    if (!node.length || !dim) return;
    const mine = node.hasClass('territory') ? cy.nodes('.mem').filter(n => n.data('lands').includes(node.id())) : cy.collection();
    cy.elements().not(node.closedNeighborhood().union(mine).union(mine.connectedEdges())).addClass('dim');
  }
  function describe(id, dim = true) {
    const node = cy.getElementById(id);
    active = node.isNode() ? id : '';
    edgeNote = '';
    cy.$('node:selected').unselect();
    paint(cy.collection(), dim);
    refreshShow();
    if (!active) { refreshPane(); return; }
    node.select();
    pulse(node);
    paint(node, dim);
    if (node.hasClass('mem') || node.hasClass('territory')) { openPane('item'); showItem(node); }
  }
  let showEdges = cy.collection();
  function refreshShow() {
    showEdges.removeClass('show');
    const keep = [];
    if (active) keep.push(cy.getElementById(active).connectedEdges());
    if (hovered?.isNode?.()) keep.push(hovered.connectedEdges());
    showEdges = keep.reduce((all, e) => all.union(e), cy.collection());
    showEdges.addClass('show');
  }

  // ---------- 右侧浮窗：详情、搜索结果、说明（界面在 graph-ui.js，这里只准备数据） ----------
  let current = 'guide';
  function openPane(name) { current = name; ui.set({pane: name}); setPanel(true); }
  function setPanel(on) {
    root.classList.toggle('panel-on', on);
    root.querySelector('[data-graph-action=panel]')?.setAttribute('aria-pressed', String(on));
    requestAnimationFrame(() => { cy.resize(); if (!touched) fit(); scheduleChrome(); });
  }
  const refreshPane = () => { if (current === 'item' && active) showItem(cy.getElementById(active)); };
  const peeks = new Map(), peekCache = new Map();
  async function peek(mid) {
    if (peeks.has(mid)) return peeks.get(mid);
    const job = fetch(`/memory/${encodeURIComponent(mid)}/peek`, {signal: AbortSignal.timeout(10000)}).then(r => r.ok ? r.json() : null).catch(() => null);
    peeks.set(mid, job);
    const result = await job;
    if (result) peekCache.set(mid, result); else peeks.delete(mid);
    return result;
  }
  const backHere = id => {
    const query = new URLSearchParams(location.search); query.delete('at'); query.delete('node');
    if (id) query.set('node', id);
    return `/graph${query.toString() ? `?${query}` : ''}`;
  };
  const memoryLinks = (mid, back) => [{href: `/memory/${encodeURIComponent(mid)}/edit?back=${encodeURIComponent(back)}`, text: '编辑', primary: true}, {href: `/memory/${encodeURIComponent(mid)}?back=${encodeURIComponent(back)}`, text: '完整页面'}];
  const flyToMemory = (mid, fallback) => async () => { const n = await reveal(mid); if (n) focusNode(n.id()); else if (fallback) location.assign(fallback); };
  let renderToken = 0, edgeNote = '';
  // 实体：名字、说明、常一起出现的实体、它的记忆，以及从总览数据里带来的最近日期。
  const entityById = new Map(), coMap = new Map();
  const nameOf = id => entityById.get(id)?.text || '';
  const coLines = id => { const top = (coMap.get(id) || []).slice(0, 3); return top.length ? [{tag: '常一起', text: top.map(n => nameOf(n.id)).join('、')}] : []; };
  function entityCard(id) {
    const d = entityById.get(id), loose = id === LOOSE, inLanes = mode === 'lanes', state = inLanes ? null : loaded.get(id);
    const goEntity = inLanes ? pickLane : focusNode;
    const memories = inLanes
      ? lanes.memoriesOf(id).map(m => ({lead: m.occurred, text: m.text, tip: memoryTip(m), go: () => selectInLanes(m.id, true)}))
      : cy.nodes('.mem').filter(n => n.data('lands').includes(id) && !n.hasClass('stacked')).toArray().sort((a, b) => String(b.data('occurred') || '').localeCompare(String(a.data('occurred') || '')))
        .map(n => ({lead: n.data('occurred'), text: n.data('text'), tip: memoryTip(n.data()), go: () => focusNode(n.id())}));
    const nearby = (coMap.get(id) || []).map(n => ({dot: true, text: `${nameOf(n.id)} · ${n.co} 条同现`, onClick: () => goEntity(n.id), tip: entityTip(entityById.get(n.id), coLines(n.id))}));
    return {
      title: loose ? '未归类' : '实体', edge: edgeNote,
      meta: [`${d.count ?? 0} 条记忆`, state && state.drawn < (d.count ?? 0) ? `已画出最近 ${state.drawn} 条` : '', d.last ? `最近 ${d.last}` : ''].filter(Boolean).join(' · '),
      name: d.text, text: d.hint || '', context: (d.aliases || []).length ? `别名：${d.aliases.join('、')}` : '',
      blocks: [
        nearby.length && {title: `经常一起出现的实体 ${nearby.length}`, rows: nearby.slice(0, 12)},
        memories.length && {title: `它的记忆 ${memories.length}${state?.more ? '+' : ''}`, rows: memories.slice(0, 60).map(m => ({lead: m.lead || '日期未记', text: short(m.text, 40), onClick: m.go, tip: m.tip}))},
      ].filter(Boolean),
      actions: loose ? [{href: '/memories', text: '在「记忆」页查看全部'}] : [{href: `/entity/${encodeURIComponent(id)}`, text: '查看实体页', primary: true}, {href: `/memories?entity=${encodeURIComponent(id)}`, text: '只看它的记忆'}],
    };
  }
  // 一条记忆：quick 是图上已有的内容（先占位），full 是读到的完整内容；hooks 说明在当前视图里「飞到哪」。
  function memoryCard({mid, quick, full, loading, back, hooks, note}) {
    const src = full || quick;
    const item = {title: '记忆', memory: true, edge: edgeNote, loading, meta: `${src.occurred || '日期未记'} · ${src.source || '来源未记'}`, text: src.text, context: src.context ? `情境：${src.context}` : '', actions: memoryLinks(mid, back), blocks: []};
    if (!full) return item;
    item.entities = (full.mentions || []).map(m => {
      const label = m.entity_name && m.entity_name !== m.name ? `${m.name} → ${m.entity_name}` : m.name;
      if (m.entity && hooks.has(m.entity)) return {label, onClick: hooks.go(m.entity, mid), tip: hooks.tip(m.entity)};
      return {label, open: !m.entity, tip: {title: m.entity_name || m.name, text: m.entity ? '这个实体记忆太少，没有单独的实体。' : '待确认：还没有指向具体实体，接入的 AI 整理时会选择。'}};
    });
    if (note) item.note = note;
    item.facts = [['写入', when(full.recorded)], ['版本', `第 ${full.revision} 版${full.edited ? `（${full.edited} 个历史版本）` : ''}`], ['编号', full.id, true]];
    const page = id => `/memory/${encodeURIComponent(id)}?back=${encodeURIComponent(back)}`;
    const linkRows = (list, suffix) => list.map(l => ({lead: l.label ? `${l.label}${suffix}` : (l.occurred || '日期未记'), text: short(l.text, 44), sub: l.reason ? `原因：${short(l.reason, 40)}` : '', tone: l.kind, onClick: hooks.memory(l.id, page(l.id)), tip: {meta: `${l.label ? `${l.label}${suffix} · ` : ''}${l.occurred || '日期未记'}`, text: l.text, sub: l.reason ? `原因：${l.reason}` : ''}}));
    if ((full.out || []).length) item.blocks.push({title: `引用了 ${full.out.length}`, rows: linkRows(full.out, '')});
    if ((full.in || []).length) item.blocks.push({title: `被引用 ${full.in.length}`, rows: linkRows(full.in, '了它')});
    if ((full.trail || []).length) item.blocks.push({title: '更正轨迹', cls: 'trail', rows: full.trail.map(t => ({lead: t.label, text: `${short(t.text, 40)} · ${t.source}`, sub: t.reason ? `原因：${short(t.reason, 40)}` : '', onClick: hooks.memory(t.id, page(t.id)), tip: {meta: `${t.label} · ${t.source} · ${when(t.recorded)}`, text: t.text, sub: t.reason ? `原因：${t.reason}` : ''}}))});
    if ((full.files || []).length) item.blocks.push({title: `附件 ${full.files.length}`, files: full.files});
    return item;
  }
  const mapHooks = {
    has: id => cy.getElementById(id).hasClass('territory'),
    tip: id => entityTip(cy.getElementById(id).data(), coLines(id)),
    go: id => () => focusNode(id),
    memory: (id, href) => flyToMemory(id, href),
  };
  const laneHooks = {
    has: id => lanes.hasLane(id),
    tip: id => entityTip(entityById.get(id), coLines(id)),
    go: (id, mid) => () => lanes.select(mid, true, id),
    memory: (id, href) => () => { if (lanes.has(id)) selectInLanes(id, true); else location.assign(href); },
  };
  // 先放上图里已有的内容，读到完整内容后再换成完整的。
  async function fillMemoryCard(token, mid, quick, back, hooks, noteFor) {
    const cached = peekCache.get(mid), base = {mid, quick, back, hooks};
    if (!cached) ui.set({item: memoryCard({...base, loading: true})});
    const full = cached || await peek(mid);
    if (token !== renderToken) return;
    if (!full) { ui.set({item: memoryCard(base)}); return; }
    ui.set({item: memoryCard({...base, full, note: noteFor()})});
  }
  async function showItem(node) {
    if (!node.length) return;
    const token = ++renderToken;
    if (node.hasClass('territory')) { ui.set({item: entityCard(node.id())}); return; }
    if (!node.hasClass('mem')) return;
    const d = node.data();
    await fillMemoryCard(token, node.id(), {occurred: d.occurred, source: d.source, text: d.full || d.text, context: d.context}, backHere(node.id()), mapHooks,
      () => node.hasClass('bridge') ? `这条记忆提到 ${d.lands.length} 个实体，所以画成「桥」，停在它们之间；细线连着每个实体的领地` : '');
  }
  // 泳道里选中：一条记忆或一个实体
  function selectInLanes(mid, reveal) {
    const m = lanes.memory(mid);
    if (!m) return;
    lanes.select(mid, reveal);
    openPane('item');
    const token = ++renderToken;
    fillMemoryCard(token, mid, {occurred: m.occurred, source: m.source, text: m.text, context: ''}, backHere(mid), laneHooks,
      () => m.lanes.length > 1 ? `这条记忆提到 ${m.lanes.length} 个实体，每个实体的泳道上各有一个点，竖线把它们连在一起` : '');
  }
  function pickLane(id) {
    lanes.selectLane(id);
    ++renderToken;
    openPane('item');
    ui.set({item: entityCard(id)});
  }
  function deselectLanes() {
    lanes.select(null); lanes.selectLane('');
    ++renderToken;
    if (searching) openPane('hits'); else { openPane('guide'); setPanel(false); }
  }
  // 清掉两个视图里的选中
  function clearSelection() { describe('', false); lanes.select(null); lanes.selectLane(''); }

  // 找到一条记忆在图里的位置：先看已经画出的，没有就载入它所属实体的记忆（必要时翻到更早的页）。
  async function landsOf(mid) {
    const d = await peek(mid);
    if (!d) return [];
    const ids = [...new Set((d.mentions || []).map(m => m.entity).filter(x => x && cy.getElementById(x).hasClass('territory')))];
    return ids.length ? ids : [LOOSE];
  }
  async function reveal(mid) {
    if (cy.getElementById(mid).length) return cy.getElementById(mid);
    for (const land of await landsOf(mid)) {
      if (!cy.getElementById(land).hasClass('territory')) continue;
      await loadLand(land);
      for (let guard = 0; !cy.getElementById(mid).length && loaded.get(land)?.more && guard < 8; guard++) await loadLand(land, loaded.get(land).page + 1);
      if (cy.getElementById(mid).length) return cy.getElementById(mid);
    }
    return null;
  }
  // 定位到一个节点：居中，缩得太小就放大到能读清为止。
  function focusNode(id) {
    const node = cy.getElementById(id);
    if (!node.length) return;
    describe(id);
    touched = true;
    if (node.hasClass('territory')) goTo(viewFor(node.boundingBox(), 60, 1.1), 380, () => { pump(); loadLand(id); });
    else {
      const zoom = Math.max(cy.zoom(), FAR + 0.25), p = node.position(), a = area();
      goTo({zoom, pan: {x: a.x0 + a.w / 2 - zoom * p.x, y: a.y0 + a.h / 2 - zoom * p.y}}, 320);
    }
  }

  // ---------- 搜索：留在地图里，命中的发光，其余淡化 ----------
  let searching = false, hitList = [], lit = [], hitIds = new Set();
  const params = () => {
    const p = new URLSearchParams();
    for (const key of ['q', 'strategy', 'from', 'through']) { const v = form.elements[key]?.value.trim(); if (v && !(key === 'strategy' && !form.elements.q.value.trim())) p.set(key, v); }
    for (const key of ['period', 'undated']) { const v = form.elements[key]?.value; if (v) p.set(key, v); }
    return p;
  };
  function markHits(nodes) {
    cy.batch(() => nodes.forEach(n => { if (!n.hasClass('mem')) return; const hit = hitIds.has(n.id()); n.toggleClass('hit', hit); n.toggleClass('miss', !hit); }));
  }
  function clearHits() {
    searching = false; hitList = []; lit = []; hitIds = new Set(); lastHits = null;
    cy.elements().removeClass('hit miss has-hit');
    cy.nodes('.territory').data('hits', 0);
    lanes.setHits(null);
    renderChips();
  }
  const syncUrl = p => {
    if (mode === 'lanes') p.set('view', 'lanes');
    const url = new URL(location.href); url.search = p.toString();
    history.replaceState(null, '', url);
  };
  let lastHits = null;
  async function runSearch() {
    const p = params();
    syncUrl(new URLSearchParams(p));
    renderChips();
    if (![...p.keys()].some(k => k !== 'strategy')) { clearHits(); if (current === 'hits') { ui.set({hits: null}); openPane('guide'); setPanel(false); } announce(); return; }
    say('正在搜索…', true);
    try {
      const response = await fetch(`/graph-hits?${p}`, {signal: AbortSignal.timeout(15000)});
      if (!response.ok) throw new Error(await response.text());
      const data = await response.json();
      say('');
      hitList = data.hits || [];
      // 弱相关只当线索列在右边；有把握的命中才在图里亮。一条有把握的都没有时，弱相关也亮出来。
      lit = hitList.filter(x => x.match !== 'weak' || data.no_strong);
      hitIds = new Set(lit.map(x => x.id));
      searching = true; lastHits = data;
      await applyHits(data);
    } catch (error) {
      say('');
      status.textContent = `搜索没有成功：${String(error.message || error).slice(0, 80)}`;
    }
  }
  // 把搜索结果画到当前的视图上：地图里命中的发光、其余淡化；泳道里命中的点亮起，视野移到命中的那段时间。
  async function applyHits(data) {
    if (mode === 'lanes') {
      lanes.setHits(hitIds); renderHits(data); openPane('hits'); lanes.focusHits(); announce();
      return;
    }
    // 命中分散在哪些领地：命中最多的先载入（最多 8 个），命中的记忆太早没在第一页里，就继续翻页找。
    const count = new Map();
    for (const hit of lit) for (const land of (hit.lands || [])) count.set(land, (count.get(land) || 0) + 1);
    const order = [...count.entries()].filter(([land]) => cy.getElementById(land).length).sort((a, b) => b[1] - a[1]).slice(0, 8).map(e => e[0]);
    await Promise.all(order.map(land => loadLand(land)));
    for (const land of order) {
      for (let guard = 0; guard < 8 && loaded.get(land)?.more && lit.some(x => (x.lands || []).includes(land) && !cy.getElementById(x.id).length); guard++) await loadLand(land, loaded.get(land).page + 1);
    }
    cy.elements().removeClass('hit miss has-hit');
    cy.batch(() => {
      cy.nodes('.mem').forEach(n => n.addClass(hitIds.has(n.id()) ? 'hit' : 'miss'));
      cy.nodes('.territory').forEach(n => { n.toggleClass('miss', !count.has(n.id())); n.toggleClass('has-hit', count.has(n.id())); n.data('hits', count.get(n.id()) || 0); });
      cy.edges().forEach(e => { if (e.hasClass('co') || e.source().hasClass('miss') || e.target().hasClass('miss')) e.addClass('miss'); });
    });
    renderHits(data);
    openPane('hits');
    const targets = cy.nodes('.hit');
    touched = true;
    if (targets.length) goTo(viewFor(targets.boundingBox(), 90, 0.95), 420);
    else if (order.length) goTo(viewFor(cy.getElementById(order[0]).boundingBox(), 60), 420);
    announce();
  }
  const matchLabel = {exact: '原话匹配', strong: '高度相关', weak: '弱相关', keyword: '仅关键词'};
  function renderHits(data) {
    const weak = hitList.length - lit.length, notes = [];
    if (data.degraded) notes.push('语义搜索暂时不可用，已按关键词搜索。');
    if (data.no_strong && hitList.length) notes.push('没有高度相关的记忆，下面这些只是线索。');
    else if (weak) notes.push(`图里亮出了 ${lit.length} 条有把握的；另外 ${weak} 条弱相关只在这里列出，作线索。`);
    const drawn = id => mode === 'lanes' ? lanes.has(id) : cy.getElementById(id).length > 0;
    const missing = hitList.filter(x => !drawn(x.id)).length;
    if (missing) notes.push(`${missing} 条没在图里画出（更早的页没载入），点一下仍可在这里看详情。`);
    if (data.more) notes.push('命中太多，只列出最相关的 150 条，可以换个说法或加上日期范围。');
    ui.set({hits: {
      title: hitList.length ? `搜索到 ${hitList.length} 条${data.more ? '以上' : ''}` : '没有找到匹配的记忆', notes,
      rows: hitList.map(x => ({
        lead: x.occurred || '日期未记', text: short(x.text, 60), cls: `hit-row${x.match === 'weak' && !data.no_strong ? ' is-weak' : ''}`,
        tag: x.match && matchLabel[x.match] ? {kind: x.match, text: matchLabel[x.match]} : null,
        tip: {meta: `${x.occurred || '日期未记'}${x.match && matchLabel[x.match] ? ` · ${matchLabel[x.match]}` : ''}${x.relevance ? ` · 相近度 ${x.relevance}` : ''}`, text: x.text},
        onClick: async () => {
          if (mode === 'lanes') { if (lanes.has(x.id)) selectInLanes(x.id, true); else showStandalone(x.id); return; }
          const n = await reveal(x.id);
          if (n) focusNode(n.id()); else showStandalone(x.id);
        },
      })),
    }});
  }
  // 命中了但没在图里画出的记忆：直接在浮窗里看详情。
  async function showStandalone(mid) {
    clearSelection();
    const token = ++renderToken;
    openPane('item');
    ui.set({item: {title: '记忆', memory: true, loading: true, actions: memoryLinks(mid, backHere(''))}});
    const full = await peek(mid);
    if (token !== renderToken) return;
    ui.set({item: full ? {title: '记忆', memory: true, meta: `${full.occurred || '日期未记'} · ${full.source || '来源未记'}`, text: full.text, context: full.context ? `情境：${full.context}` : '', actions: memoryLinks(mid, backHere(''))} : {title: '记忆', meta: '读不到这条记忆'}});
  }
  // 清空搜索词、日期和时段筛选，回到整张地图。
  function clearAllFilters() {
    for (const input of form.querySelectorAll('input[type=hidden][name=period],input[type=hidden][name=undated]')) input.remove();
    form.elements.q.value = ''; form.elements.from.value = ''; form.elements.through.value = '';
    searchBox.dispatchEvent(new Event('input'));
    runSearch();
  }
  function renderChips() {
    const p = params(), items = [];
    const chip = (text, drop) => ({text, onClick: () => { drop(); runSearch(); }});
    if (p.get('period')) items.push(chip(`时段 ${p.get('period')}`, () => form.querySelector('input[name=period]')?.remove()));
    if (p.get('undated')) items.push(chip('日期未记', () => form.querySelector('input[name=undated]')?.remove()));
    if (p.get('from') || p.get('through')) items.push(chip(`${p.get('from') || '最早'} → ${p.get('through') || '至今'}`, () => { form.elements.from.value = ''; form.elements.through.value = ''; }));
    ui.set({chips: items});
  }
  form.addEventListener('submit', event => { event.preventDefault(); runSearch(); });
  form.querySelector('[data-clear-search]')?.addEventListener('click', () => { if (searching) setTimeout(runSearch, 0); });

  // ---------- 加载 ----------
  async function load() {
    const mark = ++epoch;
    say('正在加载关系图…', true);
    try {
      const response = await fetch('/graph-data?overview=1', {signal: AbortSignal.timeout(12000)});
      if (!response.ok) throw new Error('graph request failed');
      const data = await response.json();
      retries = 0;
      if (mark !== epoch) return;
      cy.elements().remove(); solo = ''; snapshot = data; lanesData = null;
      entityById.clear(); coMap.clear(); coPairs.clear();
      for (const n of data.nodes) if (n.kind === 'entity') entityById.set(n.id, n);
      for (const e of data.edges) if (e.co) coPairs.set(pairKey(e.from, e.to), e.co);
      for (const e of data.edges) if (e.co) for (const [a, b] of [[e.from, e.to], [e.to, e.from]]) { if (!coMap.has(a)) coMap.set(a, []); coMap.get(a).push({id: b, co: e.co}); }
      for (const list of coMap.values()) list.sort((a, b) => b.co - a.co || (a.id < b.id ? -1 : 1));
      loaded.clear(); loading.clear(); setupCo(data);
      put(data);
      layout(false);
      if (mode === 'lanes') await showLanes();
      say(cy.nodes().length ? '' : '还没有实体。接入的 AI 会在读写记忆时整理它们。');
      announce(data.more ? '只画了提及最多的 80 个实体，其余的记忆在「未归类」里' : '');
      renderChips();
      const at = root.dataset.at || '', node = root.dataset.node || '';
      if (Object.keys(Object.fromEntries(params())).some(k => k !== 'strategy')) await runSearch();
      else if (at || node) await locateTarget(at || node);
    } catch {
      if (mark !== epoch) return;
      if (cy.nodes().length) announce('未能加载关系');
      else if (retries++ < 2) { say('未能加载关系图，正在重试…', true); setTimeout(() => { if (mark === epoch && !cy.nodes().length) load(); }, 1500); }
      else say('未能加载关系图，请刷新页面重试');
    }
  }
  const notFound = () => { say('没有找到这条记忆（它可能已被删除）'); setTimeout(() => say(''), 2800); };
  // 从别的页面跳来定位一条记忆或一个实体：地图里飞到它的位置，泳道里滚到它那一天。
  async function locate(mid) {
    const n = await reveal(mid);
    if (n) focusNode(n.id()); else notFound();
  }
  async function locateTarget(target) {
    if (mode === 'lanes') {
      if (entityById.has(target)) pickLane(target); else if (lanes.has(target)) selectInLanes(target, true); else notFound();
    } else if (cy.getElementById(target).hasClass('territory')) focusNode(target);
    else await locate(target);
  }
  // 点在领地内部的空白处（放大后领地只是背景圈）：选中这个领地，浮窗显示实体；点在领地外才取消选择。
  const landAt = pos => cy.nodes('.territory').filter(n => Math.hypot(n.position('x') - pos.x, n.position('y') - pos.y) < n.data('d') / 2).sort((a, b) => a.data('d') - b.data('d'))[0];
  cy.on('tap', event => { if (event.target === cy) { const inside = event.position ? landAt(event.position) : null; if (inside) { canvas.focus({preventScroll: true}); describe(inside.id()); return; } describe(''); if (current === 'item') { openPane(searching ? 'hits' : 'guide'); if (!searching) setPanel(false); } canvas.focus({preventScroll: true}); } });
  cy.on('tap', 'node', event => {
    const node = event.target;
    canvas.focus({preventScroll: true});
    if (node.hasClass('more-tab')) { loadLand(node.data('land'), (loaded.get(node.data('land'))?.page ?? 0) + 1); return; }
    describe(node.id());
    if (node.hasClass('territory')) { if (node.hasClass('t1')) focusNode(node.id()); else loadLand(node.id()); }
  });
  cy.on('tap', 'edge', event => {
    const edge = event.target;
    if (edge.hasClass('thread')) { describe(edge.source().id()); return; }
    describe(edge.source().id());
    const rk = edge.data('rk');
    const text = edge.data('co') ? (edge.data('reason') || '同时出现') : `${rk === 'corrects' ? '更正：' : rk === 'summarizes' ? '汇总：' : ''}${edge.data('reason') || '明确引用'}`;
    edgeNote = text;
    showItem(cy.getElementById(active));
  });
  // 双击：放大进领地，或把卡片放到能读清。详情已经在右边的浮窗里，不再跳页。
  cy.on('dbltap', 'node', event => {
    const node = event.target;
    if (node.hasClass('more-tab')) return;
    describe(node.id());
    if (node.hasClass('territory') && node.id() !== LOOSE) { focusNode(node.id()); return; }
    if (node.hasClass('mem')) { const p = node.position(), zoom = Math.max(cy.zoom(), 1), a = area(); goTo({zoom, pan: {x: a.x0 + a.w / 2 - zoom * p.x, y: a.y0 + a.h / 2 - zoom * p.y}}, 300); }
  });
  // ---------- 切换视图、工具条 ----------
  const tabs = [...root.querySelectorAll('[data-view-tab]')], tidyButton = $('[data-graph-action=tidy]');
  async function showLanes() {
    if (!lanesData) { lanesData = await (await fetch('/graph-lanes', {signal: AbortSignal.timeout(15000)})).json(); lanes.load([...entityById.values()], lanesData); }
    lanes.show();
  }
  const applyModeUi = () => {
    root.dataset.view = mode;
    for (const tab of tabs) tab.setAttribute('aria-pressed', String(tab.dataset.viewTab === mode));
    tidyButton.title = mode === 'lanes' ? '回到初始视图' : '重新整理布局（拖动过的位置也会复位）';
    ui.set({mode, crumb: null});
  };
  async function setMode(next) {
    if (next === mode) return;
    const here = cy.getElementById(active), picked = mode === 'lanes' ? {mid: lanes.selectedId(), entity: lanes.selectedLane()} : {mid: here.hasClass('mem') ? here.id() : '', entity: here.hasClass('territory') ? here.id() : ''};
    calm(); clearSelection();
    mode = next;
    try { localStorage.setItem(VIEW_KEY, mode); } catch { /* 隐私模式下不记 */ }
    applyModeUi();
    syncUrl(params());
    try {
      if (mode === 'lanes') await showLanes(); else { lanes.hide(); cy.resize(); if (!touched) fit(); scheduleChrome(); }
      if (searching) await applyHits(lastHits); else { openPane('guide'); setPanel(false); }
      if (picked.mid) { if (mode === 'lanes') selectInLanes(picked.mid, true); else await locate(picked.mid); }
      else if (picked.entity) { if (mode === 'lanes') pickLane(picked.entity); else focusNode(picked.entity); }
      announce();
    } catch { say('切换视图失败，请刷新页面重试'); setTimeout(() => say(''), 2800); }
  }
  for (const tab of tabs) tab.addEventListener('click', () => setMode(tab.dataset.viewTab));
  function tidy() {
    if (mode === 'lanes') { solo = ''; ui.set({solo}); lanes.setSolo(''); markFilterGone(); lanes.fit(); return; }
    touched = false; calm(); try { localStorage.removeItem(KEY); } catch { /* 忽略 */ }
    cy.nodes('.lazy,.more-tab').remove(); loaded.clear(); layout(true);
    if (searching) runSearch();
  }
  for (const btn of root.querySelectorAll('[data-graph-action]')) btn.addEventListener('click', () => {
    const action = btn.dataset.graphAction, inLanes = mode === 'lanes';
    if (action === 'fit') { if (inLanes) lanes.fit(); else { touched = false; fit(); } }
    else if (action === 'tidy') tidy();
    else if (action === 'panel') { const next = !panelOn(); if (next && current === 'item' && !active && !lanes.selectedId() && !lanes.selectedLane()) openPane(searching ? 'hits' : 'guide'); else setPanel(next); }
    else if (inLanes) lanes.zoom(action === 'in' ? 1.25 : 0.8);
    else { touched = true; cy.zoom({level: cy.zoom() * (action === 'in' ? 1.25 : 0.8), renderedPosition: {x: canvas.clientWidth / 2, y: canvas.clientHeight / 2}}); }
  });
  $('[data-graph-inspector]').querySelector('[data-panel-close]')?.addEventListener('click', () => { setPanel(false); });

  // ---------- 鼠标放上去就看内容：节点或连线旁弹出一张卡片，同时淡化无关的部分 ----------
  const stage = canvas.parentElement;
  function tipOf(ele) {
    const d = ele.data();
    if (ele.isEdge()) return {meta: '关系', text: d.co ? (d.reason || '同时出现') : `${d.rk === 'corrects' ? '更正：' : d.rk === 'summarizes' ? '汇总：' : ''}${d.reason || '明确引用'}`};
    if (d.kind === 'entity') return entityTip(d, coLines(d.id));
    if (d.kind === 'more') return {text: '点一下载入更早的记忆'};
    return memoryTip(d);
  }
  function showTipFor(ele) {
    ui.showTip(tipOf(ele), (tw, th) => {
      const W = stage.clientWidth, H = stage.clientHeight;
      let x, y;
      if (ele.isNode()) {
        const p = ele.renderedPosition(), halfW = ele.renderedOuterWidth() / 2;
        x = p.x + halfW + 12; if (x + tw > W - 8) x = p.x - halfW - 12 - tw;
        y = p.y - th / 2;
      } else {
        const m = ele.renderedMidpoint();
        x = m.x + 12; if (x + tw > W - 8) x = m.x - 12 - tw;
        y = m.y + 12;
      }
      return {x: Math.max(8, Math.min(x, W - tw - 8)), y: Math.max(8, Math.min(y, H - th - 8))};
    });
  }
  // 悬停：停一下才把别的内容压暗（鼠标划过去不闪）；换到相邻的目标直接切换，全程是渐变，不是一下子变。
  let hovered = null, hoverOn = false, hoverTimer = 0, leaveTimer = 0;
  const calm = () => { clearTimeout(hoverTimer); clearTimeout(leaveTimer); hoverOn = false; cy.elements('.faded').removeClass('faded'); ui.hideTip(); };
  function paintHover(ele) {
    hoverOn = true;
    cy.elements('.faded').removeClass('faded');
    cy.elements().not(ele.isNode() ? ele.closedNeighborhood() : ele.union(ele.connectedNodes())).addClass('faded');
    refreshShow();
    showTipFor(ele);
  }
  cy.on('mouseover', 'node,edge', event => {
    const ele = event.target;
    if (dragging || ele.hasClass('thread') || (ele.isNode() && ele.hasClass('territory') && !ele.hasClass('t1'))) return;
    hovered = ele; canvas.style.cursor = 'pointer';
    clearTimeout(leaveTimer); clearTimeout(hoverTimer);
    if (hoverOn) paintHover(ele); else hoverTimer = setTimeout(() => { if (hovered === ele) paintHover(ele); }, 110);
  });
  const leave = () => {
    hovered = null; canvas.style.cursor = ''; clearTimeout(hoverTimer); clearTimeout(leaveTimer);
    leaveTimer = setTimeout(() => { if (!hovered) { calm(); refreshShow(); } }, 140);
  };
  cy.on('mouseout', 'node,edge', leave);
  canvas.addEventListener('mouseleave', leave);
  cy.on('pan zoom', calm);
  cy.on('zoom', syncFar);

  // ---------- 拖动：领地连同里面的记忆一起走，挤到别的领地就把它们推开，松手后位置记住 ----------
  // 拖动时先把里面的记忆和引用线整体收起来（几百个元素跟着每一帧挪会很卡），松手时一次放回新位置。
  let dragging = false, dragRun = null;
  cy.on('grab', 'node', event => {
    const node = event.target;
    dragging = true; calm();
    if (node.hasClass('territory')) { dragRun = {node, from: {...node.position()}, carry: new Map()}; cy.elements('.mem,.more-tab,edge.ref,edge.thread').addClass('carried'); }
  });
  cy.on('drag', 'node.territory', event => {
    if (!dragRun) return;
    const p = event.target.position();
    dragRun.carry.set(dragRun.node.id(), {dx: p.x - dragRun.from.x, dy: p.y - dragRun.from.y});
    relaxAround(dragRun.node.id(), dragRun.carry);
    scheduleChrome();
  });
  cy.on('free', 'node', () => {
    dragging = false;
    if (dragRun) {
      const {carry} = dragRun;
      cy.batch(() => cy.nodes('.carried').forEach(n => { const c = carry.get(n.data('anchor')); if (c) n.position({x: n.position('x') + c.dx, y: n.position('y') + c.dy}); }));
      replaceBridges(new Set(carry.keys()));
      cy.elements('.carried').removeClass('carried');
      remember(); dragRun = null;
    }
    cy.$(':selected').unselect();
    if (active) cy.getElementById(active).select(); // 抓起一个节点会顺手选中它，这里还原成真正选中的那个
    scheduleChrome();
  });

  // ---------- 小地图和「我在哪」 ----------
  let chromeFrame = 0;
  const scheduleChrome = () => { if (!chromeFrame) chromeFrame = requestAnimationFrame(() => { chromeFrame = 0; drawMini(); where(); }); };
  const miniView = {x: 0, y: 0, k: 1};
  const miniColour = (n, css) => {
    const v = name => css.getPropertyValue(name).trim();
    if (n.hasClass('loose')) return v('--g-isolated');
    for (let i = 0; i < 6; i++) if (n.hasClass(`g${i}`)) return v(`--g-c${i}`);
    return v('--g-suggest');
  };
  function drawMini() {
    if (mode === 'lanes') return;
    if (!mini) return;
    const lands = cy.nodes('.territory');
    mini.hidden = !lands.length;
    if (!lands.length) return;
    const ratio = window.devicePixelRatio || 1, w = mini.clientWidth, hh = mini.clientHeight;
    if (!w || !hh) return;
    if (mini.width !== Math.round(w * ratio)) { mini.width = Math.round(w * ratio); mini.height = Math.round(hh * ratio); }
    const g = mini.getContext('2d'), css = getComputedStyle(document.documentElement);
    g.setTransform(ratio, 0, 0, ratio, 0, 0);
    g.clearRect(0, 0, w, hh);
    const box = lands.boundingBox(), pad = 8, k = Math.min((w - pad * 2) / box.w, (hh - pad * 2) / box.h);
    miniView.k = k; miniView.x = pad + (w - pad * 2 - box.w * k) / 2 - box.x1 * k; miniView.y = pad + (hh - pad * 2 - box.h * k) / 2 - box.y1 * k;
    for (const n of lands) {
      const p = n.position(), r = Math.max(1.5, n.data('d') / 2 * k);
      g.beginPath(); g.arc(miniView.x + p.x * k, miniView.y + p.y * k, r, 0, Math.PI * 2);
      g.globalAlpha = n.hasClass('miss') ? 0.12 : 0.5; g.fillStyle = miniColour(n, css); g.fill();
      if (n.hasClass('has-hit') || searching && !n.hasClass('miss')) { g.globalAlpha = 1; g.strokeStyle = css.getPropertyValue('--g-anchor').trim(); g.lineWidth = 2; g.stroke(); }
    }
    g.globalAlpha = 1;
    const ext = cy.extent();
    g.strokeStyle = css.getPropertyValue('--accent').trim(); g.lineWidth = 1.6;
    g.fillStyle = css.getPropertyValue('--accent-soft').trim();
    const rx = miniView.x + ext.x1 * k, ry = miniView.y + ext.y1 * k, rw = (ext.x2 - ext.x1) * k, rh = (ext.y2 - ext.y1) * k;
    g.globalAlpha = 0.35; g.fillRect(rx, ry, rw, rh); g.globalAlpha = 1; g.strokeRect(rx, ry, rw, rh);
  }
  const panMini = event => {
    const box = mini.getBoundingClientRect(), x = (event.clientX - box.left - miniView.x) / miniView.k, y = (event.clientY - box.top - miniView.y) / miniView.k;
    const a = area(), z = cy.zoom();
    touched = true;
    cy.pan({x: a.x0 + a.w / 2 - z * x, y: a.y0 + a.h / 2 - z * y});
  };
  let miniDrag = false;
  mini?.addEventListener('pointerdown', event => { miniDrag = true; mini.setPointerCapture(event.pointerId); panMini(event); });
  mini?.addEventListener('pointermove', event => { if (miniDrag) panMini(event); });
  for (const type of ['pointerup', 'pointercancel']) mini?.addEventListener(type, () => { miniDrag = false; });
  function where() {
    if (mode === 'lanes') return;
    const ext = cy.extent(), cx = (ext.x1 + ext.x2) / 2, cyy = (ext.y1 + ext.y2) / 2;
    let inside = null;
    if (!tier.t1) {
      for (const n of cy.nodes('.territory')) {
        const p = n.position(), r = n.data('d') / 2;
        if (Math.hypot(p.x - cx, p.y - cyy) < r && (!inside || n.data('d') < inside.data('d'))) inside = n;
      }
    }
    const text = inside ? `${inside.data('text')}（${inside.data('count')} 条）` : '';
    if ((ui.state.crumb?.id || '') !== (inside?.id() || '') || (ui.state.crumb?.text || '') !== text) ui.set({crumb: inside ? {id: inside.id(), text} : null});
  }

  // ---------- 输入：滚轮、双指、键盘 ----------
  for (const type of ['pointerdown', 'touchstart']) canvas.addEventListener(type, () => { touched = true; }, {passive: true});
  // 滚轮缩放：按滚动的距离成比例缩放，并限制每一帧的幅度。cytoscape 自带的是「每个滚轮事件固定缩一档」，
  // 触控板或自由滚轮一秒发几十个事件，会瞬间冲过整个缩放范围，一下就缩到没有内容的地方。
  let wheelSum = 0, wheelAt = null, wheelFrame = 0;
  canvas.addEventListener('wheel', event => {
    event.preventDefault(); event.stopPropagation();
    const unit = event.deltaMode === 1 ? 33 : event.deltaMode === 2 ? 400 : 1;
    wheelSum += Math.max(-120, Math.min(120, event.deltaY * unit * (event.ctrlKey ? 8 : 1)));
    const box = canvas.getBoundingClientRect();
    wheelAt = {x: event.clientX - box.left, y: event.clientY - box.top};
    touched = true;
    if (wheelFrame) return;
    wheelFrame = requestAnimationFrame(() => {
      wheelFrame = 0;
      const step = Math.max(-0.2, Math.min(0.2, -wheelSum * 0.0011));
      wheelSum = 0;
      cy.zoom({level: Math.max(cy.minZoom(), Math.min(cy.maxZoom(), cy.zoom() * Math.exp(step))), renderedPosition: wheelAt});
    });
  }, {capture: true, passive: false});
  // 触屏双指缩放（关掉了 cytoscape 自带的缩放，这里补上）。
  let pinch = null;
  const gap = touches => Math.hypot(touches[0].clientX - touches[1].clientX, touches[0].clientY - touches[1].clientY);
  canvas.addEventListener('touchstart', event => { if (event.touches.length === 2) { pinch = {d: Math.max(1, gap(event.touches)), z: cy.zoom()}; touched = true; } }, {passive: true});
  canvas.addEventListener('touchmove', event => {
    if (!pinch || event.touches.length !== 2) return;
    event.preventDefault();
    const box = canvas.getBoundingClientRect(), [a, b] = event.touches;
    cy.zoom({level: Math.max(cy.minZoom(), Math.min(cy.maxZoom(), pinch.z * gap(event.touches) / pinch.d)), renderedPosition: {x: (a.clientX + b.clientX) / 2 - box.left, y: (a.clientY + b.clientY) / 2 - box.top}});
  }, {passive: false});
  for (const type of ['touchend', 'touchcancel']) canvas.addEventListener(type, event => { if (event.touches.length < 2) pinch = null; }, {passive: true});
  // 键盘：方向键在相邻的内容之间移动，回车进入，Esc 取消，加减号缩放，0 适合窗口。
  canvas.addEventListener('keydown', event => {
    if (event.ctrlKey || event.metaKey || event.altKey) return;
    const dirs = {ArrowLeft: [-1, 0], ArrowRight: [1, 0], ArrowUp: [0, -1], ArrowDown: [0, 1]};
    if (dirs[event.key]) {
      event.preventDefault();
      const [dx, dy] = dirs[event.key], from = cy.getElementById(active), z = cy.zoom();
      const pool = z < NEAR ? cy.nodes('.territory') : cy.nodes('.mem,.territory').filter(n => !(n.hasClass('territory') && loaded.has(n.id()) && z >= NEAR) && !n.hasClass('gone') && !n.hasClass('stacked'));
      const ext = cy.extent(), origin = from.length ? from.position() : {x: (ext.x1 + ext.x2) / 2, y: (ext.y1 + ext.y2) / 2};
      let best = null, bestScore = Infinity;
      for (const n of pool) {
        if (n.id() === active) continue;
        const p = n.position(), vx = p.x - origin.x, vy = p.y - origin.y, dist = Math.hypot(vx, vy);
        if (!dist) continue;
        const cos = (vx * dx + vy * dy) / dist;
        if (cos < 0.5) continue;
        const score = dist * (2 - cos);
        if (score < bestScore) { bestScore = score; best = n; }
      }
      if (best) { describe(best.id()); touched = true; const pr = best.renderedPosition(); if (pr.x < 0 || pr.y < 0 || pr.x > canvas.clientWidth || pr.y > canvas.clientHeight) cy.animate({center: {eles: best}}, {duration: reduce ? 0 : 200}); }
    } else if (event.key === 'Enter') {
      if (active) { const n = cy.getElementById(active); if (n.hasClass('territory')) focusNode(active); else { openPane('item'); showItem(n); } }
    } else if (event.key === 'Escape') {
      if (active) { describe(''); if (!searching) setPanel(false); } else if (searching) { form.elements.q.value = ''; searchBox.dispatchEvent(new Event('input')); runSearch(); }
    } else if (event.key === '+' || event.key === '=') cy.zoom({level: cy.zoom() * 1.25, renderedPosition: {x: canvas.clientWidth / 2, y: canvas.clientHeight / 2}});
    else if (event.key === '-') cy.zoom({level: cy.zoom() * 0.8, renderedPosition: {x: canvas.clientWidth / 2, y: canvas.clientHeight / 2}});
    else if (event.key === '0') { touched = false; fit(); }
  });
  new ResizeObserver(() => { cy.resize(); if (!touched) fit(); scheduleChrome(); }).observe(canvas);
  root.querySelector('[data-graph-action=panel]')?.setAttribute('aria-pressed', 'false');
  let stored = '';
  try { stored = localStorage.getItem(VIEW_KEY) || ''; } catch { /* 隐私模式下不记 */ }
  mode = (new URLSearchParams(location.search).get('view') || stored) === 'lanes' ? 'lanes' : 'map';
  applyModeUi();
  openPane('guide'); setPanel(false);
  if (canvas.clientWidth > 0) load();
  else { const waiting = new ResizeObserver(() => { if (canvas.clientWidth > 0) { waiting.disconnect(); load(); } }); waiting.observe(canvas); }
}
