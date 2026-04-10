# 管理后台

## 概述

独立的管理后台系统，包含 API 服务和 Web 前端。

## 架构

```
apps/
├── admin-api/   # Node.js + Express API (端口 3001)
└── admin-web/   # Vue 3 + Element Plus 前端 (端口 3002)
```

## 快速启动

```powershell
# 安装依赖
npm install

# 初始化数据库
mysql -uroot -p < db/init.sql

# 启动 API 服务
npm run dev:admin-api

# 启动 Web 前端（新开终端）
npm run dev:admin-web
```

访问 `http://127.0.0.1:3002`

## 默认账号

| 用户名 | 密码 | 角色 |
|--------|------|------|
| admin | AdminPass123! | admin |

首次启动自动创建。

## 权限分级

| 角色 | 说明 |
|------|------|
| viewer | 查看状态、日志 |
| operator | viewer + 玩家管理 |
| admin | operator + 配置修改、用户管理 |

## 数据库表

### admin_accounts

| 字段 | 类型 | 说明 |
|------|------|------|
| id | BIGINT | 主键 |
| username | VARCHAR(64) | 用户名（唯一） |
| display_name | VARCHAR(64) | 显示名称 |
| password_hash | VARCHAR(256) | bcrypt 密码哈希 |
| role | VARCHAR(32) | 角色 |
| status | VARCHAR(32) | 状态 |
| created_at | DATETIME | 创建时间 |
| last_login_at | DATETIME | 最后登录 |

### admin_audit_logs

| 字段 | 类型 | 说明 |
|------|------|------|
| id | BIGINT | 主键 |
| admin_id | BIGINT | 管理员ID |
| admin_username | VARCHAR(64) | 管理员用户名 |
| action | VARCHAR(64) | 操作类型 |
| target_type | VARCHAR(32) | 目标类型 |
| target_value | VARCHAR(256) | 目标值 |
| details_json | JSON | 详情 |
| ip | VARCHAR(64) | IP地址 |
| created_at | DATETIME | 时间 |

## API 接口

### 认证

#### POST /api/v1/auth/login

管理员登录

```json
// Request
{
  "username": "admin",
  "password": "AdminPass123!"
}

// Response 200
{
  "ok": true,
  "accessToken": "eyJhbG...",
  "expiresIn": "8h",
  "admin": {
    "id": 1,
    "username": "admin",
    "displayName": "Administrator",
    "role": "admin"
  }
}
```

#### GET /api/v1/auth/me

获取当前管理员信息

```json
// Response 200
{
  "ok": true,
  "admin": {
    "id": 1,
    "username": "admin",
    "displayName": "Administrator",
    "role": "admin"
  }
}
```

#### POST /api/v1/auth/logout

登出

```json
// Response 200
{ "ok": true, "message": "Logged out" }
```

### 审计

#### GET /api/v1/audit-logs

查询管理操作审计日志

**Query Parameters:**

| 参数 | 类型 | 默认值 | 说明 |
|------|------|--------|------|
| limit | number | 50 | 每页数量（最大100） |
| offset | number | 0 | 偏移量 |
| action | string | - | 按操作类型筛选 |
| target_type | string | - | 按目标类型筛选 |

```json
// Response 200
{
  "ok": true,
  "logs": [
    {
      "id": 1,
      "admin_id": 1,
      "admin_username": "admin",
      "action": "admin_login",
      "target_type": null,
      "target_value": null,
      "details_json": null,
      "ip": "127.0.0.1",
      "created_at": "2026-04-10T12:00:00.000Z"
    }
  ],
  "total": 100,
  "limit": 50,
  "offset": 0
}
```

## 配置项

### admin-api (.env)

```env
PORT=3001                    # 服务端口
HOST=127.0.0.1             # 监听地址
NODE_ENV=development        # 环境

LOG_LEVEL=info              # 日志级别
LOG_ENABLE_CONSOLE=true    # 控制台输出
LOG_ENABLE_FILE=true       # 文件输出
LOG_DIR=logs/admin-api     # 日志目录

MYSQL_URL=mysql://root:password@127.0.0.1:3306/myserver_auth
MYSQL_POOL_SIZE=10

JWT_SECRET=your-jwt-secret  # JWT 密钥（必须修改）
JWT_EXPIRES_IN=8h          # Token 过期时间

ADMIN_USERNAME=admin        # 初始管理员用户名
ADMIN_PASSWORD=AdminPass123! # 初始管理员密码
ADMIN_DISPLAY_NAME=Administrator
```

## 安全说明

1. **JWT 认证**：所有管理接口需要 `Authorization: Bearer <token>` 头
2. **密码存储**：使用 bcrypt 加密，不可逆
3. **操作审计**：所有管理操作记录到 `admin_audit_logs`
4. **初始密码**：生产环境务必修改默认密码和 JWT_SECRET
