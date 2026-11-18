package sqlite

import (
	"errors"
	"path/filepath"
	"testing"

	. "recallport/internal/memory"
)

func TestMemoriesSurviveCorrectionsAndBackup(t *testing.T) {
	ctx := t.Context()
	dir := t.TempDir()
	s, err := Open(filepath.Join(dir, "personal # library.db"), nil)
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	a := Actor{ID: "one", Label: "Client One"}
	b := Actor{ID: "two", Label: "Client Two"}
	d := Draft{Text: "旧电脑使用 Go，工作目录 D:\\work。", Occurred: "2024-06", RequestID: "retry-1", Origin: "old-machine-setup"}
	first, err := s.Remember(ctx, a, d)
	if err != nil {
		t.Fatal(err)
	}
	second, err := s.Remember(ctx, a, d)
	if err != nil || second.ID != first.ID {
		t.Fatalf("idempotency: %v %v", second, err)
	}
	changed := d
	changed.Text = "different"
	if _, err = s.Remember(ctx, a, changed); !errors.Is(err, ErrConflict) {
		t.Fatalf("id reuse: %v", err)
	}
	account, err := s.Remember(ctx, b, Draft{Text: "旧电脑使用 Go。", Occurred: "2024-06", Origin: d.Origin, References: []Reference{{ID: first.ID, Reason: "same event"}}})
	if err != nil {
		t.Fatal(err)
	}
	newer, err := s.Remember(ctx, b, Draft{Text: "新电脑使用 Go，但没有 D:\\work。", Occurred: "2025-01"})
	if err != nil {
		t.Fatal(err)
	}
	if _, err = s.Remember(ctx, b, Draft{Text: "日期未知"}); err != nil {
		t.Fatal(err)
	}
	res, err := s.Recall(ctx, Query{Text: "Go"})
	if err != nil || len(res.Memories) != 2 {
		t.Fatalf("grouping: %+v %v", res, err)
	}
	for _, hit := range res.Memories {
		if hit.ID != newer.ID && len(hit.Accounts) != 1 {
			t.Fatalf("missing other account: %+v", hit)
		}
	}
	res, err = s.Recall(ctx, Query{Text: `D:\work`, From: "2024-06-30", Through: "2024-06-30"})
	if err != nil || len(res.Memories) != 1 || res.Memories[0].ID != first.ID {
		t.Fatalf("exact and partial date: %+v %v", res, err)
	}
	res, err = s.Timeline(ctx, Query{}, 0)
	if err != nil || len(res.Memories) != 4 || res.Memories[0].ID != newer.ID || res.Memories[3].Occurred != "" {
		t.Fatalf("timeline: %+v %v", res, err)
	}
	e, err := s.Read(ctx, first.ID)
	if err != nil || len(e.CitedBy) != 1 || e.CitedBy[0].ID != account.ID {
		t.Fatalf("backlink: %+v %v", e, err)
	}
	if err = s.Edit(ctx, first.ID, 1, Draft{Text: "旧电脑工作目录是 E:\\work。", Occurred: d.Occurred}); err != nil {
		t.Fatal(err)
	}
	if err = s.Delete(ctx, first.ID, 1); !errors.Is(err, ErrConflict) {
		t.Fatalf("stale revision: %v", err)
	}
	res, err = s.Recall(ctx, Query{Text: `E:\work`})
	if err != nil || len(res.Memories) != 1 || res.Memories[0].ID != first.ID {
		t.Fatalf("corrected index: %+v %v", res, err)
	}
	if err = s.Delete(ctx, account.ID, 1); err != nil {
		t.Fatal(err)
	}
	if _, err = s.Read(ctx, account.ID); !errors.Is(err, ErrNotFound) {
		t.Fatal(err)
	}
	path := filepath.Join(dir, "restore.db")
	if err = s.Snapshot(ctx, path); err != nil {
		t.Fatal(err)
	}
	restored, err := Open(path, nil)
	if err != nil {
		t.Fatal(err)
	}
	defer restored.Close()
	e, err = restored.Read(ctx, first.ID)
	if err != nil || e.Revision != 2 || len(e.CitedBy) != 0 {
		t.Fatalf("restore: %+v %v", e, err)
	}
	if _, err = restored.Read(ctx, account.ID); !errors.Is(err, ErrNotFound) {
		t.Fatalf("deleted memory restored: %v", err)
	}
}
