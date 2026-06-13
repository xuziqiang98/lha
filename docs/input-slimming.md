# Experimental Model Input Slimming Design

本文定义 LHA 的实验性 input slimming 设定，设计参考 Headroom，但不复制
Headroom 的 provider 代理实现。它不是现有 `/compact` 的替代品：`/compact`
负责总结并替换会话历史，而 input slimming 只改写即将发送给模型的临时
`TurnRequest` 副本。

设计优先级如下：

1. 输出质量安全
2. 可逆恢复
3. 可观测性
4. token 节省比例

因此，首版实现应优先选择保守压缩和 fail-open 行为，而不是追求最大压缩率。

## Implementation Status

Input Slimming 已有首版 product-private 实现，默认关闭。启用方式：

```toml
[features]
input_slimming = true
```

当前实现范围：

- 只压缩 latest user message 之前的旧 tool results；
- 跳过 user/system/developer/assistant/reasoning/hosted activity；
- 跳过当前 turn 新产生的 tool results；
- 跳过 `content_items: Some(...)` 的 structured tool result，因为 provider
  serialization 会优先发送 `content_items`；
- 只在至少一个 payload 被接受压缩时注入 `lha_input_retrieve`；
- 原文存入 session-scoped in-memory store，默认 capacity 为 1000，TTL 为 300
  seconds；
- marker 格式为 `<<lha-input:{hash}>>`；
- hash 使用 repo 已有的 `sha2::Sha256`，截断为 24 个 lowercase hex characters。

## Problem Statement

长会话中真正推高上下文窗口压力的，往往不是用户意图本身，而是机器生成内容：
工具输出、命令日志、搜索结果、JSON 数组、diff 等。这些内容通常有大量冗余，
但如果只做盲目截断，模型之后可能会缺少关键细节。

LHA 已有 `/compact`，它在 `docs/slash-commands.md` 中被描述为通过总结会话来
减少上下文使用。这个能力适合长会话状态收缩，但它会改变持久历史的形态：旧上下
文会被摘要项替换。input slimming 要解决的是另一个问题：在不改变持久历史的
前提下，减少单次模型请求的输入 token。

预期流程是：

- 使用 `ContextManager::for_prompt()` 生成常规模型侧历史；
- 构建常规语义层 `TurnRequest`；
- 只在该 request 的 clone 中压缩安全、低风险片段；
- 将压缩后的 clone 发给 runtime；
- rollout history 和 `ContextManager` 仍保留原始、规范的会话事实。

## Current LHA Anchors

当前代码结构说明，input slimming 应位于 provider wire format 之上、普通 turn
构建之后：

- `docs/slash-commands.md:38` 记录了 `/compact` 是摘要式上下文缩减能力。
- `docs/agent-runtime.md:99` 展示了 `SemanticConversationCompactor`，它是现有
  摘要/远端压缩路径的语义接口。
- `src/agent/cli/product/agent_runtime/src/compact.rs:42` 包含本地 compact 的
  summarization prompt 常量。
- `src/agent/cli/product/agent_runtime/src/compact_remote.rs:47` 为远端历史压缩
  构造 `TurnRequest`，随后替换历史。
- `src/agent/cli/product/agent_runtime/src/context_manager/history.rs:77` 通过
  `for_prompt()` 将持久历史规范化为模型侧历史快照。
- `src/agent/cli/product/agent_runtime/src/context_manager/history.rs:285` 已经在
  记录历史时应用工具输出截断；这与 request-time input slimming 是不同层次。
- `src/llm/src/semantic.rs:309` 定义 `TurnRequest`，也就是 provider-neutral 的
  模型请求边界。
- `src/agent/cli/product/agent_runtime/src/codex.rs:5863` 是普通 turn 构建
  `TurnRequest` 的位置；未来 compactor 应在这里之后、input-token 估算和 preflight
  检查之前运行。

## Current Headroom Anchors

Headroom 应作为设计模式来源，而不是直接移植实现。当前参考文件包括：

- `/Users/xuziqiang/Workspace/headroom/headroom/transforms/pipeline.py`
- `/Users/xuziqiang/Workspace/headroom/headroom/transforms/content_router.py`
- `/Users/xuziqiang/Workspace/headroom/headroom/transforms/compression_units.py`
- `/Users/xuziqiang/Workspace/headroom/crates/headroom-core/src/transforms/live_zone.rs`
- `/Users/xuziqiang/Workspace/headroom/crates/headroom-core/src/ccr/mod.rs`

核心结论是：Headroom 不是简单的历史总结。它选择 live content 中可变、可压缩的
片段，按内容类型路由，压缩后复核 token 节省，并把原文存入可检索的 CCR store。

## Existing `/compact` vs Input Slimming

| 能力 | `/compact` | Input Slimming |
| --- | --- | --- |
| 工作方式 | 生成摘要并替换历史 | 改写单次 `TurnRequest` clone |
| 作用时机 | 显式命令或 auto compact | 模型调用前的实验性 gate |
| 主要收益 | 长历史状态变短 | 大型工具输出 payload 变短 |
| 主要风险 | 摘要遗漏 | 压缩片段缺失细节 |
| 安全策略 | 保留选定 user messages、goal reminders、backfills | 安全区、可逆检索、token accept gate |
| 是否持久化 | 替换后的历史会持久化 | 默认不持久化压缩文本 |

两者应长期并存。`/compact` 是会话状态操作；input slimming 是请求整形优化。

## What Headroom Does

Headroom 的管线提供了五个适合迁移到 LHA 的思想。

### Cache-Aware Boundaries

Headroom 的 `CacheAligner` 会把动态内容从稳定 prompt 前缀中移走，以提高 provider
prompt cache 命中率。LHA 首版不需要实现 provider cache 优化，但应继承这个边界
意识：稳定指令、developer context、cache-hot context 默认都属于保护区。

### Content Routing

Headroom 的 `ContentRouter` 先识别内容类型，再选择策略，而不是对所有文本套同一套
截断规则。LHA 的工具输出也应采用同样原则：

- JSON 数组保留 schema、keys、短值、错误项、异常项和代表性行；
- 日志保留命令、退出状态、warning、error、stack trace 和尾部上下文；
- 搜索结果保留路径、行号和代表性命中；
- diff 保留文件头、hunk header 和关键增删行；
- plain text 在没有检索能力时只能做保守处理。

### Compression Units / Live Zones

Headroom 会从 provider-specific request 中抽取 `CompressionUnit`，只压缩 mutable
live-zone 文本。LHA 首版可以避免 provider raw JSON byte-range surgery，因为
`TurnRequest` 和 `TranscriptItem` 已经提供了语义层。compactor 应选择安全的
`TranscriptItem` payload，并且只改写 request clone。

### Token Accept Gate

Headroom 只有在模型感知的 token 统计显示 replacement 更短时才接受压缩结果。LHA
应优先使用 runtime 的 token estimator；不可用时，才回退到现有近似 token 计数。
如果 `tokens_after >= tokens_before`，必须保留原文。

### CCR: Compress-Cache-Retrieve

Headroom 会把原文存入本地 store，并在 prompt 中插入 retrieval marker。模型如果
需要完整内容，可以调用内部检索工具取回。这个机制是激进压缩仍能维持质量安全的
关键。

LHA 在没有等价检索路径前，不应启用激进的有损压缩。没有 retrieval 时，只允许
保守的结构保持型压缩。

### Fail-Open Behavior

Headroom 把压缩视为优化项。如果检测、路由、tokenizer、store 写入或 retrieval tool
注入失败，请求应原样发送。LHA 也应遵循同一规则：input slimming 不能阻断用户
turn。

## What LHA Should Migrate

LHA 应迁移架构原则，而不是 Headroom 的具体 provider adapter 代码：

- 先划定安全区，再选择候选片段；
- 首批只处理旧工具结果，不处理用户消息或 assistant reasoning；
- 按内容类型路由，而不是统一截断；
- 每个 replacement 都必须通过 token accept gate；
- 发出 marker 前，先把原文存入 retrieval store；
- 保持持久历史不变，只压缩 transient request；
- 记录足够指标，用于比较质量和 token 节省；
- 任意压缩失败都 fail open 到原始 request。

LHA 首版不应迁移 Headroom 的 provider raw JSON byte-range surgery。这个技术适合
必须保留 wire bytes 和 provider cache 字节稳定性的代理；LHA product runtime 首版
可以先在 provider 序列化前处理语义层 `TranscriptItem`。

## Proposed LHA Architecture

未来实现建议新增 product-runtime 私有模块：

```text
src/agent/cli/product/agent_runtime/src/input_slimming/
```

模块可暴露类似下面的 product-private 入口：

```rust
struct InputSlimmer;

struct InputSlimmingOutcome {
    request: TurnRequest,
    metrics: InputSlimmingMetrics,
    store_refs: Vec<InputSlimmingRef>,
}
```

这些名称只是设计候选，不是当前 public API。除非后续设计明确要稳定 SDK surface，
它们应保持在 `lha` product package 内部。

compactor 输入应包括：

- 作为源请求的 `&TurnRequest`；
- model 名称和 context-window metadata；
- 用于 before/after accounting 的 token estimator；
- 当前 turn metadata，用来识别 latest user turn 和受保护的 runtime reminders；
- retrieval 启用时可访问的 session-scoped input slimming store。

compactor 输出应包括：

- 压缩后的 `TurnRequest` clone；
- 每个 candidate 的 metrics 以及聚合 before/after token 估算；
- 已存入 retrieval store 的原文引用。

普通 turn 的插入点应在 `TurnRequest` 构建完成之后、
`estimated_input_tokens_for_turn_request` 之前。这样 preflight compact 判断和最终发送
给 provider 的 request 会基于同一个压缩后输入。

## Safety Zones

安全区是最重要的质量护栏。默认策略永远不压缩：

- `base_instructions`；
- `personality`；
- `output_schema`；
- tool descriptors；
- 当前用户输入；
- `system` 或 `developer` role 消息；
- active goal 和 proposed-plan path reminders；
- skill instructions；
- 现有 `/compact` summary messages；
- 已经包含 input slimming marker 的内容。

默认策略也应跳过以下内容，除非后续明确启用：

- assistant messages；
- reasoning items；
- hosted activity items；
- 短于配置阈值的内容。

首批候选应限制为旧工具结果：

- `ToolResultPayload::Text`；
- `ToolResultPayload::Structured.content`；
- 大型命令输出；
- `rg` 或 search results；
- JSON arrays；
- build/test logs；
- unified diffs。

## Compression Strategies

首批策略应是确定性、可解释的。ML-based compression 可以后续评估，但应等安全区、
retrieval 和观测面稳定之后再引入。

### `json_array_sample`

用于 JSON arrays，尤其是 object arrays。必须保留：

- object keys 和 shape；
- 短 scalar values；
- error-like rows；
- outliers 和 boundary rows；
- 头部和尾部样本；
- 中间代表性样本。

如果能减少 token 且不隐藏语义，常量字段可以提升到短摘要头中。

### `log_compact`

用于 build、test、command logs。必须保留：

- command identity；
- exit status；
- error lines；
- warning lines；
- stack traces；
- 用于 recency 的最后若干行。

重复行应聚合成计数，不重要的中段可以用 omission marker 表示。

### `search_result_compact`

用于 grep/ripgrep 风格输出。必须保留：

- file paths；
- line numbers；
- match text；
- 原输出已有的附近 context；
- 每个文件的代表性 hits。

大型结果集应按文件分组，让模型能决定是 retrieval 原文，还是执行更窄的搜索。

### `diff_compact`

用于 unified diffs。必须保留：

- file headers；
- hunk headers；
- changed-line counts；
- 关键 additions 和 deletions；
- 表示省略 hunks 的 markers。

首版对 source-code diffs 应保守处理。如果 diff 足够小，或 compactor 无法确信保留了
review-relevant 变化，就应保持原样。

### `plain_text_head_tail`

只作为 fallback。保留 headings、高熵 identifiers、开头和结尾。只要发生实质性省略，
这个策略就必须配合 retrieval marker；否则只能做小幅、保守缩减。

## CCR-Like Retrieval

只要 input slimming 产生有损 replacement，就应使用 session-scoped store。marker
格式候选为：

```text
<<lha-input:{hash}>>
```

hash 应稳定、确定、足够短。首版使用 workspace 已有的 `sha2::Sha256`，并截断为
24 个 lowercase hex characters，避免为了实验功能引入新依赖。后续如果需要与
Headroom 的 CCR hash 完全对齐，可以再评估切换到 BLAKE3。

store 默认值：

- scope：当前 LHA session；
- backend：首版使用 in-memory；
- capacity：1000 entries；
- TTL：300 seconds；
- value：原文，以及 strategy、tool name、原始 token count 等轻量 metadata。

内部 retrieval tool 名为：

```text
lha_input_retrieve
```

参数为：

```json
{
  "hash": "string",
  "query": "optional string"
}
```

retrieval 行为：

- 没有 `query` 时，返回原文；如果原文过大，则返回受 token budget 限制的 head/tail
  view；
- 有 `query` 时，返回匹配行或匹配 sections 及其上下文；
- store miss 时，返回包含 missing hash 的明确错误；
- 永远不伪造缺失内容。

如果 store 或 retrieval tool 不可用，compactor 必须跳过有损策略，或者 fail open 到
原始 request。

## Configuration And Rollout

input slimming 是实验能力，默认关闭。

首版 feature flag 候选名为：

```text
InputSlimming
```

首版实现应保持 product-private，不新增 public `lha-llm` API。首版也不应新增
`ConfigToml` 字段；如果后续实现决定添加配置字段，必须运行 `just write-config-schema`
同步生成 config schema。

已实现 Phase 2 的安全纵切；后续 rollout 建议：

1. Phase 3：引入更细的 content router 和 query-aware retrieval。
2. Phase 4：基于 telemetry 调整阈值和默认策略。

## Observability

该功能应产出 per-turn metrics，方便判断 input slimming 是否真的在不伤害质量的
前提下节省 token。

metrics 应包括：

- candidate count；
- compressed count；
- skipped count by reason；
- tokens before、tokens after、tokens saved；
- strategy distribution；
- retrieval marker count；
- retrieval tool-call count；
- retrieval miss count；
- fail-open count；
- per-turn latency overhead。

skip reasons 应使用稳定分类：

- `protected_role`
- `current_user_turn`
- `recent_assistant`
- `below_size_floor`
- `already_slimmed`
- `not_token_saving`
- `retrieval_unavailable`
- `structured_content_items`
- `failed_non_log_result`
- `unsupported_item`
- `compression_error`

这些指标应能对比 measure-only 估算和实际启用 replacement 后的真实效果。

## Public API And Type Impact

当前实现不改变 `lha-llm` public API，也不改变 provider protocol 的 public
contract。唯一 user-facing surface 是已有 `[features]` 表新增
`input_slimming` feature key，因此 `config.schema.json` 已同步更新。

以下名称已经作为 `lha` product-runtime 内部类型或 marker 存在，并保持
product-private：

- `InputSlimmer`
- `InputSlimmingOutcome`
- `InputSlimmingMetrics`
- `InputSlimmingStore`
- `InputSlimmingRef`
- `lha_input_retrieve`
- `<<lha-input:{hash}>>`

它们不应被视为 SDK 或 `lha-llm` 稳定 API。

## Test Matrix

当前实现覆盖以下测试场景：

- Safety zones：system messages、developer messages、current user input、active goal
  reminders 不被压缩。
- Tool text output：大型 `ToolResultPayload::Text` 被压缩，并包含 retrieval marker。
- Structured output：JSON arrays 保留 schema、errors、boundary rows 和 representative
  samples。
- Token gate：不节省 token 的 replacement 被拒绝。
- Marker detection：已有 `<<lha-input:...>>` marker 的内容不会重复压缩。
- Retrieval：`lha_input_retrieve(hash)` 能返回原文。
- Query retrieval：`lha_input_retrieve(hash, query)` 能返回相关 sections。
- Fail-open：compressor、tokenizer、store、tool-injection 出错时 request 保持原样。
- History preservation：`ContextManager` 和 rollout history 不持久化压缩文本。
- Preflight accounting：context-window 检查使用压缩后 request 的 token estimate。
- Telemetry：saved、skipped、fail-open、retrieval metrics 会被记录。

## Validation

实现变更后应运行：

```sh
just write-config-schema
just fmt
cargo test -p lha input_slimming --offline
cargo test -p lha tools::router --offline
cargo test -p lha features --offline
cargo test -p lha compact:: --offline
cargo test -p lha remote_compact --offline
cargo test -p lha goals --offline
cargo test -p lha --offline
just fix -p lha
git diff --check
```
