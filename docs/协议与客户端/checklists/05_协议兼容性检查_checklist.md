# 协议兼容性检查 Checklist

## 目标

为 packages/proto、服务本地 proto、生成代码和 mock-client 建立自动兼容检查，阻止字段号复用、破坏性类型变化和生成物漂移。首期不改变现有传输协议。

## 基础原则

- [x] 已发布字段号和 enum 数值不可复用。
- [x] 删除字段必须 reserved，新增字段默认可选兼容。
- [x] 共享协议优先收敛到 packages/proto，本地 proto 必须明确所有者。
- [x] 检查结果可在 Windows 本地与 CI 一致运行。

## 阶段 1：协议清单与基线

- 开始时间：2026-07-16 15:50:30 +08:00
- 结束时间：2026-07-16 15:59:59 +08:00
- 开发总结：新增共享协议清单、确定性语义基线和基线治理规则；所有权、消费者和未收敛的手写映射均已登记。
- 验证记录：`node tools/proto-compatibility-baseline.js --check` 通过；`node tools/run-node-tests.js tests/proto/proto-compatibility-baseline.test.mjs` 4/4 通过。

- [x] 盘点共享 proto、本地 proto、生成语言、生成脚本和消费者。（审核：`packages/proto/compatibility/inventory.json` 覆盖 4 个共享 proto、Rust/Node 消费者和生成脚本；基线测试通过。）
- [x] 标注客户端协议、内部控制协议和服务间协议。（审核：`inventory.json` 的 `classification` 与 `transports` 区分 client_gameplay、internal_control 和 service_to_service。）
- [x] 建立当前 descriptor set 或等价兼容基线。（审核：`packages/proto/compatibility/baseline.json` 固化 message/field/enum/reserved/oneof/RPC 语义，`--check` 输出 digest `sha256:2c20a25476bb20588cd698b131ef6af5d1ebe2080fd5dd93a1a6e60833bdc3ce`。）
- [x] 记录尚未收敛到共享包的重复定义。（审核：`inventory.json` 的 `unconsolidatedDefinitions` 登记手写消息号、mock-client codec 及其漂移检查器；本地 proto 扫描测试通过。）
- [x] 明确基线更新的授权与审核流程。（审核：`docs/协议与客户端/协议兼容性基线治理.md` 定义 owner 审核、破坏性变更批准、`--reason` 与 `--approved-by` 门槛。）

## 阶段 2：确定性生成

- 开始时间：2026-07-16 16:02:28 +08:00
- 结束时间：2026-07-16 16:42:45 +08:00
- 开发总结：四个 Rust proto 生成入口改用锁定的 vendored protoc；新增统一生成/漂移检查命令，并明确 Node/mock-client 为手写 codec 检查对象。
- 验证记录：`npm run check:generated-proto` 通过；`node tools/run-node-tests.js tests/proto/proto-generation.test.mjs tests/proto/proto-compatibility-baseline.test.mjs` 7/7 通过；`node --check tools/proto-generate.js` 和 `git diff --check` 通过。

- [x] 固定 protoc、插件和关键依赖版本。（审核：四个生成服务的 `Cargo.toml` 精确锁定 `protoc-bin-vendored=3.2.0`、`prost-build=0.13.5` 和适用的 `tonic-build=0.12.3`，相邻 Cargo.lock 仅新增 vendored protoc 条目。）
- [x] 提供统一生成命令，覆盖 Rust、Node.js 和 mock-client 消费物。（审核：`package.json` 新增 `generate:proto` / `check:generated-proto`，`tools/proto-generate.js` 覆盖四个 Rust 目标并运行手写 mock-client codec 检查。）
- [x] 相同输入生成相同输出，避免时间戳和路径噪声。（审核：`check:generated-proto` 在干净临时目录再生并逐字比较，2026-07-16 复跑通过。）
- [x] 检测 proto 与已提交生成代码不一致。（审核：`tools/proto-generate.js` 检测缺失、过期和内容不同的 `myserver.*.rs`，`tests/proto/proto-generation.test.mjs` 覆盖该行为。）
- [x] 生成失败输出缺少工具、版本和目标文件信息。（审核：四个 `build.rs` 与生成脚本输出 protoc/prost/tonic 版本、输入 proto、输出目标；定向测试验证失败文本。）

## 阶段 3：破坏性变更规则

- 开始时间：2026-07-16 16:45:06 +08:00
- 结束时间：2026-07-16 17:01:04 +08:00
- 开发总结：新增独立已发布协议参考、破坏性比较器和严格受控豁免；候选基线更新不能掩盖已发布协议的破坏。
- 验证记录：`npm run check:proto-breaking` 通过；`node tools/run-node-tests.js tests/proto/proto-breaking-changes.test.mjs tests/proto/proto-compatibility-baseline.test.mjs tests/proto/proto-generation.test.mjs` 12/12 通过；`node --check tools/check-proto-breaking-changes.js` 和 `git diff --check` 通过。

- [x] 检测字段号复用、字段类型不兼容和 oneof 破坏。（审核：`tools/check-proto-breaking-changes.js` 实现 `FIELD_NUMBER_REUSED`、type/label/oneof 规则；故意破坏 fixture 测试覆盖字段和 oneof 删除。）
- [x] 检测 enum 数值复用、RPC 删除和请求/响应类型变化。（审核：比较器输出 enum reuse/delete 与 RPC delete/type/stream 规则，`proto-breaking-changes.test.mjs` 覆盖。）
- [x] 检测删除字段未 reserved。（审核：同 message 未保留字段号触发 `FIELD_REMOVED_NOT_RESERVED`；同 message `reserved` 的对照样例通过。）
- [x] 区分客户端协议与可同步部署的内部协议风险等级。（审核：读取 `inventory.json` 的 classification，输出 `PLAYER_CLIENT` 或 `COORDINATED_INTERNAL` 及部署指引；定向测试覆盖两个等级。）
- [x] 支持受控豁免文件，要求原因、责任人和到期条件。（审核：`packages/proto/compatibility/breaking-exemptions.json` 与比较器校验精确 target、reason、owner、expiresAt；过期、无效、歧义和未命中均失败。）

## 阶段 4：消息类型与路由一致性

- 开始时间：2026-07-16 17:03:02 +08:00
- 结束时间：2026-07-16 17:24:19 +08:00
- 开发总结：新增静态路由一致性检查，覆盖 canonical message type、三类 game-server dispatch、mock-client、匹配 RPC、错误码目录和本地 proto ownership。
- 验证记录：`npm run check:proto-routing` 通过（113 message type、31/10/16 dispatch、174 send、8 RPC、150 error code，0 diagnostics）；`node tools/run-node-tests.js tests/proto/protocol-routing-consistency.test.mjs` 8/8 通过；mock-client 检查、语法与 diff 检查通过。

- [x] 校验 game-server message type 映射无重复、无遗漏。（审核：`check-protocol-routing-consistency.js` 比较 canonical `MessageType` 与 `from_u16`，审计 player/internal/admin dispatch；故意 duplicate、未知和缺失路由样例覆盖。）
- [x] 校验 mock-client 常量、编码解码和服务端路由一致。（审核：检查 174 个实际 send、encoder、56 decoder case 与玩家路由；chat/internal 边界由 metadata 明确排除。）
- [x] 校验协议错误码与实现使用情况。（审核：报告 57 个 proto `error_code` 字段、150 个静态目录及实现 literal，undefined/unused 均为 0；动态传播来源显式登记。）
- [x] 检测孤立消息和无消费者 RPC。（审核：无 producer 的 outbound message 必须出现在显式 deferred metadata；8 个 match RPC 均有 trait implementation，配置的 4 个 internal client 调用均存在。）
- [x] 报告重复本地 proto 与共享 proto 的漂移。（审核：复用阶段 1 `validateInventory()` 扫描，`check:proto-routing` 输出 shared proto ownership 无漂移。）

## 阶段 5：兼容测试夹具

- 开始时间：2026-07-16 17:26:11 +08:00
- 结束时间：2026-07-16 17:42:20 +08:00
- 开发总结：新增六个可读 manifest 驱动的 protobuf binary fixture、历史 v1 投影与敏感数据门禁，覆盖旧包读取、未知字段和边界值。
- 验证记录：`npm run check:proto-fixtures` 通过（6 fixture）；`node tools/run-node-tests.js tests/proto/proto-compatibility-fixtures.test.mjs` 4/4 通过；新工具语法与 diff 检查通过。

- [x] 保存关键消息的二进制 golden fixture 和可读来源。（审核：`tests/proto/fixtures/compatibility/` 提交 6 个 `.bin`、`manifest.json`、README 和 generator；manifest 记录消息、字段、预期、长度与 SHA-256，根 `.gitattributes` 将 `.bin` 固定为 binary 防止 CRLF 漂移。）
- [x] 验证新代码可读取旧 fixture。（审核：当前 `tools/mock-client/src/messages.js` 通过 `proto-compatibility-fixtures.test.mjs` 解码所有 6 个固定历史 body。）
- [x] 验证旧基线可忽略新版本未知字段。（审核：独立 `legacy-movement-snapshot-v1.mjs` 只读取 fields 1-5；future fixture 加入 unknown enum 和 field 190 后旧投影保持一致。）
- [x] 覆盖边界整数、空字段、未知 enum 和大 payload。（审核：覆盖 int32 min/max、Number.MAX_SAFE_INTEGER int64、u32 max、空 string/repeated、enum 99/77 和 64 KiB payload；总 fixture body 65,938 bytes。）
- [x] fixture 不包含真实账号、ticket 或敏感数据。（审核：fixture checker 校验 hash/清单并拒绝 JWT、Bearer、email、私钥、ticket-like 值及非 `fixture_`/`fake_` 身份字段；安全样例测试通过。）

## 阶段 6：版本协商与发布边界

- 开始时间：2026-07-16 17:46:52 +08:00
- 结束时间：2026-07-16 18:23:09 +08:00
- 开发总结：在不变更 TCP header 的前提下新增 Auth 应用协议协商、legacy v1 回退、明确升级拒绝与固定低基数版本指标；补充发布/回滚/退役边界。
- 验证记录：生成漂移、candidate baseline、breaking、版本策略检查和 Node 5/5 通过；proxy/server policy 各 4/4、metrics 各 1/1 通过；未启动服务，mybevy 仅只读检查。

- [x] 评估登录或握手中的客户端协议版本表达。（审核：确认 packet header `version=1` 仅为传输格式；`AuthReq.client_protocol_version=2` 新增应用层协商，缺失/0 回退 legacy v1。）
- [x] 定义最低支持版本、拒绝原因和升级提示字段。（审核：共享 `version-policy.rs/.json` 定义 current/minimum=1、`CLIENT_PROTOCOL_VERSION_TOO_OLD/TOO_NEW`；`AuthRes` 追加 fields 4-7，Node/Rust policy 测试通过。）
- [x] 服务间滚动升级明确生产者先后顺序。（审核：`docs/协议与客户端/协议版本协商与发布边界.md` 规定字段、玩家消息、内部 RPC 和 minimum 抬升的 proxy/server/client 发布与回滚顺序。）
- [x] 重大变更采用新字段/新消息/新 RPC，不原地改变旧语义。（审核：版本协商文档规定 expand/contract 与 reserve；published release reference 保持原值，breaking check 证明本期仅为非破坏追加。）
- [x] 记录旧版本观测指标与退役条件。（审核：proxy/server metrics 输出五个固定版本 bucket；文档规定以 proxy 聚合、连续 7 天为零和支持渠道覆盖作为 legacy v1 退役门槛。）

## 阶段 7：CI 与文档

- 开始时间：2026-07-16 18:25:26 +08:00
- 结束时间：2026-07-16 18:43:31 +08:00
- 开发总结：根 `check:proto` 汇总六类协议门禁；新增 Windows GitHub Actions、workflow 结构测试、CLI 负向 breaking 样例，并补齐本机/CI/外部客户端边界文档。
- 验证记录：`npm run check:proto` 六步通过；全部 `tests/proto/*.test.mjs` 32/32 通过，workflow 结构/负向 CLI/routing 回归 16/16 通过；`git diff --check` 通过。GitHub Actions 是 YAML/schema 最终解析器。

- [x] 根 `check:proto` 同时运行生成漂移和 breaking change 检查。（审核：`tools/check-proto.js` 顺序执行 candidate baseline、生成漂移、breaking、routing、fixture、version policy；2026-07-16 根命令六步通过。）
- [x] CI 输出具体文件、消息、字段和违反规则。（审核：聚合器继承子进程 stdio，breaking CLI 负向测试断言 `FIELD_TYPE_CHANGED`、proto 文件、`message=Example` 和 `fieldNumber=1`；workflow 在 Windows runner 执行根门禁。）
- [x] 为 Windows PowerShell 和 CI 环境提供一致安装说明。（审核：`docs/协议与客户端/协议生成与漂移检查.md` 说明 Node + stable Rust/Cargo、`npm install`/CI `npm ci`、vendored protoc 与同一根命令。）
- [x] 更新协议设计、外部客户端接入和 proto 所有权文档。（审核：协议设计、外部客户端接入、基线治理与生成检查文档均链接根门禁和 mybevy 只读/可选边界。）
- [x] 增加故意破坏样例验证检查器能失败。（审核：`proto-breaking-changes.test.mjs` 在临时仓库实际运行 breaking CLI 并断言 exit 1 与具体 rule/target 诊断。）

## 最终完成定义

- 开始时间：2026-07-16 15:50:30 +08:00
- 结束时间：2026-07-16 18:43:31 +08:00
- 验收总结：协议兼容性门禁已覆盖语义基线、锁定生成、已发布参考 breaking、路由/错误码、二进制 fixture、版本协商及 Windows CI；本轮 7 个实现提交均已通过相应审核和定向验证。

- [x] 已发布字段和枚举值的破坏性变更会自动阻断。（验收：发布 reference 比较器、严格豁免和实际 CLI 负向样例在根门禁中执行。）
- [x] proto、生成代码、路由和 mock-client 漂移可自动发现。（验收：`check:proto` 聚合 candidate baseline、Rust temp regeneration、routing consistency 与 mock-client codec 检查。）
- [x] 新旧版本至少通过关键消息 fixture 兼容验证。（验收：6 个 binary fixture、当前 decoder 与独立 historical v1 projection 测试通过，未知字段/enum 安全跳过。）
- [x] 协议基线更新有明确审计边界。（验收：候选 baseline 与 release reference 分离，更新/晋升均要求 reason、批准人、日期和 Git 审核。）
