package memory

type GraphNode struct {
	ID       string `json:"id"`
	Text     string `json:"text"`
	Occurred string `json:"occurred,omitempty"`
	Source   string `json:"source"`
}

type GraphEdge struct {
	From      string `json:"from"`
	To        string `json:"to"`
	Reason    string `json:"reason,omitempty"`
	Suggested bool   `json:"suggested,omitempty"`
}

type Graph struct {
	Nodes []GraphNode `json:"nodes"`
	Edges []GraphEdge `json:"edges"`
	More  bool        `json:"more,omitempty"`
}
