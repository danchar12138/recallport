//go:build cgo && windows && amd64

package granite

import (
	"fmt"
	"math"
	"path/filepath"
	"sync"

	ort "github.com/yalue/onnxruntime_go"
)

type Model struct {
	tokenizer *textEncoder
	session   *ort.DynamicAdvancedSession
	mu        sync.Mutex
}

func Open(folder string) (*Model, error) {
	tok, err := loadTokenizer(filepath.Join(folder, "tokenizer.json"))
	if err != nil {
		return nil, fmt.Errorf("Granite tokenizer: %w", err)
	}
	ort.SetSharedLibraryPath(filepath.Join(folder, "onnxruntime.dll"))
	if err = ort.InitializeEnvironment(); err != nil {
		return nil, fmt.Errorf("ONNX Runtime: %w", err)
	}
	closeOnError := true
	defer func() {
		if closeOnError {
			_ = ort.DestroyEnvironment()
		}
	}()
	o, err := ort.NewSessionOptions()
	if err != nil {
		return nil, err
	}
	defer o.Destroy()
	if err = o.SetIntraOpNumThreads(Threads); err != nil {
		return nil, err
	}
	if err = o.SetInterOpNumThreads(1); err != nil {
		return nil, err
	}
	session, err := ort.NewDynamicAdvancedSession(filepath.Join(folder, "model.onnx"), []string{"input_ids", "attention_mask"}, []string{"last_hidden_state"}, o)
	if err != nil {
		return nil, err
	}
	m := &Model{tokenizer: tok, session: session}
	if _, err = m.Encode("RecallPort"); err != nil {
		_ = session.Destroy()
		return nil, err
	}
	closeOnError = false
	return m, nil
}

func (m *Model) ID() string                  { return ID }
func (m *Model) Embed(text string) []float32 { values, _ := m.Encode(text); return values }

func (m *Model) Close() {
	m.mu.Lock()
	defer m.mu.Unlock()
	_ = m.session.Destroy()
	_ = ort.DestroyEnvironment()
}

func (m *Model) Encode(text string) ([]float32, error) {
	m.mu.Lock()
	defer m.mu.Unlock()
	ids, err := m.tokenizer.ids(text)
	if err != nil || len(ids) == 0 {
		return nil, err
	}
	mask := make([]int64, len(ids))
	for i := range mask {
		mask[i] = 1
	}
	shape := ort.NewShape(1, int64(len(ids)))
	x, err := ort.NewTensor(shape, ids)
	if err != nil {
		return nil, err
	}
	defer x.Destroy()
	a, err := ort.NewTensor(shape, mask)
	if err != nil {
		return nil, err
	}
	defer a.Destroy()
	output := []ort.Value{nil}
	if err = m.session.Run([]ort.Value{x, a}, output); err != nil {
		return nil, err
	}
	defer output[0].Destroy()
	y, ok := output[0].(*ort.Tensor[float32])
	if !ok {
		return nil, fmt.Errorf("unexpected Granite output tensor")
	}
	data := y.GetData()
	if len(data) != len(ids)*Dimensions {
		return nil, fmt.Errorf("unexpected Granite output dimensions")
	}
	v := append([]float32(nil), data[:Dimensions]...)
	var norm float64
	for _, n := range v {
		norm += float64(n) * float64(n)
	}
	if norm == 0 || math.IsNaN(norm) || math.IsInf(norm, 0) {
		return nil, fmt.Errorf("invalid Granite vector")
	}
	scale := float32(math.Sqrt(norm))
	for i := range v {
		v[i] /= scale
	}
	return v, nil
}
