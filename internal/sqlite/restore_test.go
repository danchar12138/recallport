package sqlite

import (
	"os"
	"path/filepath"
	"testing"

	. "recallport/internal/memory"
)

func TestRestoreSnapshotAndV1Upgrade(t *testing.T) {
	ctx := t.Context()
	dir := t.TempDir()
	s, err := Open(filepath.Join(dir, "live.db"), nil)
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	entry, err := s.Remember(ctx, Actor{ID: "old", Label: "Old source"}, Draft{Text: "RecallPort recovery test", Occurred: "2024-06"})
	if err != nil {
		t.Fatal(err)
	}
	if _, err = s.db.ExecContext(ctx, "CREATE TABLE credentials (id TEXT PRIMARY KEY); PRAGMA user_version=1"); err != nil {
		t.Fatal(err)
	}
	snapshot := filepath.Join(dir, "backup.db")
	if err = s.Snapshot(ctx, snapshot); err != nil {
		t.Fatal(err)
	}
	recovered := filepath.Join(dir, "restored data")
	if err = Restore(ctx, snapshot, recovered); err != nil {
		t.Fatal(err)
	}
	if err = Restore(ctx, snapshot, recovered); err == nil {
		t.Fatal("restore overwrote an existing directory")
	}
	r, err := Open(filepath.Join(recovered, "memory.db"), nil)
	if err != nil {
		t.Fatal(err)
	}
	defer r.Close()
	got, err := r.Read(ctx, entry.ID)
	if err != nil || got.Source != entry.Source {
		t.Fatalf("read restored memory: %+v %v", got, err)
	}
	results, err := r.Recall(ctx, Query{Text: "RecallPort"})
	if err != nil || len(results.Memories) != 1 {
		t.Fatalf("restored index: %+v %v", results, err)
	}
	var count int
	if err = r.db.QueryRowContext(ctx, "SELECT count(*) FROM sqlite_schema WHERE name='credentials'").Scan(&count); err != nil || count != 0 {
		t.Fatalf("obsolete credentials table: %d, %v", count, err)
	}
	bad := filepath.Join(dir, "bad.db")
	if err = os.WriteFile(bad, []byte("not a snapshot"), 0600); err != nil {
		t.Fatal(err)
	}
	badDest := filepath.Join(dir, "bad restore")
	if err = Restore(ctx, bad, badDest); err == nil {
		t.Fatal("invalid file restored")
	}
	if _, err = os.Stat(badDest); !os.IsNotExist(err) {
		t.Fatalf("invalid restore left a data directory: %v", err)
	}
}
