package transport

import (
	"encoding/json"
	"errors"
	"net/http"
	"strconv"
	"strings"

	"recallport/internal/memory"
)

const graphPageSize = 30

func graphQuery(r *http.Request) memory.Query {
	v := r.URL.Query()
	return memory.Query{Text: strings.TrimSpace(v.Get("q")), From: strings.TrimSpace(v.Get("from")), Through: strings.TrimSpace(v.Get("through")), Strategy: v.Get("strategy"), Period: v.Get("period"), Undated: v.Get("undated") == "1", Limit: graphPageSize}
}

func (h *web) graphPage(w http.ResponseWriter, r *http.Request) {
	q := graphQuery(r)
	h.render(w, page{View: "graph", Query: q, Period: q.Period, Undated: q.Undated})
}

func (h *web) graphData(w http.ResponseWriter, r *http.Request) {
	q := graphQuery(r)
	focus := r.URL.Query().Get("focus")
	n, _ := strconv.Atoi(r.URL.Query().Get("page"))
	n = max(1, min(n, 10000))
	var result memory.Results
	var err error
	if focus == "" {
		if q.Text != "" {
			result, err = h.store.Recall(r.Context(), q)
			n = 1
		} else {
			result, err = h.store.Timeline(r.Context(), q, (n-1)*q.Limit)
		}
	}
	if err != nil {
		graphError(w, err)
		return
	}
	ids := make([]string, 0, len(result.Memories))
	for _, hit := range result.Memories {
		ids = append(ids, hit.ID)
	}
	g, err := h.store.Graph(r.Context(), ids, focus, r.URL.Query().Get("suggested") == "1")
	if err != nil {
		graphError(w, err)
		return
	}
	w.Header().Set("Content-Type", "application/json")
	_ = json.NewEncoder(w).Encode(struct {
		memory.Graph
		Page     int  `json:"page"`
		Next     bool `json:"next"`
		Limited  bool `json:"limited"`
		Degraded bool `json:"degraded"`
	}{g, n, focus == "" && q.Text == "" && result.More && n < 10000, q.Text != "" && result.More, result.Degraded})
}

func graphError(w http.ResponseWriter, err error) {
	status := http.StatusInternalServerError
	if errors.Is(err, memory.ErrInvalid) {
		status = http.StatusBadRequest
	} else if errors.Is(err, memory.ErrNotFound) {
		status = http.StatusNotFound
	}
	http.Error(w, errorText(err), status)
}
