package granite

import "recallport/internal/memory"

const (
	ID         = "granite-embedding-97m-multilingual-r2/835ad140/fp32/cls-v1"
	Dimensions = 384
	Threads    = 4
)

func (*Model) SearchPolicy() memory.SearchPolicy {
	return memory.SearchPolicy{SemanticThreshold: memory.GraniteThreshold, GraphThreshold: memory.GraniteGraphThreshold, MaxTokens: MaxTokens, CPUThreads: Threads, Encoding: "FP32 · CLS · L2"}
}
