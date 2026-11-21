// 时间泳道：横轴是时间，每个实体一条道。一条记忆只画一个点，它提到几个实体，竖线就穿过几条道。
// 和「地图」共用搜索、详情浮窗和提示卡；这里只管画布（Canvas 2D）：排版、缩放平移、悬停、选中。
(() => {
  const {memoryTip} = RPGraphUI;
  const FONT = 'system-ui, "Microsoft YaHei UI", sans-serif';
  const LABEL = 176, LANE = 30, AXIS = 46, DAY = 86400000;
  const reduce = matchMedia('(prefers-reduced-motion: reduce)').matches;
  const ease = t => t < 0.5 ? 4 * t * t * t : 1 - Math.pow(-2 * t + 2, 3) / 2;
  const css = () => getComputedStyle(document.documentElement);
  const parseDay = text => {
    const m = /^(\d{4})(?:-(\d{2}))?(?:-(\d{2}))?/.exec(text || '');
    return m ? Date.UTC(+m[1], m[2] ? +m[2] - 1 : 0, m[3] ? +m[3] : 1) / 86400000 : null;
  };
  const dayText = (day, precise) => { const d = new Date(day * DAY), p = n => String(n).padStart(2, '0'); return precise ? `${d.getUTCFullYear()}-${p(d.getUTCMonth() + 1)}-${p(d.getUTCDate())}` : `${d.getUTCFullYear()}-${p(d.getUTCMonth() + 1)}`; };
  const hash = text => { let h = 0; for (const ch of text) h = (h * 31 + ch.charCodeAt(0)) >>> 0; return h; };

  window.RPLanes = {
    // host：canvas、共用的 ui、可视区域 area()（避开顶部搜索条和右侧浮窗）、实体的提示卡内容，以及选中时的回调。
    mount({canvas, ui, area, entityTipOf, onSelect, onPickEntity}) {
      const ctx = canvas.getContext('2d');
      let W = 0, H = 0, DPR = 1, shown = false, frame = 0;
      let lanes = [], laneOf = new Map(), mems = [], byId = new Map(), lo = 0, hi = 1, hasUndated = false;
      let view = {sx: 1, tx: 0, ty: 0}, motion = null;
      let hits = null, solo = '', selected = null, laneSel = '', hover = null, hoverLane = '', hoverAmt = 0, hoverGoal = 0, pointer = null;
      let marks = [], segments = [], hoverTimer = 0, leaveTimer = 0;

      const colour = name => css().getPropertyValue(name).trim();
      // 名字放不下时截断并加省略号
      const clip = (text, max) => {
        if (ctx.measureText(text).width <= max) return text;
        const chars = Array.from(text);
        while (chars.length > 1 && ctx.measureText(`${chars.join('')}…`).width > max) chars.pop();
        return `${chars.join('')}…`;
      };
      const laneColour = lane => Number.isInteger(lane.e.group) ? colour(`--g-c${lane.e.group}`) : colour('--g-isolated');
      const visibleLanes = () => lanes.filter(l => !solo || String(l.e.group) === solo);
      const dayOf = m => m.day ?? (lo + (hash(m.id) % 1000) / 1000 * (hasUndated ? (hi - lo) * 0.05 : 0));
      const geometry = () => { const a = area(); return {a, left: LABEL, top: a.y0 + AXIS, right: a.w - 14, bottom: a.y1}; };
      const xOf = day => LABEL + view.tx + (day - lo) * view.sx;
      const yOf = (index, g) => g.top + view.ty + index * LANE + LANE / 2;
      const dayAt = x => lo + (x - LABEL - view.tx) / view.sx;
      const maxTy = g => Math.min(0, g.bottom - g.top - lanes.length * LANE);
      const clampTy = (ty, g) => Math.max(maxTy(g), Math.min(0, ty));

      const resize = () => {
        DPR = window.devicePixelRatio || 1; W = canvas.clientWidth; H = canvas.clientHeight;
        canvas.width = Math.round(W * DPR); canvas.height = Math.round(H * DPR);
        schedule();
      };
      new ResizeObserver(resize).observe(canvas);
      const schedule = () => { if (!frame && shown) frame = requestAnimationFrame(step); };
      function step(now) {
        frame = 0;
        let again = false;
        if (motion) {
          const t = Math.min(1, (now - motion.t0) / motion.ms), k = ease(t);
          view = {sx: motion.from.sx * Math.pow(motion.to.sx / motion.from.sx, k), tx: motion.from.tx + (motion.to.tx - motion.from.tx) * k, ty: motion.from.ty + (motion.to.ty - motion.from.ty) * k};
          if (t >= 1) motion = null; else again = true;
        }
        if (hoverAmt !== hoverGoal) { hoverAmt += Math.sign(hoverGoal - hoverAmt) * Math.min(Math.abs(hoverGoal - hoverAmt), 0.12); again = true; }
        if (selected && !reduce) again = true; // 选中的那一圈在轻轻呼吸
        draw(now);
        if (again) schedule();
      }
      const goTo = (to, ms = 380) => {
        if (reduce || !ms) { view = to; motion = null; schedule(); return; }
        motion = {from: {...view}, to, ms, t0: performance.now()}; schedule();
      };

      // 视图：适合窗口 / 缩放到一段时间 / 把某条记忆放到中间
      const fitView = () => { const g = geometry(); const sx = Math.max(0.02, (g.right - g.left - 24) / (hi - lo)); return {sx, tx: 12, ty: 0}; };
      const spanView = (d0, d1, lane) => {
        const g = geometry(), width = Math.max(30, d1 - d0), sx = Math.min(400, (g.right - g.left - 80) / width);
        const to = {sx, tx: 40 - (Math.min(d0, d1) - lo) * sx, ty: view.ty};
        if (lane >= 0) to.ty = clampTy((g.bottom - g.top) / 2 - (lane + 0.5) * LANE, g);
        return to;
      };
      const zoomAt = (factor, x) => {
        const g = geometry(), floor = fitView().sx * 0.6, sx = Math.max(floor, Math.min(400, view.sx * factor));
        const day = dayAt(x ?? (g.left + g.right) / 2);
        goTo({sx, tx: (x ?? (g.left + g.right) / 2) - LABEL - (day - lo) * sx, ty: view.ty}, 160);
      };

      // 刻度随缩放换挡：年 → 季 → 月 → 周 → 日。major 是年或月的起点。
      function timeTicks(g) {
        const ticks = [], perDay = view.sx, perYear = 365 * perDay;
        if (perDay >= 9) {
          for (let day = Math.floor(dayAt(g.left)) - 1; day <= Math.ceil(dayAt(W)) + 1; day++) {
            const d = new Date(day * DAY);
            if (perDay < 40 && d.getUTCDay() !== 1) continue;
            const first = d.getUTCDate() === 1;
            ticks.push({x: xOf(day), major: first, text: first ? `${d.getUTCFullYear()}-${String(d.getUTCMonth() + 1).padStart(2, '0')}` : `${d.getUTCDate()}日`});
          }
          return ticks.filter(t => t.x >= g.left - 2 && t.x <= W);
        }
        const step = perYear > 1500 ? 1 : perYear > 520 ? 3 : 12;
        for (let y = new Date(lo * DAY).getUTCFullYear(); y <= new Date(hi * DAY).getUTCFullYear() + 1; y++) for (let m = 0; m < 12; m += step) {
          const x = xOf(Date.UTC(y, m, 1) / DAY);
          if (x >= g.left - 2 && x <= W) ticks.push({x, major: m === 0, text: m === 0 ? String(y) : `${m + 1}月`});
        }
        return ticks;
      }

      function draw(now = 0) {
        ctx.setTransform(DPR, 0, 0, DPR, 0, 0);
        ctx.clearRect(0, 0, W, H);
        const g = geometry(), list = visibleLanes(), rows = new Map(list.map((l, i) => [l.e.id, i]));
        ctx.fillStyle = colour('--surface-2'); ctx.fillRect(0, 0, W, H);
        // 画布主体只在泳道区域里画，滚动时不盖住时间轴和泳道名
        ctx.save(); ctx.beginPath(); ctx.rect(g.left, g.top, W - g.left, g.bottom - g.top); ctx.clip();
        const border = colour('--border'), strong = colour('--border-strong'), text3 = colour('--text-3');
        list.forEach((lane, i) => {
          const y = yOf(i, g) - LANE / 2;
          if (y > H || y + LANE < g.top) return;
          if (i % 2) { ctx.globalAlpha = 0.45; ctx.fillStyle = colour('--hover'); ctx.fillRect(g.left, y, W - g.left, LANE); ctx.globalAlpha = 1; }
          if (lane.e.id === laneSel || lane.e.id === hoverLane) { ctx.globalAlpha = lane.e.id === laneSel ? 0.16 : 0.09; ctx.fillStyle = colour('--accent'); ctx.fillRect(g.left, y, W - g.left, LANE); ctx.globalAlpha = 1; }
          if (i && list[i - 1].e.group !== lane.e.group) { ctx.strokeStyle = strong; ctx.beginPath(); ctx.moveTo(g.left, y + 0.5); ctx.lineTo(W, y + 0.5); ctx.stroke(); }
        });
        // 日期未记的那一段
        if (hasUndated) {
          const x0 = xOf(lo), x1 = xOf(lo + (hi - lo) * 0.05);
          ctx.globalAlpha = 0.35; ctx.fillStyle = colour('--hover'); ctx.fillRect(x0, g.top, x1 - x0, H - g.top); ctx.globalAlpha = 1;
          ctx.strokeStyle = strong; ctx.setLineDash([4, 4]); ctx.beginPath(); ctx.moveTo(x1, g.top); ctx.lineTo(x1, H); ctx.stroke(); ctx.setLineDash([]);
        }
        // 刻度线：年、季、月随缩放换挡
        const ticks = timeTicks(g);
        ctx.lineWidth = 1;
        for (const t of ticks) { ctx.strokeStyle = t.major ? strong : border; ctx.globalAlpha = t.major ? 0.9 : 0.55; ctx.beginPath(); ctx.moveTo(t.x + 0.5, g.top); ctx.lineTo(t.x + 0.5, H); ctx.stroke(); }
        ctx.globalAlpha = 1;
        // 更正与汇总：新记忆指向旧记忆的线
        ctx.lineWidth = 1.5; ctx.strokeStyle = colour('--warn');
        for (const m of mems) for (const r of m.refs) {
          const o = byId.get(r.to), a = rows.get(m.lanes[0]?.id), b = o && rows.get(o.lanes[0]?.id);
          if (!o || a === undefined || b === undefined) continue;
          const x0 = xOf(dayOf(m)), x1 = xOf(dayOf(o));
          if ((x0 < g.left && x1 < g.left) || (x0 > W && x1 > W)) continue;
          ctx.globalAlpha = hits && !(hits.has(m.id) || hits.has(o.id)) ? 0.08 : 0.7; ctx.setLineDash(r.kind === 'summarizes' ? [5, 4] : []);
          ctx.beginPath(); ctx.moveTo(x0, yOf(a, g)); ctx.lineTo(x1, yOf(b, g)); ctx.stroke();
        }
        ctx.setLineDash([]); ctx.globalAlpha = 1;
        // 记忆：竖线加每条道上一个点
        marks = []; segments = [];
        const surface = colour('--surface-2'), pulse = reduce ? 0 : (Math.sin(now / 420) + 1) / 2;
        for (const m of mems) {
          const rowsOf = m.lanes.map(l => rows.get(l.e.id)).filter(i => i !== undefined);
          if (!rowsOf.length) continue;
          const x = xOf(dayOf(m));
          if (x < g.left - 12 || x > W + 12) continue;
          const first = Math.min(...rowsOf), last = Math.max(...rowsOf);
          if (yOf(last, g) < g.top - 10 || yOf(first, g) > H + 10) continue;
          const isHover = m === hover, isSel = m === selected;
          let alpha = hits ? (hits.has(m.id) ? 1 : 0.25) : 0.92;
          if (laneSel && !hits && !m.lanes.some(l => l.e.id === laneSel)) alpha = 0.35;
          if (hoverAmt > 0 && !isHover && !isSel) alpha *= 1 - 0.5 * hoverAmt;
          const c = laneColour(m.lanes[0]);
          ctx.globalAlpha = alpha; ctx.strokeStyle = c; ctx.fillStyle = c;
          if (rowsOf.length > 1) { ctx.lineWidth = isHover || isSel ? 3 : 1.7; ctx.beginPath(); ctx.moveTo(x, yOf(first, g)); ctx.lineTo(x, yOf(last, g)); ctx.stroke(); segments.push({m, x, y0: yOf(first, g), y1: yOf(last, g)}); }
          for (const i of rowsOf) {
            const y = yOf(i, g);
            ctx.beginPath(); ctx.arc(x, y, isHover || isSel ? 6 : 4, 0, Math.PI * 2); ctx.fill();
            ctx.lineWidth = 1.5; ctx.strokeStyle = surface; ctx.stroke();
            if (isSel) { ctx.strokeStyle = colour('--text'); ctx.lineWidth = 2; ctx.globalAlpha = 0.5 + 0.5 * pulse; ctx.beginPath(); ctx.arc(x, y, 9 + 2 * pulse, 0, Math.PI * 2); ctx.stroke(); }
            ctx.strokeStyle = c; ctx.globalAlpha = alpha;
            marks.push({m, x, y});
          }
        }
        ctx.globalAlpha = 1; ctx.lineWidth = 1;
        // 指针所在的那一天
        if (pointer && pointer.x > g.left && pointer.y > g.top) { ctx.globalAlpha = 0.4; ctx.strokeStyle = colour('--text-3'); ctx.setLineDash([2, 4]); ctx.beginPath(); ctx.moveTo(pointer.x + 0.5, g.top); ctx.lineTo(pointer.x + 0.5, H); ctx.stroke(); ctx.setLineDash([]); ctx.globalAlpha = 1; }
        // 滚动条
        const total = list.length * LANE, room = g.bottom - g.top;
        if (total > room) { const h = Math.max(24, room * room / total), y = g.top + (-view.ty / (total - room)) * (room - h); ctx.globalAlpha = 0.35; ctx.fillStyle = colour('--text-3'); ctx.beginPath(); ctx.roundRect(W - 7, y, 4, h, 2); ctx.fill(); ctx.globalAlpha = 1; }
        ctx.restore();

        // 泳道名：固定在左边
        ctx.fillStyle = colour('--surface'); ctx.fillRect(0, g.top, LABEL, g.bottom - g.top);
        ctx.strokeStyle = border; ctx.beginPath(); ctx.moveTo(LABEL + 0.5, g.top); ctx.lineTo(LABEL + 0.5, g.bottom); ctx.stroke();
        ctx.save(); ctx.beginPath(); ctx.rect(0, g.top, LABEL, g.bottom - g.top); ctx.clip();
        ctx.textBaseline = 'middle';
        list.forEach((lane, i) => {
          const y = yOf(i, g); if (y < g.top - LANE || y > H + LANE) return;
          const on = lane.e.id === laneSel, over = lane.e.id === hoverLane;
          if (on || over) { ctx.globalAlpha = on ? 0.16 : 0.09; ctx.fillStyle = colour('--accent'); ctx.fillRect(0, y - LANE / 2, LABEL, LANE); ctx.globalAlpha = 1; }
          ctx.fillStyle = laneColour(lane); ctx.beginPath(); ctx.roundRect(10, y - 7, 4, 14, 2); ctx.fill();
          ctx.font = `${on ? 600 : 500} 12.5px ${FONT}`; ctx.fillStyle = colour('--text'); ctx.textAlign = 'left'; ctx.fillText(clip(lane.e.text, LABEL - 66), 22, y);
          ctx.font = `11px ${FONT}`; ctx.fillStyle = text3; ctx.textAlign = 'right'; ctx.fillText(String(lane.e.count), LABEL - 10, y);
        });
        ctx.textAlign = 'left'; ctx.restore();

        // 时间轴：固定在顶部，泳道从它下面滚过
        ctx.fillStyle = colour('--surface'); ctx.fillRect(0, g.a.y0, W, AXIS);
        ctx.strokeStyle = border; ctx.beginPath(); ctx.moveTo(0, g.a.y0 + AXIS - 0.5); ctx.lineTo(W, g.a.y0 + AXIS - 0.5); ctx.stroke();
        ctx.save(); ctx.beginPath(); ctx.rect(g.left, g.a.y0, W - g.left, AXIS); ctx.clip();
        ctx.textBaseline = 'middle';
        for (const t of ticks) {
          ctx.strokeStyle = t.major ? strong : border; ctx.beginPath(); ctx.moveTo(t.x + 0.5, g.a.y0 + (t.major ? 8 : 24)); ctx.lineTo(t.x + 0.5, g.a.y0 + AXIS); ctx.stroke();
          ctx.font = `${t.major ? 600 : 400} ${t.major ? 13 : 11}px ${FONT}`; ctx.fillStyle = t.major ? colour('--text') : text3;
          ctx.fillText(t.text, t.x + 5, g.a.y0 + (t.major ? 16 : 34));
        }
        if (!ticks.some(t => t.major && t.x > g.left + 60)) { ctx.font = `600 13px ${FONT}`; ctx.fillStyle = colour('--text'); ctx.fillText(dayText(dayAt(g.left + 2), false), g.left + 8, g.a.y0 + 16); }
        if (hasUndated) { ctx.font = `11px ${FONT}`; ctx.fillStyle = text3; ctx.fillText('日期未记', Math.max(g.left + 6, xOf(lo) + 6), g.a.y0 + 34); }
        if (pointer && pointer.x > g.left && pointer.y > g.top) {
          const label = dayText(dayAt(pointer.x), view.sx > 3), w = ctx.measureText(label).width + 14, x = Math.max(g.left + 2, Math.min(pointer.x - w / 2, W - w - 6));
          ctx.fillStyle = colour('--accent'); ctx.beginPath(); ctx.roundRect(x, g.a.y0 + AXIS - 22, w, 18, 9); ctx.fill();
          ctx.font = `600 11px ${FONT}`; ctx.fillStyle = colour('--on-accent'); ctx.textAlign = 'center'; ctx.fillText(label, x + w / 2, g.a.y0 + AXIS - 13); ctx.textAlign = 'left';
        }
        ctx.restore();
        ctx.fillStyle = text3; ctx.font = `11px ${FONT}`; ctx.textBaseline = 'middle'; ctx.fillText('实体', 12, g.a.y0 + AXIS - 14);
      }

      // ---------- 悬停：停一下才压暗别的，移得快不闪；换到相邻的点是直接切换 ----------
      // 点或悬停：先看圆点，没有再看竖线（线很细，两边各放宽几个像素）
      const nearest = (x, y) => {
        if (y > geometry().bottom) return null; // 被图例卡片盖住的那一块不算
        let best = null, d = 11;
        for (const k of marks) { const dist = Math.hypot(k.x - x, k.y - y); if (dist < d) { d = dist; best = k.m; } }
        if (best) return best;
        d = 7;
        for (const s of segments) { const dist = Math.abs(s.x - x); if (y >= s.y0 - 2 && y <= s.y1 + 2 && dist < d) { d = dist; best = s.m; } }
        return best;
      };
      const laneAtY = y => { const g = geometry(), i = Math.floor((y - g.top - view.ty) / LANE), list = visibleLanes(); return y > g.top && i >= 0 && i < list.length ? list[i] : null; };
      const tipPlace = (x, y) => (tw, th) => ({x: Math.max(8, Math.min(x + 16, W - tw - 8)), y: Math.max(8, Math.min(y + 16, H - th - 8))});
      const setHover = (m, x, y) => {
        hover = m; hoverGoal = m ? 1 : 0;
        if (m) ui.showTip(memoryTip(m), tipPlace(x, y)); else ui.hideTip();
        canvas.style.cursor = m ? 'pointer' : '';
        schedule();
      };
      const pointOver = (x, y) => {
        const g = geometry(), m = x > g.left && y > g.top ? nearest(x, y) : null;
        clearTimeout(leaveTimer);
        if (m === hover) return;
        clearTimeout(hoverTimer);
        if (!m) { leaveTimer = setTimeout(() => setHover(null), 140); return; }
        if (hover) setHover(m, x, y); else hoverTimer = setTimeout(() => setHover(m, x, y), 100);
      };
      const overLabel = (x, y) => { const lane = x < LABEL ? laneAtY(y) : null, id = lane ? lane.e.id : ''; if (id !== hoverLane) { hoverLane = id; canvas.style.cursor = id ? 'pointer' : ''; if (id) ui.showTip(entityTipOf(lane.e.id), (tw, th) => ({x: LABEL + 10, y: Math.max(8, Math.min(y - th / 2, H - th - 8))})); else ui.hideTip(); schedule(); } };

      // ---------- 指针：拖动平移、点选、双指缩放 ----------
      const touches = new Map();
      let drag = null, pinch = null;
      const local = e => { const box = canvas.getBoundingClientRect(); return {x: e.clientX - box.left, y: e.clientY - box.top}; };
      canvas.addEventListener('pointerdown', e => {
        const p = local(e); touches.set(e.pointerId, p); canvas.setPointerCapture(e.pointerId); canvas.focus({preventScroll: true});
        if (touches.size === 2) { const [a, b] = [...touches.values()]; pinch = {d: Math.max(1, Math.hypot(a.x - b.x, a.y - b.y)), sx: view.sx}; drag = null; return; }
        motion = null; drag = {x: p.x, y: p.y, tx: view.tx, ty: view.ty, moved: false};
      });
      canvas.addEventListener('pointermove', e => {
        const p = local(e); pointer = p;
        if (touches.has(e.pointerId)) touches.set(e.pointerId, p);
        if (pinch && touches.size === 2) { const [a, b] = [...touches.values()], x = (a.x + b.x) / 2, day = dayAt(x), sx = Math.max(fitView().sx * 0.6, Math.min(400, pinch.sx * Math.hypot(a.x - b.x, a.y - b.y) / pinch.d)); view = {sx, tx: x - LABEL - (day - lo) * sx, ty: view.ty}; schedule(); return; }
        if (drag) {
          if (Math.abs(p.x - drag.x) + Math.abs(p.y - drag.y) > 4) drag.moved = true;
          if (drag.moved) { ui.hideTip(); hover = null; hoverGoal = 0; view = {...view, tx: drag.tx + p.x - drag.x, ty: clampTy(drag.ty + p.y - drag.y, geometry())}; canvas.style.cursor = 'grabbing'; schedule(); return; }
        }
        overLabel(p.x, p.y);
        if (p.x > LABEL) pointOver(p.x, p.y); else if (hover) pointOver(-1, -1);
        schedule();
      });
      const release = e => {
        touches.delete(e.pointerId); if (touches.size < 2) pinch = null;
        const click = drag && !drag.moved ? local(e) : null; drag = null; canvas.style.cursor = '';
        if (!click) return;
        if (click.x < LABEL) { const lane = laneAtY(click.y); if (lane) onPickEntity(lane.e.id); return; }
        const m = nearest(click.x, click.y);
        onSelect(m ? m.id : null);
      };
      canvas.addEventListener('pointerup', release);
      canvas.addEventListener('pointercancel', e => { touches.delete(e.pointerId); pinch = null; drag = null; });
      canvas.addEventListener('pointerleave', () => { pointer = null; clearTimeout(hoverTimer); if (hoverLane) { hoverLane = ''; ui.hideTip(); } leaveTimer = setTimeout(() => setHover(null), 140); schedule(); });
      canvas.addEventListener('wheel', e => {
        e.preventDefault();
        const g = geometry();
        if (e.shiftKey) { view = {...view, ty: clampTy(view.ty - e.deltaY, g)}; motion = null; ui.hideTip(); schedule(); return; }
        if (Math.abs(e.deltaX) > Math.abs(e.deltaY)) { view = {...view, tx: view.tx - e.deltaX}; motion = null; schedule(); return; }
        const x = local(e).x, k = Math.exp(-Math.max(-120, Math.min(120, e.deltaY)) * 0.0016), floor = fitView().sx * 0.6, sx = Math.max(floor, Math.min(400, view.sx * k)), day = dayAt(x);
        motion = null; view = {sx, tx: x - LABEL - (day - lo) * sx, ty: view.ty}; ui.hideTip(); schedule();
      }, {passive: false});

      // 键盘：左右在时间上换一条记忆，上下翻泳道，加减缩放，0 适合窗口，Esc 取消
      const ordered = () => mems.filter(m => !hits || hits.has(m.id)).sort((a, b) => dayOf(a) - dayOf(b) || (a.id < b.id ? -1 : 1));
      canvas.addEventListener('keydown', e => {
        if (e.ctrlKey || e.metaKey || e.altKey) return;
        const g = geometry();
        if (e.key === 'ArrowLeft' || e.key === 'ArrowRight') {
          e.preventDefault();
          const list = ordered(), at = selected ? list.indexOf(selected) : -1, next = list[Math.max(0, Math.min(list.length - 1, at < 0 ? (e.key === 'ArrowRight' ? 0 : list.length - 1) : at + (e.key === 'ArrowRight' ? 1 : -1)))];
          if (next) onSelect(next.id);
        } else if (e.key === 'ArrowUp' || e.key === 'ArrowDown') { e.preventDefault(); goTo({...view, ty: clampTy(view.ty + (e.key === 'ArrowUp' ? 1 : -1) * LANE * 3, g)}, 160); }
        else if (e.key === '+' || e.key === '=') zoomAt(1.4);
        else if (e.key === '-') zoomAt(1 / 1.4);
        else if (e.key === '0') goTo(fitView());
        else if (e.key === 'Escape') onSelect(null);
      });

      const api = {
        // 载入：entities 是地图总览里的实体节点，list 是记忆（含它们提到的实体 id 和更正关系）。
        load(entities, list) {
          lanes = entities.filter(e => e.kind === 'entity').sort((a, b) => (a.group ?? 99) - (b.group ?? 99) || b.count - a.count).map(e => ({e}));
          laneOf = new Map(lanes.map(l => [l.e.id, l]));
          const dated = list.map(m => parseDay(m.occurred)).filter(d => d !== null);
          const first = dated.length ? Math.min(...dated) : 0, last = dated.length ? Math.max(...dated) : 1;
          hasUndated = list.some(m => parseDay(m.occurred) === null);
          const pad = Math.max(10, (last - first) * 0.02);
          lo = first - pad - (hasUndated ? (last - first) * 0.05 : 0); hi = last + pad;
          // 日期未记的放在最左边那一小段（lo 之后的 5%）
          mems = list.map(m => ({...m, day: parseDay(m.occurred), refs: m.refs || [], lanes: [...new Set(m.entity_ids.filter(id => laneOf.has(id)))].map(id => laneOf.get(id))}))
            .map(m => ({...m, lanes: m.lanes.length ? m.lanes : (laneOf.has('~loose') ? [laneOf.get('~loose')] : [])})).filter(m => m.lanes.length);
          byId = new Map(mems.map(m => [m.id, m]));
          laneSel = ''; selected = null; hover = null; hits = null; view = fitView(); motion = null; schedule();
        },
        show() { shown = true; resize(); },
        hide() { shown = false; hover = null; hoverGoal = 0; hoverAmt = 0; ui.hideTip(); },
        redraw: () => schedule(),
        // 给自动化检查用：当前画出的点，以及时间轴和泳道的位置
        marks: () => marks.map(k => ({id: k.m.id, x: k.x, y: k.y})),
        bounds: () => { const g = geometry(); return {axisTop: g.a.y0, laneTop: g.top, laneBottom: g.bottom, left: LABEL, sx: view.sx}; },
        fit: () => goTo(fitView()),
        zoom: factor => zoomAt(factor),
        has: id => byId.has(id),
        hasLane: id => laneOf.has(id),
        memory: id => byId.get(id) || null,
        selectedId: () => selected?.id || '',
        selectedLane: () => laneSel,
        entityIds: id => (byId.get(id)?.lanes || []).map(l => l.e.id),
        memoriesOf: entityId => mems.filter(m => m.lanes.some(l => l.e.id === entityId)).sort((a, b) => (b.occurred || '').localeCompare(a.occurred || '')),
        setSolo(group) { solo = group; hoverLane = ''; view = {...view, ty: 0}; schedule(); },
        setHits(set) { hits = set; schedule(); },
        // 搜索命中后：把时间放大到命中的那一段，滚到第一条命中所在的道
        focusHits() {
          const list = mems.filter(m => hits?.has(m.id)); if (!list.length) return;
          const days = list.map(dayOf), rows = visibleLanes(), lane = rows.indexOf(list[0].lanes[0]);
          goTo(spanView(Math.min(...days) - 10, Math.max(...days) + 10, lane));
        },
        // reveal：把它放到画面中间；laneId：有的话让这条道在中间（一条记忆在好几条道上都有点）
        select(id, reveal, laneId) {
          selected = id ? byId.get(id) || null : null; laneSel = selected ? '' : laneSel;
          if (selected && reveal) {
            const g = geometry(), x = xOf(dayOf(selected)), lane = visibleLanes().indexOf(laneOf.get(laneId) || selected.lanes[0]);
            const sx = Math.max(view.sx, fitView().sx * 8), day = dayOf(selected);
            if (x < g.left + 60 || x > g.right - 60 || sx !== view.sx || lane < 0) goTo({sx, tx: (g.left + g.right) / 2 - LABEL - (day - lo) * sx, ty: clampTy((g.bottom - g.top) / 2 - (lane + 0.5) * LANE, g)});
            else goTo({...view, ty: clampTy((g.bottom - g.top) / 2 - (lane + 0.5) * LANE, g)}, 260);
          }
          schedule();
        },
        selectLane(id) {
          laneSel = id || ''; if (id) selected = null;
          const list = mems.filter(m => m.lanes.some(l => l.e.id === id)), index = visibleLanes().findIndex(l => l.e.id === id);
          if (id && list.length && index >= 0) { const days = list.map(dayOf); goTo(spanView(Math.min(...days) - 10, Math.max(...days) + 10, index)); }
          schedule();
        },
      };
      return api;
    },
  };
})();
