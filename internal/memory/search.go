package memory

const (
	DefaultRecallLimit    = 8
	MaxRecallLimit        = 30
	MaxQueryRunes         = 600
	MaxPassages           = 9
	MinPassageRunes       = 7
	CandidateLimit        = 120
	LexicalTermLimit      = 36
	FusionOffset          = 45
	LexicalWeight         = 1.0
	SemanticWeight        = 0.9
	BodyWeight            = 1.0
	ContextWeight         = 0.6
	GraniteThreshold      = 0.82
	GraniteGraphThreshold = 0.86
	LegacyThreshold       = 0.32
	MaintenanceSeconds    = 60
	RebuildBatchSize      = 64
)

type SearchPolicy struct {
	SemanticThreshold float32
	GraphThreshold    float32
	MaxTokens         int
	CPUThreads        int
	Encoding          string
}

type PolicyEmbedder interface{ SearchPolicy() SearchPolicy }

func Policy(e Embedder) SearchPolicy {
	if p, ok := e.(PolicyEmbedder); ok {
		return p.SearchPolicy()
	}
	return SearchPolicy{SemanticThreshold: LegacyThreshold, GraphThreshold: LegacyThreshold}
}

type CheckedEmbedder interface {
	Encode(string) ([]float32, error)
}

func Encode(e Embedder, text string) ([]float32, error) {
	if m, ok := e.(CheckedEmbedder); ok {
		return m.Encode(text)
	}
	return e.Embed(text), nil
}
