# 服务端角色身份主键迁移 Checklist

## 目标

将服务端“进入游戏后的玩法主体”从账号 `player_id` 迁移为角色 `character_id`。迁移完成后，登录、session、封禁、踢账号、改密、ticket version、账号级并发控制继续使用 `account_player_id`；房间成员、匹配、重连、输入、移动、战斗实体、角色数据和玩法投递目标使用 `character_id`。所有日志和审计需要同时保留账号 ID 与角色 ID，避免后续排查只能看到单一身份。

本项目按新项目处理，本次修改不考虑旧数据、旧协议、旧逻辑和旧客户端兼容，不做历史数据迁移脚本，不保留为了兼容旧字段而产生的分支逻辑。实现应直接收敛到目标态，优先删除或替换旧语义代码，避免因为兼容层导致服务端逻辑臃肿。

本清单不要求一次性提交所有改动。每个阶段应能独立实现、独立验证、独立提交。

## 基础原则

- [x] 明确账号身份字段统一命名为 `account_player_id` / `accountPlayerId`，只用于账号、安全、运营控制和 ticket 失效链路。（验证：阶段 2/5/7/10 静态扫描确认 ticket、blocklist、kick/ban、logout、并发登录和 ticket version 保留账号语义）
- [x] 明确游戏内主体字段统一命名为 `character_id` / `characterId`，用于角色、房间、匹配、输入、战斗、背包、任务和展示。（验证：阶段 1/3/4/6/7/8/9 将协议、运行时索引、匹配、背包、movement/combat/transfer 和 mock-client 收敛为角色字段）
- [x] 服务端不信任客户端传入的账号 ID；账号 ID 必须来自 ticket 鉴权上下文或服务端数据库关系。（验证：`RoomReconnectReq` 编码为空 body，房间/背包/角色数据入口使用 ticket-bound `identity.character_id`，账号控制链路使用 ticket/db 上下文中的 `account_player_id`）
- [x] 重连、匹配建房、输入投递和 room transfer 不允许出现“协议是角色 ID、内部索引仍按账号 ID”的半迁移状态。（验证：阶段 3/4/6/8/10 的 room manager、match-service、input、movement/combat transfer 静态扫描和定向测试均通过）
- [x] 日志、审计、GM 操作和错误响应同时记录 `account_player_id` 与 `character_id`，必要时增加 `subject_type` / `subject_id` 区分操作对象。（验证：阶段 4 DB room event 写入 `room_subject_id` / `account_player_id` / `character_id`，阶段 7 GM 发物品改为 `targetType: "character"` 且账号级 GM 保留 `playerId`）
- [x] 不做旧数据迁移、旧协议兼容、旧客户端兼容或双字段兼容；遇到旧字段直接删除、改名或替换为目标态字段。（验证：阶段 1 删除旧协议字段，阶段 7 直接替换 `player_inventory*` 为 `character_inventory*`，阶段 10 补充 NPC transfer 旧 `targetPlayerId` 拒绝测试）
- [x] 不为旧逻辑保留 fallback、兼容开关、双写、双读或临时桥接；确需过渡的编译中间态必须在同阶段内清理。（验证：最终静态扫描旧玩法字段仅剩确认旧字段不存在或拒绝旧 schema 的负向测试）
- [x] 阶段完成后按影响范围运行对应单元测试、编译检查、协议检查和文档检查；需要真实 Redis / PostgreSQL / NATS / 服务联调时先列出依赖并等待确认。（验证：各阶段记录 Rust/Node 定向测试、`npm run check:proto`、`git diff --check`；用户确认后阶段 9/10 运行 registry e2e 服务联调）

## 阶段 1：协议语义冻结

- 开始时间：2026-06-26 19:01:27 +08:00
- 结束时间：2026-06-26 19:17:29 +08:00
- 开发总结：完成 game/match 协议身份语义冻结，房间、移动、transfer、authority 和匹配参与者字段收敛为角色 ID；`AuthRes.player_id` 仅保留账号语义；同步 Rust 生成产物和 mock-client 字段 schema，并修正 mybevy 协议检查路径。
- 验证记录：`npm run check:proto` 通过，输出确认检查到 `project\src\game\myserver\protocol.rs, project\build.rs`；`git diff --check` 通过；静态扫描确认 `packages/proto/game.proto` / `match.proto` 中旧 `player_id` 仅剩 `AuthRes.player_id`。

- [x] 梳理 `packages/proto/game.proto` 中所有 `player_id`、`player_ids`、`owner_player_id`、`target_player_ids` 字段，逐个标注账号语义或角色语义。（验证：`packages/proto/game.proto` 静态扫描旧字段仅剩 `AuthRes.player_id`，角色字段已改为 `character_id` / `character_ids` / `owner_character_id` / `target_character_ids`）
- [x] 将房间成员字段从 `RoomMember.player_id` 迁移为 `character_id`，并调整 `RoomSnapshot.owner_player_id` 为 `owner_character_id`。（验证：`packages/proto/game.proto:180` 定义 `RoomMember.character_id`，`packages/proto/game.proto:190` 定义 `RoomSnapshot.owner_character_id`）
- [x] 将 `RoomReconnectReq.player_id` 改为不传身份，或改为 `character_id` 并要求等于 ticket 绑定角色。（验证：`packages/proto/game.proto:273` 定义空 `RoomReconnectReq {}`，`tools/mock-client/src/messages.js:46` 编码为空包）
- [x] 将 `CreateMatchedRoomReq.player_ids` 迁移为 `character_ids`，并同步 `CreateMatchedRoomRes` 中快照语义。（验证：`packages/proto/game.proto:552` 定义 `character_ids`，`packages/proto/game.proto:557` 注释声明 snapshot 使用角色 ID）
- [x] 将房间离线、移动校正、定向广播、ServerRedirect、RoomTransfer payload 中表示玩法目标的 `player_id` 字段迁移为 `character_id`。（验证：`packages/proto/game.proto:163`、`:224`、`:232`、`:320`、`:330`、`:341`、`:380`、`:545` 已使用角色字段；旧目标字段扫描无残留）
- [x] 明确 `AuthRes.player_id` 为账号语义，并评估是否新增 `AuthRes.character_id` 以减少客户端反查。（验证：`packages/proto/game.proto:16` 注释标明 `AuthRes.player_id` 是账号级登录/session owner；本阶段未新增未填充的 `AuthRes.character_id`）
- [x] 确认 P1/P2 已落地的 `GetCharacterElements`、`DebugApplyCharacterElementChange`、`GetCharacterTitles`、`EquipCharacterTitle`、`GetCharacterDisciplines` 和 `DebugCharacterTitle` 协议继续保持 ticket 绑定角色语义，不允许在迁移中退回账号主体。（验证：`packages/proto/game.proto:679`、`:693`、`:728`、`:740`、`:759`、`:779` 的响应继续返回 `character_id`，请求体未新增可冒用身份字段）
- [x] 同步 `packages/proto/match.proto`，将匹配参与者字段从 `player_id` / `player_ids` 迁移为 `character_id` / `character_ids`。（验证：`packages/proto/match.proto:43`、`:58`、`:71`、`:86`、`:103`、`:117`、`:131` 已使用角色字段）
- [x] 重新生成 Rust / Node 侧 protobuf 产物，并确保生成文件没有混入无关格式化变更。（验证：`apps/game-server/src/proto/myserver.game.rs` 与 `apps/match-service/src/proto/myserver.game.rs` 内容一致，`myserver.matchservice.rs` 两侧一致；`git diff --check` 通过）
- [x] 删除旧协议字段和旧语义说明，不保留旧字段兼容、双字段优先级或旧客户端错误码分支。（验证：`apps/match-service/src/proto/myserver.r#match.rs` 旧残留生成产物已删除；旧字段静态扫描仅剩账号语义 `AuthRes.player_id`）
- [x] 验证 `npm run check:proto` 或当前仓库协议检查命令能通过；若依赖外部客户端路径缺失，记录阻塞条件。（验证：`npm run check:proto` 通过；`tools/check-mock-client-protocol.js` 已补充 mybevy `project/src/game/myserver/protocol.rs` 探测路径）

## 阶段 2：game-server 鉴权上下文和命名清理

- 开始时间：2026-06-26 19:20:04 +08:00
- 结束时间：2026-06-26 20:27:27 +08:00
- 开发总结：完成 game-server 鉴权上下文字段清理，`Session` 移除账号兼容别名，ticket payload 明确 `playerId` 反序列化到 `account_player_id` 且继续强制 `characterId`，在线注册表保持账号主索引与角色辅助索引，核心登录、限流、连接事件和踢旧日志同步记录账号/角色边界。
- 验证记录：`cargo test ticket::tests` 通过 6 项，`cargo test session::tests` 通过 2 项，`cargo test online_registry` 通过 3 项；`git diff --check` 通过；`cargo fmt --check` 已运行但失败范围为既有未格式化文件 `character_title_service.rs`、生成的 `csv_code/*` 和 `server.rs` 既有长行，未在本阶段扩大格式化改动。

- [x] 保持 game ticket payload 同时包含 `playerId` 与 `characterId`，并继续拒绝缺少 `characterId` 的 ticket。（验证：`apps/game-server/src/ticket.rs:11` 通过 `#[serde(rename = "playerId")] account_player_id` 接收账号 ID，`:13` 接收 `characterId`，`:59` 缺失时返回 `MISSING_CHARACTER_ID`）
- [x] 将 `Session.player_id` 这类账号兼容别名改名或隔离为 `account_player_id`，避免后续业务误用。（验证：`apps/game-server/src/session.rs:32` 的 `Session` 不再包含 `player_id` 字段；`rg "session\\.player_id|connection\\.session\\.player_id" apps/game-server/src` 无残留）
- [x] 保留在线注册表的账号级主索引，用于同账号踢旧、GM 踢账号、session kick 和账号级并发限制。（验证：`apps/game-server/src/core/context.rs:53` 使用 `by_account_player_id` 主索引，`:81` 提供 `get_by_account`，`:92` 通过账号和 session 清理）
- [x] 保留或完善在线注册表的角色辅助索引，用于按 `character_id` 查当前连接、房间投递和角色级操作。（验证：`apps/game-server/src/core/context.rs:54` 保留 `character_to_account_player_id`，`:85` 提供 `get_by_character`，`:75` 插入角色索引）
- [x] 为 `AuthenticatedSessionIdentity` 增加清晰注释和 helper 方法，例如 `account_player_id()`、`character_id()`，减少调用点直接拿错字段。（验证：`apps/game-server/src/session.rs:8` 注释区分账号/角色语义，`:23` 和 `:27` 提供 helper）
- [x] 审查 `server.rs`、`core_service.rs`、`context.rs` 中所有 `player_id` 日志字段，账号语义改为 `account_player_id`，角色语义改为 `character_id`。（验证：`apps/game-server/src/core/service/core_service.rs:132`、`:192` 记录 `account_player_id`/`character_id`，`apps/game-server/src/server.rs:773`、`:809`、`:996` 不再记录账号语义 `player_id`）
- [x] 更新 ticket owner、ticket version、blocklist、账号踢人相关测试，确认这些链路仍按账号 ID 运行。（验证：`apps/game-server/src/ticket.rs:72` 的 `validate_ticket_owner` 继续比较 `account_player_id`；`cargo test ticket::tests` 通过 `validate_ticket_owner_distinguishes_account_mismatch` 与 `validate_ticket_version_keeps_account_level_revocation`；`apps/game-server/src/server.rs:977` 账号级消息限流继续使用 `account_player_id`）
- [x] 运行 `cargo fmt --check`、`cargo test ticket::tests`、`cargo test session::tests`、`cargo test online_registry` 或对应定向测试。（验证：`cargo test ticket::tests` 6 passed，`cargo test session::tests` 2 passed，`cargo test online_registry` 3 passed；`cargo fmt --check` 已运行，失败仅涉及既有未格式化范围 `character_title_service.rs`、`csv_code/*` 和 `server.rs` 既有长行）

## 阶段 3：game-server 房间运行时迁移

- 开始时间：2026-06-26 19:35:06 +08:00
- 结束时间：2026-06-26 20:27:27 +08:00
- 开发总结：完成 game-server 房间运行时角色主键迁移，房间成员、owner、在线/离线索引、pending input、input history、transfer snapshot、movement/combat 消费侧和 RoomLogic 回调均收敛到 `character_id` 语义，并补充同账号多角色索引隔离测试。
- 验证记录：`cargo test core::room::tests` 通过 3 项，`cargo test core::runtime::room_manager::tests` 通过 65 项，`cargo test core::system::movement` 通过 13 项，`cargo test core::system::combat` 通过 6 项；静态扫描确认 `core/room`、`core/runtime/room_manager`、`core/logic`、`gameroom`、movement/combat 范围内旧运行时字段和旧回调名无残留；`git diff --check` 通过。

- [x] 将 `RoomMemberState.player_id` 迁移为 `character_id`，并清理“P0 账号 ID 兼容边界”注释。（验证：`apps/game-server/src/core/room/mod.rs:266` 定义 `RoomMemberState`，运行时扫描无 `RoomMemberState.player_id` 和 `P0 compatibility` 残留）
- [x] 将 `Room.owner_player_id` 迁移为 `owner_character_id`，并同步 owner 判定、转让和房主权限校验。（验证：`apps/game-server/src/core/room/mod.rs:366` 定义 `owner_character_id`；`:477`、`:518` 使用角色 ID 校验房主；`apps/game-server/src/core/runtime/room_manager/lifecycle.rs:328`、`:433`、`:708` 使用 `owner_character_id` 处理转让和权限）
- [x] 将 `Room.members` 的 key 从账号 ID 切换为角色 ID。（验证：`apps/game-server/src/core/room/mod.rs:372` 注释声明 members key 与 `character_id` 一致；`apps/game-server/src/core/runtime/room_manager/lifecycle.rs:84`、`:123` 插入 `RoomMemberState.character_id`）
- [x] 将 `player_rooms`、`offline_players`、离线 TTL、房间成员同步状态等索引改为角色 ID。（验证：`apps/game-server/src/core/runtime/room_manager/mod.rs:118` 定义 `character_rooms`，`:119` 定义 `offline_characters`，`:141`-`:247` 的索引维护均以 character 命名；`apps/game-server/src/core/runtime/room_manager/lifecycle.rs:628` 清理过期离线角色）
- [x] 将 `PlayerInputRecord.player_id`、pending input、input history、waiting frame input 的归属改为角色 ID。（验证：`apps/game-server/src/core/room/mod.rs:278` 定义 `PlayerInputRecord`，`:293` 的 pending input 以角色键归属；`apps/game-server/src/core/runtime/room_manager/lifecycle.rs:751` 构造输入记录时写入角色 ID）
- [x] 将 `join_room`、`leave_room`、`disconnect_room_member`、`reconnect_room`、`set_ready_state`、`start_game`、`submit_player_input`、`end_game` 的入参语义改为角色 ID，并同步函数名或注释。（验证：`apps/game-server/src/core/runtime/room_manager/lifecycle.rs` 中对应入口使用 `character_id` 日志字段和参数；静态扫描运行时范围无 `player_rooms`、`offline_players`、`owner_player_id` 残留）
- [x] 将 `RoomLogic` / `RoomRuntime` 中表示玩法成员的 `on_player_join`、`on_player_leave`、`on_player_input` 等接口改名为 `on_character_*` 或 `on_member_*`，或至少统一注释为角色 ID。（验证：`apps/game-server/src/core/logic/room_logic.rs:649`、`:651`、`:665`、`:667` 定义 `on_character_join` / `on_character_leave` / `on_character_input` / `validate_character_input`；旧回调名静态扫描无残留）
- [x] 将 `RoomSnapshot` 生成逻辑输出角色 ID，并确保 members 排序、checksum、transfer snapshot 稳定。（验证：`apps/game-server/src/core/room/mod.rs:433` 设置 `is_owner` 时比较 `member.character_id` 与 `owner_character_id`，`:439` 按 `character_id` 排序，`:443` 输出 `owner_character_id`；`apps/game-server/src/core/runtime/room_manager/transfer.rs:280` 导出 transfer owner 为 `owner_character_id`）
- [x] 更新 room manager 单测，覆盖同账号不同角色不会在房间成员索引中互相覆盖。（验证：`apps/game-server/src/core/runtime/room_manager/tests.rs:326` 新增 `room_member_index_keeps_same_account_characters_distinct`；`cargo test core::runtime::room_manager::tests` 65 passed）
- [x] 运行 `cargo test` 中 room、runtime、transfer、input、offline reconnect 相关定向测试。（验证：`cargo test core::room::tests` 3 passed，`cargo test core::runtime::room_manager::tests` 65 passed，`cargo test core::system::movement` 13 passed，`cargo test core::system::combat` 6 passed）

## 阶段 4：game-server 房间服务入口迁移

- 开始时间：2026-06-26 20:30:44 +08:00
- 结束时间：2026-06-26 21:11:57 +08:00
- 开发总结：完成 game-server 房间服务入口角色主体迁移，`room_service.rs` 中房间加入、离开、准备、开始、结束、输入、移动、断线、重连、观战和匹配建房均使用 ticket 绑定 `character_id` 调用 room manager；日志和审计保留 `account_player_id` / `character_id` / `room_subject_id`；DB room event schema 收敛到角色主体字段并移除旧 `player_id` / `owner_player_id` 兼容写入。
- 验证记录：`cargo test core::service::room_service` 通过 12 项，`cargo test core::runtime::room_manager::tests` 通过 65 项，`cargo test core::room::tests` 通过 3 项，`cargo test core::system::movement` 通过 13 项，`cargo test core::system::combat` 通过 6 项；阶段 2 相关 `cargo test ticket::tests`、`cargo test session::tests`、`cargo test online_registry` 同轮复跑通过；`git diff --check` 通过；`cargo fmt --check` 仍失败于既有未格式化文件 `character_title_service.rs` 与生成的 `csv_code/*`，本阶段未扩大格式化改动。

- [x] 在 `room_service.rs` 中将房间入口的业务主体从 `identity.account_player_id` 改为 `identity.character_id`。（验证：`apps/game-server/src/core/service/room_service.rs:31`/`:32` 起各入口分离 `account_player_id` 与 `character_id`，room manager 调用点静态扫描显示玩法主体均传 `character_id`）
- [x] `handle_room_join` 使用 `character_id` 加入房间，日志同时记录 `account_player_id` 与 `character_id`。（验证：`apps/game-server/src/core/service/room_service.rs:52`-`:54` 日志记录账号/角色/subject，`:111`-`:113` 调用 `join_room` 传 `character_id`）
- [x] `handle_room_leave`、`handle_room_ready`、`handle_room_start`、`handle_room_end` 使用角色 ID 校验成员和房主权限。（验证：`apps/game-server/src/core/service/room_service.rs:247` 调用 `leave_room(&character_id)`，`:347` 调用 `set_ready_state(&character_id)`，`:431` 调用 `start_game(&character_id)`，`:1132` 调用 `end_game(&character_id)`）
- [x] `handle_player_input` 与 `handle_move_input` 使用角色 ID 做输入归属、重复帧检测、异常限流和 room manager 提交。（验证：`apps/game-server/src/core/service/room_service.rs:604`/`:822` 的 `accept_player_input` 传 `character_id`；`:937`-`:963` 重复帧记录以 `character_id` 为 key；`:986`-`:1054` 异常限流记录 `account_player_id` 与 `character_id`）
- [x] `handle_disconnect_cleanup` 断线时按角色 ID 标记离线，同时保留账号 ID 审计字段。（验证：`apps/game-server/src/core/service/room_service.rs:1192`-`:1196` 分离账号/角色，`:1212` 调用 `disconnect_room_member(&character_id)`，`:1218`-`:1230` 审计写入 `accountPlayerId`、`characterId`、`roomSubjectId`）
- [x] `handle_room_reconnect` 不再解析账号 `player_id`；直接使用 ticket 绑定 `character_id` 查离线房间并恢复连接。（验证：`apps/game-server/src/core/service/room_service.rs:1284` 调用 `find_reconnect_room_for_character(&character_id)`，`:1308` 调用 `reconnect_room(&character_id)`；静态扫描 `request.player_id` / `requestedPlayerId` / `find_room_by_offline_player` 无命中）
- [x] `handle_join_as_observer` 明确观战成员是否占用角色 ID；若 observer 也来自角色，则按角色 ID 建索引。（验证：`apps/game-server/src/core/service/room_service.rs:1488` 调用 `join_room_as_observer(&character_id)`，`:1526` 和 `:1568` 审计 details 写入 `observerUsesCharacterId: true`）
- [x] `handle_create_matched_room` 校验 `character_ids` 包含当前 `character_id`，不再用账号 ID 判断是否在匹配列表内。（验证：`apps/game-server/src/core/service/room_service.rs:1626` 校验 `character_ids.contains(&character_id)`，`:1710`-`:1714` 空列表错误码为 `EMPTY_CHARACTER_IDS`）
- [x] 更新 DB room event 写入，区分 `account_player_id`、`character_id`、`room_subject_id`。（验证：`apps/game-server/src/db_store.rs:26`-`:29` 定义 `room_subject_id` / `account_player_id` / `character_id` / `owner_character_id`，`:156`-`:191` 的 `append_room_event_with_identity` 按目标字段写入；旧 room event `player_id` / `owner_player_id` 静态扫描无命中）
- [x] 更新重连账号 mismatch 测试为角色 mismatch 测试，并补充“客户端不能冒用其他 character_id 重连”的用例。（验证：`apps/game-server/src/core/service/room_service.rs:2290` 新增 `reconnect_room_lookup_uses_authenticated_character_only`，覆盖只按认证角色查找离线房间；`cargo test core::service::room_service` 12 passed）

## 阶段 5：game-proxy 路由和接入层迁移

- 开始时间：2026-06-26 21:15:33 +08:00
- 结束时间：2026-06-26 21:35:56 +08:00
- 开发总结：完成 game-proxy 角色路由迁移，路由存储从 `PlayerRouteRecord/player_routes` 收敛为 `CharacterRouteRecord/character_routes`，reconnect 按已鉴权 `character_id` 选路，redirect/reconnect/observer 成功后绑定角色 route；admin 入口切换为 `/character-routes` 与 `/character-route/upsert` 并删除旧 `/player-route*` 兼容入口；账号级 auth、blocklist 和连接限制仍使用 `account_player_id`。
- 验证记录：`cargo fmt --check` 于 `apps/game-proxy` 通过；`cargo test` 于 `apps/game-proxy` 通过 151 项；`git diff --check` 通过，仅有 Git 行尾提示；静态扫描 `PlayerRouteRecord|player_routes|select_upstream_for_player|upsert_player_route|request.player_id|player_route_upsert` 无业务残留，旧 `/player-route*` 仅保留在删除验证测试字符串中。

- [x] 保持 proxy 鉴权、blocklist、连接替换、账号并发限制按 `account_player_id` 处理。（验证：`apps/game-proxy/src/proxy_server.rs:1018` blocklist 继续检查 `account_player_id`，`:1071` 连接替换继续使用 `account_player_id`，auth 响应 `player_id` 仍返回账号 ID）
- [x] 将用于房间重连和灰度切服的 `PlayerRouteRecord.player_id` 迁移为 `CharacterRouteRecord.character_id`。（验证：`apps/game-proxy/src/route_store.rs:155` 定义 `CharacterRouteRecord.character_id`，`:239` route store 使用 `character_routes`，旧 `PlayerRouteRecord` 静态扫描无残留）
- [x] 将 `select_upstream_for_player`、`upsert_player_route`、`remove_player_route` 等接口改为 character route 语义。（验证：`apps/game-proxy/src/route_store.rs:888` `list_character_routes`，`:973` `upsert_character_route`，`:1149` `select_upstream_for_character`；旧 API 名静态扫描无命中）
- [x] 将 admin HTTP `/player-route/*` 改名为 `/character-route/*`，直接删除旧入口，不提供兼容路由。（验证：`apps/game-proxy/src/admin_server.rs:389` 暴露 `/character-routes`，`:510` 暴露 `/character-route/upsert`，`:3103` 测试确认旧 `/player-routes` 和 `/player-route/upsert` 无 route requirement）
- [x] proxy 解析 `RoomReconnectReq` 时优先使用鉴权 session 中的 `character_id`，不再依赖客户端声明账号 ID。（验证：`apps/game-proxy/src/proxy_server.rs:1192`-`:1199` 只 decode 空 `RoomReconnectReq` 并用 authenticated `character_id` 调用 `select_upstream_for_character`；静态扫描 `request.player_id` 无命中）
- [x] redirect / reconnect 成功后绑定 room owner 和 character route，而不是 player route。（验证：`apps/game-proxy/src/proxy_server.rs:1222`、`:1243`、`:1268` 成功响应后 `bind_room_owner` 传 `deferred_auth.character_id`；`apps/game-proxy/src/route_store.rs:1112` 写入 `CharacterRouteRecord`）
- [x] route store Redis key、admin response、JSONL 审计字段同步改为角色语义。（验证：`apps/game-proxy/src/route_store.rs:1409` 持久化快照写 `character_routes`；`apps/game-proxy/src/admin_server.rs:36` 状态响应为 `character_route_count`，`:210` JSONL 审计字段为 `character_id`）
- [x] 更新 proxy 单测，覆盖 transferred room owner route、stale route、reconnect route 均按角色 ID 选择上游。（验证：`apps/game-proxy/src/route_store.rs:2093` `character_reconnect_prefers_transferred_room_owner_route`，`apps/game-proxy/src/proxy_server.rs:1996` `room_reconnect_packet_prefers_transferred_room_owner_over_stale_character_route`，`:2047` `room_reconnect_packet_does_not_share_route_between_characters`；`cargo test` 151 passed）
- [x] 运行 `cargo fmt --check`、`cargo test` 于 `apps/game-proxy`，并记录需要真实 proxy / game-server 联调的场景。（验证：`cargo fmt --check` 通过，`cargo test` 151 passed；真实联调待覆盖 auth-http ticket、proxy auth/blocklist/账号并发、room transfer/redirect 后空 `RoomReconnectReq`、同账号多角色 route 隔离、Redis route store 多实例同步、admin 新旧 route 行为）

## 阶段 6：match-service 迁移

- 开始时间：2026-06-26 21:38:35 +08:00
- 结束时间：2026-06-26 22:21:08 +08:00
- 开发总结：完成 match-service 参与者主键迁移，外部匹配请求、取消、状态查询、事件流、内部建房和进退房回调 payload 均使用 `character_id` / `character_ids`；匹配候选、任务成员、joined/active 集合、角色状态机、内存/Redis runtime store 快照和 Lua 操作同步收敛到角色语义；示例探针 CLI 改为 `--character-id`，保留 proto 既有 `PlayerJoined` / `PlayerLeft` RPC 名但不再承载账号字段。
- 验证记录：`cargo fmt --check` 于 `apps/match-service` 通过；`cargo test` 于 `apps/match-service` 通过 43 项；`git diff --check` 通过，仅有 Git 行尾提示；静态扫描 `player_id|player_ids|PlayerMatchContext|PlayerState|MatchCandidate\.player_id|player_status|player_context|player not found|PlayerMatchStatus|SharedPlayerState` 在 `apps/match-service/src` 和 `examples` 中仅剩 `apps/match-service/src/proto/myserver.game.rs:13 AuthRes.player_id` 账号语义；真实 Redis/game-server 联调未在本阶段启动，后续整体联调覆盖。

- [x] 将匹配请求、取消、状态查询、事件流中的参与者 ID 从 `player_id` 改为 `character_id`。（验证：`apps/match-service/src/service/match_service.rs:46`、`:91`、`:119`、`:151` 均读取 `req.character_id` 并注册角色事件流；示例 `apps/match-service/examples/match_flow_probe.rs` 改为 `--character-id` 和 `characterIds` 输出）
- [x] 将 `MatchCandidate.player_id`、`PlayerMatchContext`、`PlayerState`、runtime store Lua key 中的玩家语义迁移为角色语义。（验证：`apps/match-service/src/pool/candidate.rs:9` 定义 `MatchCandidate.character_id`；`apps/match-service/src/state/player_state.rs:15` / `:34` 定义 `CharacterMatchStatus` / `CharacterMatchContext`；`apps/match-service/src/runtime_store.rs:241` / `:243` 使用 `character_status` / `character_context`，`:546`-`:556` Lua 操作写入角色 key）
- [x] 将 `CreateMatchedRoomReq` 调用改为传 `character_ids`。（验证：`apps/match-service/src/game_server_client.rs:60` 构造 `CreateMatchedRoomReq`，`:63` 写入 `character_ids`；`apps/match-service/src/matcher/simple_matcher.rs:546` 调用 `create_matched_room` 传角色列表）
- [x] `player_joined` / `player_left` 匹配回调改为角色 ID，保证匹配任务、joined set、active set 不再按账号合并。（验证：`apps/match-service/src/service/match_service.rs:234` / `:268` 将 `req.character_id` 传入回调；`apps/match-service/src/pool/match_pool.rs:21`-`:24` 使用 `character_ids`、`joined_characters`、`active_characters`；`:350`-`:361` 和 `:386`-`:391` 按角色更新 joined/active 集合）
- [x] 如需要限制同账号多个角色同时匹配，新增独立的 `account_player_id` 字段或服务端账号索引，不复用 `character_id`。（验证：本阶段未新增账号级并发限制；`apps/match-service/src/matcher/simple_matcher.rs:1185` 的 `different_character_ids_are_distinct_match_participants` 覆盖不同 `character_id` 作为独立匹配参与者，未把角色 ID 复用为账号索引）
- [x] 更新 match-service proto 生成文件、runtime store 快照结构和相关测试数据。（验证：`apps/match-service/src/proto/myserver.matchservice.rs:8`、`:31`、`:48`、`:73`、`:99`、`:118`、`:137` 的生成结构体字段为 `character_id` / `character_ids`；`apps/match-service/src/runtime_store.rs:183`、`:187`、`:189`、`:241`、`:243` 快照结构使用角色字段；`cargo test` 43 passed）
- [x] 更新错误码和日志文案，从 `player not found` 等账号含混描述改为 `character not found` 或 `participant not found`。（验证：`apps/match-service/src/error.rs:19` 定义 `character not found: character_id={0}`，`:39` 返回 `CHARACTER_NOT_FOUND`；旧 `player not found` 静态扫描无命中）
- [x] 运行 `cargo fmt --check`、`cargo test` 于 `apps/match-service`。（验证：`cargo fmt --check` 通过；`cargo test` 通过 43 项；`git diff --check` 通过）

## 阶段 7：剩余角色级数据存储迁移

- 开始时间：2026-06-26 22:24:39 +08:00
- 结束时间：2026-06-26 23:43:13 +08:00
- 开发总结：完成剩余背包类角色级数据迁移，`PlayerData`、`PlayerManager`、PostgreSQL runtime schema 和 `db/init.sql` 从 `player_inventory` / `player_inventory_grants` 收敛为 `character_inventory` / `character_inventory_grants`，背包服务入口改用 ticket 绑定 `character_id` 读写，账号 ID 仅保留日志审计；GM 发物品、admin-api/admin-web 和 mail-service 附件发放目标切换为 `characterId`，踢人/封禁等账号级 GM 操作继续使用 `playerId`。
- 验证记录：`git diff --check` 通过，仅有 Git LF/CRLF 提示；`cargo test admin_server` 34 passed，`cargo test player_manager` 2 passed，`cargo test player_data` 2 passed，`cargo test inventory` 11 passed，`cargo test character_element` 8 passed，`cargo test character_title` 15 passed，`cargo test character_discipline` 3 passed；admin-api `node --test --experimental-test-isolation=none --test-concurrency=1 src/gm/gm.controller.test.js` 9 passed，`src/game-admin-client.test.js` 15 passed；mail-service `node --test --experimental-test-isolation=none --test-concurrency=1 --loader ts-node/esm src/game-admin-client.test.js` 13 passed，`src/mails/mails.service.test.ts src/mail-auth.test.js` 11 passed；静态扫描 `player_inventory|player_inventory_grants` 无残留，GM 发物品旧 `playerId` 扫描仅剩踢人/封禁账号级路径；`cargo fmt --check` 已运行但失败于既有未格式化范围 `character_title_service.rs` 与生成的 `csv_code/*`；真实 PostgreSQL 空库初始化因本机缺少 `docker` 和 `psql` 未运行。

- [x] 确认 P1/P2 已完成的 `characters`、`character_element_logs`、`character_disciplines`、`character_titles` 和 `character_title_logs` 已是 `character_id` 语义，本阶段不重复迁移这些表。（验证：`db/init.sql:322`、`:354`、`:381`、`:415` 均为 character 表；`cargo test character_element` 8 passed，`cargo test character_title` 15 passed，`cargo test character_discipline` 3 passed）
- [x] 明确背包、货币、装备、任务、位置、战斗实体等剩余角色级数据全部以 `character_id` 为主键。（验证：本阶段落地的剩余持久化玩法数据 `PlayerData.character_id` 与 `character_inventory.character_id` 已迁移；`rg "player_inventory|player_inventory_grants" apps db packages` 无残留）
- [x] 将 `inventory_service.rs` 中当前仍以 `identity.account_player_id` 作为背包目标的逻辑改为读取 `identity.character_id`，账号 ID 只保留为日志和审计字段。（验证：`apps/game-server/src/core/service/inventory_service.rs:22`/`:23` 分离账号与角色，`:48`、`:151`、`:229`、`:305`、`:379`、`:469` 均使用 `character_id` 读写 `player_manager`）
- [x] 评估并迁移 `PlayerManager`、`PlayerData`、`player_inventory`、`player_inventory_grants` 的表名、字段名和主键语义。（验证：`apps/game-server/src/core/inventory/player_data.rs:10` 定义 `character_id`；`apps/game-server/src/core/player/player_manager.rs:33` 起以 `character_id` 作为内存 key；`apps/game-server/src/core/player/db_player_store.rs:14` 和 `:30` 创建 `character_inventory` / `character_inventory_grants`）
- [x] 直接替换旧账号级背包表结构和初始化 SQL，不设计旧数据迁移策略、默认角色归属或转换脚本。（验证：`db/init.sql:441` 起直接创建 `character_inventory`，`:466` 起直接创建 `character_inventory_grants`；无新增旧表迁移脚本或双写分支）
- [x] GM 发物品、扣物品、查询背包等接口明确目标是账号还是角色；默认角色级操作使用 `character_id`。（验证：`packages/proto/admin.proto:33` `GrantItemsReq.character_id`；`apps/game-server/src/admin_server.rs:414` 使用 `character_target`，`:458` 发物品传 `request.character_id`；admin-api `sendItem` 审计 `targetType: "character"`；踢人/封禁仍保留 `GmKickPlayerReq` / `GmBanPlayerReq.playerId` 账号路径）
- [x] 数据库初始化 `db/init.sql`、runtime schema 创建、测试 SQL 断言同步改为角色级字段。（验证：`db/init.sql:441`-`:478` 与 `apps/game-server/src/core/player/db_player_store.rs:14`-`:40` 均使用 `character_inventory` / `character_inventory_grants` 和 `character_id`）
- [x] 补充同账号两个角色背包互相隔离的单元测试。（验证：`apps/game-server/src/core/player/player_manager.rs` 新增 `same_account_characters_keep_inventory_isolated`，`cargo test player_manager` 2 passed）
- [x] 复跑四属性、称号和职业阶位相关测试，确认身份迁移没有破坏已按 `character_id` 落地的 P1/P2 数据链路。（验证：`cargo test character_element` 8 passed，`cargo test character_title` 15 passed，`cargo test character_discipline` 3 passed）
- [x] 运行相关 Node DB 初始化测试、game-server inventory 定向测试；真实 PostgreSQL 空库初始化验证需先列出依赖并等待确认。（验证：`cargo test inventory` 11 passed；admin-api GM/client Node 测试与 mail-service client/auth/claim Node 测试均通过；真实 PostgreSQL 空库初始化因本机缺少 `docker` / `psql` 未运行，未启动外部 DB 服务）

## 阶段 8：战斗、移动、广播和 room transfer 清理

- 开始时间：2026-06-26 23:47:12 +08:00
- 结束时间：2026-06-27 00:29:15 +08:00
- 开发总结：完成 game-server movement、combat、广播投递和 room transfer 的角色语义收敛，movement correction 与 transfer runtime JSON 改为 `target_character_ids` / `*_by_character`，combat ECS 控制实体映射改为 `character_entity_map`，定向出站 API 改为 `send_to_character`，transfer 输入导出排序按 `frame_id + character_id + action + payload` 固定，movement/combat demo 与 ui touch room 的角色状态命名同步清理。
- 验证记录：`cargo test core::system::movement` 13 passed，`cargo test core::system::combat` 6 passed，`cargo test core::runtime::room_manager::tests` 65 passed，`cargo test admin_server` 34 passed；同阶段复跑过 `cargo test transfer_state` 7 passed 与 `cargo test room_transfer` 12 passed；`git diff --check` 通过，仅有 Git LF/CRLF 提示；`rustfmt --edition 2024 --check src/core/runtime/room_manager/tests.rs src/core/system/movement/state.rs` 通过；全量 `cargo fmt --check` 仍失败于既有未格式化范围 `character_title_service.rs` 与 `csv_code/*`，本阶段未格式化这些文件；静态扫描旧 `send_to_player` 无命中，旧 movement/combat transfer JSON key 仅保留在“确认旧字段不存在”的测试断言中；未启动真实 old/new/proxy/auth 多进程联调。

- [x] 将 movement state / correction 中的 `target_player_ids` 改为 `target_character_ids`。（验证：`apps/game-server/src/core/system/movement/state.rs:66` 定义 `MovementCorrectionEnvelope.target_character_ids`，`:530`/`:545`/`:563` 的 correction 构造入参均为角色列表；`apps/game-server/src/core/system/movement/correction.rs:141` 推送写入 `target_character_ids`）
- [x] 将 combat ECS 中表示玩家控制实体的 `player_id`、`player_entity_map` 改为 `character_id`、`character_entity_map`。（验证：`apps/game-server/src/core/system/combat/ecs.rs:285` transfer 字段为 `character_entity_map`，`:308` runtime map 为 `character_entity_map`，`:351` 提供 `entity_id_by_character`；`apps/game-server/src/core/system/combat/input.rs:17`/`:29` 使用 `targetCharacterId`）
- [x] 将广播目标、定向推送、房间成员出站句柄查找统一使用角色 ID。（验证：`apps/game-server/src/core/runtime/room_manager/broadcast.rs:93`/`:166` 使用 `target_character_ids`，`:195` 暴露 `send_to_character(character_id, ...)` 并按 `character_rooms` / `room.members.get(character_id)` 查找；旧 `send_to_player` 精确扫描无命中）
- [x] 将 room transfer payload 中 owner、members、pending inputs、input history、movement state、combat state 的 ID 语义全部切为角色 ID。（验证：`apps/game-server/src/core/runtime/room_manager/transfer_codec.rs:46` transfer 输入排序按 `character_id`，`apps/game-server/src/core/system/movement/state.rs:119`-`:121` transfer JSON 字段为 `*_by_character`，`apps/game-server/src/core/system/combat/ecs.rs:285` transfer JSON 字段为 `character_entity_map`；room owner/member 语义延续阶段 3 的 `owner_character_id` / `RoomMemberState.character_id`）
- [x] 更新 transfer checksum 排序字段，确保迁移后导出和导入两侧稳定一致。（验证：`apps/game-server/src/core/runtime/room_manager/transfer_codec.rs:46` `sort_frame_inputs` 使用 `frame_id`、`character_id`、`action`、`payload_json` 排序；`cargo test room_transfer` 12 passed，包含 `export_room_transfer_checksum_is_deterministic`）
- [x] 灰度 redirect / transfer / reconnect 链路确认 route owner、member id、offline index 都是角色 ID。（验证：`apps/game-server/src/core/runtime/room_manager/rollout.rs:134`/`:254` redirect/drain 出站上下文使用角色 subject；`cargo test core::runtime::room_manager::tests` 65 passed，包含 imported room reconnect、offline character index、trigger server redirect 相关测试）
- [x] 更新 combat / movement / transfer 单测，覆盖迁移后快照恢复和重连恢复。（验证：`apps/game-server/src/core/system/movement/state.rs:990` 起断言旧 movement JSON key 不存在并恢复 `*_by_character`；`apps/game-server/src/core/system/combat/ecs.rs:2086` 起断言旧 `player_entity_map` 不存在并恢复 `character_entity_map`；`apps/game-server/src/core/runtime/room_manager/tests.rs:1477`、`:1724` 覆盖 demo transfer payload；定向测试均通过）
- [x] 运行 game-server 相关定向测试；需要真实 old/new/proxy/auth 多进程联调时先提示依赖和启动顺序。（验证：`cargo test core::system::movement` 13 passed，`cargo test core::system::combat` 6 passed，`cargo test core::runtime::room_manager::tests` 65 passed，`cargo test admin_server` 34 passed；真实多进程联调未启动，后续整体联调需依赖 auth-http、game-proxy、old/new game-server 与 Redis service registry）

## 阶段 9：mock-client、文档和外部客户端同步

- 开始时间：2026-06-27 00:39:51 +08:00
- 结束时间：2026-06-27 02:32:28 +08:00
- 开发总结：完成 mock-client、registry e2e 和当前设计文档的角色身份同步；房间、重连、匹配、输入、movement/combat/transfer 输出和 proxy route 调试入口切换为 `characterId` / `character_id`；`AuthRes.player_id` 在 mock-client 中展示为 `accountPlayerId`；`RoomReconnectReq` 编码为空 body；相关协议、外部客户端、整体架构、角色体系、game-proxy 热切换、空房灰度、帧同步、延迟补偿和背包文档同步到账号/角色身份边界。
- 验证记录：`npm run check:proto` 通过并检查到 optional mybevy `project\src\game\myserver\protocol.rs` / `project\build.rs`；`node --test --experimental-test-isolation=none --test-concurrency=1 tests\mock-client-protocol.test.mjs tests\mock-client-character.test.mjs tests\server-redirect-reconnect.test.mjs tests\room-transfer-orchestrator.test.mjs` 40 passed；`node --test --experimental-test-isolation=none --test-concurrency=1 tests\registry-discovery-e2e.test.mjs` 6 passed，覆盖本地 NATS、match-service、game-server、game-proxy、auth-http service registry 联调；`git diff --check` 通过；静态扫描旧玩法字段仅剩 `tests/mock-client-protocol.test.mjs` 中断言 `playerIds` 参数不存在的正向测试。

- [x] 更新 `tools/mock-client`，房间、重连、匹配和输入链路使用 `characterId`，账号级场景仍使用 `playerId`。（验证：`tools/mock-client/src/messages.js` 的 `encodeRoomReconnectReq()` 编码为空包、`encodeCreateMatchedRoomReq(... characterIds ...)` 写入角色列表；`tools/mock-client/src/scenarios/room.js` 重连发送空 body 且 matched room 使用 `characterIds`；`tools/mock-client/src/scenarios/movement.js` / `combat.js` 按 `characterId`、`targetCharacterId`、`targetCharacterIds` 查找和输出玩法主体）
- [x] 更新 mock-client JSON 输出字段，清晰展示 `accountPlayerId` 与 `characterId`。（验证：`tools/mock-client/src/messages.js` 解码 `AuthRes.player_id` 为 `accountPlayerId`；`tools/mock-client/src/auth.js` 的 `formatLoginSummary()` 输出账号 `accountPlayerId`、游戏内 `characterId` 和 ticket payload 摘要；registry e2e 日志显示 `AuthRes.accountPlayerId` 与 snapshot `ownerCharacterId` / member `characterId`）
- [x] 更新或复核 mock-client 的 `character-elements-debug`、`character-titles-debug` 和 `character-disciplines-debug` 场景，确认它们仍通过 ticket 绑定角色执行，不新增请求体 `characterId` 冒用路径。（验证：`tools/mock-client/src/scenarios/character.js` 的 debug 场景仍先选角拿 character-bound ticket；`GetCharacterElementsReq` / `GetCharacterTitlesReq` / `GetCharacterDisciplinesReq` 无请求体角色注入；`tests/mock-client-character.test.mjs` 的 `character-elements-debug` 通过并断言响应 `characterId` 来自 ticket）
- [x] 更新 `docs/协议与客户端/协议设计.md`，明确游戏内协议以角色 ID 为主体。（验证：`docs/协议与客户端/协议设计.md` 说明 `AuthRes.player_id` 是账号玩家 ID，房间、匹配、输入、移动、战斗、背包和 transfer 使用 ticket-bound `characterId`；`RoomReconnectReq` 文档为空 body）
- [x] 更新 `docs/协议与客户端/外部客户端接入说明.md`，删除 P0 房间字段仍用账号 ID 的旧约束。（验证：`docs/协议与客户端/外部客户端接入说明.md` 明确 `RoomMember.character_id`、`RoomSnapshot.owner_character_id`、`FrameInput.character_id`、`MovementSnapshotPush.target_character_ids`、`CreateMatchedRoomReq.character_ids` 均为角色 ID，`RoomReconnectReq` 身份来自已鉴权 ticket）
- [x] 更新 `docs/游戏服与接入层/角色体系与四属性设计.md`，将 P0 兼容边界改为已迁移完成或新阶段目标态。（验证：`docs/游戏服与接入层/角色体系与四属性设计.md` 顶部现状和 P0 边界说明房间、匹配、输入、移动、战斗、背包和 transfer 已按角色 ID 建模，账号 `playerId` 仅用于登录、安全、封禁、kick/revoke、ticket version 和 mail ownership）
- [x] 更新 `docs/总览/整体架构.md` 中账号身份和角色身份边界说明。（验证：`docs/总览/整体架构.md` 的接入流程和身份边界说明 `playerId` 为账号玩家 ID、`characterId` 为游戏内角色 ID，game ticket 必须同时包含账号和角色身份）
- [x] 更新 mybevy 外部客户端对接说明；如果本仓库无法访问 `MYSERVER_CLIENT_ROOT`，记录待外部仓库同步项。（验证：`docs/协议与客户端/外部客户端接入说明.md` 已同步外部客户端接入语义；本轮未设置 `MYSERVER_CLIENT_ROOT` 且未修改外部仓库，`npm run check:proto` 仅检查到可读 optional mybevy 路径 `project\src\game\myserver\protocol.rs` / `project\build.rs`）
- [x] 运行 mock-client 协议测试、角色流程测试、重连测试；外部客户端真实联调需先等待用户确认服务和客户端路径。（验证：`npm run check:proto` 通过；mock-client 协议/角色/重连/transfer 编排 Node 测试 40 passed；`tests\registry-discovery-e2e.test.mjs` 6 passed，覆盖本地服务注册、多服务启动、匹配建房、进房、断线、空 body 重连和 character route admin；未对外部 mybevy 仓库执行真实客户端联调）

## 阶段 10：最终验收

- 开始时间：2026-06-27 02:38:25 +08:00
- 结束时间：2026-06-27 03:55:49 +08:00
- 开发总结：完成最终身份迁移验收和最后残留清理，静态扫描确认玩法主体旧账号字段只剩负向测试；NPC transfer JSON 从 `targetPlayerId` 收敛到 `targetCharacterId` 并拒绝旧 schema；robot sync room 测试断言改为 `recentInputs[].characterId`；最终服务端、代理、匹配、Node 周边和 registry e2e 验证均通过。
- 验证记录：worker 运行 `cargo test --no-run` 覆盖 `apps/game-server`、`apps/game-proxy`、`apps/match-service`；Rust 全量测试 `apps/game-server` 289 passed、`apps/game-proxy` 151 passed、`apps/match-service` 43 passed；Node 测试覆盖 `npm run check:proto`、mock-client 相关 30 passed、auth-http config 12 passed、auth/character/db init 68 passed、admin-api 111 passed、mail-service 49 passed、rollout/transfer/redirect 37 passed；`tests/registry-discovery-e2e.test.mjs` 6 passed，覆盖本地 NATS、match-service、game-server、game-proxy、auth-http、匹配建房、进房、断线、空 body 重连和 character route；主 agent 复跑 `cargo test room_transfer` 12 passed、`cargo test npc_transfer_json` 3 passed、`git diff --check` 通过。剩余风险：`tests/integration-flow.test.mjs` 是旧拓扑脚本，缺少 match-service / registry，当前 game-server 严格发现 `match-service.grpc` 时启动失败，未到输入回环；真实外部 mybevy 客户端和 mock-client 四属性/称号/职业 debug 场景未逐项启动服务实跑，已由协议检查、mock-client 单测和 Rust 服务测试覆盖主要路径。

- [x] 静态扫描确认房间、匹配、输入、背包、战斗、移动、transfer 中表示玩法主体的字段不再使用账号 `player_id` 语义。（验证：`rg` 扫描 `RoomMember.player_id|owner_player_id|target_player_ids|player_rooms|offline_players|player_entity_map|send_to_player|CreateMatchedRoomReq.player_ids|recentInputs.*playerId|targetPlayerId` 仅剩负向测试和旧路由删除验证；阶段 10 新增 NPC transfer 旧 `targetPlayerId` 拒绝测试）
- [x] 静态扫描确认账号、安全、ticket、封禁、踢账号、并发登录控制仍使用 `account_player_id`。（验证：账号身份扫描命中 `account_player_id`、ticket version、blocklist、kick/ban、logout、连接替换和账号限流链路，均为预期账号级控制）
- [x] 编译通过 `apps/game-server`、`apps/game-proxy`、`apps/match-service`。（验证：worker 分别在三个 Rust app 运行 `cargo test --no-run` 通过，仅保留既有 warning）
- [x] Rust 单元测试覆盖 game-server、game-proxy、match-service 的身份迁移关键路径。（验证：`apps/game-server` 全量 289 passed，`apps/game-proxy` 全量 151 passed，`apps/match-service` 全量 43 passed；主 agent 复跑 `cargo test room_transfer` 12 passed、`cargo test npc_transfer_json` 3 passed）
- [x] Node 测试覆盖 auth-http、mock-client、db 初始化和协议检查。（验证：`npm run check:proto` 通过；mock-client 相关 30 passed；auth-http config 12 passed；auth/character/db init 68 passed；admin-api 111 passed；mail-service 49 passed）
- [x] 人工或自动联调覆盖：登录、选角、AuthReq、进房、输入、断线、重连、匹配建房、redirect/transfer/reconnect。（验证：`tests/registry-discovery-e2e.test.mjs` 6 passed 覆盖登录、选角后 AuthReq、匹配建房、进房、断线、空 body 重连和 character route；rollout/transfer/redirect Node 测试 37 passed 覆盖 redirect/transfer/reconnect 编排）
- [x] 同账号两个不同角色的房间、背包、匹配和战斗状态互不覆盖。（验证：阶段 3 room member index 隔离测试、阶段 7 `same_account_characters_keep_inventory_isolated`、阶段 6 `different_character_ids_are_distinct_match_participants`、阶段 8 combat `character_entity_map` transfer 测试均通过）
- [x] 四属性查询 / debug、称号查询 / 装备 / debug、职业阶位查询 / debug 在身份迁移后仍只作用于 ticket 绑定角色，并通过 mock-client 或真实联调验证。（验证：阶段 7 `cargo test character_element` 8 passed、`cargo test character_title` 15 passed、`cargo test character_discipline` 3 passed；阶段 9 `tests/mock-client-character.test.mjs` 覆盖 character debug 场景且请求体不提供可冒用角色 ID）
- [x] 账号级封禁、踢人、logout、改密、ticket revoke 能通过角色上下文反查账号并正确生效。（验证：阶段 2 ticket owner/version、blocklist、账号踢旧和 online registry 测试通过；阶段 7 GM kick/ban 保留 `playerId` 账号路径，发物品等角色级 GM 改为 `characterId`）
- [x] 运行结果、未运行的真实服务联调原因和剩余风险写入阶段验收记录。（验证：本阶段验证记录列出全部通过命令，并记录旧 `tests/integration-flow.test.mjs` 拓扑不匹配、外部 mybevy 和部分 mock-client debug 场景未逐项真实服务联调的剩余风险）

## 最终完成定义

以下项目作为整体完成标准，不要求每个开发阶段都执行，由所有相关阶段完成后统一验收。

- 开始时间：2026-06-26 19:01:27 +08:00
- 结束时间：2026-06-27 03:55:49 +08:00
- 验收总结：服务端角色身份主键迁移已按目标态完成，账号级控制链路保留 `account_player_id`，玩法主体链路收敛为 `character_id`；协议、运行时、数据库、GM/admin、mock-client、文档和多服务 registry e2e 均已同步验证。未纳入最终通过口径的是旧拓扑 `tests/integration-flow.test.mjs` 和外部真实 mybevy 客户端实机联调，原因和风险已在阶段 10 记录。

- [x] 服务端账号身份和角色身份边界清晰：账号控制只用 `account_player_id`，游戏内主体只用 `character_id`。（验证：最终静态扫描和阶段 10 账号/玩法身份扫描均通过）
- [x] 协议、运行时索引、数据库字段、日志审计、mock-client 和文档对身份语义的描述一致。（验证：阶段 1/3/4/7/9 分别完成 proto、room runtime、DB room event、character inventory、mock-client 和 docs 同步）
- [x] 客户端选择角色后，后续房间、匹配、输入、重连和角色数据链路均以 `character_id` 为准。（验证：registry e2e 6 passed 覆盖登录选角、AuthReq、匹配建房、进房、断线和重连；mock-client 协议测试确认 `RoomReconnectReq` 空 body 与 `CreateMatchedRoomReq.characterIds`）
- [x] 服务端可以从任意在线角色上下文反查 `account_player_id`，并执行账号级封禁、踢人、logout、改密和 ticket revoke。（验证：online registry 保留账号主索引和角色辅助索引，ticket owner/version、blocklist、kick/ban、logout 和账号连接替换链路测试/扫描通过）
- [x] 同账号多角色不会因为账号 ID 共用导致房间成员、匹配状态、背包数据、输入历史或重连索引互相覆盖。（验证：room member index、match participant、inventory isolation、input history/transfer 和 combat character map 测试通过）
- [x] 已完成的 P1 四属性和 P2 称号 / 职业阶位链路继续保持 `character_id` 绑定语义，查询、debug、日志和后台只读查询结果一致。（验证：`cargo test character_element`、`cargo test character_title`、`cargo test character_discipline` 通过，mock-client character debug 单测确认请求不接受冒用角色 ID）
