//go:build model

package embedding_test

import (
	"path/filepath"
	"testing"

	"recallport/internal/embedding"
	"recallport/internal/memory"
	"recallport/internal/sqlite"
)

func TestOfflineParaphraseRecall(t *testing.T) {
	ctx := t.Context()
	model, err := embedding.Open()
	if err != nil || model == nil {
		t.Fatalf("embedded model: %v", err)
	}
	s, err := sqlite.Open(filepath.Join(t.TempDir(), "memory.db"), model)
	if err != nil {
		t.Fatal(err)
	}
	defer s.Close()
	a := memory.Actor{ID: "test", Label: "test"}
	texts := []string{
		"用 Windows 任务计划程序，让应用在登录后在后台启动。",
		"提交前先跑一遍单元测试，避免发布后返工。",
		"上次在 Debian 用 SQLite 调整了事务隔离级别。",
		"用 Git 工具检查本地提交历史。",
		"桌面应用的图标调整成黑色。",
		"把产品调研的原始访谈和总结分别保存。",
		"工作站本地目录是 D:\\work。",
	}
	for _, text := range texts {
		if _, err = s.Remember(ctx, a, memory.Draft{Text: text}); err != nil {
			t.Fatal(err)
		}
	}
	for _, pair := range []struct{ query, prefix string }{
		{"开机自启", "用 Windows"},
		{"SQLite transaction isolation", "上次在 Debian"},
	} {
		r, err := s.Recall(ctx, memory.Query{Text: pair.query, Limit: 1})
		if err != nil || r.Mode != "hybrid" || len(r.Memories) != 1 || len(r.Memories[0].Text) < len(pair.prefix) || r.Memories[0].Text[:len(pair.prefix)] != pair.prefix {
			q := model.Embed(pair.query)
			for _, text := range texts {
				var similarity float32
				for i, x := range model.Embed(text) {
					similarity += q[i] * x
				}
				t.Logf("score %.3f: %s", similarity, text)
			}
			t.Fatalf("%q: %+v (%v)", pair.query, r, err)
		}
	}
	r, err := s.Recall(ctx, memory.Query{Text: "珊瑚礁的热带鱼"})
	if err != nil || len(r.Memories) > 0 {
		t.Fatalf("irrelevant memory: %+v %v", r, err)
	}
}
