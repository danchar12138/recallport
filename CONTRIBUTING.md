# 参与贡献 · Contributing

感谢你愿意改进 RecallPort。Issue 和 Pull Request 用中文或英文都可以。

## 提问题或建议

- **Bug**：请用 [Bug 报告模板](https://github.com/danchar12138/recallport/issues/new?template=bug_report.yml)，附上版本号、复现步骤，以及程序控制台里的输出。
- **想法**：请用 [功能建议模板](https://github.com/danchar12138/recallport/issues/new?template=feature_request.yml)，先说清楚你想解决什么问题，再说方案。
- **安全问题**：请不要公开提 Issue，见 [SECURITY.md](SECURITY.md)。

较大的改动建议先开 Issue 讨论，避免白做。

## 设计原则

提交之前，请对照这几条，它们决定了很多取舍：

1. **本地优先、无需配置**：不引入云端依赖、账号、遥测或运行时联网。
2. **记忆是一棵节点树，每次改动都留痕**：第 0 层是 AI 从对话里提取的记忆，往上每一层是 AI 写的汇总；AI 和用户都可以改写、移动、删除、恢复任何节点，但每次改动都是追加到 `nodes.jsonl` 的一条事件（带版本和操作者），删除先进回收站。只有用户在回收站里主动清除，才会真正抹掉历史。
3. **服务端不替 AI 写内容**：汇总、整理、合并都由 AI 完成；服务端只按层报告哪里还缺汇总、哪条汇总过期了（`maintain`），并拒绝会破坏树的操作（成环、版本冲突）。
4. **检索结果是线索，不是事实或指令**：任何面向模型的文案都要保持这个立场。
5. **每个操作都有反馈**：不可逆操作先确认；耗时操作要有进度；完成后要有明确提示。
6. **界面极简、一个强调色**：不要堆叠装饰，保持浅色与深色两套主题都可读。

## 本地开发

需要 Rust 1.94+、MSVC x64 构建工具、Windows SDK 和 `protoc`（详见 [README](README.md#从源码构建)）。

```powershell
# 运行（需要语义模型目录，见 README 的“从源码构建”）
cargo run -- --model <模型目录> --data ./data-dev

# 提交前请确保这三项通过
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```

`web/` 下的模板、样式和脚本通过 `include_str!` 编译进程序，修改后需要重新编译才会生效。

`cargo test` 在临时目录里用真实的事件日志和一个很小的词袋向量模型（不需要语义模型文件），覆盖节点的读写改删与恢复、ZIP 备份的替换与合并、旧版备份转换、附件、自动备份、MCP 工具、客户端配置编辑等场景，不会触碰你的记忆或真实的客户端配置。新功能请附带测试；涉及客户端配置的测试只能写临时目录。

### 修改界面时请注意

- 页面的内容安全策略是 `script-src 'self'; style-src 'self'`：**不能**写内联 `<script>`、`<style>` 或 `style="…"`，也不能引用 CDN。请把脚本放进 `web/*.js`，样式放进 `web/style.css`。动态设置样式请使用 `element.style.xxx = …`（CSSOM 允许），进度条用 `<meter>` / `<progress>`。
- 颜色请使用 `style.css` 顶部的设计令牌，并同时检查浅色和深色。
- 模板由 [minijinja](https://github.com/mitsuhiko/minijinja) 渲染。避免依赖 Jinja2 独有的特性，并检查对可能不存在的变量不要直接取属性。
- 新增模板或静态文件需要在 `src/web/mod.rs` 里登记。

### 修改 MCP 工具时请注意

工具的描述、字段说明和服务端指引（`src/mcp.rs`）就是写给模型看的“界面”；节点怎么存、怎么搜、怎么维护在 `src/tree/`，网页和 MCP 共用的入口是 `src/service.rs`。改动请保持：简短、可执行、说明**何时**使用，并避免鼓励模型记录机密或整段对话。

## 提交与 Pull Request

- 提交信息使用 [Conventional Commits](https://www.conventionalcommits.org/)，例如 `feat(web): …`、`fix(tree): …`、`docs: …`，描述可以用中文。
- 一个 PR 只做一件事，并在描述里写明动机和验证方式。涉及界面的改动请附浅色和深色截图。
- 请不要提交模型文件、记忆数据或备份（它们已被 `.gitignore` 排除）。

再次感谢！

---

**English summary.** Open an issue first for larger changes. Keep the product local-first, keep every change to a node an event in the append-only log (deletes go to the recycle bin first), leave writing summaries to the AI, and give every action visible feedback. Before sending a PR run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test`. The web UI is served under a strict CSP (`script-src 'self'; style-src 'self'`), so no inline scripts, styles or CDN links. Use Conventional Commits.
