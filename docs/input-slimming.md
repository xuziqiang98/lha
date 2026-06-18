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

当前后续优先级如下：

1. 算法深度对齐 Headroom core compressors
2. 复用 Headroom parity fixtures 和 LHA built-in tool output eval 验证 token savings、
   needle retention 和 retrieval 可逆性
3. 继续保持 safety / retrieval / fail-open 护栏
4. 保证 resume 后 retrieval 原文仍可取回，避免恢复会话后行为不一致

## Implementation Status

Input Slimming 已有 product-private 实现，默认关闭。目前有两个互斥策略：

```toml
[features]
# historical tool-output slimming
input_slimming = true

# live-zone tool-output slimming
input_slimming_live_zone = true
```

不要同时启用两个 key。`/experimental` 会在开启其中一个策略时自动关闭另一个策略；
如果手写 config 同时开启，runtime 会发出 warning，并禁用本次 input slimming，避免
消融实验数据混淆。

当前实现范围：

- `input_slimming` 是 historical 策略：压缩 latest user message 之前的 historical
  tool results；latest user message 之后的 live-zone tool results 受 recent output
  protection window 保护，不在同 turn follow-up request 中立即压缩；
- `input_slimming_live_zone` 是 live-zone 策略：只压缩 latest user message 之后、
  会进入当前 provider live zone 的 tool results，用于保护旧缓存前缀并提高 provider
  cached input token 命中率；
- 跳过 user/system/developer/assistant/reasoning/hosted activity；
- 只压缩当前 request 内安全的 tool results，不改写当前用户输入、reasoning、
  hosted activity、tool call 或 assistant message；
- 支持 `ToolResultPayload::Structured { content_items: Some(...) }` 中的 text
  content items；image 等非文本 items 保持不变；
- 优先使用 runtime/model-aware request estimator 做 whole-request before/after
  token gate，不可用时回退到 `approx_token_count` 并记录 fallback metrics；
- deterministic ContentRouter 已拆成 JSON、log、search、diff、plain text 策略；
- query retrieval 支持 path-aware、section-aware 和 line-context fallback；
- 支持 product-private measure-only 模式和 context-pressure adaptive policy；
- historical 策略在已有 marker 或本次接受压缩后注入 `lha_input_retrieve`；
  live-zone 策略从本轮首个 request 起广告该工具，保持 same-turn follow-up 的
  tool schema 稳定；
- 原文存入 session-scoped in-memory store，默认 capacity 为 1000，TTL 为 300
  seconds；
- accepted replacements 会把原文和 metadata 写入 rollout sidecar item，用于 resume
  后恢复 session store；压缩后的 request clone 仍不写入 transcript history；
- auto compact 同时维护 raw compaction pressure：普通模型请求可以用 slimmed request
  降低 send pressure，但触发 compact 时会估算原始 `history.for_compaction_prompt()`，
  避免持久化 raw history 被 slimming 长期遮蔽；
- session-scoped replacement cache 会按原文 hash、tool、scope、zone、策略版本和压缩
  参数复用已生成的 slimmed replacement，避免跨 turn 对同一 tool output 重复执行压缩策略；
- Sidebar `slim hist` / `slim live` 表示本次 savings 来自 historical 或 live-zone
  策略；`saved xxxK` 表示当前 session 中已被 slimmed context 采用过的
  tool-result occurrence 去重累计 saved，不是 auto compact 的触发依据；同一
  occurrence 跨 turn 复用 replacement 不重复累计，compact 后也不重置该 session
  统计；
- 如果当前模型卡片带有 standard API pricing，且本轮 provider 返回了实际
  `TokenUsage`，Sidebar 会在 Completed 后补充显示 `saved xxxK / $yyy`；美元金额基于
  actual usage 加上“未瘦身 input tokens”的反事实 usage 估算 avoided standard API cost，
  不是保证的 invoice delta。没有 pricing、没有 actual usage、或找不到匹配 pricing band
  时保留 token-only 的 `saved xxxK`；
- marker 格式为 `<<lha-input:{hash}>>`；
- hash 使用 repo 已有的 `sha2::Sha256`，截断为 24 个 lowercase hex characters。

## Historical vs Live-Zone Strategy

两种策略都工作在 semantic `TurnRequest` 层，而不是 provider wire JSON 字节层；两者
都会保留持久 transcript history 原文，并通过 `lha_input_retrieve` 恢复原始工具输出。

| 策略 | config key | 候选内容 | 主要目标 |
| --- | --- | --- | --- |
| Historical tool outputs | `input_slimming` | latest user message 之前的旧 tool results | 降低长历史工具输出的 token 压力 |
| Live-zone tool outputs | `input_slimming_live_zone` | latest user message 之后的当前 tool results | 保护旧缓存前缀，提高 cached input token 命中 |

Live-zone v1 只压缩工具输出，不压缩 latest user text。用户原始请求优先保真，即使它也
位于 provider live zone 内。

启用任一策略并产生 marker 后，LHA 会向 `tools` 列表注入 `lha_input_retrieve`。这会
改变 provider 看到的 tool schema 列表，是“更强恢复能力”和“更高 prefix cache 命中”
之间的取舍；实验分析时应单独记录 retrieve tool 注入和实际 retrieval call 次数。

## Wire API Live-Zone Boundaries

普通模型 turn 目前走三类 wire API；`Compact` / `responses/compact` 是独立路径，不纳入
live-zone slimming。

- Responses：只考虑 latest user message 之后的 `ToolResult`。结构化结果会序列化成
  `function_call_output.output`，custom text 结果会序列化成
  `custom_tool_call_output.output`；content items 只压 text，image 保持不变。live
  candidate 之前的 `input` items 必须保持 provider JSON value 相等。
- Chat Completions：只考虑 latest user message 之后序列化为 `role: "tool"` 的
  messages。system、developer/system-downgraded、历史 user/assistant/tool，以及当前
  assistant `tool_calls` message 必须保持不变。
- Anthropic Messages：只考虑 latest user message 之后序列化为 latest user/tool-result
  block 的 `ToolResult` textual content。system prompt、历史 messages 和 preceding
  assistant `tool_use` blocks 必须保持不变。

推荐消融实验同时记录：

- LHA 本地 `tokens_saved` / replacement count；
- provider 返回的 cached input tokens / cache read tokens；
- `lha_input_retrieve` 注入次数、实际 retrieval call 次数和额外 latency；
- 任务恢复质量：模型是否能在需要时通过 query retrieve 找回被省略细节；
- cache 收益与 retrieval 成本的净效果，尤其是“压缩后仍总是需要全文”的反例。

## Formal Problem Definition

更抽象地说，input slimming 是在任务效用保持约束下，最小化 agent 输入成本的
上下文表示压缩问题。对多步 agent 来说，input slimming 不是一次性把初始上下文
压成 `Z`，而是一种插入 agent loop 的 request-time context transformation policy。

设 `tau` 是当前任务，`X_tau` 是该任务的初始可观测上下文，`H_t` 是第 `t` 步前的
agent 历史或状态。runtime 先从历史构建原始模型请求，然后 input slimming 在模型调用前
改写这个 request：

```text
R_t = B(H_t)
\tilde{R}_t = C_t(R_t, tau)
```

其中 `B` 是 build-request 过程，`R_t` 是第 `t` 步原始模型请求，`C_t` 是第 `t` 步
request-time slimming policy，`\tilde{R}_t` 是实际发送给模型的 slimmed request。
`C_t` 可以隐式依赖之前的压缩决策、retrieval store、replacement cache 或预算状态；这些状态
属于实现细节，不进入主公式。

用 `A_C` 表示每一步模型请求前都应用 `C_t` 的 agent 系统，用 `A_0` 表示 no-slimming
baseline，则压缩轨迹和 baseline 轨迹可以写成：

```text
pi_C = A_C(X_tau)
pi_0 = A_0(X_tau)
```

其中 `pi_C` 是使用 input slimming 后的完整 agent 轨迹，`pi_0` 是不使用 input slimming 的
baseline 轨迹。轨迹包含模型 action、工具/环境反馈和最终输出，但这些细节不需要在主公式里
展开。

主优化目标因此应写成整条轨迹的期望成本最小化：

```text
C* = argmin_C E_{tau ~ D}[ CostTrajectory(pi_C) ]

subject to

E_{tau ~ D}[ U_tau(pi_C) ]
  >=
E_{tau ~ D}[ U_tau(pi_0) ] - epsilon
```

这里 `D` 是任务分布，`CostTrajectory(pi_C)` 是整条轨迹的有效输入成本。最简单时，
它可以近似为每一步 slimmed request 的 total input tokens；在 cache-sensitive setting
下，它应区分 cached input tokens 和 uncached input tokens：

```text
CostTrajectory(pi_C)
  =
sum_t EffectiveInputCost(\tilde{R}_t)
+ optional_overhead(pi_C)

EffectiveInputCost(\tilde{R}_t)
  =
uncached_tokens_t
+ alpha * cached_tokens_t
```

其中 `uncached_tokens_t` 是第 `t` 步没有命中 provider cache、需要重新处理的输入 token；
`cached_tokens_t` 是命中 provider cache 的输入 token；`alpha` 是 cached token 相对
uncached token 的成本权重。`alpha = 1` 时退化为普通上下文 token 压力；`alpha < 1`
时表达 provider cache 命中带来的成本折扣。

`optional_overhead(pi_C)` 可用于实验中纳入 retrieve tool schema、retrieve call、取回内容、
压缩计算、latency 等额外成本。cache hit rate 可以作为 live-zone 策略的诊断指标或可选
guardrail，但不替代 `CostTrajectory`，因为命中率是比例，不能单独反映总成本。`U_tau`
是任务效用函数，而不是文本相似度；`epsilon` 是允许的平均效用损失上界。

我们不要求 `pi_C = pi_0`，也不要求模型走相同路径、调用相同工具或输出相同字符串；只要求
压缩轨迹的任务效用接近 no-slimming baseline。

单次模型调用可以看成上述定义的退化情况：当 agent 只有一步时，`pi_C` 近似退化为
`F(C(X, tau))`。对 LHA 这类多步 agent，主定义应使用 `pi_C` 和 `CostTrajectory`。

因此 input slimming 不是普通文本压缩：文本相似度不是目标，agent 轨迹不要求相同，最终输出路径
也不要求相同；它只要求最终任务效用近似保持。对某些任务来说，一行关键证据可能比上千行冗余
日志更重要。

这个问题也不宜严格称为凸优化。`C` 通常是离散策略，`A_C` 是黑盒、随机、不可微的序贯决策过程，
`U` 也常常来自测试结果、人评或 judge。更准确的表述是 constrained black-box optimization，或者
task-conditioned rate-distortion problem：

```text
Input slimming is the problem of designing a request-time context transformation
policy C that minimizes the expected cost of an agent trajectory while preserving
expected task utility.
```

换句话说，input slimming 是设计一种 request-time 上下文变换策略 `C` 的问题；该策略在 agent 每一步
模型请求前作用，目标是在保持预期任务效用的同时，最小化整条 agent 轨迹的期望输入成本。

当前 LHA 的 historical strategy 和 live-zone strategy 是这个多步抽象问题在 LHA request 边界上的两个
工程近似。protected context、retrieve side channel、whole-request token gate 和 fail-open 都是为了约束
`U_tau(pi_C)` 相对 `U_tau(pi_0)` 的损失；sidebar saved tokens 和 token gates 是 `CostTrajectory` 中 token
成本的一部分可观测 proxy，而不是完整目标函数。后续算法和消融实验应围绕 `CostTrajectory`、`U`、
`epsilon`、retrieval overhead 和 cache effect 展开。

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

Headroom 应作为策略和 eval 设计模式来源，而不是直接移植实现。当前参考文件包括：

- `/Users/xuziqiang/Workspace/headroom/headroom/transforms/content_router.py`
- `/Users/xuziqiang/Workspace/headroom/headroom/transforms/compression_units.py`
- `/Users/xuziqiang/Workspace/headroom/headroom/transforms/smart_crusher.py`
- `/Users/xuziqiang/Workspace/headroom/headroom/transforms/log_compressor.py`
- `/Users/xuziqiang/Workspace/headroom/headroom/transforms/search_compressor.py`
- `/Users/xuziqiang/Workspace/headroom/headroom/transforms/diff_compressor.py`
- `/Users/xuziqiang/Workspace/headroom/headroom/transforms/code_compressor.py`
- `/Users/xuziqiang/Workspace/headroom/headroom/evals/README.md`
- `/Users/xuziqiang/Workspace/headroom/headroom/evals/runners/compression_only.py`
- `/Users/xuziqiang/Workspace/headroom/benchmarks/ccr_regression_benchmark.py`
- `/Users/xuziqiang/Workspace/headroom/benchmarks/adversarial_ccr_tests.py`
- `/Users/xuziqiang/Workspace/headroom/tests/parity/fixtures/`

核心结论是：Headroom 是策略和 fixture 参考，不是 LHA 的产品形态参考。LHA 只迁移
内容类型路由、压缩后 token 复核、原文可检索恢复等核心压缩原则。

## Existing `/compact` vs Input Slimming

| 能力 | `/compact` | Input Slimming |
| --- | --- | --- |
| 工作方式 | 生成摘要并替换历史 | 改写单次 `TurnRequest` clone |
| 作用时机 | 显式命令或 auto compact | 模型调用前的实验性 gate |
| 主要收益 | 长历史状态变短 | 大型工具输出 payload 变短 |
| 主要风险 | 摘要遗漏 | 压缩片段缺失细节 |
| 安全策略 | 保留选定 user messages、goal reminders、backfills | 安全区、可逆检索、token accept gate |
| 是否持久化 | 替换后的历史会持久化 | 默认不持久化压缩文本 |

两者应长期并存。`/compact` 是会话状态操作；input slimming 是请求整形优化。compact
请求必须使用原始 persistent history，不应用 input slimming，不注入
`lha_input_retrieve`，也不把 `<<lha-input:...>>` replacement 作为摘要输入。
auto compact 的触发会同时看两个 pressure：

- send pressure：普通模型请求真正发送前的 slimmed `TurnRequest` token estimate；
- raw compaction pressure：如果现在 compact，compact 模型会收到的原始 compact prompt
  token estimate。

因此 input slimming 可以降低普通请求大小，但不能无限推迟 raw history 的 compact。remote
compact 与 local compact 一样，在 raw compact prompt 已经超出 context window 时会 trim
最老 history item 后重试，以避免完全无法压缩。

## What Headroom Does

Headroom 的管线提供了五个适合迁移到 LHA 的思想。

### Safety Boundaries

Headroom 会区分稳定上下文和可变压缩内容。LHA 在 semantic `TurnRequest` 层实现这个
边界：historical 策略优先降低旧工具输出 token 压力，live-zone 策略优先保护旧缓存
前缀。稳定指令、developer context、当前用户输入和 runtime reminders 默认都属于保护区。

### Content Routing

Headroom 的 `ContentRouter` 先识别内容类型，再选择策略，而不是对所有文本套同一套
截断规则。LHA 的工具输出也应采用同样原则：

- JSON 数组保留 schema、keys、短值、错误项、异常项和代表性行；
- 日志保留命令、退出状态、warning、error、stack trace 和尾部上下文；
- 搜索结果保留路径、行号和代表性命中；
- diff 保留文件头、hunk header 和关键增删行；
- plain text 在没有检索能力时只能做保守处理。

### Compression Units / Live Zones

Headroom 会抽取 `CompressionUnit`，只压缩 mutable live-zone 文本。LHA 的
`TurnRequest` 和 `TranscriptItem` 已经提供了语义层，compactor 应选择安全的
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
- 只处理工具结果；historical 与 live-zone scope 决定处理旧工具输出还是当前
  live-zone 工具输出，不处理用户消息或 assistant reasoning；
- 按内容类型路由，而不是统一截断；
- 每个 replacement 都必须通过 token accept gate；
- 发出 marker 前，先把原文存入 retrieval store；
- 保持持久历史不变，只压缩 transient request；
- 记录足够指标，用于比较质量和 token 节省；
- 任意压缩失败都 fail open 到原始 request。

LHA product runtime 在 provider 序列化前处理语义层 `TranscriptItem`，不把
Headroom 的 provider 代理实现作为本设计的后续范围。

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
`estimated_input_tokens_for_turn_request` 之前。这样普通请求的 send pressure 和最终发送
给 provider 的 request 会基于同一个压缩后输入；auto compact 还会单独估算 raw
compaction pressure，确保 compact 模型使用完整原始上下文。

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

候选应限制为工具结果：

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

input slimming 是实验能力，默认关闭。当前 feature flags 为
`Feature::InputSlimming`（historical）和 `Feature::InputSlimmingLiveZone`
（live-zone），二者互斥。实现保持 product-private，不新增 public `lha-llm` API。
新增或调整 feature key 后必须运行 `just write-config-schema` 同步生成 config schema。

已实现 Headroom 核心机制迁移的主要 product-private 纵切：historical 和 live-zone
tool-output scopes、model-aware token gate、provider-visible structured content items、
deterministic ContentRouter、CCR-like retrieval metadata/query、measure-only hooks、
adaptive policy 和 focused eval tests。功能仍为 experimental 且默认关闭；是否默认开启
需要更多真实会话 telemetry。

## Algorithm Parity Priorities

下一阶段优先补算法深度，而不是追 Headroom 的代理、包装器或跨 agent 产品外壳。
实现仍应保持 product-private，并继续沿用现有 safety、retrieval、token gate 和
fail-open 护栏。

1. `SmartCrusher` parity for JSON arrays
   - 对齐 schema-preserving row selection、dedupe、rare/error/anomaly/change-point
     preservation。
   - 优先复用 Headroom parity fixtures 和 CCR needle tests。
   - LHA 不需要在首阶段迁移 TOIN learning，但 selection quality 应尽量接近
     Headroom core compressor。
   - 已实现 deterministic first slice：row scoring、selection reasons、rare/error/
     numeric-outlier/change-point preservation 和 duplicate representative selection。

2. `LogCompressor` parity
   - 对齐 stack trace state machine、pytest/cargo/npm/jest/make format detection、
     warning dedupe 和 summary preservation。
   - 重点覆盖 build/test logs；这是 LHA 长会话里最常见的高 token 工具输出。
   - 已实现 deterministic first slice：format detection、Python/Rust/JS stack trace
     preservation、summary/tail preservation 和 conservative warning dedupe。

3. `SearchCompressor` parity
   - 对齐 robust path parser、Windows path、dash filename、per-file and global
     ranking。
   - 使用 query/context keywords 提升与用户当前意图相关的命中保留率。
   - 已实现 deterministic first slice：Unix rg、rg context、Windows path、dash
     filename、optional column parser 和 per-file/global ranking。

4. `DiffCompressor` parity
   - 对齐 hunk/file-level preservation，优先保留 file headers、hunk headers 和
     关键 changed lines。
   - 保持 binary patch 和 unsafe diff 的保守 skip 行为。
   - 已实现 deterministic first slice：file/hunk-aware parsing、critical changed-line
     preservation、stable omitted markers，以及 binary/malformed diff skip。

5. `Plain text / Kompress`
   - 不优先接入 ML compressor。
   - LHA 先继续使用 deterministic conservative text strategy；只有 benchmark 证明
     token savings 和 answer quality 都有明确收益后，再考虑 ML/embedding 类策略。

6. `CodeCompressor`
   - 不作为第一优先级。
   - LHA tool results 中代码大多来自 search/diff/read 输出，先通过
     search/diff/log/JSON 覆盖主要场景。

## Remaining Headroom Core Migration Tasks

以下待办只覆盖 Headroom 核心压缩机制迁移。

### Live-Zone Boundaries

已实现：

- historical 策略把 `idx < latest_user_index` 的 tool results 标记为 historical
  candidates。
- historical 策略中 `idx > latest_user_index` 的 tool results 受 recent output
  protection window 保护，默认记录 `recent_assistant` skip，不进入压缩候选。
- live-zone 策略只把 `idx > latest_user_index` 的 tool results 标记为 live
  candidates，用于 same-turn follow-up request 压缩。
- latest user message、assistant/reasoning/hosted activity/tool call 等非 tool-result
  items 保持保护。
- 只改写 `TurnRequest` clone，rollout 和 `ContextManager` history 仍保留原文。

仍需后续评估：

- configurable N / high-pressure override：historical 策略当前默认保护 latest user
  message 之后的全部 live-zone tool results；后续如 telemetry 证明有必要，再评估在高
  context pressure 下放开部分 live output，或直接使用独立 live-zone 策略做消融。

### Model-Aware Token Gate

已实现：

- `codex.rs` 调用 `slim_request_with_context`，传入 runtime request-level estimator。
- gate 对当前 request 和 trial request 做 before/after 比较，marker 与 retrieval
  instruction 已计入 trial request。
- estimator 不可用时回退到近似文本 gate，并记录
  `lha.input_slimming.token_gate_fallback`。
- estimator 判定不省 token 时跳过 replacement。

后续 benchmark 可用 LHA recorded tool output 校准 estimator 和近似 fallback 的偏差。

### ContentRouter Parity

已实现：

- 策略拆分为 `strategy/json.rs`、`log.rs`、`search.rs`、`diff.rs` 和
  `plain.rs`。
- JSON 保留 schema summary、头尾样本、error-like rows、rare keys、rare scalar
  values、numeric outliers、change points、duplicate representatives 和中间代表样本。
- log 保留头部、重要 error/warning/failure/panic/traceback/exit/test lines、上下文、
  tail，并做 format detection、stack-trace state tracking 和 warning dedupe。
- search 按 path 分组，支持 Windows path、dash filename、rg context separators 和
  optional columns，限制每文件和总文件数，并记录 omitted counts。
- diff 使用 file/hunk-aware parsing，保留 file headers、hunk headers、首尾和关键
  changed lines、omitted markers，跳过 binary/malformed patches。
- per-strategy metrics 记录 before/after/saved/ratio，并带 strategy、tool、zone、
  gate method labels。

### Structured Tool Output

已实现：

- `content_items` 中只有 `InputText` 可压缩；`InputImage` 等非文本 item 不改写。
- all-text items 会同步更新 `content` 为压缩后 text join。
- mixed text/image 只改 provider-visible text item；`content` 仅在原本等于旧 text
  join 时同步更新。
- image-only structured output 记录 `structured_content_items` skip。
- Responses 和 Messages serialization tests 证明 provider-visible payload 包含
  slimming marker。

### CCR-Like Retrieval

已实现：

- store metadata 包含 strategy、tool name、original tokens、compressed tokens、
  created turn id。
- `retrieve()` 命中时递增 `retrieval_count`。
- query retrieval 先做 path-aware 匹配，再做 markdown section 匹配，最后回退到
  line contains + 上下文行。
- TTL/LRU miss 返回包含 missing hash 的明确错误，不伪造内容。
- retrieval metrics 记录 hit/miss/query matched，并带 strategy/tool labels。
- Resume-safe retrieval：resume 已包含 input-slimming marker 的 thread 时，对应原文
  仍可通过 rollout sidecar entry 恢复到当前 session store；如果原文无法恢复，miss
  文案会明确说明可能是旧 marker、TTL 过期或缺少 rollout entry。

实现边界：

- LHA 使用 rollout sidecar entries 恢复当前 thread 的 input-slimming store，而不是把
  compressed prompt clone 持久化为历史。
- resume/fork hydration 会刷新当前进程内 store 的 TTL；长期 durable、多会话共享
  store 不在当前范围。

### Safety Protection

已实现：

- 保护 current user、system/developer-like messages、assistant/reasoning/hosted
  activity、summary/proposed-plan/active-goal/skill instruction markers 和 already
  slimmed markers。

### Adaptive Policy

已实现：

- `input_slimming_options_for_context` 按 context pressure 调整阈值。
- live-zone candidates 比 historical candidates 更保守。
- shell/build/test、search、diff/apply_patch 等工具名有 product-private policy bias。
- measure-only 模式会收集 candidate/gate/savings metrics，但不替换、不存储、不注入
  retrieval tool。
- recent output protection window 只属于 historical 策略；live-zone 策略会压缩同
  turn 当前 tool outputs，但仍跳过已有 marker、retrieve 输出、非文本 image blocks 和
  不节省 token 的 replacement。

### Observability And Eval

已实现：

- metrics 覆盖 candidate/skipped/slimmed/measured-only/token-gate fallback/per-strategy
  before-after-saved-ratio/retrieval hit-miss-query/fail-open/latency。
- focused tests 覆盖错误日志、搜索结果、JSON、diff、structured provider-visible
  payload、retrieval omitted needle、high-entropy/base64-like text。
- compression-only eval adapter 覆盖 JSON/log/search/diff/plain text 的 slimming、
  marker、token savings 和 retrieval recovery。

仍需后续评估：

- Headroom parity fixtures、CCR regression/adversarial tests 和 LHA built-in tool
  output eval 的系统性覆盖。

## Benchmark Reuse Plan

Headroom benchmark/eval 可以作为 fixture 和评测方法来源，但 LHA 应通过自己的
`InputSlimmer` adapter 被测。

复用顺序：

1. Zero-cost compression-only tests。
2. Headroom parity fixtures。
3. CCR regression/adversarial tests。
4. Built-in tool output before/after eval。

LHA benchmark adapter 应把 `InputSlimmer` 包成可独立调用的 target：

- input：synthetic 或 recorded tool output text；
- output：compressed replacement text plus retrieval store；
- metrics：tokens before/after/saved、marker presence、retrievability、needle
  retention。

当前实现位于
`src/agent/cli/product/agent_runtime/src/input_slimming/bench_eval.rs`，作为
`#[cfg(test)]` 的 deterministic compression-only harness。Headroom fixtures 当前作为
场景来源被转写成小型 inline Rust fixtures；不 vendoring
`/Users/xuziqiang/Workspace/headroom/tests/parity/fixtures/` 到 LHA repo。

首批 acceptance criteria：

- JSON/log/search/diff fixtures 不低于当前 LHA 的 safety retention。
- 所有 compressed outputs 必须能通过 `lha_input_retrieve` 找回原文。
- 所有 lossful replacements 必须通过 token gate。
- Built-in tool output eval 应记录 token savings、needle retention、retrieval
  recovery；如包含 answer-quality 检查，应只覆盖 LHA 真实工具输出场景。

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
- Current live-zone tool outputs：当前 request 内安全的大工具输出可被压缩。
- Structured content items：provider-visible structured payload 被安全压缩或明确
  跳过。
- Adaptive policy：不同 context pressure 下采用不同压缩阈值。
- Quality eval：错误日志、搜索结果、JSON needle、diff 和 omitted needle retrieval
  有回归覆盖。
- Fail-open：compressor、tokenizer、store、tool-injection 出错时 request 保持原样。
- History preservation：`ContextManager` 和 rollout history 不持久化压缩文本。
- Preflight accounting：普通请求的 context-window 检查使用压缩后 request 的 token
  estimate；auto compact 另行检查 raw compaction pressure。
- Telemetry：saved、skipped、fail-open、retrieval metrics 会被记录。
- Resume-safe retrieval：resume 后已有 `<<lha-input:...>>` marker 的原文可通过
  sidecar-hydrated store 取回；旧 marker 无 sidecar 时返回明确 miss，不阻断请求。
- Historical recent output protection：historical 策略中 latest user message 之后的
  live-zone tool results 同 turn 保留原文。
- Live-zone scope：live-zone 策略中 latest user message 之后的当前 tool results 可在
  same-turn follow-up request 中压缩，同时旧 prefix 保持 provider JSON value 相等。

待补测试场景：

- Headroom parity fixtures 的系统性回归覆盖。
- LHA built-in tool output before/after eval。

## Validation

文档-only 变更至少运行：

```sh
git diff --check
```

如果后续实现上述待办，再按实现影响范围运行下方测试。

实现变更后应运行：

```sh
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

本设计不改变 `ConfigToml` 或 nested config types，因此不需要运行
`just write-config-schema`。
