use super::*;

#[test]
fn rejects_missing_admin_token() {
    let request = "GET /status HTTP/1.1\r\nhost: 127.0.0.1\r\n\r\n";

    assert!(!is_authorized(request, TOKEN));
}

#[test]
fn accepts_bearer_admin_token() {
    let request = format!("GET /status HTTP/1.1\r\nauthorization: Bearer {TOKEN}\r\n\r\n");

    assert!(is_authorized(&request, TOKEN));
}

#[test]
fn accepts_x_admin_token() {
    let request = format!("GET /status HTTP/1.1\r\nx-admin-token: {TOKEN}\r\n\r\n");

    assert!(is_authorized(&request, TOKEN));
}

#[test]
fn write_token_allows_get_and_post_admin_requests() {
    let get = format!("GET /status HTTP/1.1\r\nauthorization: Bearer {TOKEN}\r\n\r\n");
    let post = format!("POST /maintenance/on HTTP/1.1\r\nauthorization: Bearer {TOKEN}\r\n\r\n");

    assert_eq!(
        authorize_method(&get, "GET", &auth_config()),
        Ok(AdminPermission::Write)
    );
    assert_eq!(
        authorize_method(&post, "POST", &auth_config()),
        Ok(AdminPermission::Write)
    );
}

#[test]
fn read_token_allows_get_admin_requests() {
    let request = format!("GET /status HTTP/1.1\r\nauthorization: Bearer {READ_TOKEN}\r\n\r\n");

    assert_eq!(
        authorize_method(&request, "GET", &auth_config()),
        Ok(AdminPermission::Read)
    );
}

#[test]
fn read_token_rejects_post_admin_requests_with_forbidden() {
    let request =
        format!("POST /maintenance/on HTTP/1.1\r\nauthorization: Bearer {READ_TOKEN}\r\n\r\n");

    assert_eq!(
        authorize_method(&request, "POST", &auth_config()),
        Err((403, "insufficient admin permission"))
    );
}

#[test]
fn read_token_rejects_non_get_admin_requests_with_forbidden() {
    let request = format!("DELETE /rollout HTTP/1.1\r\nauthorization: Bearer {READ_TOKEN}\r\n\r\n");

    assert_eq!(
        authorize_method(&request, "DELETE", &auth_config()),
        Err((403, "insufficient admin permission"))
    );
}

#[test]
fn scoped_maintenance_token_allows_only_maintenance_writes() {
    let token = "maintenance-token";
    let config = scoped_auth_config(token, vec![AdminPermissionScope::MaintenanceWrite]);
    let request = format!("POST /maintenance/on HTTP/1.1\r\nauthorization: Bearer {token}\r\n\r\n");
    let maintenance = admin_route_requirement("POST", "/maintenance/on").unwrap();
    let rollout = admin_route_requirement("POST", "/rollout/start").unwrap();
    let route = admin_route_requirement("POST", "/room-route/upsert").unwrap();
    let switch = admin_route_requirement("POST", "/switch/game-server-2").unwrap();

    assert_eq!(
        authorize_route(&request, maintenance, &config),
        Ok(AdminPermission::Scoped(vec![
            AdminPermissionScope::MaintenanceWrite
        ]))
    );
    assert_eq!(
        authorize_route(&request, rollout, &config),
        Err((403, "insufficient admin permission"))
    );
    assert_eq!(
        authorize_route(&request, route, &config),
        Err((403, "insufficient admin permission"))
    );
    assert_eq!(
        authorize_route(&request, switch, &config),
        Err((403, "insufficient admin permission"))
    );
}

#[test]
fn scoped_rollout_token_allows_rollout_but_not_route_writes() {
    let token = "rollout-token";
    let config = scoped_auth_config(token, vec![AdminPermissionScope::RolloutWrite]);
    let request = format!("POST /rollout/start HTTP/1.1\r\nx-admin-token: {token}\r\n\r\n");
    let rollout_start = admin_route_requirement("POST", "/rollout/start").unwrap();
    let rollout_end = admin_route_requirement("POST", "/rollout/end").unwrap();
    let rollout_complete = admin_route_requirement("POST", "/rollout/complete-if-drained").unwrap();
    let route = admin_route_requirement("POST", "/character-route/upsert").unwrap();

    assert!(authorize_route(&request, rollout_start, &config).is_ok());
    assert!(authorize_route(&request, rollout_end, &config).is_ok());
    assert!(authorize_route(&request, rollout_complete, &config).is_ok());
    assert_eq!(
        authorize_route(&request, route, &config),
        Err((403, "insufficient admin permission"))
    );
}

#[test]
fn scoped_route_token_allows_route_writes_and_switch_only() {
    let token = "route-token";
    let config = scoped_auth_config(token, vec![AdminPermissionScope::RouteWrite]);
    let request =
        format!("POST /room-route/upsert HTTP/1.1\r\nauthorization: Bearer {token}\r\n\r\n");
    let room_route = admin_route_requirement("POST", "/room-route/upsert").unwrap();
    let character_route = admin_route_requirement("POST", "/character-route/upsert").unwrap();
    let switch = admin_route_requirement("POST", "/switch/game-server-2").unwrap();
    let maintenance = admin_route_requirement("POST", "/maintenance/off").unwrap();

    assert!(authorize_route(&request, room_route, &config).is_ok());
    assert!(authorize_route(&request, character_route, &config).is_ok());
    assert!(authorize_route(&request, switch, &config).is_ok());
    assert_eq!(
        authorize_route(&request, maintenance, &config),
        Err((403, "insufficient admin permission"))
    );
}

#[test]
fn scoped_read_and_wildcard_permissions_work() {
    let read_token = "scoped-read-token";
    let read_config = scoped_auth_config(read_token, vec![AdminPermissionScope::Read]);
    let read_request =
        format!("GET /status HTTP/1.1\r\nauthorization: Bearer {read_token}\r\n\r\n");
    let read = admin_route_requirement("GET", "/status").unwrap();
    let maintenance = admin_route_requirement("POST", "/maintenance/on").unwrap();

    assert!(authorize_route(&read_request, read, &read_config).is_ok());
    assert_eq!(
        authorize_route(&read_request, maintenance, &read_config),
        Err((403, "insufficient admin permission"))
    );

    let all_token = "all-token";
    let all_config = scoped_auth_config(all_token, vec![AdminPermissionScope::All]);
    let all_request =
        format!("POST /maintenance/on HTTP/1.1\r\nx-admin-token: {all_token}\r\n\r\n");

    assert!(authorize_route(&all_request, read, &all_config).is_ok());
    assert!(authorize_route(&all_request, maintenance, &all_config).is_ok());
}

#[test]
fn rejects_admin_token_from_query_string() {
    let request = format!("GET /status?admin_token={TOKEN} HTTP/1.1\r\nhost: 127.0.0.1\r\n\r\n");

    assert!(!is_authorized(&request, TOKEN));
    assert_eq!(authorize(&request, &auth_config()), None);
}

#[test]
fn rejects_query_admin_token_even_with_valid_header() {
    let request = format!(
        "GET /status?proxy_admin_token=ignored HTTP/1.1\r\nauthorization: Bearer {TOKEN}\r\n\r\n"
    );

    assert!(!is_authorized(&request, TOKEN));
}

#[test]
fn rejects_empty_configured_admin_token() {
    let request = "GET /status HTTP/1.1\r\nauthorization: Bearer \r\n\r\n";

    assert!(!is_authorized(request, ""));
}
