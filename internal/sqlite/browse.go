package sqlite

import (
	"context"
	"fmt"
	"strings"

	"recallport/internal/memory"
)

func (s *Store) Periods(ctx context.Context) ([]memory.Period, error) {
	rows, err := s.db.QueryContext(ctx, `SELECT substr(occurred,1,7),count(*) FROM memories GROUP BY 1 ORDER BY 1 DESC`)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []memory.Period
	for rows.Next() {
		var p memory.Period
		if err := rows.Scan(&p.Date, &p.Count); err != nil {
			return nil, err
		}
		out = append(out, p)
	}
	return out, rows.Err()
}

func (s *Store) Related(ctx context.Context, id string) ([]memory.Hit, error) {
	e, err := s.Read(ctx, id)
	if err != nil {
		return nil, err
	}
	if s.embed == nil {
		return nil, nil
	}
	q := trimRunes(e.Text, 300) + " " + trimRunes(e.Context, 150)
	if strings.TrimSpace(q) == "" {
		return nil, nil
	}
	res, err := s.Recall(ctx, memory.Query{Text: q, Limit: 20, Strategy: "semantic"})
	if err != nil {
		return nil, err
	}
	if res.Mode != "semantic" {
		return nil, nil
	}
	seen := map[string]bool{id: true}
	for _, rs := range [][]memory.Reference{e.References, e.CitedBy} {
		for _, r := range rs {
			seen[r.ID] = true
		}
	}
	var out []memory.Hit
	for _, h := range res.Memories {
		if !seen[h.ID] {
			out = append(out, h)
			if len(out) == 5 {
				break
			}
		}
	}
	return out, nil
}

func (s *Store) SetReference(ctx context.Context, id string, r memory.Reference, remove bool) error {
	r.Reason = strings.TrimSpace(r.Reason)
	if len(id) != 32 || len(r.ID) != 32 || id == r.ID || len(r.Reason) > 800 {
		return memory.ErrInvalid
	}
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	if remove {
		if _, err = tx.ExecContext(ctx, `DELETE FROM refs WHERE source_id=? AND target_id=?`, id, r.ID); err != nil {
			return err
		}
		return tx.Commit()
	}
	var count int
	if err = tx.QueryRowContext(ctx, `SELECT count(*) FROM memories WHERE id IN (?,?)`, id, r.ID).Scan(&count); err != nil {
		return err
	}
	if count != 2 {
		return memory.ErrNotFound
	}
	if err = tx.QueryRowContext(ctx, `SELECT count(*) FROM refs WHERE source_id=? AND target_id<>?`, id, r.ID).Scan(&count); err != nil {
		return err
	}
	if count >= 16 {
		return fmt.Errorf("%w: maximum 16 outgoing references", memory.ErrInvalid)
	}
	if _, err = tx.ExecContext(ctx, `INSERT INTO refs VALUES(?,?,?) ON CONFLICT(source_id,target_id) DO UPDATE SET reason=excluded.reason`, id, r.ID, r.Reason); err != nil {
		return err
	}
	return tx.Commit()
}
