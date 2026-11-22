// 关系图的浮层界面：右侧浮窗（说明 / 搜索结果 / 详情）、提示卡、筛选标签、路径条、分组图例。
// 用 Preact 按状态渲染。画布和图数据仍由 graph.js（Cytoscape）管，它只往这里放「数据」，不拼页面元素。
(() => {
  const {h, render} = preact;
  const html = htm.bind(h);

  // ---------- 两个视图共用的小工具和提示卡内容 ----------
  const short = (text, limit) => { const items = Array.from(String(text ?? '').trim().replace(/\s+/g, ' ')); return items.length > limit ? `${items.slice(0, limit).join('')}…` : items.join(''); };
  const when = iso => { const t = Date.parse(iso); return Number.isNaN(t) ? String(iso || '') : new Date(t).toLocaleString('zh-CN', {hour12: false}); };
  const entityTip = (d, lines = []) => ({meta: `实体 · ${d.count ?? 0} 条记忆${d.last ? ` · 最近 ${d.last}` : ''}`, title: d.text, text: d.hint || '', sub: (d.aliases || []).length ? `别名：${d.aliases.join('、')}` : '', lines});
  // 记忆的提示卡：原文、情境、提到的实体，再加上它的更正 / 汇总关系和被引用的次数，不用点进去就能判断。
  const memoryTip = d => ({
    meta: `${d.occurred || '日期未记'} · ${d.source || '来源未记'}`,
    text: short(d.full || d.text, 360), sub: d.context ? `情境：${short(d.context, 120)}` : '', chips: d.entities || [],
    lines: [
      ...(d.replaces || []).filter(r => r.kind === 'corrects').map(r => ({tag: '更正了', warn: true, text: `${short(r.text, 34)}${r.occurred ? `（${r.occurred}）` : ''}`})),
      ...(d.kids?.length ? [{tag: '汇总', warn: true, text: `汇总了 ${d.desc} 条记忆${d.level > 1 ? `，共 ${d.level} 层` : ''}`}] : []),
      ...(d.replaced_by ? [{tag: d.replaced_by.kind === 'corrects' ? '已被更正' : '已被汇总', warn: true, text: `→ ${short(d.replaced_by.text, 34)}`}] : []),
      ...(d.out || d.ins ? [{tag: '关联', text: [d.out ? `引用 ${d.out} 条` : '', d.ins ? `被 ${d.ins} 条引用` : ''].filter(Boolean).join(' · ')}] : []),
    ],
  });

  // ---------- 提示卡 ----------
  const TipBody = ({d}) => html`
    ${d.meta && html`<p class="tip-meta">${d.meta}</p>`}
    ${d.title && html`<p class="tip-title">${d.title}</p>`}
    ${d.text && html`<p class="tip-text">${d.text}</p>`}
    ${d.sub && html`<p class="tip-sub">${d.sub}</p>`}
    ${d.chips?.length ? html`<p class="tip-chips">${d.chips.map(name => html`<span class="chip chip-entity">${name}</span>`)}</p>` : null}
    ${d.lines?.length ? html`<div class="tip-lines">${d.lines.map(l => html`<p class=${'tip-line' + (l.warn ? ' is-warn' : '')}><b>${l.tag}</b>${l.text}</p>`)}</div>` : null}`;
  const Tip = ({tip}) => html`<div class="graph-tip" role="tooltip" hidden=${!tip}>${tip && html`<${TipBody} d=${tip}/>`}</div>`;

  // ---------- 浮窗里的零件 ----------
  const Block = ({title, cls, children}) => html`<div class=${'item-block' + (cls ? ' ' + cls : '')}><h4>${title}</h4>${children}</div>`;
  const hoverTip = (ui, tip) => tip ? {
    onMouseEnter: event => ui.tipBeside(tip, event.currentTarget),
    onFocus: event => ui.tipBeside(tip, event.currentTarget),
    onMouseLeave: ui.hideTip,
    onBlur: ui.hideTip,
  } : {};
  const Row = ({row, ui}) => html`
    <button type="button" class=${'pick-row' + (row.cls ? ' ' + row.cls : '')} data-tone=${row.tone || null} onClick=${row.onClick} ...${hoverTip(ui, row.tip)}>
      ${row.dot ? html`<b class="dot entity"></b>` : html`<small>${row.lead}</small>`}<span>${row.text}${row.sub ? html`<i class="row-sub">${row.sub}</i>` : null}</span>${row.tag ? html`<em class=${'hit-tag is-' + row.tag.kind}>${row.tag.text}</em>` : null}
    </button>`;
  const Rows = ({rows, ui}) => html`<div class="pick-list">${rows.map(row => html`<${Row} row=${row} ui=${ui}/>`)}</div>`;
  const EntityChip = ({chip, ui}) => chip.onClick
    ? html`<button type="button" class="chip chip-entity" onClick=${chip.onClick} ...${hoverTip(ui, chip.tip)}>${chip.label}</button>`
    : html`<span class=${'chip chip-entity' + (chip.open ? ' is-open' : '')} tabindex="0" ...${hoverTip(ui, chip.tip)}>${chip.label}</span>`;

  // ---------- 三种内容 ----------
  const Guide = () => html`
    <section class="pane" data-pane="guide">
      <h2>怎么用</h2>
      <ul class="guide-list">
        <li><b>缩小</b>看每个实体的领地：圆越大，只属于它的记忆越多；颜色相同的经常一起出现，它们之间的空隙越宽，共享的记忆越多。</li>
        <li><b>放大进一个实体</b>，只属于它的记忆一条条出现：离中心越近越新，意思相近的坐在同一侧；被更正的旧记忆叠在新记忆下面。</li>
        <li><b>提到多个实体的记忆</b>画成六边形的「桥」，停在它们所属的领地之间，细线连着每个领地。</li>
        <li><b>汇总</b>是接入的 AI 写的：一条汇总连同它汇总的记忆排成一团，用虚线圈圈住；缩小时只看到一个写着条数的圆，放大一层才展开一层，汇总的汇总同理。</li>
        <li><b>点一下</b>看详情和相连的内容；<b>双击</b>放大过去；拖动领地可以挪位置。</li>
        <li><b>上方搜索</b>不会离开这里：命中的记忆发光，其余淡下去，右边列出结果。</li>
        <li>键盘：方向键移动，回车进入，Esc 取消，+ − 缩放，0 适合窗口，/ 搜索。</li>
      </ul>
    </section>`;

  const Hits = ({hits, ui}) => html`
    <section class="pane" data-pane="hits">
      <div class="pane-head"><h2 data-hits-title>${hits.title}</h2><button type="button" class="btn btn-ghost btn-sm" data-hits-clear onClick=${ui.act.clearSearch}>清除搜索</button></div>
      ${hits.notes.length ? html`<p class="graph-meta" data-hits-note>${hits.notes.join(' ')}</p>` : null}
      <div class="pick-list" data-hits-list>${hits.rows.map(row => html`<${Row} row=${row} ui=${ui}/>`)}</div>
    </section>`;

  const Item = ({item, ui}) => html`
    <section class="pane" data-pane="item">
      <div class="pane-head"><h2 data-item-title>${item.title}</h2></div>
      <div class="item-body" data-item-body>
        ${item.edge ? html`<p class="graph-edge-reason">${item.edge}</p>` : null}
        ${item.meta ? html`<p class="graph-meta">${item.meta}</p>` : null}
        ${item.name ? html`<h3 class="item-name">${item.name}</h3>` : null}
        ${item.text ? html`<p class=${'graph-text' + (item.memory ? ' item-text' : '')}>${item.text}</p>` : null}
        ${item.context ? html`<p class="graph-context">${item.context}</p>` : null}
        ${item.entities?.length ? html`<${Block} title="提到的实体"><div class="chip-row">${item.entities.map(chip => html`<${EntityChip} chip=${chip} ui=${ui}/>`)}</div><//>` : null}
        ${item.note ? html`<p class="graph-meta">${item.note}</p>` : null}
        ${item.facts ? html`<dl class="item-facts">${item.facts.map(([k, v, code]) => html`<div><dt>${k}</dt><dd>${code ? html`<code>${v}</code>` : v}</dd></div>`)}</dl>` : null}
        ${item.loading ? html`<p class="graph-meta">正在读取完整内容…</p>` : null}
        ${(item.blocks || []).map(b => html`<${Block} title=${b.title} cls=${b.cls}>
          ${b.rows ? html`<${Rows} rows=${b.rows} ui=${ui}/>` : html`<div class="chip-row">${b.files.map(f => html`<a class="chip chip-soft" href=${f.url} target="_blank" rel="noopener">${f.name}</a>`)}</div>`}
        <//>`)}
      </div>
      <div class="graph-inspector-actions" data-item-actions>
        ${(item.actions || []).map(a => html`<a class=${'btn btn-sm' + (a.primary ? ' btn-primary' : '')} href=${a.href}>${a.text}</a>`)}
      </div>
    </section>`;

  const Inspector = ({s, ui}) => s.pane === 'item' && s.item ? html`<${Item} item=${s.item} ui=${ui}/>`
    : s.pane === 'hits' && s.hits ? html`<${Hits} hits=${s.hits} ui=${ui}/>` : html`<${Guide}/>`;

  // ---------- 图例：随当前视图换内容 ----------
  const Glyph = ({kind}) => ({
    line: html`<svg class="lg-glyph" viewBox="0 0 22 12"><path d="M1 6h20" class="lg-stroke"/></svg>`,
    ring: html`<svg class="lg-glyph" viewBox="0 0 22 12"><circle cx="11" cy="6" r="5" class="lg-ring"/></svg>`,
    bridge: html`<svg class="lg-glyph" viewBox="0 0 22 12"><path d="M11 0.8 15.6 3.4V8.6L11 11.2 6.4 8.6V3.4Z" class="lg-hex"/></svg>`,
    summary: html`<svg class="lg-glyph" viewBox="0 0 22 12"><circle cx="11" cy="6" r="5.4" class="lg-sum"/><circle cx="11" cy="6" r="1.8" class="lg-sum-core"/></svg>`,
    bracket: html`<svg class="lg-glyph" viewBox="0 0 22 12"><path d="M2 9V4h18v5" class="lg-stroke lg-sum-line"/><circle cx="20" cy="4" r="2.2" class="lg-sum-core"/></svg>`,
    dot: html`<svg class="lg-glyph" viewBox="0 0 22 12"><circle cx="11" cy="6" r="4" class="lg-fill"/></svg>`,
    span: html`<svg class="lg-glyph" viewBox="0 0 22 12"><path d="M11 1v10" class="lg-stroke"/><circle cx="11" cy="2" r="2.4" class="lg-fill"/><circle cx="11" cy="10" r="2.4" class="lg-fill"/></svg>`,
    warn: html`<svg class="lg-glyph" viewBox="0 0 22 12"><path d="M1 6h20" class="lg-stroke lg-warn"/></svg>`,
    band: html`<svg class="lg-glyph" viewBox="0 0 22 12"><rect x="3" y="1" width="16" height="10" rx="2" class="lg-band"/></svg>`,
  })[kind];
  const LEGENDS = {
    map: [['ring', '实体的领地'], ['dot', '记忆'], ['bridge', '桥：提到多个实体的记忆'], ['summary', '汇总：圈住它汇总的记忆'], ['line', '引用']],
    lanes: [['dot', '记忆'], ['span', '一条记忆提到多个实体'], ['bracket', '汇总：括住它汇总的那段时间'], ['warn', '更正'], ['band', '日期未记']],
  };
  const Legend = ({mode}) => LEGENDS[mode].map(([kind, text]) => html`<span class="lg-item"><${Glyph} kind=${kind}/>${text}</span>`);

  // ---------- 筛选标签、路径条、分组图例 ----------
  const Chips = ({chips, ui}) => chips.length ? html`
    ${chips.map(c => html`<button type="button" class="chip chip-x" title="移除此筛选" onClick=${c.onClick}>${c.text}<svg class="icon" aria-hidden="true"><use href="#x"/></svg></button>`)}
    <button type="button" class="btn btn-ghost btn-sm" onClick=${ui.act.clearAll}>清除全部</button>` : null;
  const Crumbs = ({crumb, ui}) => crumb ? html`
    <button type="button" data-crumb-root onClick=${ui.act.crumbRoot}>全局</button><span aria-hidden="true">›</span>
    <button type="button" data-crumb-here onClick=${ui.act.crumbHere}><span data-crumb-land>${crumb.text}</span></button>` : null;
  const Groups = ({groups, solo, ui}) => groups.map(g => html`
    <button type="button" data-group=${String(g.group)} aria-pressed=${String(String(g.group) === solo)} title="只看这一组；再点一次回到全部" onClick=${() => ui.act.pickGroup(String(g.group))}>
      <b class=${'dot g' + g.group}></b>${g.name} 等 ${g.size} 个
    </button>`);

  window.RPGraphUI = {
    short, when, memoryTip, entityTip,
    // roots: panel / tip / chips / crumbs / groups / legend 这几个挂载点；act: 页面控制器（graph.js）提供的动作。
    mount(roots, act) {
      const state = {pane: 'guide', item: null, hits: null, chips: [], crumb: null, groups: [], solo: '', tip: null, mode: 'map'};
      const draws = {
        pane: () => render(html`<${Inspector} s=${state} ui=${ui}/>`, roots.panel),
        item: () => draws.pane(), hits: () => draws.pane(),
        tip: () => render(html`<${Tip} tip=${state.tip}/>`, roots.tip),
        chips: () => render(html`<${Chips} chips=${state.chips} ui=${ui}/>`, roots.chips),
        crumb: () => { roots.crumbs.hidden = !state.crumb; render(html`<${Crumbs} crumb=${state.crumb} ui=${ui}/>`, roots.crumbs); },
        groups: () => render(html`<${Groups} groups=${state.groups} solo=${state.solo} ui=${ui}/>`, roots.groups),
        solo: () => draws.groups(),
        mode: () => render(html`<${Legend} mode=${state.mode}/>`, roots.legend),
      };
      const tipEl = () => roots.tip.firstElementChild;
      const ui = {
        act, state,
        set(patch) {
          // 浮窗内容换了，鼠标下的条目可能已经不在了（不会再触发 mouseleave），提示卡要一起收掉。
          if (state.tip && ('pane' in patch || 'item' in patch || 'hits' in patch)) { state.tip = null; draws.tip(); }
          Object.assign(state, patch);
          for (const fn of new Set(Object.keys(patch).map(key => draws[key]).filter(Boolean))) fn();
        },
        // 在画布上的节点旁边弹出：place(宽, 高) 返回 {x, y}。
        showTip(data, place) {
          state.tip = data; draws.tip();
          const el = tipEl(), {x, y} = place(el.offsetWidth, el.offsetHeight);
          el.style.left = `${x}px`; el.style.top = `${y}px`;
        },
        // 浮窗里的条目：弹在整个浮窗的左边，不盖住浮窗本身。
        tipBeside(data, target) {
          const stage = roots.tip.offsetParent || roots.tip.parentElement, aside = roots.panel.closest('aside');
          ui.showTip(data, (tw, th) => {
            const sr = stage.getBoundingClientRect(), r = target.getBoundingClientRect(), panel = aside.getBoundingClientRect();
            let x = panel.left - sr.left - tw - 10;
            if (x < 8) x = Math.min(r.right - sr.left + 10, stage.clientWidth - tw - 8);
            return {x: Math.max(8, x), y: Math.max(8, Math.min(r.top - sr.top - 4, stage.clientHeight - th - 8))};
          });
        },
        hideTip() { if (state.tip) { state.tip = null; draws.tip(); } },
      };
      for (const key of ['pane', 'tip', 'chips', 'crumb', 'groups', 'mode']) draws[key]();
      return ui;
    },
  };
})();
