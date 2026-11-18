package transport

import (
	"embed"
	"encoding/json"
	"errors"
	"html/template"
	"log"
	"net"
	"net/http"
	"net/url"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"time"

	"recallport/internal/memory"
)

//go:embed *.html style.css controls.js graph.js vendor/*.css vendor/*.js
var assets embed.FS

type web struct {
	store memory.Repository
	base  string
	tmpl  *template.Template
}
type row struct {
	memory.Hit
	Period string
}
type page struct {
	View, Base, Error, Back  string
	Entry                    memory.Entry
	Rows                     []row
	Query                    memory.Query
	Prev, Next               string
	Semantic, Saved, More    bool
	Notice, Period           string
	Undated                  bool
	Recovery                 string
	Years                    []yearView
	Total, Unknown           int
	Incoming, Outgoing       []linkView
	Choices                  []memory.Hit
	LinkQuery                string
	Index                    memory.IndexStatus
	Degraded                 bool
	Chronological            bool
	ByDate, ByRank, GraphURL string
}

func New(store memory.Repository, host string) (http.Handler, error) {
	_, port, err := net.SplitHostPort(host)
	if err != nil {
		return nil, err
	}
	t, err := template.New("page.html").Funcs(template.FuncMap{"localTime": displayTime}).ParseFS(assets, "*.html")
	if err != nil {
		return nil, err
	}
	h := &web{store: store, base: "http://" + host, tmpl: t}
	m := http.NewServeMux()
	m.Handle("/mcp", newMCP(store))
	m.HandleFunc("GET /health", func(w http.ResponseWriter, _ *http.Request) { _, _ = w.Write([]byte("ok\n")) })
	for _, name := range []string{"style.css", "controls.js", "graph.js", "vendor/cytoscape.min.js", "vendor/bootstrap.min.css", "vendor/flatpickr.min.css", "vendor/flatpickr.min.js"} {
		m.HandleFunc("GET /"+name, func(w http.ResponseWriter, r *http.Request) { http.ServeFileFS(w, r, assets, name) })
	}
	m.HandleFunc("GET /{$}", h.timeline)
	m.HandleFunc("GET /overview", h.overview)
	m.HandleFunc("GET /graph", h.graphPage)
	m.HandleFunc("GET /graph-data", h.graphData)
	m.HandleFunc("GET /memory/new", func(w http.ResponseWriter, r *http.Request) {
		h.render(w, page{View: "detail", Back: localBack(r.URL.Query().Get("back"))})
	})
	m.HandleFunc("POST /memory/new", h.create)
	m.HandleFunc("GET /memory/{id}", func(w http.ResponseWriter, r *http.Request) { h.detail(w, r, "", false) })
	m.HandleFunc("POST /memory/{id}", h.edit)
	m.HandleFunc("POST /memory/{id}/delete", h.remove)
	m.HandleFunc("POST /memory/{id}/links", h.link)
	m.HandleFunc("GET /clients", func(w http.ResponseWriter, _ *http.Request) { h.render(w, page{View: "clients"}) })
	m.HandleFunc("GET /data", h.dataPage)
	m.HandleFunc("GET /index-status", func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("Content-Type", "application/json")
		_ = json.NewEncoder(w).Encode(h.store.IndexStatus(r.Context()))
	})
	m.HandleFunc("POST /backup", h.backup)
	m.HandleFunc("POST /restore", h.restore)
	m.HandleFunc("POST /reindex", h.rebuild)
	m.HandleFunc("GET /recovery", h.recovery)
	m.HandleFunc("POST /recovery", h.rollback)
	return http.HandlerFunc(func(w http.ResponseWriter, r *http.Request) {
		w.Header().Set("X-Content-Type-Options", "nosniff")
		w.Header().Set("Referrer-Policy", "same-origin")
		w.Header().Set("Cache-Control", "no-store")
		w.Header().Set("Content-Security-Policy", "default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src data:; form-action 'self'; frame-ancestors 'none'; base-uri 'none'")
		if r.Host != host && r.Host != net.JoinHostPort("localhost", port) {
			http.Error(w, "Invalid Host", http.StatusForbidden)
			return
		}
		if origin := r.Header.Get("Origin"); origin != "" && origin != "http://"+r.Host {
			http.Error(w, "Invalid Origin", http.StatusForbidden)
			return
		}
		if site := r.Header.Get("Sec-Fetch-Site"); site == "cross-site" || site == "same-site" {
			http.Error(w, "Cross-site request", http.StatusForbidden)
			return
		}
		if r.URL.Path != "/mcp" && r.URL.Path != "/restore" && r.Method == http.MethodPost {
			r.Body = http.MaxBytesReader(w, r.Body, 96<<10)
			if err := r.ParseForm(); err != nil {
				http.Error(w, "Invalid form", http.StatusBadRequest)
				return
			}
		}
		m.ServeHTTP(w, r)
	}), nil
}

func (h *web) render(w http.ResponseWriter, p page) {
	p.Base, p.Semantic = h.base, h.store.UsesVectors()
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	if err := h.tmpl.Execute(w, p); err != nil {
		log.Print("render: ", err)
	}
}

func (h *web) timeline(w http.ResponseWriter, r *http.Request) {
	v := r.URL.Query()
	q := memory.Query{Text: strings.TrimSpace(v.Get("q")), From: strings.TrimSpace(v.Get("from")), Through: strings.TrimSpace(v.Get("through")), Limit: 20, Period: v.Get("period"), Undated: v.Get("undated") == "1", Strategy: v.Get("strategy")}
	p := page{View: "timeline", Query: q, Back: r.URL.RequestURI(), Period: q.Period, Undated: q.Undated}
	n, _ := strconv.Atoi(v.Get("page"))
	n = max(1, min(10000, n))
	var result memory.Results
	var err error
	if q.Text != "" {
		result, err = h.store.Recall(r.Context(), q)
		p.More = result.More
		p.Degraded = result.Degraded
		p.Chronological = v.Get("order") == "date"
		p.ByRank = withOrder(r.URL.Query(), "")
		p.ByDate = withOrder(r.URL.Query(), "date")
		gf := r.URL.Query()
		gf.Del("order")
		gf.Del("page")
		p.GraphURL = "/graph?" + gf.Encode()
		if p.Chronological {
			sort.SliceStable(result.Memories, func(i, j int) bool {
				a, b := result.Memories[i].Occurred, result.Memories[j].Occurred
				if a == "" {
					return false
				}
				if b == "" {
					return true
				}
				return a < b
			})
		}
	} else {
		result, err = h.store.Timeline(r.Context(), q, (n-1)*q.Limit)
		link := func(n int) string {
			values := url.Values{"from": {q.From}, "through": {q.Through}, "page": {strconv.Itoa(n)}, "period": {q.Period}}
			if q.Undated {
				values.Set("undated", "1")
			}
			return "/?" + values.Encode()
		}
		if n > 1 {
			p.Prev = link(n - 1)
		}
		if result.More {
			p.Next = link(n + 1)
		}
	}
	if err != nil {
		p.Error = errorText(err)
	}
	previous := ""
	for _, hit := range result.Memories {
		r := row{Hit: hit}
		if q.Text == "" || p.Chronological {
			period := "日期未记"
			if len(hit.Occurred) >= 7 {
				period = hit.Occurred[:7]
			} else if hit.Occurred != "" {
				period = hit.Occurred
			}
			if previous != period {
				r.Period = period
				previous = period
			}
		}
		p.Rows = append(p.Rows, r)
	}
	h.render(w, p)
}

func withOrder(v url.Values, order string) string {
	v.Del("page")
	if order == "" {
		v.Del("order")
	} else {
		v.Set("order", order)
	}
	return "/?" + v.Encode()
}

func localBack(raw string) string {
	u, err := url.ParseRequestURI(raw)
	if err != nil || u.Host != "" || (u.Path != "/" && u.Path != "/graph") || !strings.HasPrefix(raw, "/") || strings.HasPrefix(raw, "//") {
		return "/"
	}
	return u.RequestURI()
}

func displayTime(value string) string {
	t, err := time.Parse(time.RFC3339Nano, value)
	if err != nil {
		return value
	}
	return t.Local().Format("2006-01-02 15:04")
}

func (h *web) detail(w http.ResponseWriter, r *http.Request, msg string, keepDraft bool) {
	e, err := h.store.Read(r.Context(), r.PathValue("id"))
	if err != nil {
		status := http.StatusInternalServerError
		if errors.Is(err, memory.ErrNotFound) {
			status = http.StatusNotFound
		}
		http.Error(w, errorText(err), status)
		return
	}
	if keepDraft {
		d := formDraft(r)
		e.Text, e.Context, e.Occurred = d.Text, d.Context, d.Occurred
	}
	p := page{View: "detail", Entry: e, Error: msg, Saved: r.URL.Query().Get("saved") == "1", Back: localBack(r.FormValue("back")), LinkQuery: strings.TrimSpace(r.URL.Query().Get("find"))}
	p.Incoming = h.linkViews(r, e.CitedBy)
	p.Outgoing = h.linkViews(r, e.References)
	if p.LinkQuery != "" {
		if res, err := h.store.Recall(r.Context(), memory.Query{Text: p.LinkQuery, Limit: 12}); err == nil {
			p.Choices = without(res.Memories, e.ID)
		} else if p.Error == "" {
			p.Error = errorText(err)
		}
	} else if res, err := h.store.Timeline(r.Context(), memory.Query{Limit: 12}, 0); err == nil {
		p.Choices = without(res.Memories, e.ID)
	}
	h.render(w, p)
}

func (h *web) edit(w http.ResponseWriter, r *http.Request) {
	f := r.PostForm
	rev, _ := strconv.Atoi(f.Get("revision"))
	err := h.store.Edit(r.Context(), r.PathValue("id"), rev, formDraft(r))
	if err != nil {
		h.detail(w, r, errorText(err), errors.Is(err, memory.ErrInvalid))
		return
	}
	q := url.Values{"back": {localBack(f.Get("back"))}, "saved": {"1"}}
	http.Redirect(w, r, "/memory/"+r.PathValue("id")+"?"+q.Encode(), http.StatusSeeOther)
}

func (h *web) remove(w http.ResponseWriter, r *http.Request) {
	rev, _ := strconv.Atoi(r.PostForm.Get("revision"))
	if err := h.store.Delete(r.Context(), r.PathValue("id"), rev); err != nil {
		h.detail(w, r, errorText(err), false)
		return
	}
	http.Redirect(w, r, localBack(r.PostForm.Get("back")), http.StatusSeeOther)
}

func (h *web) backup(w http.ResponseWriter, r *http.Request) {
	dir, err := os.MkdirTemp("", "recallport-backup-")
	if err != nil {
		http.Error(w, errorText(err), 500)
		return
	}
	path := filepath.Join(dir, "memory.db")
	defer os.Remove(dir)
	defer os.Remove(path)
	if err := h.store.Snapshot(r.Context(), path); err != nil {
		http.Error(w, errorText(err), 500)
		return
	}
	f, err := os.Open(path)
	if err != nil {
		http.Error(w, errorText(err), 500)
		return
	}
	defer f.Close()
	w.Header().Set("Content-Disposition", "attachment; filename=recallport-"+time.Now().Format("2006-01-02-150405")+".db")
	w.Header().Set("Content-Type", "application/octet-stream")
	http.ServeContent(w, r, "memory.db", time.Time{}, f)
}

func errorText(err error) string {
	switch {
	case errors.Is(err, memory.ErrNotFound):
		return "没有找到这条记忆。"
	case errors.Is(err, memory.ErrConflict):
		return "这条记忆已变化，请检查当前内容后重试。"
	case errors.Is(err, memory.ErrInvalid):
		return "输入无效，请检查正文长度和日期范围。"
	case errors.Is(err, memory.ErrBackup):
		return "无法读取此备份。请选择 RecallPort 导出的完整 SQLite 快照（最大 1 GiB）。"
	default:
		log.Print("web memory operation: ", err)
		return "操作未完成。"
	}
}
