package embedding

import (
	"encoding/binary"
	"fmt"
	"math"
	"strings"
	"unicode"
	"unicode/utf8"

	"golang.org/x/text/unicode/norm"
)

const Magic = "RPMODEL1"

type token struct {
	id    int
	score float64
}
type Model struct {
	vocabulary           map[string]token
	weights              string
	dimensions, maxBytes int
	unknown              float64
}

func Open() (*Model, error) {
	if bundled == "" {
		return nil, nil
	}
	return parse(bundled)
}

func parse(raw string) (*Model, error) {
	if len(raw) < 16 || raw[:8] != Magic {
		return nil, fmt.Errorf("invalid RecallPort model")
	}
	d := int(binary.LittleEndian.Uint32([]byte(raw[8:12])))
	n := int(binary.LittleEndian.Uint32([]byte(raw[12:16])))
	if d < 1 || d > 2048 || n < 2 || n > 1_000_000 {
		return nil, fmt.Errorf("invalid model dimensions")
	}
	m := &Model{vocabulary: make(map[string]token, n), dimensions: d, unknown: 0}
	pos := 16
	for i := 0; i < n; i++ {
		if len(raw)-pos < 6 {
			return nil, fmt.Errorf("truncated vocabulary")
		}
		width := int(binary.LittleEndian.Uint16([]byte(raw[pos : pos+2])))
		score := float64(math.Float32frombits(binary.LittleEndian.Uint32([]byte(raw[pos+2 : pos+6]))))
		pos += 6
		if width < 1 || len(raw)-pos < width {
			return nil, fmt.Errorf("invalid vocabulary entry")
		}
		if i > 1 {
			m.vocabulary[raw[pos:pos+width]] = token{i, score}
			m.maxBytes = max(m.maxBytes, width)
			m.unknown = min(m.unknown, score)
		}
		pos += width
	}
	m.unknown -= 10
	if len(raw)-pos != n*(4+d) {
		return nil, fmt.Errorf("invalid weight matrix")
	}
	m.weights = raw[pos:]
	return m, nil
}

func (m *Model) ID() string { return "potion-multilingual-128m/73908c34/int8/unigram-v1" }

func normalize(raw string) string {
	if len(raw) > 36_000 {
		raw = raw[:36_000]
		for !utf8.ValidString(raw) {
			raw = raw[:len(raw)-1]
		}
	}
	var b strings.Builder
	for _, r := range norm.NFKC.String(raw) {
		switch {
		case unicode.IsSpace(r):
			b.WriteByte(' ')
		case unicode.IsControl(r) || unicode.Is(unicode.Cf, r):
		case r >= 33 && r <= 126 && !unicode.IsLetter(r) && !unicode.IsDigit(r):
			b.WriteByte(' ')
			b.WriteRune(r)
			b.WriteByte(' ')
		default:
			b.WriteRune(r)
		}
	}
	joined := strings.Join(strings.Fields(b.String()), "\u2581")
	if joined == "" {
		return ""
	}
	return "\u2581" + joined
}

func (m *Model) Embed(text string) []float32 {
	return m.embedWith(text, m.addQuantized)
}

func (m *Model) embedWith(text string, add func([]float32, int)) []float32 {
	s := normalize(text)
	if s == "" {
		return nil
	}
	type step struct {
		previous, id int
		score        float64
	}
	path := make([]step, len(s)+1)
	for i := 1; i < len(path); i++ {
		path[i].score = math.Inf(-1)
	}
	for i := 0; i < len(s); {
		_, first := utf8.DecodeRuneInString(s[i:])
		advance := func(end, id int, score float64) {
			if score > path[end].score {
				path[end] = step{i, id, score}
			}
		}
		advance(i+first, 1, path[i].score+m.unknown)
		for j := i + first; j <= len(s) && j-i <= m.maxBytes; {
			if t, ok := m.vocabulary[s[i:j]]; ok {
				advance(j, t.id, path[i].score+t.score)
			}
			if j == len(s) {
				break
			}
			_, size := utf8.DecodeRuneInString(s[j:])
			j += size
		}
		i += first
	}
	v := make([]float32, m.dimensions)
	for i := len(s); i > 0; {
		p := path[i]
		i = p.previous
		if p.id < 2 {
			continue
		}
		add(v, p.id)
	}
	var ss float64
	for _, x := range v {
		ss += float64(x) * float64(x)
	}
	if ss == 0 {
		return nil
	}
	norm := float32(math.Sqrt(ss))
	for i := range v {
		v[i] /= norm
	}
	return v
}

func (m *Model) addQuantized(v []float32, id int) {
	o := id * (4 + m.dimensions)
	scale := math.Float32frombits(binary.LittleEndian.Uint32([]byte(m.weights[o : o+4])))
	for j := range v {
		v[j] += scale * float32(int8(m.weights[o+4+j]))
	}
}
