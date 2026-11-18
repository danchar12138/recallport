for (const workspace of document.querySelectorAll('[data-graph]')) mountGraph(workspace);

function mountGraph(root) {
  const el = key => root.querySelector(`[data-graph-${key}]`);
  const canvas = el('canvas'), select = el('select'), suggested = el('suggestions');
  const status = el('status'), message = el('message');
  const selection = el('selection'), expand = el('expand'), open = el('open');
  const prev = el('prev'), next = el('next');
  const initial = root.dataset.focus || '';
  const filters = new URLSearchParams(location.search);
  const initialSelection = initial || filters.get('node');
  const pageNumber = Math.min(10000, Math.max(1, Number.parseInt(filters.get('page'),10) || 1));
  filters.delete('node');
  filters.delete('focus');
  filters.delete('suggested');
  let snapshot, active = '', epoch = 0, busy = false, page = pageNumber;
  const maxNodes = 120;
  if (typeof window.cytoscape !== 'function') { message.textContent='关系图组件未加载'; return; }

  const cy = window.cytoscape({container:canvas, minZoom:0.14, maxZoom:2, boxSelectionEnabled:false,
    style:[
      {selector:'node',style:{width:24,height:24,'background-color':'#537f71','border-width':2,'border-color':'#fff',label:'data(label)','font-size':13,'font-family':'system-ui, sans-serif','text-valign':'bottom','text-margin-y':6,'text-wrap':'ellipsis','text-max-width':'152px',color:'#203b36','text-background-color':'#f7faf8','text-background-opacity':0.92,'text-background-padding':2}},
      {selector:'node.isolated',style:{'background-color':'#829098'}},
      {selector:'node.anchor',style:{width:28,height:28,'background-color':'#176450','border-color':'#b1d8cb','border-width':4}},
      {selector:'node:selected',style:{'background-color':'#167c60','border-width':5,'border-color':'#bce1d4','z-index':5}},
      {selector:'edge',style:{width:2,'curve-style':'bezier','line-color':'#748b82','target-arrow-shape':'triangle','target-arrow-color':'#748b82','arrow-scale':0.78}},
      {selector:'edge.suggested',style:{width:1.6,'line-style':'dashed','line-dash-pattern':[5,4],'line-color':'#a5783c','target-arrow-shape':'none'}},
      {selector:'edge:selected',style:{width:3,'line-color':'#218167','target-arrow-color':'#218167'}},
    ]});

  const short = (text, limit) => {const items = Array.from(text.trim().replace(/\s+/g,' ')); return items.length > limit ? `${items.slice(0,limit).join('')}…` : items.join('');};
  const layout = () => {
    if (!cy.nodes().length) return;
    cy.nodes().toggleClass('isolated',false);
    cy.nodes().filter(n => !n.connectedEdges().length).addClass('isolated');
    const grid = cy.edges().length===0;
    cy.layout(grid ? {name:'grid', fit:false, avoidOverlap:true, spacingFactor:1.6}
      : {name:'cose', fit:false, animate:false, randomize:true, nodeRepulsion:4200, idealEdgeLength:88, componentSpacing:48, nodeDimensionsIncludeLabels:true, numIter:600}).run();
    fit();
  };
  const fit = () => {
    if (!cy.nodes().length) return;
    cy.fit(cy.elements(), canvas.clientWidth<500 ? 25 : 32);
    if (cy.zoom()>1.35) {cy.zoom(1.35);cy.center(cy.elements());}
  };
  function updateSelect() {
    select.replaceChildren(new Option('选择记忆',''));
    for (const node of cy.nodes()) {
      const d=node.data();
      select.add(new Option(`${d.occurred || '日期未记'} · ${short(d.text,34)}`,d.id));
    }
  }
  function describe(id, center = false) {
    const node=cy.getElementById(id);
    active=node.isNode() ? id : '';
    select.value=active;
    selection.hidden=!active;
    el('edge-reason').hidden=true;
    cy.$('node:selected').unselect();
    if (!active) return;
    const d=node.data();
    node.select();
    if (center) cy.center(node);
    el('meta').textContent=`${d.occurred || '日期未记'} · ${d.source || '来源未记'}`;
    el('text').textContent=d.text;
    const back=initial ? root.dataset.back : (()=>{
      const v=new URLSearchParams(filters); v.set('page',String(page));v.set('node',active);return `/graph?${v}`;
    })();
    open.href=`/memory/${encodeURIComponent(id)}?${new URLSearchParams({back:back || '/'})}#relations`;
    expand.disabled=busy || cy.nodes().length>=maxNodes;
  }

  function put(data) {
    cy.batch(()=>{
      for (const d of data.nodes) {
        if (cy.getElementById(d.id).length || cy.nodes().length>=maxNodes) continue;
        cy.add({group:'nodes',data:{...d,label:short(d.text,17)},classes:d.id===initial ? 'anchor' : ''});
      }
      for (const e of data.edges) {
        if (!cy.getElementById(e.from).length || !cy.getElementById(e.to).length) continue;
        const ends=e.suggested ? [e.from,e.to].sort() : [e.from,e.to];
        const id=`${e.suggested ? 's':'r'}:${ends.join(':')}`;
        if (!cy.getElementById(id).length) cy.add({group:'edges',data:{id,source:e.from,target:e.to,reason:e.reason,suggested:e.suggested},classes:e.suggested ? 'suggested':''});
      }
    });
    updateSelect();
  }
  function announce(extra='') {
    const edges=cy.edges();
    const soft=edges.filter('.suggested').length;
    status.textContent=`${cy.nodes().length} 条记忆 · ${edges.length-soft} 条引用${soft ? ` · ${soft} 条语义相似`:''}${prev ? ` · 第 ${page} 页`:''}${extra ? ` · ${extra}`:''}`;
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
    if (mode==='base') {message.hidden=false;message.textContent='正在加载关系图…';}
    const query=new URLSearchParams(mode==='base' && !initial ? filters : '');
    if (id) query.set('focus',id);
    if (suggested?.checked && id) query.set('suggested','1');
    if (!id) query.set('page',String(page));
    try {
      const response=await fetch(`/graph-data?${query}`,{signal:AbortSignal.timeout(8000)});
      if (!response.ok) throw new Error('graph request failed');
      const data=await response.json();
      if (mark!==epoch) return;
      const keep=active;
      if (mode==='base') {cy.elements().remove();snapshot=data;page=data.page;}
      put(data);
      layout();
      const focused=id || (cy.getElementById(initialSelection || '').length ? initialSelection : '') || (cy.getElementById(keep).length ? keep : '') || cy.nodes().first().id();
      describe(focused || '');
      message.hidden=Boolean(cy.nodes().length);
      message.textContent='没有匹配的记忆';
      announce(data.limited ? '仅显示相关性最高的 30 条' : data.degraded ? '检索已降级' : data.more ? '邻居已截断' : cy.nodes().length>=maxNodes ? '已达到视图上限' : '');
      if (mode==='base' && !initial && restoreNode) {
        const wanted=restoreNode;restoreNode='';
        if (!cy.getElementById(wanted).length) load('expand',wanted);
      }
    } catch {
      if (mark!==epoch) return;
      if (cy.nodes().length) announce('未能加载关系');
      else {message.hidden=false;message.textContent='未能加载关系图';}
    } finally {
      if (mark===epoch) {busy=false;controls();}
    }
  }
  cy.on('tap','node',event=>describe(event.target.id()));
  cy.on('tap','edge',event=>{
    const edge=event.target;
    describe(edge.source().id());
    el('edge-reason').textContent=edge.data('suggested') ? '语义相似' : (edge.data('reason') || '明确引用');
    el('edge-reason').hidden=false;
  });
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
      case 'reset':load('base',initial);break;
      default:cy.zoom({level:cy.zoom()*(btn.dataset.graphAction==='in' ? 1.25 : 0.8),renderedPosition:{x:canvas.clientWidth/2,y:canvas.clientHeight/2}});
    }
  });
  prev?.addEventListener('click',()=>{if (page>1){page--;load('base',initial);}});
  next?.addEventListener('click',()=>{if (snapshot?.next){page++;load('base',initial);}});
  new ResizeObserver(()=>cy.resize()).observe(canvas);
  let restoreNode=initialSelection || '';
  load('base',initial);
}
