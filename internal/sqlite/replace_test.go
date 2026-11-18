package sqlite

import (
	"os"
	"path/filepath"
	"strings"
	"testing"

	"recallport/internal/memory"
)

func TestReplaceAndRecover(t *testing.T) {
	ctx := t.Context()
	dir := t.TempDir()
	s, err := Open(filepath.Join(dir, "memory.db"), nil)
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	a := memory.Actor{ID: "test", Label: "Test"}
	x, err := s.Remember(ctx, a, memory.Draft{Text: "earlier decision", Occurred: "2025-03"})
	if err != nil {
		t.Fatal(err)
	}
	y, err := s.Remember(ctx, a, memory.Draft{Text: "a follow-up", References: []memory.Reference{{ID: x.ID, Reason: "revisited"}}})
	if err != nil {
		t.Fatal(err)
	}
	snapshot := filepath.Join(dir, "backup.db")
	if err = s.Snapshot(ctx, snapshot); err != nil {
		t.Fatal(err)
	}
	if err = s.Edit(ctx, x.ID, 1, memory.Draft{Text: "unbacked change"}); err != nil {
		t.Fatal(err)
	}
	if err = s.Replace(ctx, strings.NewReader("not sqlite")); err == nil {
		t.Fatal("invalid snapshot accepted")
	}
	if e, err := s.Read(ctx, x.ID); err != nil || e.Text != "unbacked change" {
		t.Fatalf("original changed: %v %v", e, err)
	}
	f, err := os.Open(snapshot)
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	if err = s.Replace(ctx, f); err != nil {
		t.Fatal(err)
	}
	if e, err := s.Read(ctx, x.ID); err != nil || e.Text != "earlier decision" || len(e.CitedBy) != 1 || e.CitedBy[0].ID != y.ID {
		t.Fatalf("restored content: %v %v", e, err)
	}
	if result, err := s.Recall(ctx, memory.Query{Text: "earlier"}); err != nil || len(result.Memories) != 1 {
		t.Fatalf("restored index: %v %v", result, err)
	}
	recovery, _, err := s.OpenRecovery()
	if err != nil {
		t.Fatal(err)
	}
	defer recovery.Close()
	if err = s.Replace(ctx, recovery); err != nil {
		t.Fatal(err)
	}
	if e, err := s.Read(ctx, x.ID); err != nil || e.Text != "unbacked change" {
		t.Fatalf("recovered content: %v %v", e, err)
	}
}
