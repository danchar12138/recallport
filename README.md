# RecallPort

> **换个 AI，往事不必重讲。**
> Switch AIs. Skip the introductions.

RecallPort 是一款即插即用的本地 AI 记忆服务。记忆随日常使用自然积累、长期保留，无需手动整理。换个 AI，工作不必从头交代。

## 使用

1. 在发布目录启动 `recallport.exe`，在本机 3765 端口打开管理页。不要拆开 exe 和 `model` 目录。无需登录。
2. 在「接入」按三步操作：复制 MCP 地址、把配置加入客户端（Codex 或 Claude Code）、点「测试本机连接」逐项检查。客户端选择 Streamable HTTP，不需要令牌。
3. 在时间线新建、查阅、筛选、纠错或删除记忆；搜索结果可切换相关性排序和话题时间线。「概览」可按年、月跳转，详情页可维护引用。
4. 「关系图」按话题和日期筛选，逐页浏览或展开单条记忆的邻居。图里的实线是已保存引用，虚线是按需计算的语义候选，不会写入记忆。

界面快捷键：`/` 聚焦搜索，`N` 新建记忆，`Ctrl+Enter` 保存正在编辑的记忆，`?` 查看全部。删除、移除引用、恢复备份等不可逆操作都会先弹出确认；重建索引、备份、恢复会显示进度，并在完成后通知。浏览器设为深色时界面自动跟随。

是否主动调用记忆工具，取决于客户端和模型。RecallPort 不监听完整对话。

默认 MCP 端点为本机 3765 端口的 /mcp，支持 recall、read、remember、link 四个工具。服务端内置使用指引，会引导模型在任务开始前主动 recall、在决策或纠错后主动 remember，并在发现重复或过时记忆时用 link 与引用来维护。remember 会同时返回相似记忆供模型去重和关联；来源名称由调用者提供，不证明身份。接入页还提供一段可粘贴到客户端规则文件（如 AGENTS.md、CLAUDE.md）的提示，让模型更主动地使用记忆。

服务只监听本机。本机其他程序无需凭据也可访问，请勿把服务转发到局域网或公网。

## 搜索索引

LanceDB 负责两路候选检索：关键词使用 BM25、Jieba 和中文双字切片；语义使用 IBM Granite 97M Multilingual R2 官方 FP32 ONNX。整条记忆及其片段各自编码为 384 维向量，以最匹配片段的余弦相似度入选。库小则精确搜索，至少 256 条已编码记忆时自动建立 IVF-HNSW-Flat 近似索引，不量化向量。

语义候选门槛为 `0.80`，图谱建议为 `0.86`；它们不是置信度或两路占比。关键词及语义独立入选、按排名融合，字面精确命中优先。「数据」页显示当前运行参数、覆盖率和实际启用的索引，并支持分别重建。

Web 使用自带的极简样式、Flatpickr 和 Cytoscape.js，静态资源随 exe 打包，无 CDN 或 Node.js 运行依赖。Web 依赖许可证在 `web/vendor`。

## 备份与恢复

在 Web 的「数据」页点击「下载备份」。`.recallport.json` 包含正文、时间、来源、引用和写入去重记录，不依赖某个 LanceDB 版本或模型文件。恢复时，在同页选择备份并确认。恢复会替换当前库，不会合并；程序保留替换前的备份，并重建派生索引，无需重启。最近的回退备份可在同页下载或恢复。

搬迁优先使用 Web 备份；也可停止服务后搬动整个便携目录。不要在写入时单独复制库内文件。备份是明文，可能包含之后已删除的记忆，不应上传到公开位置。

## 结构

`src/memory.rs` 定义记忆模型，`src/store.rs` 管理 LanceDB 存储、检索和备份，`src/embedding.rs` 封装本机模型，`src/web.rs` 和 `src/mcp.rs` 分别提供 Web 和 MCP 接口。只运行一个服务进程，主库在便携目录的 `data/recallport.lance`。正文、出向引用和写入去重键在同一行提交。

## 构建

Windows x64 构建环境需要 Rust 1.94 或更新版本、MSVC x64 构建工具、Windows SDK 和 `protoc`。安装 `x86_64-pc-windows-msvc` Rust target，再运行：

    ./scripts/build.ps1

产物在 `dist/RecallPort`。脚本从构建环境定位可随应用分发的 Microsoft x64 CRT，或通过 `-VCRedistDir` 指定目录。打包后无需安装开发工具或联网。模型文件、记忆和备份不提交到 Git。

构建前准备下列上游原件。脚本校验 SHA-256，不转换、不量化。资产缓存默认 `%TEMP%/recallport-native-compare`，可用 `-AssetCache` 指定：

- Hugging Face `ibm-granite/granite-embedding-97m-multilingual-r2` 修订 `835ad14087e140460703cf0fae09f97d469d65c2`：`onnx/model.onnx` 放到 `granite97/onnx_model.onnx`，`tokenizer.json` 放到 `granite97/tokenizer.json`。
- PyPI 的 `onnxruntime-1.32.0-cp311-cp311-win_amd64.whl` 放到 `onnxruntime-win-amd64.whl`。只提取官方 Windows DLL 和声明，不发布 Python 包。
