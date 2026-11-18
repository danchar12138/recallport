package sqlite

import (
	"path/filepath"
	"testing"

	"recallport/internal/memory"
)

type testEmbedding struct{}

func (testEmbedding) ID() string { return "test-index" }
func (testEmbedding) Embed(text string) []float32 {
	if text == "!\n" {
		return nil
	}
	return []float32{1, 0, 0}
}

func TestIndexAutoRepairAndLiteralFallback(t *testing.T) {
	ctx := t.Context()
	s, err := Open(filepath.Join(t.TempDir(), "memory.db"), testEmbedding{})
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	actor := memory.Actor{ID: "test", Label: "test"}
	e, err := s.Remember(ctx, actor, memory.Draft{Text: "A distinctive record"})
	if err != nil {
		t.Fatal(err)
	}
	if _, err = s.Remember(ctx, actor, memory.Draft{Text: "!"}); err != nil {
		t.Fatal(err)
	}
	if _, err = s.db.ExecContext(ctx, `DROP TABLE memory_fts; DROP TABLE vectors`); err != nil {
		t.Fatal(err)
	}
	r, err := s.Recall(ctx, memory.Query{Text: "distinctive"})
	if err != nil || !r.Degraded || len(r.Memories) != 1 || r.Memories[0].ID != e.ID {
		t.Fatalf("fallback: %+v %v", r, err)
	}
	s.checkIndex(ctx)
	check := func() {
		t.Helper()
		status := s.IndexStatus(ctx)
		if status.State != "ready" || status.Keyword != "ready" || status.Indexed != 1 || status.Skipped != 1 || status.Total != 2 || status.RepairedAt == nil {
			t.Fatalf("index status: %+v", status)
		}
	}
	check()
	if _, err = s.db.ExecContext(ctx, `UPDATE vectors SET vector=X'00' WHERE memory_id=?`, e.ID); err != nil {
		t.Fatal(err)
	}
	if status := s.IndexStatus(ctx); status.State != "degraded" {
		t.Fatalf("invalid vector not detected: %+v", status)
	}
	if _, err = s.db.ExecContext(ctx, `INSERT INTO memory_fts(memory_fts) VALUES('delete-all')`); err != nil {
		t.Fatal(err)
	}
	s.checkIndex(ctx)
	check()
	var matched int
	if err = s.db.QueryRowContext(ctx, `SELECT count(*) FROM memory_fts WHERE memory_fts MATCH 'distinctive'`).Scan(&matched); err != nil || matched != 1 {
		t.Fatalf("stale FTS not repaired: %d %v", matched, err)
	}
	r, err = s.Recall(ctx, memory.Query{Text: "reworded"})
	if err != nil || r.Mode != "hybrid" || len(r.Memories) != 1 {
		t.Fatalf("semantic index not restored: %+v %v", r, err)
	}
	r, err = s.Recall(ctx, memory.Query{Text: "reworded", Strategy: "semantic"})
	if err != nil || r.Mode != "semantic" || len(r.Memories) != 1 {
		t.Fatalf("semantic strategy: %+v %v", r, err)
	}
	r, err = s.Recall(ctx, memory.Query{Text: "reworded", Strategy: "lexical"})
	if err != nil || r.Mode != "lexical" || len(r.Memories) != 0 {
		t.Fatalf("lexical strategy used semantic results: %+v %v", r, err)
	}
}
