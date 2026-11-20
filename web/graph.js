for (const workspace of document.querySelectorAll('[data-graph]')) mountGraph(workspace);

function mountGraph(root) {
  const el = key => root.querySelector(`[data-graph-${key}]`);
  const canvas = el('canvas'), select = el('select'), suggested = el('suggestions');
  const status = el('status'), message = el('message');
  const selection = el('selection'), expand = el('expand'), open = el('open');
  const prev = el('prev'), next = el('next'), hint = el('hint');
  const initial = root.dataset.focus || '';
  const overview = root.hasAttribute('data-overview');
  const filters = new URLSearchParams(location.search);
  const initialSelection = initial || filters.get('node');
  const pageNumber = Math.min(10000, Math.max(1, Number.parseInt(filters.get('page'),10) || 1));
  filters.delete('node');
  filters.delete('focus');
  filters.delete('suggested');
  let snapshot, active = '', epoch = 0, busy = false, page = pageNumber, retries = 0, minCo = 1;
  const coFilter = root.closest('main,body').querySelector('[data-co-filter]');
  const coRange = coFilter?.querySelector('[data-co-range]'), coValue = coFilter?.querySelector('[data-co-value]');
  const legend = el('groups');
  const maxNodes = 120;
  if (typeof window.cytoscape !== 'function') { message.classList.remove('is-loading'); message.textContent='关系图组件未加载'; return; }

  const theme = () => {
    const css = getComputedStyle(document.documentElement);
    const v = name => css.getPropertyValue(name).trim();
    const font = 'system-ui, "Microsoft YaHei UI", sans-serif';
    return [
      {selector:'node',style:{width:24,height:24,'background-color':v('--g-node'),'border-width':2,'border-color':v('--g-halo'),label:'data(label)','font-size':13,'font-family':font,'text-valign':'bottom','text-margin-y':6,'text-wrap':'ellipsis','text-max-width':'152px',color:v('--g-label'),'text-background-color':v('--g-label-bg'),'text-background-opacity':0.92,'text-background-padding':2}},
      {selector:'node.isolated',style:{'background-color':v('--g-isolated')}},
      {selector:'node.anchor',style:{width:30,height:30,'background-color':v('--g-anchor'),'border-color':v('--g-node'),'border-width':4}},
      {selector:'node:selected',style:{'background-color':v('--g-anchor'),'border-width':5,'border-color':v('--g-suggest'),'z-index':5}},
      {selector:'edge',style:{width:2,'curve-style':'bezier','line-color':v('--g-edge'),'target-arrow-shape':'triangle','target-arrow-color':v('--g-edge'),'arrow-scale':0.78}},
      {selector:'edge.suggested',style:{width:1.6,'line-style':'dashed','line-dash-pattern':[5,4],'line-color':v('--g-suggest'),'target-arrow-shape':'none'}},
      {selector:'node.entity',style:{shape:'diamond',width:'mapData(count,1,20,28,54)',height:'mapData(count,1,20,28,54)','background-color':v('--g-suggest'),'border-color':v('--g-halo'),'font-weight':600}},
      {selector:'node.entity:selected',style:{'border-color':v('--g-anchor'),'border-width':5}},
      ...[0,1,2,3,4,5].map(n=>({selector:`node.g${n}`,style:{'background-color':v(`--g-c${n}`)}})),
      {selector:'edge.mention',style:{width:1.6,'line-style':'dotted','line-color':v('--g-suggest'),'target-arrow-shape':'none'}},
      {selector:'edge.co',style:{width:'mapData(co,1,8,1.6,6)','line-color':v('--g-suggest'),'target-arrow-shape':'none',opacity:0.75}},
      {selector:'edge.corrects',style:{'line-color':v('--warn'),'target-arrow-color':v('--warn'),width:2.6}},
      {selector:'edge.summarizes',style:{'line-style':'dashed','line-dash-pattern':[8,3]}},
      {selector:'edge:selected',style:{width:3,'line-color':v('--g-anchor'),'target-arrow-color':v('--g-anchor')}},
    ];
  };
  const cy = window.cytoscape({container:canvas, minZoom:0.14, maxZoom:2, boxSelectionEnabled:false, style:theme()});
  const restyle=()=>cy.style(theme());
  window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change',restyle);
  window.addEventListener('rp-theme',restyle);

  const say = (text, loading = false) => {message.hidden=!text;message.textContent=text;message.classList.toggle('is-loading',loading);};
  const short = (text, limit) => {const items = Array.from(text.trim().replace(/\s+/g,' ')); return items.length > limit ? `${items.slice(0,limit).join('')}…` : items.join('');};
  const layout = () => {
    if (!cy.nodes().length) return;
    cy.nodes().toggleClass('isolated',false);
    cy.nodes().filter(n => !n.connectedEdges().length).addClass('isolated');
    const grid = cy.edges().length===0;
    cy.layout(grid ? {name:'grid', fit:false, avoidOverlap:true, spacingFactor:1.6}
      : {name:'cose', fit:false, animate:false, randomize:true, nodeRepulsion:4200, idealEdgeLength:88, componentSpacing:48, nodeDimensionsIncludeLabels:true, numIter:600}).run();
    if (!grid) {
      // cose tends to build tall, narrow clusters; stretch them to the canvas shape so a wide screen is used.
      const box = cy.elements().boundingBox(), ratio = canvas.clientWidth / Math.max(1, canvas.clientHeight);
      const k = Math.min(2.4, (ratio * box.h * 0.85) / Math.max(1, box.w));
      if (k > 1.15) cy.nodes().positions(n => ({x: (n.position('x') - box.x1) * k + box.x1, y: n.position('y')}));
    }
    fit();
  };
  const fit = () => {
    if (!cy.nodes().length) return;
    // In immersive mode the inspector floats over the canvas: fit into the part that stays visible.
    const covered = root.classList.contains('is-immersive') && !root.classList.contains('no-panel') ? Math.min(352, canvas.clientWidth * 0.4) : 0;
    const pad = canvas.clientWidth < 500 ? 25 : 40, width = Math.max(120, canvas.clientWidth - covered), height = canvas.clientHeight;
    const box = cy.elements().boundingBox();
    const zoom = Math.max(0.14, Math.min(1.6, (width - pad * 2) / Math.max(1, box.w), (height - pad * 2) / Math.max(1, box.h)));
    cy.zoom(zoom);
    cy.pan({x: width / 2 - zoom * (box.x1 + box.w / 2), y: height / 2 - zoom * (box.y1 + box.h / 2)});
  };
  function updateSelect() {
    select.replaceChildren(new Option('选择节点',''));
    for (const node of cy.nodes()) {
      const d=node.data();
      select.add(new Option(d.kind==='entity' ? `实体 · ${short(d.text,34)}` : `${d.occurred || '日期未记'} · ${short(d.text,34)}`,d.id));
    }
  }
  function describe(id, center = false) {
    const node=cy.getElementById(id);
    active=node.isNode() ? id : '';
    select.value=active;
    selection.hidden=!active;
    if (hint) hint.hidden=Boolean(active);
    el('edge-reason').hidden=true;
    cy.$('node:selected').unselect();
    if (!active) return;
    const d=node.data();
    node.select();
    if (center) cy.center(node);
    if (d.kind==='entity') {
      el('meta').textContent=`实体 · ${d.count ?? 0} 条记忆`;
      el('text').textContent=[d.text, d.hint, (d.aliases||[]).length ? `别名：${d.aliases.join('、')}` : ''].filter(Boolean).join('\n');
      open.href=`/entity/${encodeURIComponent(id)}`;
      open.textContent='查看实体';
      expand.textContent='展开它的记忆';
    } else {
      el('meta').textContent=`${d.occurred || '日期未记'} · ${d.source || '来源未记'}`;
      el('text').textContent=d.text;
      const back=initial && root.dataset.back ? root.dataset.back : (()=>{
        const v=new URLSearchParams(filters); v.set('page',String(page));v.set('node',active);return `/graph?${v}`;
      })();
      open.href=`/memory/${encodeURIComponent(id)}?${new URLSearchParams({back:back || '/'})}#relations`;
      open.textContent='查看详情';
      expand.textContent='展开邻居';
    }
    expand.disabled=busy || cy.nodes().length>=maxNodes;
  }

  function put(data) {
    cy.batch(()=>{
      for (const d of data.nodes) {
        if (cy.getElementById(d.id).length || cy.nodes().length>=maxNodes) continue;
        cy.add({group:'nodes',data:{...d,label:short(d.text,17)},classes:d.kind==='entity' ? `entity${Number.isInteger(d.group) ? ` g${d.group}` : ''}` : d.id===initial ? 'anchor' : ''});
      }
      for (const e of data.edges) {
        if (!cy.getElementById(e.from).length || !cy.getElementById(e.to).length) continue;
        if (e.co && e.co<minCo) continue;
        const ends=e.suggested || e.co ? [e.from,e.to].sort() : [e.from,e.to];
        const tag=e.suggested ? 's' : e.mention ? 'm' : e.co ? 'c' : 'r';
        const id=`${tag}:${ends.join(':')}`;
        const classes=[e.suggested && 'suggested', e.mention && 'mention', e.co && 'co', e.rk && e.rk!=='relates' && e.rk].filter(Boolean).join(' ');
        if (!cy.getElementById(id).length) cy.add({group:'edges',data:{id,source:e.from,target:e.to,reason:e.reason,suggested:e.suggested,mention:e.mention,co:e.co,rk:e.rk},classes});
      }
    });
    updateSelect();
  }
  // Entity map: colour legend, and a slider that hides links between entities that rarely appear together.
  function setupCo(data) {
    minCo=data.suggested_min || 1;
    if (coFilter && coRange) {
      coRange.max=String(Math.max(1,data.max_co || 1));
      coRange.value=String(minCo);
      coValue.textContent=String(minCo);
      coFilter.hidden=(data.max_co || 1)<2;
    }
    if (legend) {
      legend.replaceChildren(...(data.groups || []).map(g=>{
        const item=document.createElement('span'), dot=document.createElement('b');
        dot.className=`dot g${g.group}`;
        item.append(dot,`${g.name} 等 ${g.size} 个`);
        return item;
      }));
    }
  }
  coRange?.addEventListener('input',()=>{
    minCo=Number(coRange.value);
    coValue.textContent=String(minCo);
    cy.edges('.co').remove();
    put({nodes:[],edges:snapshot?.edges ?? []});
    layout();announce();
  });
  function announce(extra='') {
    const edges=cy.edges();
    const soft=edges.filter('.suggested').length;
    const people=cy.nodes('.entity').length;
    const refs=edges.filter(e=>!e.hasClass('suggested')&&!e.hasClass('mention')&&!e.hasClass('co')).length;
    const parts=[`${cy.nodes().length-people} 条记忆`];
    if (people) parts.push(`${people} 个实体`);
    parts.push(overview ? `${edges.length} 条共现` : `${refs} 条引用`);
    if (soft) parts.push(`${soft} 条语义相似`);
    if (prev) parts.push(`第 ${page} 页`);
    if (extra) parts.push(extra);
    status.textContent=parts.join(' · ');
  }
  function controls() {
    if (prev) prev.disabled=busy||page<=1;
    if (next) next.disabled=busy||!snapshot?.next;
    expand.disabled=busy||!active||cy.nodes().length>=maxNodes;
    if (suggested) suggested.disabled=busy;
  }
  async function load(mode,id='') {
    const mark=++epoch;
    busy=true;
    controls();
    if (mode==='base') say('正在加载关系图…',true);
    else {expand.classList.add('is-busy');}
    const query=new URLSearchParams(mode==='base' && !initial && !overview ? filters : '');
    if (overview && mode==='base') query.set('overview','1');
    if (id) query.set('focus',id);
    if (suggested?.checked && id) query.set('suggested','1');
    if (!id && !overview) query.set('page',String(page));
    try {
      const response=await fetch(`/graph-data?${query}`,{signal:AbortSignal.timeout(8000)});
      if (!response.ok) throw new Error('graph request failed');
      const data=await response.json();
      retries=0;
      if (mark!==epoch) return;
      const keep=active;
      if (mode==='base') {cy.elements().remove();snapshot=data;page=data.page;}
      if (overview && mode==='base') setupCo(data);
      put(data);
      layout();
      const focused=id || (cy.getElementById(initialSelection || '').length ? initialSelection : '') || (cy.getElementById(keep).length ? keep : '') || cy.nodes().first().id();
      describe(focused || '');
      say(cy.nodes().length ? '' : overview ? '还没有实体。接入的 AI 会在读写记忆时整理它们。' : initial ? '这里还没有可显示的关系' : '没有匹配的记忆');
      announce(data.limited ? '仅显示相关性最高的 30 条' : data.degraded ? '检索已降级' : data.more ? (overview ? '仅显示提及最多的 40 个实体' : '已截断，可继续展开') : cy.nodes().length>=maxNodes ? '已达到视图上限' : '');
      if (mode==='base' && !initial && restoreNode) {
        const wanted=restoreNode;restoreNode='';
        if (!cy.getElementById(wanted).length) load('expand',wanted);
      }
    } catch {
      if (mark!==epoch) return;
      if (cy.nodes().length) announce('未能加载关系');
      else if (retries++<2) {say('未能加载关系图，正在重试…',true);setTimeout(()=>{if (mark===epoch && !cy.nodes().length) load(mode,id);},1500);}
      else say('未能加载关系图，请刷新页面重试');
    } finally {
      if (mark===epoch) {busy=false;expand.classList.remove('is-busy');controls();}
    }
  }
  cy.on('tap','node',event=>describe(event.target.id()));
  cy.on('tap','edge',event=>{
    const edge=event.target;
    describe(edge.source().id());
    const rk=edge.data('rk');
    el('edge-reason').textContent=edge.data('suggested') ? '语义相似' : edge.data('mention') ? '记忆提及了这个实体' : edge.data('co') ? (edge.data('reason') || '同时出现') : `${rk==='corrects' ? '更正：' : rk==='summarizes' ? '汇总：' : ''}${edge.data('reason') || (rk && rk!=='relates' ? '' : '明确引用')}` || '明确引用';
    el('edge-reason').hidden=false;
  });
  cy.on('dbltap','node',event=>{describe(event.target.id());if (!expand.disabled) load('expand',event.target.id());});
  select.addEventListener('change',()=>describe(select.value,true));
  expand.addEventListener('click',()=>{if (active) load('expand',active);});
  suggested?.addEventListener('change',()=>{
    if (suggested.checked && active) load('expand',active);
    else {
      const candidates=cy.edges('.suggested').connectedNodes();
      cy.edges('.suggested').remove();
      const keep=new Set((snapshot?.nodes ?? []).map(n=>n.id));
      candidates.filter(n=>!keep.has(n.id())&&!n.connectedEdges().length).remove();
      layout();updateSelect();describe(cy.getElementById(active).length ? active : initial || cy.nodes().first().id() || '');announce();
    }
  });
  for (const btn of root.querySelectorAll('[data-graph-action]')) btn.addEventListener('click',()=>{
    switch(btn.dataset.graphAction) {
      case 'fit':fit();break;
      case 'panel':togglePanel();break;
      case 'full':toggleFull();break;
      default:cy.zoom({level:cy.zoom()*(btn.dataset.graphAction==='in' ? 1.25 : 0.8),renderedPosition:{x:canvas.clientWidth/2,y:canvas.clientHeight/2}});
    }
  });
  prev?.addEventListener('click',()=>{if (page>1){page--;load('base',initial);}});
  next?.addEventListener('click',()=>{if (snapshot?.next){page++;load('base',initial);}});
  // The first layout can run before the canvas has its final size: keep fitting until the user pans or zooms.
  let touched=false;
  for (const type of ['wheel','pointerdown','touchstart']) canvas.addEventListener(type,()=>{touched=true;},{passive:true});
  new ResizeObserver(()=>{cy.resize();if (!touched) fit();}).observe(canvas);
  const panelButton=root.querySelector('[data-graph-action=panel]'), fullButton=root.querySelector('[data-graph-action=full]');
  const togglePanel=()=>{const hidden=root.classList.toggle('no-panel');panelButton?.setAttribute('aria-pressed',String(!hidden));requestAnimationFrame(()=>{cy.resize();if (!touched) fit();});};
  const setImmersive=on=>{
    root.classList.toggle('is-immersive',on);
    document.body.classList.toggle('graph-immersive',on);
    fullButton?.querySelector('[data-full-label]')&&(fullButton.querySelector('[data-full-label]').textContent=on ? '退出全屏' : '全屏');
    fullButton?.querySelector('use')?.setAttribute('href',on ? '#shrink' : '#expand');
    requestAnimationFrame(()=>{cy.resize();touched=false;fit();});
  };
  const toggleFull=()=>{
    if (root.classList.contains('is-immersive')) {
      if (document.fullscreenElement) document.exitFullscreen().catch(()=>{});
      setImmersive(false);
    } else {
      setImmersive(true);
      root.requestFullscreen?.().catch(()=>{});
    }
  };
  document.addEventListener('fullscreenchange',()=>{if (!document.fullscreenElement && root.classList.contains('is-immersive')) setImmersive(false);});
  document.addEventListener('keydown',event=>{
    if (event.ctrlKey||event.metaKey||event.altKey||event.target.closest('input,textarea,select,[contenteditable]')) return;
    if (event.key==='f'||event.key==='F') {event.preventDefault();toggleFull();}
    else if (event.key==='Escape' && root.classList.contains('is-immersive')) {if (document.fullscreenElement) document.exitFullscreen().catch(()=>{});setImmersive(false);}
  });
  panelButton?.setAttribute('aria-pressed','true');
  let restoreNode=initialSelection || '';
  load('base',initial);
}
