package transport

import (
	"fmt"
	"recallport/internal/memory"
)

type configGroup struct {
	Title string
	Rows  []configRow
}
type configRow struct{ Label, Value, Meaning string }

func searchConfig(s memory.IndexStatus) []configGroup {
	var groups []configGroup
	if s.Model != "" {
		groups = append(groups, configGroup{"语义编码", []configRow{
			{"模型", s.Model, "模型版本与向量格式；版本变更时重算向量。"},
			{"精度与池化", s.Policy.Encoding, "CLS 取首个标记；L2 归一化后，点积等于余弦相似度。"},
			{"向量维数", fmt.Sprint(s.Dimensions), "每段保存的浮点数数量。"},
			{"输入上限 / CPU", fmt.Sprintf("%d tokens / %d 线程", s.Policy.MaxTokens, s.Policy.CPUThreads), "单次编码长度上限和用于推理算子的 CPU 线程。"},
			{"记忆切片", fmt.Sprintf("整条 + 最多 %d 段；片段 ≥ %d 字符", memory.MaxPassages-1, memory.MinPassageRunes), "正文和情境一起编码；补充片段按标点分隔。"},
		}}, configGroup{"语义候选", []configRow{
			{"相似度门槛", fmt.Sprintf("%.2f", s.Policy.SemanticThreshold), "纯语义和混合模式的向量候选需达此分数；不是概率。"},
			{"图谱建议门槛", fmt.Sprintf("%.2f", s.Policy.GraphThreshold), "语义虚线比搜索更保守，不影响明确引用。"},
			{"向量加速索引", "未使用", "SQLite 保存向量；按日期筛选后，Go 逐条计算相似度。"},
		}})
	}
	return append(groups, configGroup{"关键词候选", []configRow{
		{"索引 / 分词", "SQLite FTS5 / trigram", "按连续三个字符建立全文索引；短查询或索引异常可扫描字面。"},
		{"查询片段上限", fmt.Sprint(memory.LexicalTermLimit), "控制全文匹配式的规模。"},
		{"BM25 字段权重", fmt.Sprintf("正文 %.1f / 情境 %.1f", memory.BodyWeight, memory.ContextWeight), "正文命中比情境命中权重更高。"},
	}}, configGroup{"排序和结果", []configRow{
		{"候选上限", fmt.Sprintf("每路 %d 条", memory.CandidateLimit), "关键词和向量分别筛选候选；不是向量扫描上限。"},
		{"排名融合", fmt.Sprintf("偏移 %d / 关键词 %.1f / 语义 %.1f", memory.FusionOffset, memory.LexicalWeight, memory.SemanticWeight), "权重除以偏移加排名再相加；字面精确命中优先。"},
		{"查询与结果", fmt.Sprintf("%d 字符 / 默认 %d 条 / 最多 %d 条", memory.MaxQueryRunes, memory.DefaultRecallLimit, memory.MaxRecallLimit), "MCP 的查询长度和返回数量限制。"},
	}}, configGroup{"自动维护", []configRow{
		{"检查间隔", fmt.Sprintf("%d 秒", memory.MaintenanceSeconds), "启动时检查一次；运行时检查关键词完整性及向量覆盖。"},
		{"重算批次", fmt.Sprintf("每批 %d 条", memory.RebuildBatchSize), "缺失或版本不符的向量分批补算，不改正文和引用。"},
	}})
}
