package sqlite

import (
	"path/filepath"
	"testing"

	"recallport/internal/memory"
)

type testEmbedding struct{}

type graniteTestEmbedding struct{}

func (graniteTestEmbedding) ID() string { return "granite-test-384" }
func (graniteTestEmbedding) Embed(s string) []float32 {
	v := make([]float32, 384)
	v[0] = 1
	return v
}

func TestMigrationRebuildsWideVectors(t *testing.T) {
	ctx := t.Context()
	path := filepath.Join(t.TempDir(), "memory.db")
	old, err := Open(path, testEmbedding{})
	if err != nil {
		t.Fatal(err)
	}
	actor := memory.Actor{ID: "test", Label: "test"}
	first, err := old.Remember(ctx, actor, memory.Draft{Text: "Original linked memory"})
	if err != nil {
		t.Fatal(err)
	}
	entry, err := old.Remember(ctx, actor, memory.Draft{Text: "number one passage; number two passage; number three passage; number four passage; number five passage; number six passage; number seven passage; number eight passage", References: []memory.Reference{{ID: first.ID}}})
	if err != nil {
		t.Fatal(err)
	}
	if err = old.Close(); err != nil {
		t.Fatal(err)
	}
	next, err := Open(path, graniteTestEmbedding{})
	if err != nil {
		t.Fatal(err)
	}
	defer next.Close()
	next.checkIndex(ctx)
	status := next.IndexStatus(ctx)
	if status.Semantic.State != "ready" || status.Indexed != 2 || status.Dimensions != 384 {
		t.Fatalf("index repair: %+v", status)
	}
	var size int
	if err = next.db.QueryRowContext(ctx, "SELECT length(vector) FROM vectors WHERE memory_id=?", entry.ID).Scan(&size); err != nil || size != 384*4*memory.MaxPassages {
		t.Fatalf("vector size=%d, error=%v", size, err)
	}
	got, err := next.Read(ctx, entry.ID)
	if err != nil || got.Text != entry.Text || len(got.References) != 1 || got.References[0].ID != first.ID {
		t.Fatalf("memory after migration: %+v (%v)", got, err)
	}
}

func (testEmbedding) ID() string { return "test-index" }
func (testEmbedding) Embed(text string) []float32 {
	if text == "!\n" {
		return nil
	}
	return []float32{1, 0, 0}
}

func TestManualIndexScopes(t *testing.T) {
	ctx := t.Context()
	s, err := Open(filepath.Join(t.TempDir(), "memory.db"), testEmbedding{})
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	s.checkIndex(ctx)
	baseline := s.IndexStatus(ctx)
	if !s.RequestRebuild(memory.KeywordIndex) {
		t.Fatal("keyword request not queued")
	}
	s.checkIndex(ctx)
	keyword := s.IndexStatus(ctx)
	if keyword.Keyword.RepairedAt == nil || keyword.Semantic.RepairedAt != baseline.Semantic.RepairedAt {
		t.Fatalf("keyword scope: %+v", keyword)
	}
	if !s.RequestRebuild(memory.SemanticIndex) {
		t.Fatal("semantic request not queued")
	}
	s.checkIndex(ctx)
	semantic := s.IndexStatus(ctx)
	if semantic.Semantic.RepairedAt == nil || semantic.Keyword.RepairedAt != keyword.Keyword.RepairedAt {
		t.Fatalf("semantic scope: %+v", semantic)
	}
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
		if status.Semantic.State != "ready" || status.Keyword.State != "ready" || status.Indexed != 1 || status.Skipped != 1 || status.Total != 2 || status.Semantic.RepairedAt == nil || status.Keyword.RepairedAt == nil {
			t.Fatalf("index status: %+v", status)
		}
	}
	check()
	if _, err = s.db.ExecContext(ctx, `UPDATE vectors SET vector=X'00' WHERE memory_id=?`, e.ID); err != nil {
		t.Fatal(err)
	}
	if status := s.IndexStatus(ctx); status.Semantic.State != "degraded" {
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
