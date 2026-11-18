# RecallPort

> **换个 AI，往事不必重讲。**
> Switch AIs. Skip the introductions.

RecallPort 是一款即插即用的本地 AI 记忆服务。记忆随日常使用自然积累、长期保留，无需手动整理。换个 AI，工作不必从头交代。

## 使用

1. 启动 recallport.exe，在本机 3765 端口打开管理页。无需登录。
2. 在「接入」复制 MCP 地址，在 AI 客户端中选择 Streamable HTTP，不需要令牌。
3. 在时间线新建、查阅、筛选、纠错或删除记忆；「概览」可按年、月跳转，详情页可维护引用。

是否主动调用记忆工具，取决于客户端和模型。RecallPort 不监听完整对话。

默认 MCP 端点为本机 3765 端口的 /mcp，支持 remember、recall、read。remember 可选填来源；这个名称由调用者提供，不证明身份。

服务只监听本机。本机其他程序无需凭据也可访问，请勿把服务转发到局域网或公网。

## 搜索索引

默认混合检索，可切换关键词或语义模式。两类索引都在 SQLite 中。「数据」页显示语义索引覆盖量、检查和修复时间，可手动提交后台重建。启动后立即检查，运行中每分钟复查；关键词索引查询失败时可回退到字面扫描，向量索引不可读时仍可使用字面搜索。修复不修改记忆正文或引用。整个数据库不可读时，应从快照恢复。

Web 使用 Bootstrap 和 Flatpickr，静态资源随 exe 打包，无 CDN 或 Node.js 运行依赖。依赖许可证在 internal/transport/vendor。

从源码对照原始与量化权重时，可将 `RECALLPORT_FLOAT_WEIGHTS` 指向固定版本的 `model.safetensors`，然后运行 `go test -tags model -run TestQuantizationComparison -v ./internal/embedding`。该对照使用合成数据和相同的分词、检索逻辑，不读取个人记忆库。

## 备份与恢复

在 Web 的「数据」页点击「下载快照」。恢复时，在同页选择快照并确认。恢复会替换当前库，不会合并；程序先保留替换前的快照，再恢复记忆和引用并重建索引，无需重启。最近的回退快照可在同页下载或直接恢复。

请使用可信的 RecallPort 快照，不要在服务运行时单独复制数据库文件代替快照。旧快照可能包含之后已删除的记忆。

## 结构

记忆核心与端口位于 internal/memory。Web 和 MCP 位于 internal/transport；SQLite 存储和检索在 internal/sqlite；离线向量模型在 internal/embedding。cmd/recallport 负责组装和启动，仍是一个进程、一个 exe、一个主库。

## 构建

Windows 上用 Go 1.26 或更新版本运行：

    ./scripts/build.ps1

构建时校验固定版本的模型，量化后打进 dist/recallport.exe。运行时不需要 Go、联网下载模型或另起推理进程。传入 -LexicalOnly 可构建无模型版。

首次从源码构建完整版本时，请从 Hugging Face 的 minishlab/potion-multilingual-128M（MIT）下载 tokenizer.json 和 model.safetensors。固定修订为 73908c3438cf03b6a01bcb9611d62b23d0726f08。将两个文件放入临时目录下的 recallport-model，或通过 -ModelCache 指定目录。构建会校验 SHA-256。使用已构建的 exe 不需要这两个文件。
