# RecallPort

> **换个 AI，往事不必重讲。**
> Switch AIs. Skip the introductions.

RecallPort 是一款即插即用的本地 AI 记忆服务。记忆随日常使用自然积累、长期保留，无需手动整理。换个 AI，工作不必从头交代。

## 使用

1. 在发布目录启动 `recallport.exe`，在本机 3765 端口打开管理页。不要拆开 exe 和 `model` 目录。无需登录。
2. 在「接入」复制 MCP 地址，在 AI 客户端中选择 Streamable HTTP，不需要令牌。
3. 在时间线新建、查阅、筛选、纠错或删除记忆；搜索结果可切换相关性排序和话题时间线。「概览」可按年、月跳转，详情页可维护引用。
4. 「关系图」按话题和日期筛选，逐页浏览或展开单条记忆的邻居。图里的实线是已保存引用，虚线是按需计算的语义候选，不会写入记忆。

是否主动调用记忆工具，取决于客户端和模型。RecallPort 不监听完整对话。

默认 MCP 端点为本机 3765 端口的 /mcp，支持 remember、recall、read。remember 可选填来源；这个名称由调用者提供，不证明身份。

服务只监听本机。本机其他程序无需凭据也可访问，请勿把服务转发到局域网或公网。

## 搜索索引

关键词使用 SQLite FTS5，语义使用 IBM Granite 97M Multilingual R2 官方 FP32 ONNX。`vectors` 表保存 384 维向量，查询逐条比较余弦相似度，不使用 HNSW、IVF 或独立向量数据库。切换模型后后台重算旧向量，正文和引用不动。「数据」页显示实际运行参数、覆盖率和维护状态，并支持分别重建。整个数据库不可读时，应从快照恢复。

当前语义相似度门槛为 `0.82`，语义图谱建议用 `0.86`。在 26 条有答案的合成查询和 14 条无答案查询上，`0.82` 保留了 20 条前五命中，挡住了 12 条无答案查询的语义候选。分数不是置信度；混合检索仍会返回独立的关键词结果。

Web 使用 Bootstrap、Flatpickr 和 Cytoscape.js，静态资源随 exe 打包，无 CDN 或 Node.js 运行依赖。依赖许可证在 internal/transport/vendor。

从源码对照原始与量化权重时，可将 `RECALLPORT_FLOAT_WEIGHTS` 指向固定版本的 `model.safetensors`，然后运行 `go test -tags model -run TestQuantizationComparison -v ./internal/embedding`。该对照使用合成数据和相同的分词、检索逻辑，不读取个人记忆库。

## 备份与恢复

在 Web 的「数据」页点击「下载快照」。恢复时，在同页选择快照并确认。恢复会替换当前库，不会合并；程序先保留替换前的快照，再恢复记忆和引用并重建索引，无需重启。最近的回退快照可在同页下载或直接恢复。

请使用可信的 RecallPort 快照，不要在服务运行时单独复制数据库文件代替快照。旧快照可能包含之后已删除的记忆。

## 结构

记忆核心与端口在 `internal/memory`。Web 和 MCP 在 `internal/transport`，SQLite 存储和检索在 `internal/sqlite`，Granite 分词和本机推理在 `internal/granite`。只需运行一个服务进程，使用一份主库。

## 构建

Windows x64 上用 Go 1.26 或更新版本和 Zig 0.15.2 构建：

    ./scripts/build.ps1

产物在 `dist/RecallPort-Granite`。运行不需要 Go、Zig、Python 或联网。ONNX Runtime 需要 Microsoft VC++ x64 运行库：目标机没有时，可在构建时通过 `-VCRedistDir` 指向有再分发权限的 Microsoft CRT 目录，将 DLL 随应用发布。也可用 `recallport.exe serve -lexical-only` 临时只用关键词。

构建前准备下列上游原件。脚本校验 SHA-256，不转换、不量化。资产缓存默认 `%TEMP%/recallport-native-compare`，可用 `-AssetCache` 指定：

- Hugging Face `ibm-granite/granite-embedding-97m-multilingual-r2` 修订 `835ad14087e140460703cf0fae09f97d469d65c2`：`onnx/model.onnx` 放到 `granite97/onnx_model.onnx`，`tokenizer.json` 放到 `granite97/tokenizer.json`。
- PyPI 的 `onnxruntime-1.32.0-cp311-cp311-win_amd64.whl` 放到 `onnxruntime-win-amd64.whl`。只提取官方 Windows DLL 和声明，不发布 Python 包。
