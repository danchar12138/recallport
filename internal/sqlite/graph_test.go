package sqlite

import (
	"errors"
	"path/filepath"
	"strings"
	"testing"

	"recallport/internal/memory"
)

func TestGraphSeparatesReferencesAndSuggestions(t *testing.T) {
	ctx := t.Context()
	s, err := Open(filepath.Join(t.TempDir(), "memory.db"), testEmbedding{})
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	add := func(text string, refs ...memory.Reference) string {
		t.Helper()
		entry, err := s.Remember(ctx, memory.Actor{ID: "test", Label: "Test"}, memory.Draft{Text: text, References: refs})
		if err != nil {
			t.Fatal(err)
		}
		return entry.ID
	}
	a := add("first memory")
	b := add("second memory", memory.Reference{ID: a, Reason: "follow-up"})
	c := add("third memory")
	full, err := s.Graph(ctx, []string{a, b, c}, "", false)
	if err != nil || len(full.Nodes) != 3 || len(full.Edges) != 1 || full.Edges[0].From != b || full.Edges[0].To != a || full.Edges[0].Reason != "follow-up" {
		t.Fatalf("full graph: %+v %v", full, err)
	}
	local, err := s.Graph(ctx, nil, a, false)
	if err != nil || len(local.Nodes) != 2 || len(local.Edges) != 1 || local.Nodes[0].ID != a {
		t.Fatalf("local graph: %+v %v", local, err)
	}
	local, err = s.Graph(ctx, nil, a, true)
	if err != nil || len(local.Nodes) != 3 || len(local.Edges) != 2 || local.Edges[0].Suggested || !local.Edges[1].Suggested || local.Edges[1].To != c {
		t.Fatalf("with suggestions: %+v %v", local, err)
	}
	e, err := s.Read(ctx, a)
	if err != nil || len(e.References) != 0 || len(e.CitedBy) != 1 {
		t.Fatalf("suggestion was persisted: %+v %v", e, err)
	}
	if _, err = s.Graph(ctx, nil, strings.Repeat("z", 32), false); !errors.Is(err, memory.ErrNotFound) {
		t.Fatalf("missing focus: %v", err)
	}
}
