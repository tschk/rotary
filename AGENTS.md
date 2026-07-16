# rotary — 代理框架引擎

**所有回复必须用英文。** 本文件用中文以节省 token。

## 定位

rotary 是**纯代理框架引擎**。负责 agent loop、tools、providers、sessions、permissions、computer-use、IPC。不含产品 UI。

宿主（telekinesis CLI/TUI、omi desktop）通过 `cargo add rotary` 嵌入或通过 JSON-RPC IPC 连接 `rotary serve`。

## 技术栈

- Rust 2021（MSRV 1.75）
- tokio（async runtime，feature-gated）
- serde / serde_json
- rs_peekaboo 0.3.2（crates.io，computer-use，feature-gated）
- reqwest（providers，feature-gated）
- MPL-2.0 许可证

## 模块

| 文件 | 职责 |
|---|---|
| `agent.rs` | 事件驱动 loop、tool registry、streaming |
| `provider.rs` | 多 provider OpenAI 兼容客户端 |
| `tools.rs` | 内置 FS/shell/find 工具 |
| `session.rs` | 会话树（fork/merge）+ JSONL 持久化 |
| `permissions.rs` | 策略模式、allow/deny、approver |
| `hooks.rs` | 生命周期钩子 |
| `mode.rs` | scope（coding/research/plan/ask/computer_use） |
| `context.rs` | AGENTS.md 加载、system prompt 组合 |
| `slash.rs` | slash 命令解析 |
| `guardrails.rs` | 空轮次检测、重复失败检测 |
| `extract.rs` | 结构化提取（JSON contracts） |
| `ranking.rs` | 主动排序 |
| `computer_use.rs` | rs_peekaboo 原生集成（无 FFI） |
| `ipc.rs` | Unix socket JSON-RPC 服务器 |
| `config.rs` | 配置文件 + env |
| `plugin.rs` | 插件注册表 |
| `acp.rs` | ACP host |
| `lsp.rs` | LSP 管理器 |
| `compaction.rs` | 上下文压缩（auto-compact） |
| `cost.rs` | 成本追踪（per-model pricing） |
| `graph_memory.rs` | 知识图谱（pagerank、community detection） |
| `http.rs` | reqwest HTTP 客户端（providers feature） |
| `marketplace.rs` | 插件市场索引 + 安装器 |
| `mcp.rs` | MCP 客户端（JSON-RPC 2.0 over stdio） |
| `memory.rs` | SQLite 持久化记忆存储 |
| `model_router.rs` | 分层模型路由（lite/standard/heavy/subagent） |
| `models.rs` | 模型注册表 + 兼容配置 |
| `multiagent.rs` | 多 agent 协调（coordinator/worker/reviewer/researcher） |
| `prompt_cache.rs` | Anthropic ephemeral cache_control |
| `repomap.rs` | pagerank 排序符号提取 |
| `rollout.rs` | rollout 追踪 + trace writer |
| `routing.rs` | 智能路由（turn complexity、agent route） |
| `sandbox.rs` | 沙箱配置 + 违规检测 |
| `secrets.rs` | 密钥脱敏（pattern redaction） |
| `skill_engine.rs` | 自改进技能引擎（bayesian confidence） |
| `sse.rs` | SSE 流解析 |
| `subagent.rs` | 子 agent 管理（git worktree 隔离） |

## Feature flags

```toml
default = ["ipc", "builtin-tools", "pi-compat"]
computer-use = ["dep:rs_peekaboo"]
ipc = ["dep:tokio", "tokio/net", "tokio/io-util", "tokio/sync", "tokio/process", "tokio/rt-multi-thread", "tokio/macros", "dep:cancellation-token"]
providers = ["dep:reqwest", "dep:eventsource-stream", "dep:futures"]
builtin-tools = ["dep:rayon", "dep:glob", "dep:ignore", "dep:regex"]
memory = ["dep:rusqlite"]
mcp = ["dep:rmcp", "dep:tokio", "tokio/process", "tokio/io-util", "tokio/sync"]
sqlite-sessions = ["dep:rusqlite"]
pi-compat = ["dep:serde_yaml", "dep:dirs"]
pi-extensions = ["pi-compat", "dep:rquickjs", "dep:regex"]
```

## 规则

- 不硬编码 API key，不加遥测。
- 新 agent 功能先落 rotary，再通过 IPC/slash 暴露给宿主。
- computer-use 通过 crates.io `rs_peekaboo` 依赖，不 vendor，不做 FFI。
- scope 不是 agent 名——是工作模式。
- 保持 MPL-2.0。

## 验证

```bash
cargo build
cargo build --all-features
cargo test --all-features
cargo clippy --all-features
cargo fmt --check
```

## 提交

英文 Conventional Commits，例：`feat(agent): stream tool call deltas`。
