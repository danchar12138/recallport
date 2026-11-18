package sqlite

import (
	"context"
	"database/sql"
	"encoding/binary"
	"fmt"
	"math"
	"sort"
	"strings"
	"unicode"

	"recallport/internal/memory"
)

func (s *Store) makeVector(text, circumstances string) []byte {
	if s.embed == nil {
		return nil
	}
	text += "\n" + circumstances
	parts := []string{text}
	for _, fragment := range strings.FieldsFunc(text, func(r rune) bool { return strings.ContainsRune("\n。！？；，.!?;,", r) }) {
		fragment = strings.TrimSpace(fragment)
		if len([]rune(fragment)) >= 7 && len(parts) < 9 {
			parts = append(parts, fragment)
		}
	}
	var b []byte
	for _, part := range parts {
		for _, x := range s.embed.Embed(part) {
			b = binary.LittleEndian.AppendUint32(b, math.Float32bits(x))
		}
	}
	return b
}

func (s *Store) indexID() string { return s.embed.ID() + "/passages-v1" }

func (s *Store) setVector(ctx context.Context, tx *sql.Tx, id string, v []byte) error {
	if s.embed == nil {
		_, err := tx.ExecContext(ctx, "DELETE FROM vectors WHERE memory_id=?", id)
		return err
	}
	_, err := tx.ExecContext(ctx, `INSERT INTO vectors VALUES(?,?,coalesce(?, X'')) ON CONFLICT(memory_id) DO UPDATE SET model=excluded.model,vector=excluded.vector`, id, s.indexID(), v)
	return err
}

// Batch old memories without holding the live connection during inference.
func (s *Store) reindex(ctx context.Context, force bool) (int, error) {
	if s.embed == nil {
		return 0, nil
	}
	var rebuilt int
	var after int64
	for {
		rows, err := s.db.QueryContext(ctx, `SELECT m.rowid,m.id,m.body,m.circumstances,m.revision FROM memories m LEFT JOIN vectors v ON v.memory_id=m.id WHERE m.rowid>? AND (? OR v.model IS NULL OR v.model<>? OR NOT (`+validVector+`)) ORDER BY m.rowid LIMIT 64`, after, force, s.indexID(), max(1, s.vectorBytes))
		if err != nil {
			return rebuilt, err
		}
		var group []memory.Entry
		for rows.Next() {
			var e memory.Entry
			if err = rows.Scan(&after, &e.ID, &e.Text, &e.Context, &e.Revision); err != nil {
				rows.Close()
				return rebuilt, err
			}
			group = append(group, e)
		}
		err = rows.Err()
		rows.Close()
		if err != nil {
			return rebuilt, err
		}
		if len(group) == 0 {
			return rebuilt, nil
		}
		for _, e := range group {
			v := s.makeVector(e.Text, e.Context)
			_, err = s.db.ExecContext(ctx, `INSERT INTO vectors SELECT id,?,coalesce(?,X'') FROM memories WHERE id=? AND revision=? ON CONFLICT(memory_id) DO UPDATE SET model=excluded.model,vector=excluded.vector`, s.indexID(), v, e.ID, e.Revision)
			if err != nil {
				return rebuilt, err
			}
			rebuilt++
		}
	}
}

func timeFilter(q memory.Query) (string, []any, error) {
	if q.Undated {
		return " AND m.occurred=''", nil, nil
	}
	lo, _, err := memory.DateRange(q.From)
	if err != nil {
		return "", nil, err
	}
	_, hi, err := memory.DateRange(q.Through)
	if err != nil {
		return "", nil, err
	}
	if lo != "" && hi != "" && lo > hi {
		return "", nil, fmt.Errorf("%w: reversed date range", memory.ErrInvalid)
	}
	var cond string
	var args []any
	if q.Period != "" {
		if _, _, err := memory.DateRange(q.Period); err != nil {
			return "", nil, err
		}
		cond += " AND substr(m.occurred,1,7)=?"
		args = append(args, q.Period)
	}
	if lo != "" {
		cond += " AND m.end_day>=?"
		args = append(args, lo)
	}
	if hi != "" {
		cond += " AND m.start_day<>'' AND m.start_day<=?"
		args = append(args, hi)
	}
	return cond, args, nil
}

const chrono = " ORDER BY m.start_day DESC,m.recorded DESC,m.id DESC"

func (s *Store) Timeline(ctx context.Context, q memory.Query, offset int) (memory.Results, error) {
	result := memory.Results{Memories: []memory.Hit{}, Mode: "timeline"}
	cond, args, err := timeFilter(q)
	if err != nil {
		return result, err
	}
	limit := q.Limit
	if limit <= 0 || limit > 30 {
		limit = 30
	}
	if offset < 0 || offset > 1000000 {
		return result, fmt.Errorf("%w: invalid offset", memory.ErrInvalid)
	}
	rows, err := s.db.QueryContext(ctx, "SELECT "+fields+" FROM memories m WHERE 1=1"+cond+chrono+" LIMIT ? OFFSET ?", append(args, limit+1, offset)...)
	if err != nil {
		return result, err
	}
	defer rows.Close()
	for rows.Next() {
		e, err := scanEntry(rows)
		if err != nil {
			return result, err
		}
		if len(result.Memories) == limit {
			result.More = true
			break
		}
		result.Memories = append(result.Memories, memory.Hit{Entry: preview(e)})
	}
	return result, rows.Err()
}

func preview(e memory.Entry) memory.Entry {
	e.Text = trimRunes(e.Text, 500)
	e.Context = trimRunes(e.Context, 240)
	return e
}

func trimRunes(s string, n int) string {
	for i := range s {
		if n == 0 {
			return s[:i] + "\u2026"
		}
		n--
	}
	return s
}

func lexicalTerms(q string) string {
	terms := []string{}
	seen := map[string]bool{}
	add := func(s string) {
		if len([]rune(s)) >= 3 && !seen[s] && len(terms) < 36 {
			seen[s] = true
			terms = append(terms, `"`+strings.ReplaceAll(s, `"`, `""`)+`"`)
		}
	}
	add(q)
	for _, word := range strings.FieldsFunc(q, func(r rune) bool { return !unicode.IsLetter(r) && !unicode.IsNumber(r) }) {
		add(word)
		r := []rune(word)
		for i := 0; i+3 <= len(r); i++ {
			if unicode.Is(unicode.Han, r[i]) {
				add(string(r[i : i+3]))
			}
		}
	}
	return strings.Join(terms, " OR ")
}

type candidate struct {
	id    string
	rank  float64
	exact bool
}

func (s *Store) Recall(ctx context.Context, q memory.Query) (memory.Results, error) {
	q.Text = strings.TrimSpace(q.Text)
	if q.Limit < 0 || q.Limit > 30 {
		return memory.Results{}, fmt.Errorf("%w: limit must be 1-30", memory.ErrInvalid)
	}
	if q.Text == "" {
		if q.Limit <= 0 {
			q.Limit = 8
		}
		return s.Timeline(ctx, q, 0)
	}
	result := memory.Results{Memories: []memory.Hit{}, Mode: "lexical"}
	if len([]rune(q.Text)) > 600 {
		return result, fmt.Errorf("%w: query exceeds 600 characters", memory.ErrInvalid)
	}
	limit := q.Limit
	if limit == 0 {
		limit = 8
	}
	if limit < 1 || limit > 30 {
		return result, fmt.Errorf("%w: limit must be 1-30", memory.ErrInvalid)
	}
	cond, dates, err := timeFilter(q)
	if err != nil {
		return result, err
	}
	pathQuery := isAbsolutePathQuery(q.Text)
	switch q.Strategy {
	case "", "hybrid", "lexical", "semantic":
	default:
		return result, fmt.Errorf("%w: unknown search strategy", memory.ErrInvalid)
	}
	semanticOnly := q.Strategy == "semantic" && !pathQuery
	var queryVec []float32
	if s.embed != nil && !pathQuery && q.Strategy != "lexical" {
		queryVec = s.embed.Embed(q.Text)
	}
	tx, err := s.db.BeginTx(ctx, &sql.TxOptions{ReadOnly: true})
	if err != nil {
		return result, err
	}
	defer tx.Rollback()
	byID := map[string]*candidate{}
	add := func(id string, rank float64, exact bool) {
		c := byID[id]
		if c == nil {
			c = &candidate{id: id}
			byID[id] = c
		}
		c.rank += rank
		c.exact = c.exact || exact
	}
	phrase := lexicalTerms(q.Text)
	if pathQuery && phrase != "" {
		phrase = `"` + strings.ReplaceAll(q.Text, `"`, `""`) + `"`
	}
	lexical := func() error {
		var hits []candidate
		var err error
		if phrase != "" {
			hits, err = lexicalCandidates(ctx, tx, `SELECT m.id,(instr(lower(m.body),lower(?))>0 OR instr(lower(m.circumstances),lower(?))>0) exact FROM memory_fts JOIN memories m ON m.rowid=memory_fts.rowid WHERE memory_fts MATCH ?`+cond+` ORDER BY exact DESC,bm25(memory_fts,1.0,0.6),m.id LIMIT 120`, append([]any{q.Text, q.Text, phrase}, dates...)...)
		}
		if err != nil {
			s.scheduleCheck()
			result.Degraded = true
		}
		if phrase == "" || err != nil {
			hits, err = lexicalCandidates(ctx, tx, `SELECT m.id,1 FROM memories m WHERE (instr(lower(m.body),lower(?))>0 OR instr(lower(m.circumstances),lower(?))>0)`+cond+chrono+` LIMIT 120`, append([]any{q.Text, q.Text}, dates...)...)
		}
		if err != nil {
			return err
		}
		for i, h := range hits {
			add(h.id, 1/float64(45+i), h.exact)
		}
		return nil
	}
	if !semanticOnly {
		if err := lexical(); err != nil {
			return result, err
		}
	}
	if len(queryVec) > 0 {
		rows, err := tx.QueryContext(ctx, `SELECT m.id,v.vector FROM vectors v JOIN memories m ON m.id=v.memory_id WHERE v.model=?`+cond, append([]any{s.indexID()}, dates...)...)
		if err != nil {
			s.scheduleCheck()
			result.Degraded = true
		}
		if err == nil {
			var best []candidate
			for rows.Next() {
				var id string
				var b sql.RawBytes
				if err = rows.Scan(&id, &b); err != nil {
					rows.Close()
					return result, err
				}
				stride := 4 * len(queryVec)
				if len(b) == 0 {
					continue
				}
				if len(b)%stride != 0 || len(b) > stride*9 {
					s.scheduleCheck()
					result.Degraded = true
					continue
				}
				result.Mode = "hybrid"
				if semanticOnly {
					result.Mode = "semantic"
				}
				var dot float32
				for offset := 0; offset < len(b); offset += stride {
					var score float32
					for i, x := range queryVec {
						score += x * math.Float32frombits(binary.LittleEndian.Uint32(b[offset+4*i:]))
					}
					if score > dot {
						dot = score
					}
				}
				if dot < 0.32 {
					continue
				}
				at := sort.Search(len(best), func(i int) bool {
					return best[i].rank < float64(dot) || (best[i].rank == float64(dot) && best[i].id > id)
				})
				if at >= 120 {
					continue
				}
				best = append(best, candidate{})
				copy(best[at+1:], best[at:])
				best[at] = candidate{id: id, rank: float64(dot)}
				if len(best) > 120 {
					best = best[:120]
				}
			}
			err = rows.Err()
			rows.Close()
			if err != nil {
				s.scheduleCheck()
				result.Degraded = true
			}
			for i, c := range best {
				add(c.id, 0.9/float64(45+i), false)
			}
		}
	}
	if semanticOnly && result.Mode != "semantic" {
		if err := lexical(); err != nil {
			return result, err
		}
		result.Degraded = s.embed == nil || result.Degraded || len(byID) > 0
	}
	all := make([]*candidate, 0, len(byID))
	for _, c := range byID {
		all = append(all, c)
	}
	sort.Slice(all, func(i, j int) bool {
		a, b := all[i], all[j]
		if a.exact != b.exact {
			return a.exact
		}
		if a.rank != b.rank {
			return a.rank > b.rank
		}
		return a.id < b.id
	})
	seen := map[struct{ origin, occurred string }]bool{}
	for _, c := range all {
		e, err := readEntry(ctx, tx, c.id)
		if err != nil {
			return result, err
		}
		key := struct{ origin, occurred string }{e.Origin, e.Occurred}
		if e.Origin != "" && seen[key] {
			continue
		}
		seen[key] = true
		if len(result.Memories) >= limit {
			result.More = true
			break
		}
		hit := memory.Hit{Entry: preview(e)}
		if e.Origin != "" {
			rows, err := tx.QueryContext(ctx, "SELECT "+fields+" FROM memories m WHERE m.origin=? AND m.occurred=? AND m.id<>?"+cond+chrono+" LIMIT 5", append([]any{e.Origin, e.Occurred, e.ID}, dates...)...)
			if err != nil {
				return result, err
			}
			for rows.Next() {
				a, err := scanEntry(rows)
				if err != nil {
					rows.Close()
					return result, err
				}
				hit.Accounts = append(hit.Accounts, preview(a))
			}
			err = rows.Err()
			rows.Close()
			if err != nil {
				return result, err
			}
		}
		result.Memories = append(result.Memories, hit)
	}
	return result, nil
}

func lexicalCandidates(ctx context.Context, tx *sql.Tx, query string, args ...any) ([]candidate, error) {
	rows, err := tx.QueryContext(ctx, query, args...)
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	var out []candidate
	for rows.Next() {
		var c candidate
		if err = rows.Scan(&c.id, &c.exact); err != nil {
			return nil, err
		}
		out = append(out, c)
	}
	return out, rows.Err()
}

func isAbsolutePathQuery(query string) bool {
	return strings.HasPrefix(query, "/") || strings.HasPrefix(query, `\\`) ||
		(len(query) >= 3 && query[1] == ':' && (query[2] == '\\' || query[2] == '/') &&
			((query[0] >= 'A' && query[0] <= 'Z') || (query[0] >= 'a' && query[0] <= 'z')))
}
