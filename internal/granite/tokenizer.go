package granite

import (
	"encoding/json"
	"fmt"
	"os"
	"strings"
	"time"

	"github.com/dlclark/regexp2"
	"github.com/sugarme/tokenizer/model/bpe"
)

const MaxTokens = 512

type textEncoder struct {
	split      *regexp2.Regexp
	model      *bpe.BPE
	vocab      map[string]int
	start, end int64
}

func loadTokenizer(path string) (*textEncoder, error) {
	f, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	var spec struct {
		Model struct {
			Type         string
			Vocab        map[string]int
			Merges       [][2]string
			IgnoreMerges bool `json:"ignore_merges"`
		}
		PreTokenizer struct {
			Type  string
			Parts []struct {
				Type     string
				Pattern  map[string]string
				Behavior string
				Invert   bool
				UseRegex bool `json:"use_regex"`
				Prefix   bool `json:"add_prefix_space"`
			} `json:"pretokenizers"`
		} `json:"pre_tokenizer"`
		PostProcessor struct {
			Type   string
			Tokens map[string]struct{ IDs []int64 } `json:"special_tokens"`
		} `json:"post_processor"`
	}
	if err := json.NewDecoder(f).Decode(&spec); err != nil {
		return nil, err
	}
	pre := spec.PreTokenizer
	if spec.Model.Type != "BPE" || !spec.Model.IgnoreMerges || pre.Type != "Sequence" || len(pre.Parts) != 2 || pre.Parts[0].Type != "Split" || pre.Parts[0].Behavior != "Isolated" || pre.Parts[0].Invert || pre.Parts[1].Type != "ByteLevel" || pre.Parts[1].Prefix || pre.Parts[1].UseRegex || spec.PostProcessor.Type != "TemplateProcessing" {
		return nil, fmt.Errorf("unexpected Granite tokenizer format")
	}
	start, end := spec.PostProcessor.Tokens["<|startoftext|>"].IDs, spec.PostProcessor.Tokens["<|return|>"].IDs
	if len(start) != 1 || len(end) != 1 || pre.Parts[0].Pattern["Regex"] == "" {
		return nil, fmt.Errorf("missing Granite tokenizer rules")
	}
	pattern, err := regexp2.Compile(pre.Parts[0].Pattern["Regex"], 0)
	if err != nil {
		return nil, err
	}
	pattern.MatchTimeout = time.Second
	merges := make([]string, len(spec.Model.Merges))
	for i, pair := range spec.Model.Merges {
		merges[i] = pair[0] + " " + pair[1]
	}
	model, err := bpe.New(spec.Model.Vocab, merges, nil, nil, nil, nil)
	if err != nil {
		return nil, err
	}
	return &textEncoder{pattern, model, spec.Model.Vocab, start[0], end[0]}, nil
}

var byteAlphabet = func() [256]rune {
	var v [256]rune
	n := rune(256)
	for i := range v {
		switch {
		case i >= 33 && i <= 126, i >= 161 && i <= 172, i >= 174 && i <= 255:
			v[i] = rune(i)
		default:
			v[i] = n
			n++
		}
	}
	return v
}()

func (t *textEncoder) ids(text string) ([]int64, error) {
	if strings.TrimSpace(text) == "" {
		return nil, nil
	}
	buf := make([]int64, 1, 64)
	buf[0] = t.start
	appendPiece := func(piece []rune) error {
		if len(piece) == 0 || len(buf) >= MaxTokens-1 {
			return nil
		}
		var bytes strings.Builder
		for _, c := range []byte(string(piece)) {
			bytes.WriteRune(byteAlphabet[c])
		}
		mapped := bytes.String()
		// Granite's BPE uses the vocabulary directly when a complete piece exists.
		if id, ok := t.vocab[mapped]; ok {
			buf = append(buf, int64(id))
			return nil
		}
		tokens, err := t.model.Tokenize(mapped)
		if err != nil {
			return err
		}
		for _, token := range tokens {
			if len(buf) >= MaxTokens-1 {
				break
			}
			buf = append(buf, int64(token.Id))
		}
		return nil
	}
	input := []rune(text)
	offset := 0
	m, err := t.split.FindRunesMatch(input)
	for m != nil && err == nil && len(buf) < MaxTokens-1 {
		if err := appendPiece(input[offset:m.Index]); err != nil {
			return nil, err
		}
		offset = m.Index + m.Length
		if err := appendPiece(input[m.Index:offset]); err != nil {
			return nil, err
		}
		m, err = t.split.FindNextMatch(m)
	}
	if err != nil {
		return nil, err
	}
	if err := appendPiece(input[offset:]); err != nil {
		return nil, err
	}
	return append(buf, t.end), nil
}
