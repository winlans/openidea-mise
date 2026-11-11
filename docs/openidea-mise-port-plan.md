# OpenIDEA Mise 插件移植与发布计划

## 目标与基线

本项目把 `intellij-mise` 的核心工作流移植到 OpenIDEA，而不是逐行翻译
IntelliJ Platform 代码。移植基线为 `intellij-mise v6.4.0`，目标运行时为
OpenIDEA `v0.1.40`、Plugin API `1.4`。

插件以独立社区插件发布：

- 插件 ID：`winlans.mise`
- Publisher：`winlans`
- Rust edition：2024
- 构建目标：`wasm32-wasip2`
- 许可证：Apache-2.0

每个阶段只有在原生测试、Wasm 构建、`openidea-plugin check`、本地安装和
OpenIDEA smoke test 都通过后才算完成。

## 阶段 0：工程与交付基础

1. 初始化 Rust/WASI 插件、`plugin.toml`、设置 schema、开发脚本和测试框架。
2. 固定 OpenIDEA SDK 到 `v0.1.40`，固定 Rust 到 `1.98.1`。
3. 建立 Mise 命令客户端、JSON 兼容解析、项目缓存、错误分类与日志脱敏。
4. 建立 CI：
   - format、clippy、原生测试；
   - `wasm32-wasip2` release 构建；
   - OpenIDEA package check；
   - 临时密钥签名打包 smoke test。
5. 建立 Release Please 自动版本管理和生产发布：
   - 合并 Release PR 后自动创建 tag 和 GitHub Release；
   - 使用 Ed25519 私钥签名 `.oiplugin`；
   - 生成 SHA-256；
   - 自动向 `l0ng-ai/openidea-plugins` 提交 registry PR。

生产私钥使用独立的 OpenIDEA publisher key，不复用 SSH key。开发机备份路径
为 `~/.config/openidea-mise/signing.key`，权限为 `0600`；GitHub Actions 中
使用受保护的 `release` Environment secret
`OPENIDEA_PLUGIN_SIGNING_KEY_B64`。Registry PR 使用 `REGISTRY_TOKEN`。

## 阶段 1：任务中心

通过项目根目录中的 Mise CLI 获取数据：

- `mise tasks ls --json`
- `mise ls --json`
- `mise env --json`

底部工具窗口包含 Tasks、Tools、Environment 三个页签。Tasks 支持搜索、
刷新和运行；Tools 展示安装与激活状态；Environment 默认遮盖敏感变量。
环境值只保存在当前 Wasm 实例内，不写入 storage、日志或发布 artifact。

任务列表生成动态 OpenIDEA Run Configuration：

- 名称：`Mise: <task>`
- 程序：`mise`
- 参数：`run <task>`
- 工作目录：项目根目录
- ID：`mise-task-<stable-hash>`

任务与工具元数据可持久缓存；环境值不可持久缓存。Mise 缺失、权限被拒绝、
项目不可信、命令失败和 JSON schema 变化都必须显示可操作错误。

## 阶段 2：Mise TOML 编辑能力

支持 `mise.toml`、`.mise.toml`、`mise.local.toml` 和 `.mise.local.toml`：

- Mise 配置键、工具名、任务名和依赖任务补全；
- 任务定义与 `depends`、`depends_post`、`wait_for` 的跳转和 Rename；
- 工具、任务、来源和当前版本 Hover；
- 任务定义旁的 Run gutter；
- `run` 和脚本字段的 Shell 注入。

编辑路径不得执行慢速 CLI 命令，只能读取当前文档、项目索引和阶段 1 缓存。
所有源码位置使用 UTF-16 行列，并容忍编辑中的不完整 TOML。

## 阶段 3：Mise Run Target

注册项目级 `mise.exec` Run Target，将用户显式选择的 Run/Test/Debug 命令包装为：

```text
mise exec -- <original-program> <original-args>
```

参数始终使用 argv 传递，不经过 shell 拼接。Run Target 仅在项目可信且
`process:mise` 已授权时注册；注册只使目标可选，不自动改变现有运行配置。

## 阶段 4：稳定化

- 任务依赖图、循环和缺失依赖提示；
- 大项目缓存上限、取消、超时和跨平台路径；
- Mise JSON 向前兼容 fixtures；
- 用户文档、故障排查、密钥轮换和 marketplace runbook；
- 发布前完整安装、升级与签名链验证。

下列 IntelliJ 专属拦截能力不在当前 OpenIDEA API 的可安全移植范围内：

- IntelliJ SDK 自动切换；
- 全局 VCS 环境拦截；
- 数据库认证拦截；
- 向任意 IDE 子进程全局注入环境变量。

若 OpenIDEA 后续提供对应 extension point，再单独实现。

## 自动发布契约

Release Please 同步维护 `Cargo.toml`、`Cargo.lock`、`plugin.toml` 和
`CHANGELOG.md`。tag、Cargo version 和 manifest version 不一致时发布失败。

生产包名固定为：

```text
winlans.mise-<version>.oiplugin
```

Registry entry 固定为 `plugins/winlans.mise.toml`，首个版本同时注册
`publishers/winlans.toml`。Release workflow 必须幂等：同一版本重跑时更新
同一个 fork branch/PR，不创建重复版本。

社区 marketplace 的最终可见性取决于 registry 维护者合并 PR；GitHub Release
和 registry PR 的创建本身全部自动完成。

## 实施状态

截至 2026-11-11：

- 阶段 0：工程、版本一致性检查、CI、Release Please、签名打包和 registry PR
  自动化已实现；专用生产 publisher key 已生成并以 `0600` 权限备份在
  `~/.config/openidea-mise/signing.key`。正式 GitHub Environment/Secrets
  需要仓库创建后配置。
- 阶段 1：Tasks、Tools、Environment 工具窗口、脱敏、缓存和动态 Run
  Configuration 已实现。
- 阶段 2：Mise TOML 补全、Hover、跳转、索引 Rename/Find Usages、Run gutter
  和 Shell 注入已实现。
- 阶段 3：可选 `mise.exec` Run Target 已实现。
- 阶段 4：依赖图、缺失依赖、循环检测和图节点运行已实现；后续版本继续做超大
  项目压力测试和更多 Mise JSON fixtures。

本地原生测试、format、Clippy、`wasm32-wasip2` release 构建、
OpenIDEA v0.1.40 `openidea-plugin check` 和临时密钥签名包 smoke test
均已通过。真实签名包已验证包含 `SIGNATURE.json`，registry 更新脚本也已通过
首次写入与幂等重跑测试；CI 同样覆盖该真实签名包到 registry 的完整路径。

官方 CLI 的隔离目录 `link → links → unlink` 本地安装注册测试也已通过。当前
自动化执行环境没有 GTK display，工具窗口、命令和编辑器交互仍需在桌面
OpenIDEA 会话中完成一次人工 smoke test。

剩余交付步骤是推送公开 GitHub 仓库、创建 `release` Environment、配置
Secrets，以及由首个正式 Release 自动提交 Marketplace PR；这些步骤需要有效的
GitHub 登录和仓库远端。
