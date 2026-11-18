//go:build model

package embedding

import (
	"encoding/binary"
	"encoding/json"
	"math"
	"os"
	"path/filepath"
	"testing"

	"recallport/internal/memory"
	"recallport/internal/sqlite"
)

type floatModel struct {
	*Model
	file   *os.File
	offset int64
	rows   map[int][]float32
	t      *testing.T
}

func (m *floatModel) ID() string { return m.Model.ID() + "/float32-reference" }
func (m *floatModel) Embed(text string) []float32 {
	return m.embedWith(text, func(out []float32, id int) {
		row, ok := m.rows[id]
		if !ok {
			buf := make([]byte, 4*m.dimensions)
			if _, err := m.file.ReadAt(buf, m.offset+int64(id*len(buf))); err != nil {
				m.t.Fatal(err)
			}
			row = make([]float32, m.dimensions)
			for i := range row {
				row[i] = math.Float32frombits(binary.LittleEndian.Uint32(buf[4*i:]))
			}
			m.rows[id] = row
		}
		for i, x := range row {
			out[i] += x
		}
	})
}

func TestQuantizationComparison(t *testing.T) {
	path := os.Getenv("RECALLPORT_FLOAT_WEIGHTS")
	if path == "" {
		t.Skip("set RECALLPORT_FLOAT_WEIGHTS to the original pinned model.safetensors")
	}
	m, err := Open()
	if err != nil || m == nil {
		t.Fatalf("model: %v", err)
	}
	f, err := os.Open(path)
	if err != nil {
		t.Fatal(err)
	}
	defer f.Close()
	var headerSize uint64
	if err := binary.Read(f, binary.LittleEndian, &headerSize); err != nil || headerSize > 1<<20 {
		t.Fatalf("bad model header: %v", err)
	}
	var tensor struct {
		Dtype  string
		Shape  []int
		Offset []int64 `json:"data_offsets"`
	}
	// The metadata entry is not a tensor; decode only the embeddings member.
	var raw map[string]json.RawMessage
	buf := make([]byte, headerSize)
	if _, err := f.ReadAt(buf, 8); err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal(buf, &raw); err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal(raw["embeddings"], &tensor); err != nil {
		t.Fatal(err)
	}
	if tensor.Dtype != "F32" || len(tensor.Shape) != 2 || tensor.Shape[1] != m.dimensions || tensor.Shape[0] != len(m.weights)/(4+m.dimensions) || len(tensor.Offset) != 2 {
		t.Fatal("unexpected weight shape")
	}
	full := &floatModel{m, f, 8 + int64(headerSize) + tensor.Offset[0], map[int][]float32{}, t}
	type sample struct{ text, query string }
	corpus := []sample{
		{"用 Windows 任务计划程序，让应用在登录后在后台启动。", "开机自启"},
		{"数据库迁移前从 Web 下载完整快照，恢复后再核对索引。", "升级前给 SQLite 留个可还原副本"},
		{"旧笔记本的工作目录是 D:\\work，换电脑以后不再沿用。", "之前那台机器代码放哪"},
		{"这次选 Go 是因为要生成一个免安装的 Windows 可执行文件。", "why we chose Go for a portable exe"},
		{"生产库有多年记录，先检查备份和回滚方法，不要直接改。", "动正式数据之前先确认能退回"},
		{"开始时只监听本机回环地址，不把服务暴露到局域网。", "远程设备为什么连不上 127.0.0.1"},
		{"中文两个字的查询走字面扫描，三字索引覆盖不了。", "为什么短中文词不走全文索引"},
		{"不在数据库存原始聊天，只摘录值得记住的一小段。", "不要把整个对话记录下来"},
		{"项目用 semantic search 找同义改写，版本号和路径走精确匹配。", "不同说法用什么检索"},
		{"以前在项目甲上用依赖注入，项目乙只有一个入口，没必要照搬。", "项目乙适不适合照抄架构"},
		{"恢复快照会覆盖当前数据，不会和现有数据合并。", "restore replaces instead of merging"},
		{"刷新索引会补算缺的向量，正文和引用都不动。", "重建搜索是否更改原始记忆"},
		{"图中的虚线只是模型认为相关，不是某人明确建立的引用。", "语义关系不能当成事实"},
		{"网页里生成的快照包含 WAL 里的数据，不要直接复制打开的数据库。", "can I copy memory.db while it is running"},
		{"单元测试能过，但在另一台干净电脑上启动仍然失败。", "本机验证通过为什么还不能发版"},
		{"构建缓存放临时目录，运行时不需要 Node 或另装 Go。", "发布出去需要安装运行环境吗"},
		{"代码改动容易撤回时，先做第一步，避免无谓追问。", "什么时候可以直接动手不必问我"},
		{"真正的发生日期只知道到月份，不要编一个具体日子。", "怎么记录不确定的时间"},
		{"升级进程后检查健康端点，然后在浏览器核对页面。", "服务更新后如何确认已启动"},
		{"从现有时间线按年份跳转，没记录日期的单独展示。", "怎么翻到某一年留下的事情"},
		{"Windows 长路径问题是临时工具链目录太深导致。", "路径过长导致构建失败"},
		{"多个客户端共享一个服务，但彼此不需要在同一会话在线。", "cross-session memory for different agents"},
		{"选择浅色主题是为了在办公室长时间看代码。", "界面偏好为何不是深色"},
		{"不要把上次测试环境的目录当成当前机器的真实路径。", "旧路径今天还能直接用吗"},
		{"把独立打包器留在 tools，避免把模型加工逻辑编进服务。", "为什么模型转换是构建步骤"},
		{"专注写作时把通知关掉，结束后恢复。", "关闭提醒以减少打扰"},
	}
	ctx := t.Context()
	var totals [2][2]int
	var hybridTop5 [2]int
	var unrelatedHits [2]int
	rankings := make([][][]int, 2)
	for version, model := range []memory.Embedder{full, m} {
		s, err := sqlite.Open(filepath.Join(t.TempDir(), "comparison.db"), model)
		if err != nil {
			t.Fatal(err)
		}
		defer s.Close()
		ids := map[string]int{}
		for k, x := range corpus {
			e, err := s.Remember(ctx, memory.Actor{ID: "benchmark", Label: "benchmark"}, memory.Draft{Text: x.text})
			if err != nil {
				t.Fatal(err)
			}
			ids[e.ID] = k
		}
		for k, x := range corpus {
			result, err := s.Recall(ctx, memory.Query{Text: x.query, Strategy: "semantic", Limit: 5})
			if err != nil {
				t.Fatal(err)
			}
			list := make([]int, 0, len(result.Memories))
			for pos, h := range result.Memories {
				id := ids[h.ID]
				list = append(list, id)
				if id == k {
					totals[version][1]++
					if pos == 0 {
						totals[version][0]++
					}
				}
			}
			rankings[version] = append(rankings[version], list)
			hybrid, err := s.Recall(ctx, memory.Query{Text: x.query, Strategy: "hybrid", Limit: 5})
			if err != nil {
				t.Fatal(err)
			}
			found := false
			for _, hit := range hybrid.Memories {
				if ids[hit.ID] == k {
					hybridTop5[version]++
					found = true
					break
				}
			}
			if !found && version == 1 {
				t.Logf("hybrid miss: %s, %d returned", x.query, len(hybrid.Memories))
			}
		}
		for _, query := range []string{"珊瑚礁的热带鱼", "烤面包用多少酵母", "周末的古典音乐会", "月球的引力是多少", "where to grow basil", "水彩颜料怎么调色"} {
			r, err := s.Recall(ctx, memory.Query{Text: query, Strategy: "hybrid", Limit: 5})
			if err != nil {
				t.Fatal(err)
			}
			if len(r.Memories) > 0 {
				unrelatedHits[version]++
				if version == 1 {
					t.Logf("unrelated match: %s -> %s", query, r.Memories[0].Text)
				}
			}
		}
	}
	var shared, comparable, topChanged int
	for i, a := range rankings[0] {
		b := rankings[1][i]
		comparable += max(len(a), len(b))
		for _, id := range a {
			for _, other := range b {
				if id == other {
					shared++
					break
				}
			}
		}
		if len(a) == 0 && len(b) == 0 {
			continue
		}
		if len(a) == 0 || len(b) == 0 || a[0] != b[0] {
			topChanged++
		}
	}
	var mean, worst float64
	count := 0
	for _, x := range corpus {
		for _, text := range []string{x.text, x.query} {
			a, b := full.Embed(text), m.Embed(text)
			if len(a) == 0 || len(b) == 0 {
				continue
			}
			var dot float64
			for i, v := range a {
				dot += float64(v) * float64(b[i])
			}
			mean += dot
			worst = max(worst, 1-dot)
			count++
		}
	}
	t.Logf("%d synthetic queries: float32 hit@1=%d hit@5=%d; int8 hit@1=%d hit@5=%d; top-1 changed=%d; top-5 overlap=%d/%d; embedding cosine mean=%.6f, min=%.6f", len(corpus), totals[0][0], totals[0][1], totals[1][0], totals[1][1], topChanged, shared, comparable, mean/float64(count), 1-worst)
	t.Logf("hybrid hit@5: float32=%d int8=%d; unrelated queries with any result (out of 6): float32=%d int8=%d", hybridTop5[0], hybridTop5[1], unrelatedHits[0], unrelatedHits[1])
}
