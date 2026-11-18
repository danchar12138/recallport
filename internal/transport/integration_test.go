package transport

import (
	"encoding/json"
	"io"
	"net"
	"net/http"
	"net/http/httptest"
	"net/url"
	"path/filepath"
	"strings"
	"testing"

	"github.com/modelcontextprotocol/go-sdk/mcp"
	"recallport/internal/memory"
	"recallport/internal/sqlite"
)

func TestKeylessClientsAndWeb(t *testing.T) {
	ctx := t.Context()
	s, err := sqlite.Open(filepath.Join(t.TempDir(), "memory.db"), nil)
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	srv := httptest.NewUnstartedServer(nil)
	addr := srv.Listener.Addr().(*net.TCPAddr)
	srv.Config.Handler, err = New(memory.New(s), addr.String())
	if err != nil {
		t.Fatal(err)
	}
	srv.Start()
	defer srv.Close()
	connect := func(name string) *mcp.ClientSession {
		c := mcp.NewClient(&mcp.Implementation{Name: name, Version: "1"}, nil)
		session, err := c.Connect(ctx, &mcp.StreamableClientTransport{Endpoint: srv.URL + "/mcp", DisableStandaloneSSE: true, MaxRetries: -1}, nil)
		if err != nil {
			t.Fatal(err)
		}
		t.Cleanup(func() { session.Close() })
		return session
	}
	writer := connect("Client A")
	reader := connect("Client B")
	call := func(c *mcp.ClientSession, name string, args any) *mcp.CallToolResult {
		r, err := c.CallTool(ctx, &mcp.CallToolParams{Name: name, Arguments: args})
		if err != nil {
			t.Fatal(err)
		}
		return r
	}
	type pair map[string]string
	tools, err := writer.ListTools(ctx, nil)
	if err != nil || len(tools.Tools) != 3 {
		t.Fatalf("tools: %+v %v", tools, err)
	}
	r := call(writer, "remember", pair{"text": "旧电脑采用本地 SQLite 存储。", "occurred": "2025-03", "source": "Reported client A"})
	if r.IsError {
		t.Fatalf("remember: %+v", r.Content)
	}
	var entry memory.Entry
	b, _ := json.Marshal(r.StructuredContent)
	if err = json.Unmarshal(b, &entry); err != nil || entry.Source != "Reported client A" {
		t.Fatalf("reported provenance: %s %v", b, err)
	}
	r = call(reader, "recall", pair{"query": "SQLite"})
	b, _ = json.Marshal(r.StructuredContent)
	if r.IsError || !strings.Contains(string(b), entry.ID) {
		t.Fatalf("shared recall: %s", b)
	}
	if r = call(reader, "remember", pair{"text": "another local memory"}); r.IsError {
		t.Fatal("second client could not write")
	}
	if r = call(reader, "read", pair{"id": entry.ID}); r.IsError {
		t.Fatal("reader could not read")
	}
	browser := srv.Client()
	get := func(path string) string {
		resp, err := browser.Get(srv.URL + path)
		if err != nil {
			t.Fatal(err)
		}
		defer resp.Body.Close()
		b, err := io.ReadAll(resp.Body)
		if err != nil {
			t.Fatal(err)
		}
		return string(b)
	}
	page := get("/")
	if !strings.Contains(page, "2025-03") || !strings.Contains(page, "SQLite") {
		t.Fatal("timeline missing entry")
	}
	resp, err := browser.PostForm(srv.URL+"/backup", url.Values{})
	if err != nil {
		t.Fatal(err)
	}
	header := make([]byte, 16)
	_, err = io.ReadFull(resp.Body, header)
	resp.Body.Close()
	if err != nil || string(header) != "SQLite format 3\x00" {
		t.Fatalf("invalid backup response: %q %v", header, err)
	}
	for _, tc := range []struct{ path, host, origin, fetch string }{
		{"/", "outside.invalid", "", ""},
		{"/backup", "", "null", ""},
		{"/mcp", "", srv.URL + "1", ""},
		{"/memory/" + entry.ID + "/delete", "", "", "cross-site"},
	} {
		req, err := http.NewRequestWithContext(ctx, http.MethodPost, srv.URL+tc.path, nil)
		if err != nil {
			t.Fatal(err)
		}
		if tc.host != "" {
			req.Host = tc.host
		}
		if tc.origin != "" {
			req.Header.Set("Origin", tc.origin)
		}
		if tc.fetch != "" {
			req.Header.Set("Sec-Fetch-Site", tc.fetch)
		}
		resp, err := browser.Do(req)
		if err != nil {
			t.Fatal(err)
		}
		resp.Body.Close()
		if resp.StatusCode != http.StatusForbidden {
			t.Fatalf("unexpected status %d for %+v", resp.StatusCode, tc)
		}
	}
}
