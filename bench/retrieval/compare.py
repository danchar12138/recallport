"""Replays the old (reciprocal-rank) and the current (score fusion) ranking on dumps made by `study_dump`.

    python compare.py dump_P*.json

The current ranking here is the same maths as src/tree/search.rs; `cargo test study_eval` runs the real code on the
same corpora, and the two agree to within a percentage point.
"""
import sys
import numpy as np
from study import Persona, search_base, evaluate, summarize

SEMANTIC, TWIN = 0.75, 0.90
NOW = ('现在', '目前', '当前', '最新', '如今', '眼下', 'now', 'current', 'currently', 'latest', 'nowadays', 'at the moment')


def z(x):
    return (x - x.mean()) / (x.std() + 1e-9)


def score(P, e):
    return SEMANTIC * z(P.cos(e['v'])) + (1 - SEMANTIC) * z(P.bm25(e['terms']))


def asks_now(qvs):
    return any(w in e['q'].lower() for e in qvs for w in NOW)


def fused(P, qvs, k):
    """All phrasings scored together; of two near-identical versions the newer goes first when the question is about now."""
    total = sum(score(P, e) for e in qvs)
    cos = np.max([P.cos(e['v']) for e in qvs], 0)
    order = [int(i) for i in np.argsort(-total, kind='stable')[:max(k, 12)]]
    if asks_now(qvs):
        V, when = P.V['v_cur'], [x['occurred'] or '' for x in P.nodes]
        again = True
        while again:
            again = False
            for pos, old in enumerate(order):
                if P.layer[old] or not when[old]:
                    continue
                later = [p for p in range(pos + 1, len(order)) if not P.layer[order[p]]
                         and when[order[p]] > when[old] and float(V[old] @ V[order[p]]) >= TWIN]
                if later:
                    order.insert(max(later), order.pop(pos))
                    again = True
                    break
    ex = np.max([P.exact(e['q']) for e in qvs], 0)
    out = []
    for i in order[:k]:
        near = sorted(P.children.get(i, []), key=lambda j: -cos[j])
        shown = max(3, sum(1 for j in near if cos[j] >= cos[near[0]] - 0.08)) if near else 0
        out.append((i, float(total[i]), True, True, float(cos[i]), bool(ex[i]), tuple(near[:min(shown, 8)])))
    return out


def main(paths):
    personas = [Persona(p) for p in paths]
    old = lambda P, e, k: search_base(P, e['q'], e['v'], e['terms'], k)
    for name, kw in (('one phrasing', dict(multi=False)), ('2-3 phrasings', dict(multi=True))):
        for label, run in (('old (reciprocal rank)', dict(searcher=old)), ('new (score fusion)', dict(searcher=old, joint=fused))):
            rows = []
            for P in personas:
                rows += evaluate(P, run['searcher'], joint=run.get('joint'), **kw)
            line = summarize(rows)[0].splitlines()[-1]
            print(f'{name:14s} {label:22s} {line}')


if __name__ == '__main__':
    main(sys.argv[1:])
