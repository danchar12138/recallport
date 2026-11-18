package transport

import (
	"errors"
	"fmt"
	"io"
	"mime"
	"net/http"
	"net/url"
	"os"
	"strconv"
	"strings"
	"time"

	"recallport/internal/memory"
)

type linkView struct {
	memory.Reference
	Text, Date string
}
type monthView struct {
	Label, URL string
	Count, Max int
}
type yearView struct {
	Year, URL string
	Count     int
	Months    []monthView
}

func (h *web) overview(w http.ResponseWriter, r *http.Request) {
	p := page{View: "overview"}
	periods, err := h.store.Periods(r.Context())
	if err != nil {
		p.Error = errorText(err)
	}
	years := map[string]int{}
	for _, period := range periods {
		p.Total += period.Count
		if len(period.Date) < 4 {
			p.Unknown += period.Count
			continue
		}
		y := period.Date[:4]
		i, ok := years[y]
		if !ok {
			i = len(p.Years)
			years[y] = i
			months := make([]monthView, 13)
			for month := 1; month <= 12; month++ {
				key := fmt.Sprintf("%s-%02d", y, month)
				months[month-1] = monthView{Label: fmt.Sprintf("%02d", month), URL: "/?period=" + key, Max: 1}
			}
			months[12] = monthView{Label: "仅年份", URL: "/?period=" + y, Max: 1}
			p.Years = append(p.Years, yearView{Year: y, URL: "/?from=" + y + "-01-01&through=" + y + "-12-31", Months: months})
		}
		m := 12
		if len(period.Date) == 7 {
			n, _ := strconv.Atoi(period.Date[5:])
			if n >= 1 && n <= 12 {
				m = n - 1
			}
		}
		p.Years[i].Months[m].Count += period.Count
		p.Years[i].Count += period.Count
	}
	for i := range p.Years {
		maxCount := 1
		for _, m := range p.Years[i].Months {
			maxCount = max(maxCount, m.Count)
		}
		for j := range p.Years[i].Months {
			p.Years[i].Months[j].Max = maxCount
		}
	}
	h.render(w, p)
}

func formDraft(r *http.Request) memory.Draft {
	date := r.PostForm.Get("occurred")
	switch precision := r.PostForm.Get("precision"); precision {
	case "unknown":
		date = ""
	case "day", "month", "year":
		date = r.PostForm.Get("occurred_" + precision)
	}
	return memory.Draft{Text: r.PostForm.Get("text"), Context: r.PostForm.Get("context"), Occurred: date}
}

func (h *web) create(w http.ResponseWriter, r *http.Request) {
	d := formDraft(r)
	back := localBack(r.PostForm.Get("back"))
	e, err := h.store.Remember(r.Context(), memory.Actor{ID: "web", Label: "Web"}, d)
	if err != nil {
		h.render(w, page{View: "detail", Back: back, Error: errorText(err), Entry: memory.Entry{Text: d.Text, Context: d.Context, Occurred: d.Occurred}})
		return
	}
	http.Redirect(w, r, "/memory/"+e.ID+"?"+url.Values{"back": {back}, "saved": {"1"}}.Encode(), http.StatusSeeOther)
}

func without(all []memory.Hit, id string) []memory.Hit {
	var out []memory.Hit
	for _, x := range all {
		if x.ID != id {
			out = append(out, x)
		}
	}
	return out
}

func (h *web) linkViews(r *http.Request, refs []memory.Reference) []linkView {
	var out []linkView
	for _, ref := range refs {
		e, err := h.store.Read(r.Context(), ref.ID)
		if err != nil {
			continue
		}
		text := []rune(strings.Join(strings.Fields(e.Text), " "))
		if len(text) > 100 {
			text = append(text[:100], '…')
		}
		out = append(out, linkView{Reference: ref, Text: string(text), Date: e.Occurred})
	}
	return out
}

func (h *web) link(w http.ResponseWriter, r *http.Request) {
	id := r.PathValue("id")
	err := h.store.SetReference(r.Context(), id, memory.Reference{ID: r.PostForm.Get("target"), Reason: r.PostForm.Get("reason")}, r.PostForm.Get("action") == "remove")
	if err != nil {
		h.detail(w, r, errorText(err), false)
		return
	}
	http.Redirect(w, r, "/memory/"+id+"?"+url.Values{"back": {localBack(r.PostForm.Get("back"))}}.Encode()+"#relations", http.StatusSeeOther)
}

func (h *web) showData(w http.ResponseWriter, r *http.Request, msg, notice string) {
	f, name, err := h.store.OpenRecovery()
	if err == nil {
		f.Close()
	} else {
		name = ""
	}
	h.render(w, page{View: "data", Error: msg, Notice: notice, Recovery: name, Index: h.store.IndexStatus(r.Context())})
}

func (h *web) dataPage(w http.ResponseWriter, r *http.Request) {
	var notice string
	switch r.URL.Query().Get("done") {
	case "restore":
		notice = "记忆已恢复。原库快照可在下方下载。"
	case "index":
		notice = "索引重建已提交。"
	}
	h.showData(w, r, "", notice)
}

func (h *web) restore(w http.ResponseWriter, r *http.Request) {
	ctl := http.NewResponseController(w)
	_ = ctl.SetReadDeadline(time.Now().Add(15 * time.Minute))
	_ = ctl.SetWriteDeadline(time.Now().Add(15 * time.Minute))
	r.Body = http.MaxBytesReader(w, r.Body, memory.MaxBackupBytes+(64<<10))
	parts, err := r.MultipartReader()
	if err != nil {
		h.showData(w, r, errorText(memory.ErrBackup), "")
		return
	}
	confirmed := false
	for {
		part, err := parts.NextPart()
		if err != nil {
			break
		}
		if part.FormName() == "confirm" {
			b, _ := io.ReadAll(io.LimitReader(part, 8))
			confirmed = string(b) == "yes"
		}
		if part.FormName() == "backup" && part.FileName() != "" && confirmed {
			if err = h.store.Replace(r.Context(), part); err != nil {
				h.showData(w, r, errorText(err), "")
				return
			}
			http.Redirect(w, r, "/data?done=restore", http.StatusSeeOther)
			return
		}
	}
	h.showData(w, r, "请选择备份文件并确认替换。", "")
}

func (h *web) recovery(w http.ResponseWriter, r *http.Request) {
	f, name, err := h.store.OpenRecovery()
	if errors.Is(err, os.ErrNotExist) {
		http.NotFound(w, r)
		return
	}
	if err != nil {
		http.Error(w, errorText(err), 500)
		return
	}
	defer f.Close()
	w.Header().Set("Content-Disposition", mime.FormatMediaType("attachment", map[string]string{"filename": name}))
	w.Header().Set("Content-Type", "application/octet-stream")
	http.ServeContent(w, r, name, time.Time{}, f)
}

func (h *web) rollback(w http.ResponseWriter, r *http.Request) {
	f, name, err := h.store.OpenRecovery()
	if err != nil {
		h.showData(w, r, "没有可恢复的快照。", "")
		return
	}
	defer f.Close()
	if r.PostForm.Get("confirm") != "yes" || r.PostForm.Get("snapshot") != name {
		h.showData(w, r, "请确认要恢复的回退快照。", "")
		return
	}
	_ = http.NewResponseController(w).SetWriteDeadline(time.Now().Add(15 * time.Minute))
	if err = h.store.Replace(r.Context(), f); err != nil {
		h.showData(w, r, errorText(err), "")
		return
	}
	http.Redirect(w, r, "/data?done=restore", http.StatusSeeOther)
}

func (h *web) rebuild(w http.ResponseWriter, r *http.Request) {
	h.store.RequestRebuild()
	http.Redirect(w, r, "/data?done=index", http.StatusSeeOther)
}
