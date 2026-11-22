# 安全策略 · Security Policy

## 支持的版本

只对[最新发布版本](https://github.com/danchar12138/recallport/releases/latest)提供安全修复。

## 威胁模型

RecallPort 是**单用户、仅本机**的服务：

- 只监听本机回环地址，监听其他地址会拒绝启动。
- 校验 `Host` 与 `Origin`，以抵御其他网页对本机服务的跨站请求和 DNS 重绑定。
- **有意不做认证**：本机上的其他程序无需凭据即可访问。因此请勿把服务转发到局域网或公网，也不要在多人共用的账户下运行。
- **不区分私密程度**：账号、密码等私人信息可以按原文保存，与普通节点一样被检索、整理和备份。备份与附件文件都是明文。
- **删除不是立刻抹掉**：删除的节点先进回收站（30 天后自动清除，也可以在网页里立即清除），节点的修改历史保存在 `nodes.jsonl` 的事件里；只有“永久删除”才会连历史一起移除。已经进过备份的内容，备份文件里仍然有。

以下属于**安全问题**，欢迎报告：

- 绕过回环监听限制，或绕过 `Host` / `Origin` 校验。
- 附件收纳与服务环节的问题：路径检查被绕过（网络共享、设备路径）、上传的文件在网页中被当作脚本执行。
- 「接入」功能写入客户端配置时越出 `recallport` 这一项，或在失败时损坏原文件。
- 网页界面中的脚本注入（XSS）、跨站请求伪造，或内容安全策略被绕过。
- 恢复备份、读取文件等环节的路径穿越或任意文件读写。
- 经由构造的记忆内容、备份文件或 MCP 请求造成的崩溃、数据损坏或越权读取。

以下**不属于**漏洞：本机其他程序因为“没有认证”而能访问服务（这是设计选择，已在文档中说明）；明文备份被他人拿到。

## 如何报告

请使用 GitHub 的**私密漏洞报告**：在仓库的 **Security** 标签页点击 **Report a vulnerability**。请不要在公开 Issue 里披露细节。

报告里请尽量包含：影响的版本、复现步骤、预期与实际行为、你认为的影响范围。我会尽快确认，并在修复发布后致谢（如你愿意）。

---

**English.** Only the latest release receives security fixes. RecallPort is a single-user, loopback-only service with intentionally no authentication; do not expose it to a network. Please report vulnerabilities privately through the repository's **Security → Report a vulnerability** page rather than in a public issue. In scope: bypassing the loopback/`Host`/`Origin` checks, XSS/CSRF/CSP bypass in the web UI, path traversal in restore or file handling, and crashes or data corruption from crafted memories, backups or MCP requests.
