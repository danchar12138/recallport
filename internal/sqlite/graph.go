package sqlite

import (
	"context"
	"database/sql"
	"strings"

	"recallport/internal/memory"
)

const graphLimit = 30

// Graph returns an induced subgraph of the requested records or one hop around a focus.
func (s *Store) Graph(ctx context.Context, ids []string, focus string, suggestions bool) (memory.Graph, error) {
	g := memory.Graph{Nodes: []memory.GraphNode{}, Edges: []memory.GraphEdge{}}
	if len(ids) > graphLimit || (focus != "" && len(focus) != 32) {
		return g, memory.ErrInvalid
	}
	var related []memory.Hit
	if focus != "" && suggestions && s.UsesVectors() {
		var err error
		related, err = s.Related(ctx, focus)
		if err != nil {
			return g, err
		}
	}
	tx, err := s.db.BeginTx(ctx, &sql.TxOptions{ReadOnly: true})
	if err != nil {
		return g, err
	}
	defer tx.Rollback()
	seen := make(map[string]bool)
	ordered := make([]string, 0, graphLimit+6)
	add := func(id string) {
		if !seen[id] {
			ordered = append(ordered, id)
			seen[id] = true
		}
	}
	if focus != "" {
		add(focus)
		rows, err := tx.QueryContext(ctx, `SELECT CASE WHEN r.source_id=? THEN r.target_id ELSE r.source_id END neighbor FROM refs r WHERE r.source_id=? OR r.target_id=? GROUP BY neighbor ORDER BY neighbor LIMIT ?`, focus, focus, focus, graphLimit)
		if err != nil {
			return g, err
		}
		for rows.Next() {
			var id string
			if err = rows.Scan(&id); err != nil {
				rows.Close()
				return g, err
			}
			if len(ordered) >= graphLimit {
				g.More = true
				break
			}
			add(id)
		}
		err = rows.Err()
		rows.Close()
		if err != nil {
			return g, err
		}
		for _, hit := range related {
			add(hit.ID)
		}
	} else {
		for _, id := range ids {
			add(id)
		}
	}
	if len(ordered) == 0 {
		return g, nil
	}
	params := make([]any, len(ordered))
	for i, id := range ordered {
		params[i] = id
	}
	marks := strings.TrimSuffix(strings.Repeat("?,", len(params)), ",")
	rows, err := tx.QueryContext(ctx, `SELECT id,body,occurred,source FROM memories WHERE id IN (`+marks+`)`, params...)
	if err != nil {
		return g, err
	}
	nodes := map[string]memory.GraphNode{}
	for rows.Next() {
		var n memory.GraphNode
		if err = rows.Scan(&n.ID, &n.Text, &n.Occurred, &n.Source); err != nil {
			rows.Close()
			return g, err
		}
		n.Text = trimRunes(n.Text, 340)
		nodes[n.ID] = n
	}
	err = rows.Err()
	rows.Close()
	if err != nil {
		return g, err
	}
	if focus != "" && nodes[focus].ID == "" {
		return g, memory.ErrNotFound
	}
	for _, id := range ordered {
		if n, ok := nodes[id]; ok {
			g.Nodes = append(g.Nodes, n)
		}
	}
	rows, err = tx.QueryContext(ctx, `SELECT source_id,target_id,reason FROM refs WHERE source_id IN (`+marks+`) AND target_id IN (`+marks+`) ORDER BY source_id,target_id`, append(params, params...)...)
	if err != nil {
		return g, err
	}
	explicit := map[string]bool{}
	for rows.Next() {
		var e memory.GraphEdge
		if err = rows.Scan(&e.From, &e.To, &e.Reason); err != nil {
			rows.Close()
			return g, err
		}
		g.Edges = append(g.Edges, e)
		if e.From == focus {
			explicit[e.To] = true
		}
		if e.To == focus {
			explicit[e.From] = true
		}
	}
	err = rows.Err()
	rows.Close()
	if err != nil {
		return g, err
	}
	for _, hit := range related {
		if nodes[hit.ID].ID != "" && !explicit[hit.ID] {
			g.Edges = append(g.Edges, memory.GraphEdge{From: focus, To: hit.ID, Suggested: true})
		}
	}
	return g, nil
}
