# 活动模块06：运营后台前端 Checklist

## 目标

实现 admin-web 活动控制面。公共页面负责列表、版本工作流、通用字段、阶段容器、预检、发布/下线和记录查询；登录奖励和随机抽奖的字段表单分别位于同级 types/ 目录中的独立文件。

## 基础原则

- [x] 前端不承担最终权限、时间、奖励、概率和配置合法性判断。（验收：公共编辑器只编辑/序列化输入，发布前调用 admin-api preflight；权限由路由与服务端 API 共同校验，类型 schema 仅用于表单契约。）
- [x] 类型表单通过 activity_type 注册和动态组件装配，不在公共页面写专属分支。（验收：`src/modules/activity/type-registry.js` 注册并动态加载两类类型，Activities 的筛选、模板和标签均消费 registry。）
- [x] 加载、保存、预检、发布、下线、空数据和失败状态可见。（验收：Activities 使用 skeleton/loading、empty、error/retry、确认弹窗、预检错误面板和 records 空/失败状态；16 个活动测试与 build 通过。）

## 引用文档

- [活动功能总纲](../活动功能总纲.md)：后台活动工作流、公共字段、类型组件和发布预检设计。
- [管理后台设计](../../../后台与运维/管理后台设计.md)：admin-web 与 admin-api 的控制面职责、权限和审计展示。
- [管理权限与高风险操作治理设计](../../../后台与运维/管理权限与高风险操作治理设计.md)：发布、下线和敏感操作的前端权限边界。
- [协议设计](../../../协议与客户端/协议设计.md)：前后端接口字段和协议兼容性约束。
- [协议版本协商与发布边界](../../../协议与客户端/协议版本协商与发布边界.md)：接口版本和发布兼容要求。

## 阶段 1：列表与版本工作流

- 开始时间：2026-08-21 18:42:36 +08:00
- 结束时间：2026-08-21 18:51:11 +08:00
- 开发总结：完成活动控制面列表、草稿及版本操作入口，接入权限、路由和错误状态。
- 验证记录：`node --test src/api/activity.test.js src/router/active-menu.test.js src/auth/permissions.test.js`（6/6）；`npm run build` 通过；`git diff --check` 通过。

- [x] 实现活动列表、状态/类型/时间筛选和分页。（验证：`apps/admin-web/src/views/Activities.vue` 提供状态、类型、标识、日期筛选及分页；`src/api/activity.test.js` 覆盖列表归一化与时间过滤。）
- [x] 实现草稿创建、编辑、预览、保存、发布和下线。（验证：`apps/admin-web/src/views/Activities.vue` 接入 `activityApi` 的 create/detail/update/preflight/publish/offline 工作流与抽屉编辑器。）
- [x] 展示版本、配置摘要、发布时间、发布人和下线原因。（验证：活动列表/详情表格展示版本、配置摘要、发布时间、发布人及下线原因字段，并兼容后端可选字段。）
- [x] 发布、下线和未保存变更有明确确认和错误反馈。（验证：活动视图使用 `ElMessageBox` 处理发布/下线/未保存确认，`activityError` 映射权限、版本冲突、网络失败；定向测试与 build 通过。）

## 阶段 2：公共编辑器与类型组件

- 开始时间：2026-08-21 18:52:10 +08:00
- 结束时间：2026-08-21 19:03:47 +08:00
- 开发总结：完成 schema 驱动的公共活动编辑器、通用结构容器和两类动态类型编辑器。
- 验证记录：13 个 admin-web 定向测试通过；`npm run build` 通过（仅既有 chunk size 警告）；`git diff --check` 通过。

- [x] 实现标题、资源、时间、时区、领取方式和公共状态字段。（验证：`apps/admin-web/src/views/Activities.vue` 公共表单覆盖 title/resources/startAt/endAt/claimDeadline/timezone/claimMode/status。）
- [x] 实现阶段、奖励组和奖励项通用编辑容器。（验证：`src/modules/activity/ActivityStructureEditor.vue` 与 `structure-utils.js` 支持增删改及空状态，selectionMode 对齐 fixed/weighted 合约。）
- [x] 按类型注册动态加载 `types/login_reward.ts` 与 `types/lottery.ts`。（验证：`src/modules/activity/type-registry.js` 提供 registry、schema、异步 editor/module loader；registry 测试覆盖两类动态映射。）
- [x] 公共组件只消费类型 schema，不读写类型内部状态。（验证：公共结构组件仅通过通用 props/emits 工作，类型专属字段分别位于 `LoginRewardTypeEditor.vue`、`LotteryTypeEditor.vue`；13 个定向测试和 build 通过。）

## 阶段 3：预检、审计与记录

- 开始时间：2026-08-21 19:05:28 +08:00
- 结束时间：2026-08-21 19:11:08 +08:00
- 开发总结：接入服务端预检、字段级建议、活动记录查询和操作状态展示。
- 验证记录：6 个 admin-web 定向测试通过；`npm run build` 通过（仅既有 chunk size 警告）；`git diff --check` 通过。

- [x] 展示服务端字段级预检错误和修复建议。（验证：`src/api/activity-records.js` 归一化 422 details 并生成 suggestion，Activities 预检面板展示 path/code/message/suggestion；Axios 嵌套 details 回归测试通过。）
- [x] 实现领奖、抽奖、发奖和审计记录查询。（验证：Activities 记录面板接入 `activityApi.records`，支持 status/characterId/version/from/to/requestId 与分页，并展示 recordType、详情 JSON 和审计相关结果字段。）
- [x] 展示处理中、可重试、容量不足、人工复核和永久失败。（验证：`ACTIVITY_RECORD_STATUS_LABELS` 与 tag 映射覆盖 pending/processing、retryable、capacity 别名、reconciliation/manual_review、permanent_failure，未知状态保留原值。）
- [x] 处理权限不足、版本冲突和网络失败。（验证：活动列表/详情/预检/记录错误均通过 `activityError` 显示权限、409、503/网络错误，预检和记录面板提供重试/刷新入口。）

## 阶段 4：前端验证

- 开始时间：2026-08-21 19:12:15 +08:00
- 结束时间：2026-08-21 19:15:19 +08:00
- 开发总结：补齐列表/空态/错误/dirty 草稿/预检/记录/类型装配测试，并完成管理分辨率静态布局约束检查。
- 验证记录：全活动定向测试 16/16；`npm run build` 通过（仅既有 chunk size 警告）；`git diff --check` 通过；`package.json` 无 lint 或通用 test script，未伪造不存在的命令。

- [x] 覆盖列表、编辑、预检失败、发布、下线和空状态。（验证：`src/api/activity.test.js` 覆盖空列表、dirty 草稿、版本命令与 403/503；`activity-records.test.js` 覆盖预检失败和记录状态；Activities 提供发布/下线确认与错误面板。）
- [x] 覆盖两个类型组件的动态装配。（验证：`type-registry.test.js`、login_reward/lottery 类型测试及 build 均通过，registry 实际动态加载同级类型模块。）
- [x] 验证管理分辨率下表格、表单、弹窗和错误文本布局。（验证：Activities/结构编辑器/类型编辑器 CSS 在 1280/1024 管理宽度及移动断点下启用 flex-wrap、overflow-wrap:anywhere、单列降级和稳定表格布局。）
- [x] 运行 admin-web build/lint/test 命令并记录结果。（验证：16/16 定向 node tests 与 `npm run build` 通过；项目无 lint/通用 test script，已明确记录。）

## 最终完成定义

- 开始时间：2026-08-21 19:15:19 +08:00
- 结束时间：2026-08-21 19:16:51 +08:00
- 验收总结：阶段 1-4 实现、审查、提交和完整回归均完成；活动控制面支持两种类型的创建、编辑、预检、发布、下线和记录查询。

- [x] 运营可以不改代码完成两种活动创建、预检、发布和下线。（验收：Activities 活动控制面接入列表、草稿、公共编辑器、动态类型表单、预检、发布/下线确认和记录面板；阶段 1-4 测试/build 全部通过。）
- [x] 公共页面没有登录或抽奖专属规则分支。（验收：Activities 类型选项/标签/模板均来自 `type-registry.js`，公共结构编辑器只通过通用 props/emits 工作；`97a223ad` 收敛动态装配。）
- [x] 类型表单与同级类型代码文件一一对应。（验收：registry 的 `module()` 分别动态加载 `types/login_reward.ts` 与 `types/lottery.ts`，并装配 `LoginRewardTypeEditor.vue`、`LotteryTypeEditor.vue`；registry/type tests 通过。）
