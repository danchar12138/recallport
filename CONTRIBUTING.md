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
2. **记忆是追加式的**：模型通过新增记忆来更正旧记忆，而不是改写历史。网页上的编辑和删除是给人用的纠错入口。
3. **检索结果是线索，不是事实或指令**：任何面向模型的文案都要保持这个立场。
4. **每个操作都有反馈**：不可逆操作先确认；耗时操作要有进度；完成后要有明确提示。
5. **界面极简、一个强调色**：不要堆叠装饰，保持浅色与深色两套主题都可读。

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

### 修改界面时请注意

- 页面的内容安全策略是 `script-src 'self'; style-src 'self'`：**不能**写内联 `<script>`、`<style>` 或 `style="…"`，也不能引用 CDN。请把脚本放进 `web/*.js`，样式放进 `web/style.css`。动态设置样式请使用 `element.style.xxx = …`（CSSOM 允许）。
- 颜色请使用 `style.css` 顶部的设计令牌，并同时检查浅色和深色。
- 模板由 [minijinja](https://github.com/mitsuhiko/minijinja) 渲染。避免依赖 Jinja2 独有的特性，并检查对可能不存在的变量不要直接取属性。
- 新增静态文件需要在 `src/web.rs` 里登记路由。

### 修改 MCP 工具时请注意

工具的描述、字段说明和服务端指引（`src/mcp.rs`、`src/memory.rs`）就是写给模型看的“界面”。改动请保持：简短、可执行、说明**何时**使用，并避免鼓励模型记录机密或整段对话。

## 提交与 Pull Request

- 提交信息使用 [Conventional Commits](https://www.conventionalcommits.org/)，例如 `feat(web): …`、`fix(store): …`、`docs: …`，描述可以用中文。
- 一个 PR 只做一件事，并在描述里写明动机和验证方式。涉及界面的改动请附浅色和深色截图。
- 请不要提交模型文件、记忆数据或备份（它们已被 `.gitignore` 排除）。

再次感谢！

---

**English summary.** Open an issue first for larger changes. Keep the product local-first, append-only for AI-written memory, and give every action visible feedback. Before sending a PR run `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings` and `cargo test`. The web UI is served under a strict CSP (`script-src 'self'; style-src 'self'`), so no inline scripts, styles or CDN links. Use Conventional Commits.
