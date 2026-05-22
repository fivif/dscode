# Safety Rules

> 自动注入到 DS Code 每一次会话的系统提示。
> 列出 Agent 必须避开的危险操作。

## 严禁

- `rm -rf` 任何系统目录
- `git push --force` 到主分支
- 修改 `.dscode/spec/` 下任何文件（需 --unsafe 标志）
- 提交包含 API key / 密码 / 私钥的代码
- 在生产环境直接运行未测试的迁移脚本

## 谨慎

- 任何 `git reset --hard` 操作（先 side-git 快照）
- 任何大批量文件重命名（先 dry-run 输出）
- 调用付费 API（先确认成本估算）

## 默认行为

- 不确定时拒绝 > 不确定时放行（FAIL-CLOSED）
- 每个 MAGI 轮次自动 side-git 快照
- 任何 `do_bash` 调用超过 60s 自动 SIGKILL
