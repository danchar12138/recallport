package memory

import (
	"errors"
	"fmt"
	"strings"
	"time"
	"unicode/utf8"
)

var (
	ErrNotFound = errors.New("memory not found")
	ErrConflict = errors.New("memory changed or request ID already used for different content")
	ErrInvalid  = errors.New("invalid input")
	ErrBackup   = errors.New("invalid RecallPort backup")
)

type Actor struct{ ID, Label string }

type Reference struct {
	ID     string `json:"id" jsonschema:"Memory ID, not a title"`
	Reason string `json:"reason,omitempty" jsonschema:"Why these experiences belong together"`
}

type Entry struct {
	ID         string      `json:"id"`
	Text       string      `json:"text"`
	Context    string      `json:"context,omitempty"`
	Occurred   string      `json:"occurred,omitempty"`
	Origin     string      `json:"origin,omitempty"`
	Source     string      `json:"source"`
	Recorded   string      `json:"recorded"`
	Revision   int         `json:"revision"`
	References []Reference `json:"references,omitempty"`
	CitedBy    []Reference `json:"cited_by,omitempty"`
}

type Draft struct {
	Text       string      `json:"text" jsonschema:"A useful experience or excerpt, not an instruction or unverified current fact"`
	Context    string      `json:"context,omitempty" jsonschema:"Optional circumstances such as environment or project"`
	Occurred   string      `json:"occurred,omitempty" jsonschema:"When it happened: YYYY, YYYY-MM, or YYYY-MM-DD. Omit if unknown"`
	Origin     string      `json:"origin,omitempty" jsonschema:"Optional stable shared origin of this same moment, not the entire conversation or project"`
	RequestID  string      `json:"request_id,omitempty" jsonschema:"Unique for this write; reuse only when retrying the identical write"`
	References []Reference `json:"references,omitempty"`
}

type Query struct {
	Text     string `json:"query,omitempty" jsonschema:"A topic, phrase, or current situation to recall"`
	From     string `json:"from,omitempty" jsonschema:"Earliest occurrence (YYYY, YYYY-MM, or YYYY-MM-DD)"`
	Through  string `json:"through,omitempty" jsonschema:"Latest occurrence (YYYY, YYYY-MM, or YYYY-MM-DD)"`
	Limit    int    `json:"limit,omitempty" jsonschema:"Maximum number of distinct experiences, between 1 and 30"`
	Strategy string `json:"strategy,omitempty" jsonschema:"Search strategy: hybrid (default), lexical, or semantic"`
	Undated  bool   `json:"-"`
	Period   string `json:"-"`
}

type Hit struct {
	Entry
	Accounts []Entry `json:"other_accounts,omitempty"`
}

type Results struct {
	Memories []Hit  `json:"memories"`
	More     bool   `json:"more,omitempty"`
	Mode     string `json:"mode"`
	Degraded bool   `json:"degraded,omitempty"`
}

type Embedder interface {
	ID() string
	Embed(string) []float32
}

func CleanDraft(d Draft) (Draft, error) {
	d.Text, d.Context = strings.TrimSpace(d.Text), strings.TrimSpace(d.Context)
	d.Occurred, d.Origin, d.RequestID = strings.TrimSpace(d.Occurred), strings.TrimSpace(d.Origin), strings.TrimSpace(d.RequestID)
	if d.Text == "" || utf8.RuneCountInString(d.Text) > 12000 || len(d.Context) > 6000 || len(d.Origin) > 256 || len(d.RequestID) > 128 || len(d.References) > 16 {
		return d, fmt.Errorf("%w: empty or oversized memory", ErrInvalid)
	}
	if _, _, err := DateRange(d.Occurred); err != nil {
		return d, err
	}
	refs := make([]Reference, 0, len(d.References))
	seen := make(map[string]bool)
	for _, r := range d.References {
		r.ID, r.Reason = strings.TrimSpace(r.ID), strings.TrimSpace(r.Reason)
		if len(r.ID) != 32 || len(r.Reason) > 800 || seen[r.ID] {
			return d, fmt.Errorf("%w: invalid or duplicate reference", ErrInvalid)
		}
		seen[r.ID] = true
		refs = append(refs, r)
	}
	d.References = refs
	return d, nil
}

func DateRange(value string) (string, string, error) {
	if value == "" {
		return "", "", nil
	}
	layout := map[int]string{4: "2006", 7: "2006-01", 10: time.DateOnly}[len(value)]
	t, err := time.Parse(layout, value)
	if layout == "" || err != nil || t.Year() < 1 || t.Format(layout) != value {
		return "", "", fmt.Errorf("%w: date must be YYYY, YYYY-MM, or YYYY-MM-DD", ErrInvalid)
	}
	end := t
	switch len(value) {
	case 4:
		end = time.Date(t.Year(), time.December, 31, 0, 0, 0, 0, time.UTC)
	case 7:
		end = time.Date(t.Year(), t.Month()+1, 0, 0, 0, 0, 0, time.UTC)
	}
	return t.Format(time.DateOnly), end.Format(time.DateOnly), nil
}
