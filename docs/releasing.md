# 发布与 Marketplace 自动化

## 发布模型

`main` 上的 conventional commits 由 Release Please 汇总为 Release PR。
合并 Release PR 后，`.github/workflows/release.yml` 自动完成：

1. 创建 `v<version>` tag 和 GitHub Release；
2. 校验 `Cargo.toml`、`Cargo.lock`、`plugin.toml` 版本一致；
3. 构建 `wasm32-wasip2` 组件并运行 `openidea-plugin check`；
4. 使用 publisher 私钥签名
   `winlans.mise-<version>.oiplugin`；
5. 上传插件包和 `.sha256` 文件到 GitHub Release；
6. 从包内 `SIGNATURE.json` 读取公钥；
7. 幂等更新 `publishers/winlans.toml` 和
   `plugins/winlans.mise.toml`；
8. 推送 `winlans/openidea-plugins` 的
   `automation/winlans-mise` 分支，并创建或更新上游 PR。

Marketplace 最终上线仍需要 `l0ng-ai/openidea-plugins` 维护者合并 PR。
如果 Release 已创建但后续签名、上传或 registry 步骤失败，可在 Actions 中重新
运行失败的 job；也可以手工触发 `Release` workflow，并输入已有 tag（例如
`v0.1.0`）。手工重跑会覆盖同名 Release 资产，并复用同一个 registry 分支/PR。

## 一次性 GitHub 配置

创建公开仓库 `winlans/openidea-mise`，默认分支为 `main`。在仓库 Settings
中创建名为 `release` 的 Environment，并配置：

- Environment secret `OPENIDEA_PLUGIN_SIGNING_KEY_B64`：publisher key 文件
  内容再次 Base64 编码后的结果；
- Environment secret `REGISTRY_TOKEN`：能够写入
  `winlans/openidea-plugins` fork 并向上游创建 PR 的 fine-grained token。

首次配置时先创建 registry fork：

```sh
gh repo fork l0ng-ai/openidea-plugins --clone=false
```

建议对 `release` Environment 开启 required reviewers，避免 tag 创建后私钥被
未经审核的工作流使用。工作流本身只在 Release Please 实际创建 Release 后进入
该 Environment。

## 首次生成 publisher key

使用 OpenIDEA v0.1.45 的 CLI：

```sh
scripts/install-openidea-plugin.sh
mkdir -p ~/.config/openidea-mise
.tools/openidea-plugin keygen ~/.config/openidea-mise/signing.key
chmod 0600 ~/.config/openidea-mise/signing.key
base64 -w 0 ~/.config/openidea-mise/signing.key
```

安装脚本固定校验 v0.1.45 Linux x86_64 release asset 的 SHA-256。升级 CLI
版本时必须在脚本中审查并更新摘要；临时测试其他版本时可显式设置
`OPENIDEA_CLI_SHA256`，不要跳过校验。

最后一条命令的输出保存为
`OPENIDEA_PLUGIN_SIGNING_KEY_B64`。不要复用 SSH key，不要提交私钥、Base64
私钥或 Actions 日志中的私钥内容。CLI 输出的公钥是公开信息；首个签名包中也会
包含同一公钥，自动发布脚本用它创建 publisher registry 文件。

macOS 的 Base64 命令可使用：

```sh
base64 < ~/.config/openidea-mise/signing.key | tr -d '\n'
```

## Token 最小权限

`REGISTRY_TOKEN` 应限定到 `winlans/openidea-plugins` fork，授予 Contents
读写权限。向 `l0ng-ai/openidea-plugins` 创建 PR 还需要该 token 对公开上游
仓库具备 Pull requests 读写能力。若 GitHub 的 fine-grained token 无法向上游
创建 PR，可使用仅限该发布用途的 classic token，并定期轮换。

## 本地发布验证

日常开发需要生成可从磁盘安装的未签名包时，运行：

```sh
mise run package-local
```

产物为 `dist/winlans.mise-<version>-local.oiplugin` 及对应的
`.sha256` 文件。可从项目根目录运行
`sha256sum -c dist/winlans.mise-<version>-local.oiplugin.sha256` 校验。
该任务不会读取 production publisher key。

需要验证正式签名发布链时，再运行：

```sh
scripts/install-openidea-plugin.sh
python3 scripts/check_versions.py

tmp_dir="$(mktemp -d)"
.tools/openidea-plugin keygen "$tmp_dir/test.key"
OPENIDEA_PLUGIN_BIN=.tools/openidea-plugin \
OPENIDEA_SIGNING_KEY_FILE="$tmp_dir/test.key" \
scripts/build-package.sh 0.1.0
```

生产包只能使用备份在 `~/.config/openidea-mise/signing.key` 和 GitHub
Environment 中的 publisher key；临时 key 仅用于 CI smoke test。

## 版本与幂等性

- tag 固定为 `v<version>`；
- 包名固定为 `winlans.mise-<version>.oiplugin`；
- 同一版本的 URL 或 SHA-256 不同会使 registry 更新失败；
- 同一版本、URL 和 SHA-256 完全相同时不重复追加；
- 自动化发现未注册的新签名 key 时会失败，密钥轮换必须显式执行 endorsement
  流程；
- registry 自动分支固定为 `automation/winlans-mise`，因此重跑会更新同一个
  PR，不会生成重复 PR。

## 密钥轮换

先生成新 key，再由旧 key 对新公钥签名：

```sh
.tools/openidea-plugin keygen new-signing.key
.tools/openidea-plugin endorse "<new-public-key>" \
  --key ~/.config/openidea-mise/signing.key \
  --publisher winlans
```

手工向 `publishers/winlans.toml` 增加新旧公钥和 endorsement，等待 registry
PR 合并后再替换 GitHub Secret 和本地备份。旧 key 在确认升级链正常前保持有效；
私钥丢失或泄露时应立即请求 registry 维护者 revoke。
