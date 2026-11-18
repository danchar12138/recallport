//go:build cgo && windows && amd64

package granite

import (
	"math"
	"os"
	"slices"
	"testing"
)

func TestOfficialGraniteAssets(t *testing.T) {
	dir := os.Getenv("RECALLPORT_GRANITE_DIR")
	if dir == "" {
		t.Skip("official Granite assets not provided")
	}
	m, err := Open(dir)
	if err != nil {
		t.Fatal(err)
	}
	defer m.Close()
	for _, c := range []struct {
		text string
		ids  []int64
	}{
		{"开机自启", []int64{179934, 2986, 4615, 6799, 51702, 179938}},
		{`C:\work\RecallPort`, []int64{179934, 34, 15688, 1546, 59, 154757, 8087, 179938}},
		{"SQLite WAL 与备份", []int64{179934, 95837, 163593, 162784, 20055, 25544, 179938}},
		{"  leading  spaces \n  中文", []int64{179934, 180, 7980, 180, 18212, 749, 180, 78930, 179938}},
		{"版本 v1.23.4 and we're testing", []int64{179934, 38460, 283, 16, 13, 1807, 13, 19, 286, 15612, 11293, 179938}},
		{"Emoji \U0001F642 and caf\u00e9。", []int64{179934, 90828, 9388, 16054, 286, 29642, 744, 179938}},
	} {
		got, err := m.tokenizer.ids(c.text)
		if err != nil || !slices.Equal(got, c.ids) {
			t.Errorf("token IDs for %q: %v, want %v (%v)", c.text, got, c.ids, err)
		}
	}
	v, err := m.Encode("开机自启")
	if err != nil || len(v) != Dimensions {
		t.Fatalf("encode: len %d: %v", len(v), err)
	}
	var sum float64
	for _, x := range v {
		sum += float64(x) * float64(x)
	}
	if math.Abs(sum-1) > 1e-5 {
		t.Fatal(sum)
	}
}
