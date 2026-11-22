//! Grouping memories that belong together, so `health` can hand the AI exactly the batches that still wait for a
//! summary. The server never writes a summary itself: it only says where one is missing.
//!
//! Method: average-linkage hierarchical clustering on the cosine distance of the memories' semantic vectors
//! (nearest-neighbour chain, O(n²) time and memory), cut into groups small enough to summarize in one go.

struct Merge {
    a: usize,
    b: usize,
    size: usize,
}

/// Leaves are 0..n; the k-th merge makes node n + k.
fn average_linkage(unit: &[Vec<f32>]) -> Vec<Merge> {
    let n = unit.len();
    let dot = |a: &[f32], b: &[f32]| a.iter().zip(b).map(|(p, q)| p * q).sum::<f32>();
    let mut dist = vec![0f32; n * n];
    for i in 0..n {
        for j in i + 1..n {
            let d = 1.0 - dot(&unit[i], &unit[j]);
            dist[i * n + j] = d;
            dist[j * n + i] = d;
        }
    }
    let mut active = vec![true; n];
    let mut size = vec![1usize; n];
    let mut node: Vec<usize> = (0..n).collect();
    let mut merges: Vec<Merge> = Vec::with_capacity(n.saturating_sub(1));
    let mut chain: Vec<usize> = vec![];
    let mut remaining = n;
    while remaining > 1 {
        if chain.is_empty() {
            chain.push((0..n).find(|&i| active[i]).expect("an active cluster"));
        }
        loop {
            let a = *chain.last().expect("chain");
            let prev = chain.len().checked_sub(2).map(|i| chain[i]);
            let mut best = usize::MAX;
            let mut best_d = f32::INFINITY;
            for j in (0..n).filter(|&j| active[j] && j != a) {
                let d = dist[a * n + j];
                if d < best_d || (d == best_d && Some(j) == prev) {
                    best_d = d;
                    best = j;
                }
            }
            if Some(best) == prev {
                chain.pop();
                chain.pop();
                let (lo, hi) = (a.min(best), a.max(best));
                let (sa, sb) = (size[lo] as f32, size[hi] as f32);
                for k in (0..n).filter(|&k| active[k] && k != lo && k != hi) {
                    let d = (sa * dist[lo * n + k] + sb * dist[hi * n + k]) / (sa + sb);
                    dist[lo * n + k] = d;
                    dist[k * n + lo] = d;
                }
                merges.push(Merge {
                    a: node[lo],
                    b: node[hi],
                    size: size[lo] + size[hi],
                });
                node[lo] = n + merges.len() - 1;
                size[lo] += size[hi];
                active[hi] = false;
                remaining -= 1;
                break;
            }
            chain.push(best);
        }
    }
    merges
}

pub fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b).map(|(p, q)| p * q).sum();
    let norm = |v: &[f32]| v.iter().map(|x| x * x).sum::<f32>().sqrt();
    dot / (norm(a) * norm(b)).max(1e-9)
}

/// Splits the memories (indexes into `vectors`) into groups that each become one summary: every memory lands in
/// exactly one group of `min..=max` (a little more when stragglers had to join), and the groups follow how the
/// memories cluster. Merges are replayed from the tightest up and refused once the result would be too big; what is
/// left over under `min` joins the nearest group.
pub fn partition(vectors: &[Vec<f32>], min: usize, max: usize) -> Vec<Vec<usize>> {
    let n = vectors.len();
    if n < 2 {
        return vec![];
    }
    let unit: Vec<Vec<f32>> = vectors
        .iter()
        .map(|v| {
            let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
            v.iter().map(|x| x / norm).collect()
        })
        .collect();
    if n <= max {
        return vec![(0..n).collect()];
    }
    let merges = average_linkage(&unit);
    // Node n + k is the k-th merge. `None` marks a merge that was refused: nothing more may join it.
    let mut members: Vec<Option<Vec<usize>>> = (0..n).map(|i| Some(vec![i])).collect();
    let mut closed = vec![false; n];
    for m in &merges {
        let fits = !closed[m.a] && !closed[m.b] && m.size <= max;
        if fits {
            let mut joined = members[m.a].take().unwrap_or_default();
            joined.extend(members[m.b].take().unwrap_or_default());
            members.push(Some(joined));
            closed.push(false);
        } else {
            closed[m.a] = true;
            closed[m.b] = true;
            members.push(None);
            closed.push(true);
        }
    }
    let (mut groups, mut small): (Vec<Vec<usize>>, Vec<Vec<usize>>) =
        members.into_iter().flatten().partition(|g| g.len() >= min);
    if groups.is_empty() {
        return vec![small.into_iter().flatten().collect()];
    }
    let centre = |g: &[usize]| -> Vec<f32> {
        let mut sum = vec![0f32; unit[0].len()];
        for &i in g {
            for (s, x) in sum.iter_mut().zip(&unit[i]) {
                *s += x;
            }
        }
        let norm = sum.iter().map(|x| x * x).sum::<f32>().sqrt().max(1e-9);
        sum.iter().map(|x| x / norm).collect()
    };
    let mut centres: Vec<Vec<f32>> = groups.iter().map(|g| centre(g)).collect();
    small.sort_by_key(|g| std::cmp::Reverse(g.len()));
    for stray in small {
        let c = centre(&stray);
        let nearest = (0..groups.len())
            .filter(|&g| groups[g].len() + stray.len() <= max * 2)
            .max_by(|&a, &b| {
                let sim = |g: usize| centres[g].iter().zip(&c).map(|(p, q)| p * q).sum::<f32>();
                sim(a).total_cmp(&sim(b))
            });
        match nearest {
            Some(g) => {
                groups[g].extend(stray);
                centres[g] = centre(&groups[g]);
            }
            None => {
                centres.push(c);
                groups.push(stray);
            }
        }
    }
    for g in &mut groups {
        g.sort_unstable();
    }
    groups.sort_by_key(|g| std::cmp::Reverse(g.len()));
    groups
}
