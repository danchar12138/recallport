package sqlite

import (
	"context"
	"log"
	"sync"
	"time"

	"recallport/internal/memory"
)

type indexManager struct {
	mu     sync.Mutex
	wake   chan struct{}
	force  bool
	status memory.IndexStatus
}

func (s *Store) RequestRebuild() bool {
	i := &s.index
	i.mu.Lock()
	defer i.mu.Unlock()
	if i.force || i.status.State == "working" {
		return false
	}
	i.force = true
	i.status.State = "queued"
	s.scheduleCheck()
	return true
}

func (s *Store) scheduleCheck() {
	select {
	case s.index.wake <- struct{}{}:
	default:
	}
}

func (s *Store) MaintainIndex(ctx context.Context) {
	ticker := time.NewTicker(time.Minute)
	defer ticker.Stop()
	s.checkIndex(ctx)
	for {
		select {
		case <-ctx.Done():
			return
		case <-ticker.C:
		case <-s.index.wake:
		}
		if ctx.Err() == nil {
			s.checkIndex(ctx)
		}
	}
}

func (s *Store) checkIndex(ctx context.Context) {
	i := &s.index
	i.mu.Lock()
	force := i.force
	i.force = false
	i.status.State = "working"
	i.status.Problem = ""
	i.mu.Unlock()
	s.maintenance.Lock()
	changed, keyword, err := s.repairIndex(ctx, force)
	s.maintenance.Unlock()
	now := time.Now()
	i.mu.Lock()
	defer i.mu.Unlock()
	i.status.CheckedAt = &now
	i.status.Keyword = keyword
	i.status.State = "ready"
	if err != nil {
		i.status.State = "error"
		i.status.Problem = "index_check_failed"
		if ctx.Err() == nil {
			log.Printf("search index: %v", err)
		}
	} else if changed {
		i.status.RepairedAt = &now
	}
	if i.force {
		i.status.State = "queued"
	}
}

func (s *Store) repairIndex(ctx context.Context, force bool) (bool, string, error) {
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return false, "unknown", err
	}
	defer tx.Rollback()
	var fts, triggers, vectors int
	if err = tx.QueryRowContext(ctx, `SELECT count(*) FILTER(WHERE type='table' AND name='memory_fts'), count(*) FILTER(WHERE type='trigger' AND name IN ('index_insert','index_update','index_delete')), count(*) FILTER(WHERE type='table' AND name='vectors') FROM sqlite_schema`).Scan(&fts, &triggers, &vectors); err != nil {
		return false, "unknown", err
	}
	structural := fts == 0 || triggers != 3
	if structural {
		if _, err = tx.ExecContext(ctx, `DROP TRIGGER IF EXISTS index_insert; DROP TRIGGER IF EXISTS index_update; DROP TRIGGER IF EXISTS index_delete; DROP TABLE IF EXISTS memory_fts;`+ftsSchema); err != nil {
			return false, "degraded", err
		}
	}
	fixFTS := force || structural
	if !fixFTS {
		_, err = tx.ExecContext(ctx, `INSERT INTO memory_fts(memory_fts,rank) VALUES('integrity-check',1)`)
		fixFTS = err != nil
	}
	if fixFTS {
		if _, err = tx.ExecContext(ctx, `INSERT INTO memory_fts(memory_fts) VALUES('rebuild')`); err != nil {
			return false, "degraded", err
		}
	}
	if vectors == 0 {
		if _, err = tx.ExecContext(ctx, vectorSchema); err != nil {
			return false, "ready", err
		}
	}
	if err = tx.Commit(); err != nil {
		return false, "degraded", err
	}
	changed := fixFTS || vectors == 0
	if s.embed != nil {
		n, err := s.reindex(ctx, force)
		if err != nil {
			return changed, "ready", err
		}
		changed = changed || n > 0
	}
	return changed, "ready", nil
}

func (s *Store) IndexStatus(ctx context.Context) memory.IndexStatus {
	s.index.mu.Lock()
	out := s.index.status
	s.index.mu.Unlock()
	if out.State == "" {
		out.State = "queued"
	}
	if out.Keyword == "" {
		out.Keyword = "unknown"
	}
	if s.embed != nil {
		out.Model = s.embed.ID()
	}
	if err := s.db.QueryRowContext(ctx, `SELECT count(*) FROM memories`).Scan(&out.Total); err != nil {
		return indexStatusError(out)
	}
	if s.embed == nil {
		return out
	}
	if err := s.db.QueryRowContext(ctx, `SELECT count(*) FILTER (WHERE length(v.vector)>0), count(*) FILTER (WHERE length(v.vector)=0) FROM vectors v JOIN memories m ON m.id=v.memory_id WHERE v.model=? AND `+validVector, s.indexID(), max(1, s.vectorBytes)).Scan(&out.Indexed, &out.Skipped); err != nil {
		s.scheduleCheck()
		return indexStatusError(out)
	}
	if out.State == "ready" && out.Total > out.Indexed+out.Skipped {
		out.State = "degraded"
		s.scheduleCheck()
	}
	return out
}

const validVector = `typeof(v.vector)='blob' AND length(v.vector) % ?=0 AND length(v.vector)<=9216`

func indexStatusError(out memory.IndexStatus) memory.IndexStatus {
	if out.State != "working" && out.State != "queued" {
		out.State = "degraded"
	}
	out.Problem = "index_check_failed"
	return out
}
