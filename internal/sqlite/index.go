package sqlite

import (
	"context"
	"log"
	"sync"
	"time"

	"recallport/internal/memory"
)

type indexManager struct {
	mu                sync.Mutex
	wake              chan struct{}
	keyword, semantic bool
	status            memory.IndexStatus
}

func (s *Store) RequestRebuild(kind memory.IndexKind) bool {
	i := &s.index
	i.mu.Lock()
	defer i.mu.Unlock()
	added := false
	if (kind == memory.KeywordIndex || kind == memory.AllIndexes) && !i.keyword && i.status.Keyword.State != "working" {
		i.keyword = true
		i.status.Keyword.State = "queued"
		added = true
	}
	if (kind == memory.SemanticIndex || kind == memory.AllIndexes) && s.embed != nil && !i.semantic && i.status.Semantic.State != "working" {
		i.semantic = true
		i.status.Semantic.State = "queued"
		added = true
	}
	if added {
		s.scheduleCheck()
	}
	return added
}

func (s *Store) scheduleCheck() {
	select {
	case s.index.wake <- struct{}{}:
	default:
	}
}

func (s *Store) MaintainIndex(ctx context.Context) {
	ticker := time.NewTicker(memory.MaintenanceSeconds * time.Second)
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
	k, v := i.keyword, i.semantic
	i.keyword, i.semantic = false, false
	auto := !k && !v
	if auto || k {
		i.status.Keyword.State = "working"
		i.status.Keyword.Problem = ""
	}
	if s.embed != nil && (auto || v) {
		i.status.Semantic.State = "working"
		i.status.Semantic.Problem = ""
	}
	i.mu.Unlock()
	s.maintenance.Lock()
	defer s.maintenance.Unlock()
	if auto || k {
		changed, err := s.repairKeyword(ctx, k)
		i.mu.Lock()
		finishIndex(ctx, "keyword", &i.status.Keyword, changed, err)
		i.mu.Unlock()
	}
	if auto || v {
		changed, err := s.repairVectors(ctx, v)
		i.mu.Lock()
		finishIndex(ctx, "semantic", &i.status.Semantic, changed, err)
		i.mu.Unlock()
	}
}

func finishIndex(ctx context.Context, name string, h *memory.IndexHealth, changed bool, err error) {
	now := time.Now()
	h.CheckedAt = &now
	h.State = "ready"
	if err != nil {
		h.State = "error"
		h.Problem = "index_check_failed"
		if ctx.Err() == nil {
			log.Printf("%s search index: %v", name, err)
		}
	} else if changed {
		h.RepairedAt = &now
	}
}

func (s *Store) repairKeyword(ctx context.Context, force bool) (bool, error) {
	tx, err := s.db.BeginTx(ctx, nil)
	if err != nil {
		return false, err
	}
	defer tx.Rollback()
	var fts, triggers int
	if err = tx.QueryRowContext(ctx, `SELECT count(*) FILTER(WHERE type='table' AND name='memory_fts'), count(*) FILTER(WHERE type='trigger' AND name IN ('index_insert','index_update','index_delete')) FROM sqlite_schema`).Scan(&fts, &triggers); err != nil {
		return false, err
	}
	structural := fts == 0 || triggers != 3
	if structural {
		if _, err = tx.ExecContext(ctx, `DROP TRIGGER IF EXISTS index_insert; DROP TRIGGER IF EXISTS index_update; DROP TRIGGER IF EXISTS index_delete; DROP TABLE IF EXISTS memory_fts;`+ftsSchema); err != nil {
			return false, err
		}
	}
	fixFTS := force || structural
	if !fixFTS {
		_, err = tx.ExecContext(ctx, `INSERT INTO memory_fts(memory_fts,rank) VALUES('integrity-check',1)`)
		fixFTS = err != nil
	}
	if fixFTS {
		if _, err = tx.ExecContext(ctx, `INSERT INTO memory_fts(memory_fts) VALUES('rebuild')`); err != nil {
			return false, err
		}
	}
	return fixFTS, tx.Commit()
}

func (s *Store) repairVectors(ctx context.Context, force bool) (bool, error) {
	var n int
	if err := s.db.QueryRowContext(ctx, `SELECT count(*) FROM sqlite_schema WHERE type='table' AND name='vectors'`).Scan(&n); err != nil {
		return false, err
	}
	if n == 0 {
		if _, err := s.db.ExecContext(ctx, vectorSchema); err != nil {
			return false, err
		}
	}
	indexed, err := s.reindex(ctx, force)
	return n == 0 || indexed > 0 || force, err
}

func (s *Store) IndexStatus(ctx context.Context) memory.IndexStatus {
	s.index.mu.Lock()
	out := s.index.status
	s.index.mu.Unlock()
	if out.Keyword.State == "" {
		out.Keyword.State = "queued"
	}
	if out.Semantic.State == "" {
		out.Semantic.State = "queued"
	}
	if s.embed != nil {
		out.Model = s.embed.ID()
		out.Dimensions = s.vectorBytes / 4
		out.Policy = memory.Policy(s.embed)
	} else {
		out.Semantic.State = "disabled"
	}
	if err := s.db.QueryRowContext(ctx, `SELECT count(*) FROM memories`).Scan(&out.Total); err != nil {
		indexStatusError(&out.Keyword)
		if s.embed != nil {
			indexStatusError(&out.Semantic)
		}
		return out
	}
	if s.embed == nil {
		return out
	}
	if err := s.db.QueryRowContext(ctx, `SELECT count(*) FILTER (WHERE length(v.vector)>0), count(*) FILTER (WHERE length(v.vector)=0) FROM vectors v JOIN memories m ON m.id=v.memory_id WHERE v.model=? AND `+validVector, s.indexID(), max(1, s.vectorBytes), s.vectorBytes*memory.MaxPassages).Scan(&out.Indexed, &out.Skipped); err != nil {
		s.scheduleCheck()
		indexStatusError(&out.Semantic)
		return out
	}
	if out.Semantic.State == "ready" && out.Total > out.Indexed+out.Skipped {
		out.Semantic.State = "degraded"
		s.scheduleCheck()
	}
	return out
}

const validVector = `typeof(v.vector)='blob' AND length(v.vector) % ?=0 AND length(v.vector)<=?`

func indexStatusError(out *memory.IndexHealth) {
	if out.State == "ready" {
		out.State = "degraded"
	}
	out.Problem = "index_check_failed"
}
