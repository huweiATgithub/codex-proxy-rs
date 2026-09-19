# Fork 补丁维护指南

本指南用于在 `huweiATgithub/codex-proxy-rs` 中维护个人补丁，并采用上游
`zyycn/codex-proxy-rs` 的发行版本。每次发行由选定的上游 tag 加上本 fork 的补丁组成，
使用与上游相同的版本号和公开 tag 名称。

下文以当前基线 `v3.10.0`、下一次采用的基线 `v3.11.0` 为例。版本号是示例，执行时应替换为
实际选定且已经存在的上游发行。命令从仓库根目录执行；提交和验证约定见 [贡献与审查](../CONTRIBUTING.md)。

## 分支与 tag 的职责

| 引用 | 职责 |
| --- | --- |
| `origin` | 本 fork 的远端 |
| `upstream` | 原仓库的远端 |
| `main` | 上游 `main` 的副本，只同步上游提交 |
| `refs/upstream/tags/v3.10.0` | 本地保存的原始上游 tag |
| `patches/v3.10.0` | 原始上游 `v3.10.0` 加上可重放的个人补丁 |
| `refs/tags/v3.10.0` | 本 fork 已发布版本的固定引用 |

两个仓库的 `v3.10.0` 名称相同，指向的提交不同。原始上游 tag 放在独立的本地引用中，
避免与本 fork 的公开 tag 冲突。不要将上游 tag 批量导入本地 `refs/tags/*` 或推送到本 fork。

补丁分支保持线性提交，每个提交围绕一个明确修改；应用代码、发行工具和 fork 配置都属于可重放补丁。
`release/version.yaml` 和 `release/notes.md` 保持对应上游 tag 的内容。
`main` 上尚未发行的提交不合入这些版本分支。

## 准备远端并同步 main

需要 Git；通过命令行发版还需要已认证的 GitHub CLI。开始前查看 `git status`，
先处理未提交内容再切换、同步或重放分支；不要让一次升级顺带暂存或丢弃其他工作。

用 `git remote -v` 核对 `origin` 指向本 fork。首次添加上游远端：

```bash
git remote add --no-tags upstream https://github.com/zyycn/codex-proxy-rs.git
```

如果 `upstream` 已存在，核对其地址并关闭自动获取同名 tag：

```bash
git config remote.upstream.tagOpt --no-tags
```

同步 `main` 与补丁升级是独立操作。需要更新副本时执行：

```bash
git fetch --no-tags upstream &&
git switch main &&
git merge-base --is-ancestor HEAD upstream/main &&
git merge --ff-only upstream/main &&
git push origin main
```

祖先检查保证本地 `main` 没有独有提交；仅执行 `merge --ff-only` 不能排除本地已领先的情况。
任一步失败就检查分支差异，不用 merge commit、reset 或强推来掩盖分歧。

## 创建和维护补丁分支

首次采用一个上游版本时，先取回原始 tag，再从它创建补丁分支：

```bash
git fetch --no-tags upstream \
  refs/tags/v3.10.0:refs/upstream/tags/v3.10.0 &&
git switch -c patches/v3.10.0 'refs/upstream/tags/v3.10.0^{commit}'
```

若该补丁分支已存在，切换到现有分支继续维护，不重复创建。新克隆中只有远端分支时，使用：

```bash
git fetch --no-tags origin &&
git switch --track -c patches/v3.10.0 origin/patches/v3.10.0
```

新增或修复个人补丁时，在当前 `patches/*` 分支工作，或从它分出功能分支、向它提交 PR。
补丁 PR 以对应的 `patches/*` 为目标，使用 squash 或 rebase 保持补丁历史线性；其余步骤见 [PR 流程](../CONTRIBUTING.md#pr-流程)。按实际修改执行验证，
只暂存本次修改的文件，以英文 Conventional Commits 提交；普通补丁不更新版本号或发行说明。

检查相对原始上游的完整差异，确认只有需要维护的修改：

```bash
git log --oneline refs/upstream/tags/v3.10.0..patches/v3.10.0
git diff refs/upstream/tags/v3.10.0 patches/v3.10.0
```

验证并提交完成后，保存补丁分支：

```bash
git push -u origin patches/v3.10.0
```

如果本 fork 的 `v3.10.0` 已发布，后续补丁仍可在该补丁分支维护，随下一次采用的上游版本发行。
不要移动已发布的 tag 来容纳新补丁。两次上游发行之间需要独立补丁版本时，应先另行确定版本方案；
当前发布入口只支持与选定上游版本同名的发行。

## 升级到下一个上游 tag

先确认旧补丁分支已包含所有要保留的提交、工作区干净，并取回新旧两个原始 tag。
新克隆不会自动带有 `refs/upstream/tags/*`，不能用本 fork 的同名 tag 代替它们。

```bash
git fetch --no-tags upstream \
  refs/tags/v3.10.0:refs/upstream/tags/v3.10.0 \
  refs/tags/v3.11.0:refs/upstream/tags/v3.11.0 &&
git switch -c patches/v3.11.0 patches/v3.10.0 &&
git rebase --onto refs/upstream/tags/v3.11.0 refs/upstream/tags/v3.10.0
```

这会把旧补丁分支中原始 `v3.10.0` 之后的提交，重放到原始 `v3.11.0` 之上。
新分支从 `patches/v3.10.0` 创建，以包含上次发行后继续维护的补丁。
发行说明不进入补丁提交，因此不会随补丁重放。旧补丁分支和已发布 tag 保持原样。

遇到冲突时，按新版本的实际实现调整补丁，只暂存解决冲突的文件，然后执行 `git rebase --continue`。
只有确认上游已经完整提供该补丁的行为，或该补丁已不再需要时，才删除相应补丁；不要为了通过 rebase 而直接跳过冲突。
放弃本次尝试用 `git rebase --abort`，新分支回到此次重放前的状态。

重放完成后，比较补丁的变化及新版本相对上游的完整差异：

```bash
git range-diff \
  refs/upstream/tags/v3.10.0..patches/v3.10.0 \
  refs/upstream/tags/v3.11.0..patches/v3.11.0
git diff refs/upstream/tags/v3.11.0 patches/v3.11.0
```

无冲突不等于行为正确。检查每项补丁是否仍有必要、是否适配新的调用关系，并按
[验证约定](../CONTRIBUTING.md#验证) 运行相关检查和行为验收。维护本 fork 的发行补丁时，
还要核对镜像、安装器和更新器仍指向本 fork，并将 README 安装 URL 中的补丁分支更新为 `patches/v3.11.0`。

验证完成后推送新补丁分支：

```bash
git push -u origin patches/v3.11.0
```

此后在 `patches/v3.11.0` 上维护新补丁。升级过程中若旧分支又新增了修改，应先把缺失的修改
带到新分支并重新验证，避免新分支成为维护入口后遗失它们。

## 从补丁准备发行

以下以 `v3.11.0` 为例；首次发布 `v3.10.0` 使用同样步骤并替换版本号。
本 fork 先创建发行 tag，再触发发布工作流。仓库保留的 `release/publish` 是上游入口，会修改版本、创建提交和 tag 并推送，
不用于本指南的 fork 发版。

确认工作区干净、补丁已提交并验证通过；在补丁分支的当前提交创建 tag，一起推送分支和 tag：

```bash
git switch patches/v3.11.0 &&
git tag -a v3.11.0 -m 'Release v3.11.0' &&
git push --atomic origin refs/heads/patches/v3.11.0 refs/tags/v3.11.0
```

已有同名 tag 时先核对其提交，不重复创建或强制覆盖。推送失败后先核对远端分支和 tag，确认状态再重试推送。
`release/version.yaml` 和 `release/notes.md` 保留上游内容。

推送成功后，在 GitHub Actions 的 Release 工作流中选择 `patches/v3.11.0`，填写 `tag=v3.11.0`。

也可通过命令行触发：

```bash
gh workflow run release.yml \
  --repo huweiATgithub/codex-proxy-rs \
  --ref patches/v3.11.0 \
  --raw-field tag=v3.11.0
```

该命令直接触发真实发版，不检查本地工作区或替你推送代码。
所选补丁分支提供工作流定义，`tag` 输入决定发布源码。工作流检出已推送的 tag，将其解析为提交 SHA，
沿用上游流程对该提交执行检查、构建和发布 GitHub Release、镜像及附件。
后续推进补丁分支不会改变 tag 指向的源码；触发成功也不等于产物已发布。

中断或失败时先核对对应 run、远端 tag 和 GitHub Release，确认已完成的步骤。重试已有 run 的失败任务，
例如 `gh run rerun <run-id> --failed --repo huweiATgithub/codex-proxy-rs`，沿用原始 tag 输入；
不要删除 tag、强推或改版本号来消除报错。

若需要代码修复，直接在对应补丁分支修改并验证。重新 dispatch 同一 tag 仍构建该 tag 的源码，
不会包含分支上的新提交；不要移动已发布的 tag，独立补丁修订需另行确定版本方案。
发布完成后核对 run 的固定提交与 tag 一致，检查 GitHub Release 正文，并对照 `release/platforms.yaml`
和工作流确认镜像、附件及校验和齐全。仍在构建、失败或缺少产物时不算发行完成；发布也不代表运行实例已经升级。
保留已发布 tag；旧补丁分支可在确认新分支包含所需修改、对应发版验收完成后归档或删除。
删除分支不改变 tag 固定的发行提交。
