"""Offline retrieval study: loads dumps made by src/tree/study.rs and replays search variants on numpy.
Faithfully reproduces the product search (BM25 + cosine + RRF + multi-query merge) as variant 'base'."""
import json, math, sys, glob, collections
import numpy as np

STRONG = 0.88


class Persona:
    def __init__(self, path):
        d = path if isinstance(path, dict) else json.load(open(path, encoding='utf8'))
        self.path = path
        self.name = d['persona'][:40]
        self.d = d
        nodes = d['nodes']
        self.nodes = nodes
        self.n = len(nodes)
        self.keys = [x['key'] for x in nodes]
        self.idx = {k: i for i, k in enumerate(self.keys)}
        self.layer = np.array([x['layer'] for x in nodes])
        self.V = {name: np.array([x[name] for x in nodes], dtype=np.float32) for name in ('v_cur', 'v_content', 'v_tagged')}
        self.queries = d['queries']
        self.text = [(x['title'] + '\n' + x['content'] + '\n' + ' '.join(x['tags'])).lower() for x in nodes]
        self.children = collections.defaultdict(list)
        for i, x in enumerate(nodes):
            if x['parent'] in self.idx:
                self.children[self.idx[x['parent']]].append(i)
        self.build_bm25()

    # ---------- BM25 (same maths as src/tree/lexicon.rs) ----------
    def build_bm25(self, title_w=2, tag_w=1, use_title=True):
        self.title_w, self.tag_w = title_w, tag_w
        post = collections.defaultdict(dict)
        lens = np.zeros(self.n)
        for i, x in enumerate(self.nodes):
            tf = collections.Counter()
            for t in x['terms_title']:
                tf[t] += title_w
            for t in x['terms_content']:
                tf[t] += 1
            for t in x['terms_tags']:
                tf[t] += tag_w
            lens[i] = sum(tf.values())
            for t, c in tf.items():
                post[t][i] = c
        self.post, self.lens = post, lens
        self.avg = max(lens.sum() / max(self.n, 1), 1.0)

    def bm25(self, qterms, k1=1.2, b=0.75, idf_floor=None):
        key = (tuple(qterms), k1, b, idf_floor, self.title_w, self.tag_w)
        c = self.__dict__.setdefault('_bm', {})
        if key not in c:
            c[key] = self._bm25(qterms, k1, b, idf_floor)
        return c[key]

    def _bm25(self, qterms, k1, b, idf_floor):
        n = self.n
        s = np.zeros(n)
        seen = set()
        for t in qterms:
            if t in seen:
                continue
            seen.add(t)
            p = self.post.get(t)
            if not p:
                continue
            df = len(p)
            idf = math.log(1 + (n - df + 0.5) / (df + 0.5))
            if idf_floor is not None and idf < idf_floor:
                continue
            for i, tf in p.items():
                s[i] += idf * tf * (k1 + 1) / (tf + k1 * (1 - b + b * self.lens[i] / self.avg))
        return s

    def cos(self, qv, which='v_cur'):
        key = (id(qv), which)
        c = self.__dict__.setdefault('_cs', {})
        if key not in c:
            c[key] = self.V[which] @ np.array(qv, dtype=np.float32)
        return c[key]

    def exact(self, q):
        q = q.strip().lower()
        return np.array([q in t for t in self.text])


def rrf_lists(lists, k=60.0, weights=None):
    """lists: list of ordered index arrays; returns dict idx->score."""
    sc = collections.defaultdict(float)
    for li, lst in enumerate(lists):
        w = 1.0 if weights is None else weights[li]
        for r, i in enumerate(lst):
            sc[int(i)] += w / (k + r + 1)
    return sc


def search_base(P, q, qv, qterms, k=8, params=None):
    """One phrasing -> list of (idx, fused, kw, sem, cos, exact). params override defaults."""
    p = dict(rrf=60.0, pool='auto', wk=1.0, ws=1.0, k1=1.2, b=0.75, which='v_cur', kwfloor=0.0, idf_floor=None)
    if params:
        p.update(params)
    kw = P.bm25(qterms, p['k1'], p['b'], p['idf_floor'])
    c = P.cos(qv, p['which'])
    kw_order = [i for i in np.argsort(-kw, kind='stable') if kw[i] > p['kwfloor']]
    sem_order = list(np.argsort(-c, kind='stable'))
    pool = max(k * 8, 40) if p['pool'] == 'auto' else p['pool']
    if pool is not None:
        kw_order, sem_order = kw_order[:pool], sem_order[:pool]
    sc = rrf_lists([kw_order, sem_order], p['rrf'], [p['wk'], p['ws']])
    kws, sems = set(map(int, kw_order)), set(map(int, sem_order))
    ex = P.exact(q)
    ranked = sorted(sc.items(), key=lambda t: (-t[1], P.keys[t[0]]))[:k]
    full_c = P.cos(qv, 'v_cur')
    def exp(i):
        ch = sorted(P.children.get(i, []), key=lambda j: -full_c[j])[:3]
        return tuple(ch)
    return [(i, s, i in kws, i in sems, float(full_c[i]), bool(ex[i]), exp(i)) for i, s in ranked]


def merge(lists, limit=8):
    sc = collections.defaultdict(float)
    best = {}
    for lst in lists:
        for r, h in enumerate(lst):
            sc[h[0]] += 1.0 / (60 + r + 1)
            if h[0] not in best or (h[5], h[4]) > (best[h[0]][5], best[h[0]][4]):
                best[h[0]] = h
    ranked = sorted(sc.items(), key=lambda t: (-t[1], t[0]))[:limit]
    return [best[i] for i, _ in ranked]


def strong(hits):
    return any(h[5] or h[4] >= STRONG for h in hits)


def evaluate(P, searcher, multi=False, k=8, joint=None, nq=None):
    """searcher(P, q_dict, k) -> hits. Returns list of per-query result dicts."""
    out = []
    for q in P.queries:
        qvs = q['qv'] if multi else q['qv'][:1]
        if nq: qvs = qvs[:nq]
        lists = [searcher(P, e, k) for e in qvs]
        hits = joint(P, qvs, k) if joint else (merge(lists, k) if len(lists) > 1 else lists[0])
        any_strong = any(strong(l) for l in lists)
        order = [h[0] for h in hits]
        # what the AI actually sees: the hit itself plus the children a summary brings along
        reach = [set([h[0]]) | set(h[6]) for h in hits]
        gold = [P.idx[g] for g in q['gold'] if g in P.idx]
        stale = [P.idx[g] for g in q.get('stale', []) if g in P.idx]
        r = dict(id=q['id'], type=q['type'], strong=any_strong, ngold=len(gold))
        if gold:
            ranks = [next((r for r, c in enumerate(reach) if g in c), 99) for g in gold]
            best = min(ranks)
            strict = min([order.index(g) if g in order else 99 for g in gold])
            r['s1'] = strict == 0; r['s3'] = strict < 3
            r.update(h1=best == 0, h3=best < 3, h5=best < 5, h8=best < 8, mrr=1 / (best + 1) if best < 8 else 0,
                     rec5=sum(1 for x in ranks if x < 5) / len(gold), all5=all(x < 5 for x in ranks))
            if stale:
                sr = min([next((r_ for r_, c in enumerate(reach) if s in c), 99) for s in stale])
                r.update(stale_top3=sr < 3, stale_above=sr < best)
            top1 = order[0] if order else None
            r['top1_gold'] = top1 in gold
        r['abstain_ok'] = (not any_strong) if not gold else None
        r['flagged_strong_when_top1_ok'] = (any_strong and r.get('h1')) if gold else None
        out.append(r)
    return out


def summarize(rows, title=''):
    types = collections.OrderedDict()
    for r in rows:
        types.setdefault(r['type'], []).append(r)

    def pct(rs, key):
        v = [x[key] for x in rs if key in x]
        return (100.0 * sum(v) / len(v), len(v)) if v else (None, 0)

    lines = []
    for t, rs in types.items():
        if t == 'absent':
            a, n = pct(rs, 'abstain_ok')
            lines.append(f'{t:11s} n={n:3d}  abstain={a:5.1f}')
        else:
            h1, n = pct(rs, 'h1'); h3, _ = pct(rs, 'h3'); h5, _ = pct(rs, 'h5'); mrr, _ = pct(rs, 'mrr')
            extra = ''
            if t in ('overview', 'multihop'):
                extra = f' rec5={pct(rs, "rec5")[0]:5.1f} all5={pct(rs, "all5")[0]:5.1f}'
            if t in ('latest', 'neardup'):
                sa = pct(rs, 'stale_above')
                extra = f' stale_above={sa[0]:5.1f}' if sa[0] is not None else ''
            lines.append(f'{t:11s} n={n:3d}  @1={h1:5.1f} @3={h3:5.1f} @5={h5:5.1f} mrr={mrr/100:.3f}{extra}')
    g = [r for r in rows if r['ngold'] and r['type'] != 'history']
    g1 = 100 * sum(r['h1'] for r in g) / len(g)
    g3 = 100 * sum(r['h3'] for r in g) / len(g)
    g5 = 100 * sum(r['h5'] for r in g) / len(g)
    mrr = sum(r['mrr'] for r in g) / len(g)
    ab = [r for r in rows if r['type'] == 'absent']
    ab_ok = 100 * sum(r['abstain_ok'] for r in ab) / max(len(ab), 1)
    ok1 = [r for r in rows if r.get('h1')]
    flagged = 100 * sum(1 for r in ok1 if r['strong']) / max(len(ok1), 1)
    g_s1 = 100 * sum(r['s1'] for r in g) / len(g); g_s3 = 100 * sum(r['s3'] for r in g) / len(g)
    lines.append(f'{"ALL(gold)":11s} n={len(g):3d}  @1={g1:5.1f} @3={g3:5.1f} @5={g5:5.1f} mrr={mrr:.3f} | strict@1={g_s1:5.1f} @3={g_s3:5.1f} | abstain_ok={ab_ok:5.1f} strong_when_top1={flagged:5.1f}')
    return '\n'.join(lines), dict(h1=g1, h3=g3, h5=g5, mrr=mrr, abstain=ab_ok, strong_top1=flagged, s1=g_s1, s3=g_s3)


def load_all(pattern):
    return [Persona(p) for p in sorted(glob.glob(pattern))]
