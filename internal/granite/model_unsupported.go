//go:build !cgo || !windows || !amd64

package granite

import "errors"

type Model struct{}

func Open(string) (*Model, error) {
	return nil, errors.New("Granite requires the Windows amd64 portable build")
}
func (*Model) ID() string             { return ID }
func (*Model) Embed(string) []float32 { return nil }
func (*Model) Close()                 {}
