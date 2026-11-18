package main

import (
	"crypto/sha256"
	"encoding/hex"
	"fmt"
	"io"
	"os"
	"path/filepath"
)

func verifyAndPack(cache, destination string) error {
	if err := os.MkdirAll(cache, 0700); err != nil {
		return err
	}
	tokenizer, err := getModelFile(cache, "tokenizer.json", "19f1909063da3cfe3bd83a782381f040dccea475f4816de11116444a73e1b6a1", 18616131)
	if err != nil {
		return err
	}
	weights, err := getModelFile(cache, "model.safetensors", "14b5eb39cb4ce5666da8ad1f3dc6be4346e9b2d601c073302fa0a31bf7943397", 512361560)
	if err != nil {
		return err
	}
	f, err := os.CreateTemp(filepath.Dir(destination), ".pack-*")
	if err != nil {
		return err
	}
	name := f.Name()
	defer os.Remove(name)
	if err := f.Close(); err != nil {
		return err
	}
	if err := pack([]string{tokenizer, weights, name}); err != nil {
		return err
	}
	return os.Rename(name, destination)
}

func getModelFile(cache, name, hash string, size int64) (string, error) {
	path := filepath.Join(cache, name)
	f, err := os.Open(path)
	if err != nil {
		return "", fmt.Errorf("model asset %s: %w", name, err)
	}
	defer f.Close()
	h := sha256.New()
	n, err := io.Copy(h, io.LimitReader(f, size+1))
	if err != nil {
		return "", err
	}
	if n != size || hex.EncodeToString(h.Sum(nil)) != hash {
		return "", fmt.Errorf("model checksum mismatch: %s", name)
	}
	return path, nil
}
