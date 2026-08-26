# MyServer 主世界 4000 米权威移动 Checklist

## 目标

在 `MyServer` 仓库中将现有 `grassland_01 / movement_demo` 从 16 米测试场景扩展为 `4000m x 4000m` 的主世界权威移动范围，迁移默认出生点，调整适合 0.25 米客户端角色的纠正参数，并在当前阶段关闭 movement AOI、使用最多 32 人的全房间移动同步。

本清单复用现有移动协议、room policy、SceneQuery、纠正、重连和 room transfer 能力；不新建主世界 scene、不新建 room policy、不处理客户端 Mesh、灯光、摄像机或方圆玩家外观。

## 依赖与边界

- 需求来源：`mybevy/summary/主世界尺度角色摄像机与联机移动需求设计.md`。
- 实际开发仓库：`H:\project\MyServer`，执行时以 MyServer 当前工作区和 `AGENTS.md` 为准。
- 客户端继续使用 `world.main`，服务端继续使用 `grassland_01 / scene_id=1 / spawn_id=1001`。
- 公共房间继续使用 `main-world-public / movement_demo`。
- Protobuf 继续使用 `MoveInputReq`、`EntityTransform`、`MovementSnapshotPush`、`MovementRejectPush` 和 `MovementRecoveryState`。
- 本清单涉及真实客户端登录、选角、加房或移动联调时，必须优先尝试正式服登录；不得默认启动或连接本地服务端，避免干扰当前正在测试的其他服务端功能。无登录的服务端单元测试、静态配置检查和离线 fixture 可继续在本地执行。
- 根据 MyServer 仓库约定，运行服务端测试或启动依赖前先向用户说明所需服务和执行范围并取得确认。

## 基础原则

- [x] `character_id` 继续作为玩法身份，账号 `player_id` 不进入房间移动主体。（验证：movement state、room lifecycle、recovery 和 transfer 均以 character_id 建模，未引入 player_id 移动主体）
- [x] 服务端始终拥有最终位置；客户端 `client_state` 只用于漂移检测和纠正。（验证：movement 22/22 包含 client_state 不覆盖权威位置及漂移/Reject 测试）
- [x] 继续复用 `movement_demo`，不创建同义 scene、policy 或消息号。（验证：最终 diff 复核继续使用 grassland_01、movement_demo 与既有 protobuf）
- [x] 配置、网格、出生点、运行时和跨端坐标契约必须在同一服务端版本内一致。（验证：CSV/grid 解析、scene 13/13、movement/transfer 测试和三份正式文档共同覆盖）
- [x] 每个阶段保持独立验证和提交边界；实际 Git 提交仅在用户明确授权后执行。（验证：各阶段均记录独立验证证据；本轮未获提交授权，因此未创建 commit）

## 阶段 1：冻结 4000 米服务端契约

- 开始时间：2026-08-17 16:11:49 +08:00
- 结束时间：2026-08-18 10:25:32 +08:00
- 开发总结：建立 `grassland_01` 集中世界契约，冻结 4000 米排他边界、40 x 40 / 100 米网格、出生点与 movement_demo 目标参数；运行时配置仍以 CSV/grid 为事实源。
- 验证记录：`cargo check` 通过；`cargo test core::system::scene::` 13/13 通过；`git diff --check` 通过。

  - 2026-08-18 09:33:46 +08:00：中断恢复；现有未提交 diff 作为阶段 1～3 待审核实现继续处理。worker `/root/world_movement_worker` 连续 5 次恢复均在命令结果返回前发生响应流中断，已解除绑定并按规则创建替代 worker。
  - 替代 worker `/root/world_movement_replacement` 已创建；首次执行及第 1/5 次恢复发生响应流中断，第 2/5 次恢复成功并完成阶段 1～3；业务提交为 `e4998eec`。

- [x] 冻结 `grassland_01` 的服务端二维范围为 `0 <= x < 4000`、`0 <= y < 4000`。（验证：`world_contract.rs:14` 定义 4000 米世界尺寸，`query.rs:1136` 覆盖负值、3999.999 与 4000 排他边界，scene 测试 13/13 通过）
- [x] 冻结网格为 `Width=40`、`Height=40`、`CellSize=100.0`，总计 1600 个逻辑单元。（验证：`world_contract.rs:10-15` 集中定义尺寸和单元数，PowerShell JSON 解析确认 40 x 40、CellSize 100、两层各 1600 项）
- [x] 冻结默认出生点 `1001=(2002,2002)`，映射到客户端后为 `(2,0,2)`。（验证：`world_contract.rs:8,18-20` 定义服务端出生与客户端映射说明，`SceneSpawnPoint.csv` 的 1001 行为 2002.0/2002.0）
- [x] 冻结 `movement_demo` 为 20Hz、基础速度 4m/s、纠正周期 3 帧、推荐纠正阈值 0.05m、movement AOI 关闭。（验证：`world_contract.rs:23-27` 集中声明目标契约；阶段 4 负责应用到 room policy）
- [x] 明确 100 米网格只表达全平地边界，不承诺单元内部障碍精度。（验证：`world_contract.rs:16` 明确 100 米单元仅表达全平地世界边界）
- [x] 核对受影响的 SceneTable、SceneSpawnPoint、grid、SceneCatalog、movement 和 transfer 测试范围。（验证：变更覆盖两份 CSV、grid、scene loader/query/validator、runtime reload、factory 与 movement_demo；`cargo check` 通过）
- [x] 增加集中契约测试，避免 4000、40、100、2002 等常量在无关联位置分叉。（验证：`world_contract.rs` 集中常量，`query.rs:1136` 对运行时 catalog 逐项比对，scene 测试 13/13 通过）

## 阶段 2：扩展 grassland_01 配置与网格

- 开始时间：2026-08-17 16:11:49 +08:00
- 结束时间：2026-08-18 10:25:32 +08:00
- 开发总结：将 grassland 配置扩展为 40 x 40 / 100 米确定性全平地网格，强化 layer 长度校验与 SceneQuery 排他边界测试，未改变 scene 身份和周边玩法配置。
- 验证记录：PowerShell 解析确认 `walkable` 1600 项且全 1、`block` 1600 项且全 0；scene 测试 13/13 通过。

- [x] 更新 `apps/game-server/csv/SceneTable.csv` 中 `grassland_01` 的 Width、Height 和 CellSize。（验证：grassland 行为 `40,40,100.0`，其余字段未变）
- [x] 将 `grassland_01.grid.json` 更新为 40 x 40 网格。（验证：PowerShell JSON 解析返回 Width=40、Height=40、CellSize=100）
- [x] 生成严格 1600 项的 `walkable=1` 和 1600 项的 `block=0`，保持 JSON 为可审查的确定性文本。（验证：JSON 解析返回 Walkable=1600/总和1600、Block=1600/总和0）
- [x] 保持 scene code、数值 ID、grid 文件名、DefaultSpawnId 和无关 tags 不变。（验证：`SceneTable.csv` 仍为 `1,grassland_01,...,grassland_01.grid.json,...,1001,pve|outdoor|safe_zone`）
- [x] 验证 grid loader 拒绝层长度与 40 x 40 不一致的输入。（验证：`grid.rs:127` 构造 2 x 2 但仅 3 项 fixture 并断言拒绝，scene 测试通过）
- [x] 增加 `(0,0)`、中心、`3999.x`、负值和 `4000` 排他边界的 SceneQuery 测试。（验证：`query.rs:1136` 覆盖所有指定边界及 NaN，scene 测试通过）
- [x] 核对现有 NPC、区域、交互和其他 grassland 配置测试；只处理尺寸变化造成的兼容问题，不扩展玩法。（验证：scene 模块全部 13 项测试通过，NPC branch、region、interaction、context 测试均无回退）

## 阶段 3：迁移出生点与多人生成

- 开始时间：2026-08-17 16:11:49 +08:00
- 结束时间：2026-08-18 10:25:32 +08:00
- 开发总结：迁移默认出生点并补齐有限性/可行走校验、多 character 独立状态以及 offline/leave 隔离测试；不引入碰撞或出生偏移。
- 验证记录：runtime reload 成功/失败原子保留测试各 1/1 通过；多人 offline/leave 测试 1/1 通过。

- [x] 将 `SceneSpawnPoint.csv` 中默认玩家出生点 1001 调整为 `(2002,2002)`。（验证：CSV 1001 行坐标为 `2002.0,2002.0`）
- [x] 验证出生点属于 scene 1、坐标有限、位于范围内且对应网格可行走。（验证：`query.rs:1136` 校验 scene ID/坐标/可行走，`validator.rs:25` 显式拒绝非有限默认出生点，scene 测试通过）
- [x] 保持 `movement_demo` 按加入角色创建独立 `entity_id` 和 `character_id` 移动状态。（验证：`movement_demo/mod.rs:439` 对两个 character 断言 character_id 与 entity_id 独立）
- [x] 增加两个以上 character 同房间生成时身份、位置、方向和 last input frame 独立的测试。（验证：`movement_demo/mod.rs:439` 校验共同出生点、初始方向及输入后的独立方向/frame；定向测试 1/1 通过）
- [x] 明确本阶段玩家之间无实体碰撞；如增加出生偏移，必须确定性且经过 SceneQuery 校验。（验证：`movement_demo/mod.rs:104` 明确共享配置出生点且不实现玩家碰撞/偏移，测试确认两人均出生于 2002/2002）
- [x] 验证 Room Leave 和 offline TTL 路径只移除或停止目标 character，不影响其他玩家。（验证：`movement_demo/mod.rs:439` 校验 offline 仅停止 A、leave 仅删除 A，B 的 entity/方向/frame/moving 不变；定向测试通过）

## 阶段 4：调整 movement_demo 策略与全房间同步

- 开始时间：2026-08-18 10:27:49 +08:00
- 结束时间：2026-08-18 11:56:08 +08:00
- 开发总结：将 movement_demo 运行参数集中到 room policy，应用 20Hz、4m/s、3 帧纠正周期、0.05m 阈值、AOI disabled 与 32 人上限；AOI 关闭后 full sync/recovery 返回全量实体，周期增量面向全房间 recipients，Reject/强纠正仅面向目标角色，并强化 main-world-public 的严格 policy 准入。
- 验证记录：`cargo check --manifest-path apps/game-server/Cargo.toml` 通过；movement correction 3/3、room policy 定向 1/1、main-world-public lifecycle 定向 1/1、movement_demo 2/2 通过；阶段文件 `rustfmt --check` 与 `git diff --check` 通过。32 人 protobuf 实测 full snapshot 1163B、recovery 1133B；按 20Hz 每帧全量分别发送给 32 人的保守房间出口上界为 744320B/s（约 727 KiB/s），正常周期为每 3 帧且增量仅含变化实体。

  - 2026-08-18 11:26:06 +08:00：用户确认中断续作方案；原 worker 已不存在，按继承规则创建替代 worker 接管阶段 4 现有未提交 diff，并在阶段 4～7 串行复用。
  - 2026-08-18 11:31:18 +08:00：替代 worker `/root/world_movement_continuation` 首轮响应流中断，未产生新增 diff；准备执行第 1/5 次恢复派发。
  - 2026-08-18 11:35:20 +08:00：第 1/5 次恢复派发再次响应流中断，仍未产生新增 diff；第 2/5 次恢复将任务收窄为仅完成阶段 4。
  - 2026-08-18 11:39:33 +08:00：第 2/5 次恢复仍发生响应流中断且无新增 diff；继续按同一 worker 恢复上限处理。

- [x] 保持 `active_room_fps=20`、默认移动速度 4m/s 和 `movement_control_stop_frames=3`。（验证：`room_policy.rs:5-11,174-196` 集中定义并应用 20/4.0/3，`builtin_room_policy_defaults_cover_tick_input_and_capacity` 通过；movement_demo 从同一速度常量生成实体）
- [x] 将 `movement_correction_threshold` 调整为 0.05m 初值，并保留可配置来源和定向测试。（验证：`room_policy.rs:9,193,441` 定义、应用并断言 0.05；`factory.rs:37-48` 从 registry policy 注入 MovementDemoLogic）
- [x] 保持 `movement_correction_interval_frames=3`，验证正常同步不产生每帧强纠正。（验证：`correction.rs:264` 周期增量测试确认 frame 1 发送、frame 2/3 不发送、frame 4 再发送，定向测试通过）
- [x] 将 `movement_aoi_enabled` 设为 false，并使 snapshot/recovery 对房间内最多 32 个玩家提供明确全量语义。（验证：`room_policy.rs:5,10,174,194` 固定 32 人与 AOI false；`correction.rs:220` 解码 full snapshot/recovery 并确认均包含 32 个实体）
- [x] 验证 full sync、普通增量、MovementReject 和 recovery 在 AOI 关闭后 target/recipient 逻辑一致。（验证：`correction.rs:220,264,294` 覆盖 full sync 隐式房间广播、增量显式 32 recipients、Reject/强纠正单角色目标及 recovery 全量实体，3/3 通过）
- [x] 记录最大 32 人下单次 `EntityTransform` 集合大小、推送频率和估算带宽。（验证：真实 protobuf `encoded_len` 实测 snapshot=1163B、recovery=1133B；20Hz/32 recipients 保守上界 744320B/s，正常 3 帧周期增量低于上界）
- [x] 保证 room policy registry、factory 和 `main-world-public` 的严格 policy 准入不回退。（验证：registry 定向测试确认 movement_demo 参数；`factory.rs:37-48` 注入 policy；`lifecycle.rs:191-198` 与 `tests/lifecycle.rs:100` 在首次创建前拒绝非 movement policy，定向测试通过）

## 阶段 5：强化权威输入与边界校验

- 开始时间：2026-08-18 12:02:15 +08:00
- 结束时间：2026-08-18 12:09:09 +08:00
- 开发总结：补齐主世界权威移动的输入、归一化、4000 米排他边界、漂移、Reject 与控制超时回归；非法 payload 不再复用旧 client_state，避免 Reject 将缓存样本误报为当前输入坐标。
- 验证记录：`cargo test --manifest-path apps/game-server/Cargo.toml core::system::movement:: --quiet` 22/22 通过；错误 room/非成员与旧帧 room manager 定向测试各 1/1 通过；`cargo check --manifest-path apps/game-server/Cargo.toml --quiet`、movement 文件 `rustfmt --check` 和 `git diff --check` 通过。`tick.rs` 全文件 rustfmt 仍受本轮前既有第 498 行格式差异阻塞，本轮新增测试本身符合 rustfmt 输出。

- [x] 保持 MoveInput frame、type、方向有限性、非零方向和 client_state 有限性校验。（验证：`input.rs:89-238` 校验 input type、方向 finite/安全范围/非零及 client_state finite/安全范围；input 定向测试 6/6 通过，room manager 旧 frame 测试通过）
- [x] 确保输入方向归一化或限幅，客户端向量幅度不能改变 4m/s 权威速度。（验证：`state.rs:371-387` 对 MoveDir/FaceTo 归一化；`sim.rs:485` 以 3000/4000 向量验证 20Hz 单帧仍只移动 0.2m，即 4m/s）
- [x] 使用 SceneQuery clamp 所有目标位置，越界输入不能把实体推进到 4000 米范围外。（验证：`sim.rs:126-139` 所有移动目标经 `clamp_position`；`sim.rs:531` 从 3999.9 向 +X 推进时保持 `<4000`、停止并返回 MOVEMENT_BLOCKED）
- [x] 保持 `client_state` 只参与漂移检测，不直接覆盖权威位置。（验证：`sim.rs:507` 输入 client=(3000,3000) 后权威位置仍按 4m/s 从 (1,1) 推进到 (1.2,1)，同时产生 drift record）
- [x] 验证错误 room、错误 character membership、旧 frame 和非法输入得到稳定拒绝或忽略结果。（验证：`tests/tick.rs:568,589` 分别断言 INPUT_FRAME_EXPIRED、ROOM_NOT_FOUND、ROOM_MEMBER_NOT_FOUND；`input.rs:331-378` 断言 unknown/zero/out-of-range 错误码）
- [x] MovementReject 返回 corrected transform、reference frame、correction kind/reason 和必要的 server/client 对比位置。（验证：`correction.rs:326-342` 解码 protobuf 并断言 corrected、reference=8、Strong、MovementRejected、client=2100/2100、server=2002/2002）
- [x] 日志只记录必要 room、character、frame、reason 和误差，不输出 ticket、token 或敏感 payload。（验证：`movement_demo/mod.rs:190-197` 拒绝日志字段仅 room_id/frame_id/character_id/error_code；movement runtime 日志搜索无 ticket/token/payload 输出）
- [x] 增加越界、超大方向、非有限数值、漂移和控制超时测试。（验证：`sim.rs:397,485,507,531,595` 覆盖 control timeout、超大方向、漂移、4000 米越界和非法输入旧 client_state 隔离；`input.rs` 覆盖 NaN/Infinity/安全范围，movement 22/22 通过）

## 阶段 6：验证恢复、迁移与热加载兼容

- 开始时间：2026-08-18 12:13:52 +08:00
- 结束时间：2026-08-18 12:23:14 +08:00
- 开发总结：收紧 movement_demo room transfer 导入边界，目标实例按当前 SceneCatalog 与 room policy 校验 scene、4000 米可行走位置、4m/s 速度、纠正参数、控制超时和 AOI disabled 契约；补齐离线停止后的 reconnect recovery、主世界 transfer roundtrip、旧 AOI 契约拒绝以及 CSV reload 不重置既有实体状态的定向测试。
- 验证记录：`git diff --check` 通过；源码复核确认 transfer schema/version 继续明确拒绝，新增不兼容 movement 契约错误码与定向测试。依照 MyServer 协作约定，本阶段未自动运行 `cargo test`、`cargo check` 或 `rustfmt`，待阶段 7 经用户确认后统一执行。

- [x] 验证 RoomReconnect recovery 保留 4000 米范围内的位置、方向、moving 和 last input frame。（验证：`movement_demo/mod.rs` 的 `offline_recovery_stops_authoritative_movement_without_losing_last_input` 在 3999.5/2002 位置断言 recovery 保留 scene/位置/方向，离线停止后 moving=false 且 last_input_frame 更新为权威停止帧 30）
- [x] 验证 room transfer 导入导出保持 scene ID、速度、位置、纠正参数和 AOI disabled 状态。（验证：`state.rs` roundtrip 使用 3999.75/2002、4m/s、0.05m、3 帧与 AOI disabled；`demo_payload.rs` 同时断言集成导出和导入后 recovery 契约）
- [x] 验证离线时权威实体停止并下发对应 correction，重连后不会继续旧方向漂移。（验证：离线 recovery 测试解码 pending `MovementSnapshotPush`，断言 `PlayerOffline`、moving=false；随后 `ReconnectRecovery` 同样返回停止状态）
- [x] 验证运行时 CSV reload 后 SceneQuery 使用新 4000 米 catalog，现有实体状态不被静默重置。（验证：`csv_reload_updates_scene_catalog_without_resetting_existing_entities` reload 出生点后断言 character-a 的 entity/位置/frame 不变，新加入 character-b 使用 reload 后的 2003.5/2002 出生点）
- [x] 对不兼容旧 transfer schema 保持明确拒绝，不通过默认值掩盖 AOI 或范围差异。（验证：schema/version 仍返回 `ROOM_TRANSFER_UNSUPPORTED_SCHEMA`；新增导入契约校验对 scene、纠正参数、AOI、速度和 SceneQuery 范围差异返回 `ROOM_TRANSFER_INCOMPATIBLE_MOVEMENT_STATE`，旧 AOI enabled payload 有独立测试）
- [x] 增加 recovery、transfer roundtrip、invalid transfer 和配置 reload 定向测试。（验证：新增/更新 `offline_recovery_stops_authoritative_movement_without_losing_last_input`、`transfer_state_roundtrip_restores_runtime_fields`、`movement_demo_transfer_rejects_legacy_aoi_contract`、`csv_reload_updates_scene_catalog_without_resetting_existing_entities`；测试命令留待阶段 7 经用户确认执行）

## 阶段 7：服务端测试、文档与交付验证

- 开始时间：2026-08-18（用户确认后，未单独记录精确时刻）
- 结束时间：2026-08-18 13:55:46 +08:00
- 开发总结：完成主世界权威移动的交付验证与文档同步；修正 transfer 导入校验对协议 transform 不存在 speed 字段的错误访问，改为校验内部 movement entity，并新增 32 人 full snapshot 本机 debug fixture 处理开销采样。场景地图、房间生命周期和外部客户端接入文档已统一 4000 米排他边界、出生点、movement 参数、AOI disabled 与客户端同步发布约束。
- 验证记录：用户确认后执行全部离线验证，不启动 PostgreSQL、Redis、NATS、auth-http、game-proxy 或 game-server。`cargo check` 通过（保留仓库既有 warning）；scene 13/13、movement 22/22、room policy 1/1、准确命名的 main-world lifecycle 2/2、movement_demo 4/4、movement transfer 4/4 通过；阶段 Rust 文件 `rustfmt --check`、`git diff --check` 和 PowerShell grid/CSV 契约解析通过。模糊 `main_world_public` filter 曾匹配 0 项，未计入通过证据，随后使用两个准确测试名复验。32 人 fixture：snapshot 1163B、recovery 1133B、20Hz/32 recipients 保守出口上界 744320B/s；2000 次 full snapshot 构造与序列化总计 122.0684ms、平均约 61.0us/次、累计编码 2321740B，该耗时仅代表本机 debug test fixture，不作为生产性能承诺。

- [x] 按 MyServer 协作约定先向用户说明测试依赖与范围，确认后运行格式化、静态检查和定向测试。（验证：已说明仅需 Windows Rust 工具链与 Cargo 缓存、无需启动服务；用户明确确认后执行）
- [x] 运行 SceneCatalog/grid/validator、room policy、movement system、movement demo 和 room lifecycle 定向测试。（验证：scene 13/13、policy 1/1、movement 22/22、movement_demo 4/4、main-world lifecycle 2/2 通过）
- [x] 运行 reconnect recovery 和 movement room transfer 定向测试。（验证：movement_demo recovery/reload 纳入 4/4；movement transfer restore/invalid/schema/legacy AOI 4/4 通过）
- [x] 使用 mock-client 或等价 fixture 验证两个 character 的移动、停止、纠正和全房间快照；如需要真实登录/进场，优先使用正式服，不启动本地服务端。（验证：采用等价离线 Rust fixture；两 character 独立状态、权威移动/停止、Reject/强纠正、AOI disabled 全房间 snapshot、offline recovery 均通过，未启动或连接服务）
- [x] 记录最大 32 人快照的序列化大小、发送频率和 game-server 处理开销。（验证：snapshot 1163B、recovery 1133B；周期 3 帧，保守 20Hz 全量出口 744320B/s；本机 debug fixture 2000 次平均约 61.0us/次）
- [x] 更新 MyServer 场景地图、移动同步和外部客户端接入文档中的 4000 米契约。（验证：更新 `场景地图格式设计.md`、`帧同步与房间生命周期设计.md`、`外部客户端接入说明.md`，文档关键词复核与 `git diff --check` 通过）
- [x] 明确客户端需要同步升级坐标边界，避免新服务端与仍校验 16 米的旧客户端组合发布。（验证：外部客户端接入文档明确旧 16 米客户端不得与 4000 米服务端组合发布）

## 最终完成定义

以下项目作为整体完成标准，不要求每个开发阶段都执行，由所有相关阶段完成后统一验收。

- 开始时间：2026-08-18 13:55:13 +08:00
- 结束时间：2026-08-18 13:55:46 +08:00
- 验收总结：阶段 1～7 的代码、配置、离线 fixture 和正式文档证据已闭环。主世界继续复用既有 scene、policy 和 movement 协议，以 character 为玩法身份；4000 米网格、默认出生、20Hz/4m/s 权威推进、0.05m 纠正、AOI disabled 全房间同步、输入与边界拒绝、离线/重连、热加载和 transfer 均通过定向验证。未执行真实客户端登录联调，因为等价离线 fixture 已满足本清单交付项且无需启动或连接服务。

- [x] `grassland_01` 通过 40 x 40、CellSize 100 的 grid 表达 `4000m x 4000m` 范围。（验证：PowerShell 解析确认 CSV/grid 均为 40 x 40 / 100，walkable/block 各 1600 项，scene 13/13 通过）
- [x] 默认出生点 1001 为 `(2002,2002)` 且通过 SceneCatalog 校验。（验证：CSV 解析为 2002/2002，SceneCatalog/grid/validator 13/13 通过）
- [x] `movement_demo` 保持 20Hz 和 4m/s，并使用 0.05m 初始纠正阈值。（验证：room policy 1/1、movement 22/22、movement_demo 4/4 通过）
- [x] movement AOI 在本阶段关闭，最多 32 人使用全房间移动快照。（验证：32 人 full snapshot/recovery fixture 与 recipient/target 测试通过）
- [x] 非法输入、越界、漂移、停止、离线和 Reject 均有权威且可测试的结果。（验证：movement 22/22 与 movement_demo 4/4 覆盖）
- [x] 多 character、reconnect recovery 和 room transfer 行为不回退。（验证：两 character 等价 fixture、offline recovery/reload 4/4、movement transfer 4/4 通过）
- [x] 未新增重复 scene、policy、协议消息或非 character-bound 玩法身份。（验证：diff 复核仅收紧既有 movement_demo/transfer，实现继续使用 grassland_01、movement_demo 与既有 protobuf；房间主体仍为 character_id）
- [x] 经用户确认执行的服务端定向测试、检查和文档更新均完成。（验证：`cargo check`、全部上述定向测试、rustfmt、diff check、配置解析与三份文档更新完成）
