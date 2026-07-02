use crate::config::{AdminPermissionScope, AdminScopedTokenConfig};

#[derive(Clone)]
pub struct AdminAuthConfig {
    write_token: String,
    read_token: Option<String>,
    scoped_tokens: Vec<AdminScopedToken>,
}

#[derive(Clone)]
struct AdminScopedToken {
    token: String,
    permissions: Vec<AdminPermissionScope>,
}

impl AdminAuthConfig {
    #[cfg(test)]
    pub fn new(write_token: String, read_token: Option<String>) -> Self {
        Self::with_scoped_tokens(write_token, read_token, Vec::new())
    }

    pub fn with_scoped_tokens(
        write_token: String,
        read_token: Option<String>,
        scoped_tokens: Vec<AdminScopedTokenConfig>,
    ) -> Self {
        Self {
            write_token,
            read_token: read_token.filter(|token| !token.trim().is_empty()),
            scoped_tokens: scoped_tokens
                .into_iter()
                .filter(|entry| !entry.token.trim().is_empty())
                .map(|entry| AdminScopedToken {
                    token: entry.token,
                    permissions: entry.permissions,
                })
                .collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) enum AdminPermission {
    Read,
    Write,
    Scoped(Vec<AdminPermissionScope>),
}

impl AdminPermission {
    fn allows(&self, required: AdminPermissionScope) -> bool {
        match self {
            Self::Write => true,
            Self::Read => required == AdminPermissionScope::Read,
            Self::Scoped(permissions) => permissions
                .iter()
                .any(|permission| permission_grants(*permission, required)),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct AdminRouteRequirement {
    pub(super) permission: AdminPermissionScope,
    pub(super) action: &'static str,
    pub(super) is_write: bool,
}

pub(super) fn authorize(request: &str, auth_config: &AdminAuthConfig) -> Option<AdminPermission> {
    let write_token = auth_config.write_token.trim();
    if write_token.is_empty() {
        return None;
    }

    if request_contains_query_token(request) {
        return None;
    }

    let matches_write = request
        .lines()
        .skip(1)
        .take_while(|line| !line.is_empty())
        .any(|line| header_matches_token(line, write_token));
    if matches_write {
        return Some(AdminPermission::Write);
    }

    if let Some(read_token) = auth_config.read_token.as_deref().map(str::trim) {
        if !read_token.is_empty()
            && request
                .lines()
                .skip(1)
                .take_while(|line| !line.is_empty())
                .any(|line| header_matches_token(line, read_token))
        {
            return Some(AdminPermission::Read);
        }
    }

    auth_config
        .scoped_tokens
        .iter()
        .find(|entry| {
            let token = entry.token.trim();
            !token.is_empty()
                && request
                    .lines()
                    .skip(1)
                    .take_while(|line| !line.is_empty())
                    .any(|line| header_matches_token(line, token))
        })
        .map(|entry| AdminPermission::Scoped(entry.permissions.clone()))
}

pub(super) fn authorize_route<'a>(
    request: &str,
    route_requirement: AdminRouteRequirement,
    auth_config: &AdminAuthConfig,
) -> Result<AdminPermission, (u16, &'a str)> {
    let Some(permission) = authorize(request, auth_config) else {
        return Err((401, "missing or invalid admin token"));
    };

    if !permission.allows(route_requirement.permission) {
        return Err((403, "insufficient admin permission"));
    }

    Ok(permission)
}

pub(super) fn admin_route_requirement(
    method: &str,
    route_path: &str,
) -> Option<AdminRouteRequirement> {
    match (method, route_path) {
        ("GET", "/status")
        | ("GET", "/instances")
        | ("GET", "/rollout")
        | ("GET", "/room-routes")
        | ("GET", "/character-routes") => Some(AdminRouteRequirement {
            permission: AdminPermissionScope::Read,
            action: "admin_read",
            is_write: false,
        }),
        ("POST", "/maintenance/on") => Some(AdminRouteRequirement {
            permission: AdminPermissionScope::MaintenanceWrite,
            action: "maintenance_on",
            is_write: true,
        }),
        ("POST", "/maintenance/off") => Some(AdminRouteRequirement {
            permission: AdminPermissionScope::MaintenanceWrite,
            action: "maintenance_off",
            is_write: true,
        }),
        ("POST", "/rollout/start") => Some(AdminRouteRequirement {
            permission: AdminPermissionScope::RolloutWrite,
            action: "rollout_start",
            is_write: true,
        }),
        ("POST", "/rollout/end") => Some(AdminRouteRequirement {
            permission: AdminPermissionScope::RolloutWrite,
            action: "rollout_end",
            is_write: true,
        }),
        ("POST", "/rollout/state") => Some(AdminRouteRequirement {
            permission: AdminPermissionScope::RolloutWrite,
            action: "rollout_state",
            is_write: true,
        }),
        ("POST", "/rollout/complete-if-drained") => Some(AdminRouteRequirement {
            permission: AdminPermissionScope::RolloutWrite,
            action: "rollout_complete_if_drained",
            is_write: true,
        }),
        ("POST", "/room-route/upsert") => Some(AdminRouteRequirement {
            permission: AdminPermissionScope::RouteWrite,
            action: "room_route_upsert",
            is_write: true,
        }),
        ("POST", "/character-route/upsert") => Some(AdminRouteRequirement {
            permission: AdminPermissionScope::RouteWrite,
            action: "character_route_upsert",
            is_write: true,
        }),
        ("POST", route_path) if route_path.strip_prefix("/switch/").is_some() => {
            Some(AdminRouteRequirement {
                permission: AdminPermissionScope::RouteWrite,
                action: "switch",
                is_write: true,
            })
        }
        _ => None,
    }
}

pub(super) fn fallback_route_requirement(method: &str) -> AdminRouteRequirement {
    if method == "GET" {
        AdminRouteRequirement {
            permission: AdminPermissionScope::Read,
            action: "admin_read",
            is_write: false,
        }
    } else {
        AdminRouteRequirement {
            permission: AdminPermissionScope::Write,
            action: "admin_write",
            is_write: true,
        }
    }
}

fn permission_grants(granted: AdminPermissionScope, required: AdminPermissionScope) -> bool {
    match granted {
        AdminPermissionScope::All => true,
        AdminPermissionScope::Write => required != AdminPermissionScope::Read,
        _ => granted == required,
    }
}

#[allow(dead_code)]
pub(super) fn authorize_method<'a>(
    request: &str,
    method: &str,
    auth_config: &AdminAuthConfig,
) -> Result<AdminPermission, (u16, &'a str)> {
    authorize_route(request, fallback_route_requirement(method), auth_config)
}

#[cfg(test)]
pub(super) fn is_authorized(request: &str, admin_token: &str) -> bool {
    authorize(
        request,
        &AdminAuthConfig::new(admin_token.to_string(), None),
    )
    .is_some()
}

fn request_contains_query_token(request: &str) -> bool {
    let request_target = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or_default();
    let Some((_, query_string)) = request_target.split_once('?') else {
        return false;
    };

    query_string.split('&').any(|pair| {
        let key = pair.split_once('=').map(|(key, _)| key).unwrap_or(pair);
        key.eq_ignore_ascii_case("admin_token") || key.eq_ignore_ascii_case("proxy_admin_token")
    })
}

fn header_matches_token(line: &str, admin_token: &str) -> bool {
    let Some((name, value)) = line.split_once(':') else {
        return false;
    };
    let name = name.trim();
    let value = value.trim();

    if name.eq_ignore_ascii_case("authorization") {
        let Some(token) = value.strip_prefix("Bearer ") else {
            return false;
        };
        return token.trim() == admin_token;
    }

    name.eq_ignore_ascii_case("x-admin-token") && value == admin_token
}
