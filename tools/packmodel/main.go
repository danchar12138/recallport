package main

import (
	"bufio"
	"encoding/binary"
	"encoding/json"
	"fmt"
	"io"
	"math"
	"os"

	"recallport/internal/embedding"
)

func main() {
	if len(os.Args) == 4 && os.Args[1] == "-verified" {
		if err := verifyAndPack(os.Args[2], os.Args[3]); err != nil {
			fmt.Fprintln(os.Stderr, err)
			os.Exit(1)
		}
		return
	}
	if len(os.Args) != 4 {
		fmt.Fprintln(os.Stderr, "usage: packmodel tokenizer.json model.safetensors potion.rpm")
		os.Exit(2)
	}
	if err := pack(os.Args[1:]); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func pack(args []string) error {
	var t struct {
		Model struct {
			Type  string
			Vocab [][]json.RawMessage
		}
	}
	j, err := os.Open(args[0])
	if err != nil {
		return err
	}
	defer j.Close()
	if err = json.NewDecoder(j).Decode(&t); err != nil {
		return err
	}
	if t.Model.Type != "Unigram" {
		return fmt.Errorf("expected a Unigram tokenizer")
	}
	f, err := os.Open(args[1])
	if err != nil {
		return err
	}
	defer f.Close()
	var size uint64
	if err = binary.Read(f, binary.LittleEndian, &size); err != nil {
		return err
	}
	if size > 1<<20 {
		return fmt.Errorf("oversized safetensors header")
	}
	b := make([]byte, int(size))
	if _, err = io.ReadFull(f, b); err != nil {
		return err
	}
	var tensors map[string]json.RawMessage
	if err = json.Unmarshal(b, &tensors); err != nil {
		return err
	}
	var tensor struct {
		Dtype   string
		Shape   []int
		Offsets []int64 `json:"data_offsets"`
	}
	if err = json.Unmarshal(tensors["embeddings"], &tensor); err != nil {
		return fmt.Errorf("embedding tensor: %w", err)
	}
	if tensor.Dtype != "F32" || len(tensor.Shape) != 2 || len(tensor.Offsets) != 2 || tensor.Shape[0] != len(t.Model.Vocab) || tensor.Shape[1] != 256 {
		return fmt.Errorf("unexpected embedding tensor layout")
	}
	if _, err = f.Seek(8+int64(size)+tensor.Offsets[0], io.SeekStart); err != nil {
		return err
	}
	out, err := os.Create(args[2])
	if err != nil {
		return err
	}
	defer out.Close()
	w := bufio.NewWriterSize(out, 1<<20)
	put := func(v any) error { return binary.Write(w, binary.LittleEndian, v) }
	if _, err = w.WriteString(embedding.Magic); err != nil {
		return err
	}
	dim := tensor.Shape[1]
	if err = put(uint32(dim)); err != nil {
		return err
	}
	if err = put(uint32(tensor.Shape[0])); err != nil {
		return err
	}
	for _, piece := range t.Model.Vocab {
		if len(piece) != 2 {
			return fmt.Errorf("invalid token")
		}
		var token string
		var score float32
		if err = json.Unmarshal(piece[0], &token); err != nil {
			return err
		}
		if err = json.Unmarshal(piece[1], &score); err != nil {
			return err
		}
		if len(token) == 0 || len(token) > 65535 {
			return fmt.Errorf("invalid token length")
		}
		if err = put(uint16(len(token))); err != nil {
			return err
		}
		if err = put(score); err != nil {
			return err
		}
		if _, err = w.WriteString(token); err != nil {
			return err
		}
	}
	r := bufio.NewReaderSize(f, 1<<20)
	row := make([]byte, 4*dim)
	quant := make([]byte, 4+dim)
	for range tensor.Shape[0] {
		if _, err = io.ReadFull(r, row); err != nil {
			return err
		}
		var maxAbs float64
		at := func(i int) float64 { return float64(math.Float32frombits(binary.LittleEndian.Uint32(row[i*4:]))) }
		for i := range dim {
			maxAbs = max(maxAbs, math.Abs(at(i)))
		}
		scale := maxAbs / 127
		if scale == 0 {
			scale = 1
		}
		binary.LittleEndian.PutUint32(quant, math.Float32bits(float32(scale)))
		for i := range dim {
			quant[4+i] = byte(int8(math.Round(at(i) / scale)))
		}
		if _, err = w.Write(quant); err != nil {
			return err
		}
	}
	return w.Flush()
}
