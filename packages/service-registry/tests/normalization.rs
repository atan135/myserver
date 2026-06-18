use service_registry::{ServiceEndpoint, ServiceInstance, SERVICE_INSTANCE_SCHEMA_VERSION};

fn endpoint<'a>(instance: &'a ServiceInstance, name: &str) -> Option<&'a ServiceEndpoint> {
    instance
        .endpoints
        .iter()
        .find(|endpoint| endpoint.name == name)
}

#[test]
fn v1_instance_maps_legacy_fields_to_endpoints() {
    let json = r#"{
        "id": "game-001",
        "name": "game-server",
        "host": "127.0.0.1",
        "port": 7000,
        "admin_port": 7500,
        "local_socket": "game-server.sock",
        "weight": 100,
        "metadata": {},
        "registered_at": 1710000000,
        "healthy": true
    }"#;

    let instance = serde_json::from_str::<ServiceInstance>(json)
        .expect("v1 instance")
        .normalized();

    assert_eq!(instance.schema_version, SERVICE_INSTANCE_SCHEMA_VERSION);

    let client = endpoint(&instance, "client").expect("client endpoint");
    assert_eq!(client.protocol, "tcp");
    assert_eq!(client.host, "127.0.0.1");
    assert_eq!(client.port, 7000);
    assert_eq!(client.visibility, "public");

    let admin = endpoint(&instance, "admin").expect("admin endpoint");
    assert_eq!(admin.host, "127.0.0.1");
    assert_eq!(admin.port, 7500);
    assert_eq!(admin.visibility, "admin");

    let socket = endpoint(&instance, "local_socket").expect("local socket endpoint");
    assert_eq!(socket.protocol, "local_socket");
    assert_eq!(socket.socket, "game-server.sock");
}

#[test]
fn v2_endpoint_normalization_keeps_valid_endpoints_without_legacy_backfill() {
    let json = r#"{
        "schema_version": 2,
        "id": "chat-001",
        "name": "chat-server",
        "host": "10.0.0.2",
        "port": 9001,
        "endpoints": [
            {
                "name": "grpc",
                "protocol": "",
                "host": "10.0.0.2",
                "port": 9101,
                "socket": "",
                "visibility": "",
                "metadata": null,
                "healthy": true
            }
        ],
        "healthy": true
    }"#;

    let instance = serde_json::from_str::<ServiceInstance>(json)
        .expect("v2 instance")
        .normalized();

    let grpc = endpoint(&instance, "grpc").expect("grpc endpoint");
    assert_eq!(grpc.protocol, "tcp");
    assert_eq!(grpc.visibility, "internal");
    assert!(grpc.metadata.is_object());

    assert!(endpoint(&instance, "client").is_none());
    assert!(endpoint(&instance, "admin").is_none());
    assert!(endpoint(&instance, "local_socket").is_none());
}

#[test]
fn endpoint_normalization_accepts_supported_visibilities() {
    let json = r#"{
        "schema_version": 2,
        "id": "visibility-001",
        "name": "visibility-service",
        "host": "127.0.0.1",
        "port": 9000,
        "endpoints": [
            {
                "name": "public-http",
                "protocol": "http",
                "host": "127.0.0.1",
                "port": 9000,
                "socket": "",
                "visibility": "public",
                "metadata": {},
                "healthy": true
            },
            {
                "name": "internal-grpc",
                "protocol": "grpc",
                "host": "127.0.0.1",
                "port": 9001,
                "socket": "",
                "visibility": "internal",
                "metadata": {},
                "healthy": true
            },
            {
                "name": "admin-tcp",
                "protocol": "tcp",
                "host": "127.0.0.1",
                "port": 9002,
                "socket": "",
                "visibility": "admin",
                "metadata": {},
                "healthy": true
            },
            {
                "name": "local-socket",
                "protocol": "local_socket",
                "host": "",
                "port": 0,
                "socket": "visibility.sock",
                "visibility": "local",
                "metadata": {},
                "healthy": true
            }
        ],
        "healthy": true
    }"#;

    let instance = serde_json::from_str::<ServiceInstance>(json)
        .expect("visibility instance")
        .normalized();

    assert_eq!(
        endpoint(&instance, "public-http")
            .expect("public endpoint")
            .visibility,
        "public"
    );
    assert_eq!(
        endpoint(&instance, "internal-grpc")
            .expect("internal endpoint")
            .visibility,
        "internal"
    );
    assert_eq!(
        endpoint(&instance, "admin-tcp")
            .expect("admin endpoint")
            .visibility,
        "admin"
    );
    assert_eq!(
        endpoint(&instance, "local-socket")
            .expect("local endpoint")
            .visibility,
        "local"
    );
}

#[test]
fn invalid_endpoint_visibility_is_rejected_by_validation() {
    let invalid = ServiceEndpoint::tcp("private-http", "127.0.0.1", 9000, "private");
    assert!(!invalid.is_valid());

    let json = r#"{
        "schema_version": 2,
        "id": "invalid-visibility-001",
        "name": "invalid-visibility-service",
        "host": "127.0.0.1",
        "port": 9000,
        "endpoints": [
            {
                "name": "private-http",
                "protocol": "http",
                "host": "127.0.0.1",
                "port": 9000,
                "socket": "",
                "visibility": "private",
                "metadata": {},
                "healthy": true
            }
        ],
        "healthy": true
    }"#;

    let instance = serde_json::from_str::<ServiceInstance>(json)
        .expect("invalid visibility instance")
        .normalized();

    assert!(endpoint(&instance, "private-http").is_none());
}

#[test]
fn empty_endpoint_visibility_uses_protocol_default() {
    let json = r#"{
        "schema_version": 2,
        "id": "default-visibility-001",
        "name": "default-visibility-service",
        "host": "127.0.0.1",
        "port": 9000,
        "endpoints": [
            {
                "name": "tcp-default",
                "protocol": "tcp",
                "host": "127.0.0.1",
                "port": 9000,
                "socket": "",
                "visibility": "",
                "metadata": {},
                "healthy": true
            },
            {
                "name": "socket-default",
                "protocol": "local_socket",
                "host": "",
                "port": 0,
                "socket": "default.sock",
                "visibility": "",
                "metadata": {},
                "healthy": true
            }
        ],
        "healthy": true
    }"#;

    let instance = serde_json::from_str::<ServiceInstance>(json)
        .expect("default visibility instance")
        .normalized();

    assert_eq!(
        endpoint(&instance, "tcp-default")
            .expect("tcp endpoint")
            .visibility,
        "internal"
    );
    assert_eq!(
        endpoint(&instance, "socket-default")
            .expect("socket endpoint")
            .visibility,
        "local"
    );
}

#[test]
fn v2_explicit_empty_endpoints_does_not_use_legacy_backfill() {
    let json = r#"{
        "schema_version": 2,
        "id": "legacy-001",
        "name": "legacy-service",
        "host": "127.0.0.1",
        "port": 9000,
        "admin_port": 9001,
        "local_socket": "legacy.sock",
        "endpoints": [],
        "healthy": true
    }"#;

    let instance = serde_json::from_str::<ServiceInstance>(json)
        .expect("v2 empty endpoints instance")
        .normalized();

    assert!(instance.endpoints.is_empty());
}

#[test]
fn v2_missing_endpoints_does_not_use_legacy_backfill() {
    let json = r#"{
        "schema_version": 2,
        "id": "v2-missing",
        "name": "game-server",
        "host": "127.0.0.1",
        "port": 7000,
        "admin_port": 7500,
        "local_socket": "legacy.sock",
        "healthy": true
    }"#;

    let instance = serde_json::from_str::<ServiceInstance>(json)
        .expect("v2 missing endpoints instance")
        .normalized();

    assert!(instance.endpoints.is_empty());
}

#[test]
fn invalid_port_or_empty_socket_does_not_generate_valid_endpoint() {
    let json = r#"{
        "id": "bad-001",
        "name": "bad-service",
        "host": "127.0.0.1",
        "port": 0,
        "local_socket": "",
        "endpoints": [
            {
                "name": "bad-port",
                "host": "127.0.0.1",
                "port": 0,
                "healthy": true
            },
            {
                "name": "bad-socket",
                "socket": "   ",
                "healthy": true
            }
        ],
        "healthy": true
    }"#;

    let instance = serde_json::from_str::<ServiceInstance>(json)
        .expect("invalid endpoints instance")
        .normalized();

    assert!(endpoint(&instance, "client").is_none());
    assert!(endpoint(&instance, "local_socket").is_none());
    assert!(endpoint(&instance, "bad-port").is_none());
    assert!(endpoint(&instance, "bad-socket").is_none());
}

#[test]
fn missing_endpoint_is_none_after_normalization() {
    let instance = ServiceInstance::new(
        "game-001".to_string(),
        "game-server".to_string(),
        "127.0.0.1".to_string(),
        7000,
    );

    assert!(endpoint(&instance, "admin").is_none());
}

#[test]
fn v1_game_proxy_maps_legacy_fields_to_proxy_protocols() {
    let json = r#"{
        "id": "proxy-001",
        "name": "game-proxy",
        "host": "127.0.0.1",
        "port": 4000,
        "admin_port": 7101,
        "weight": 100,
        "metadata": {},
        "registered_at": 1710000000,
        "healthy": true
    }"#;

    let instance = serde_json::from_str::<ServiceInstance>(json)
        .expect("v1 proxy instance")
        .normalized();

    let client = endpoint(&instance, "client").expect("client endpoint");
    assert_eq!(client.protocol, "kcp");
    assert_eq!(client.host, "127.0.0.1");
    assert_eq!(client.port, 4000);
    assert_eq!(client.visibility, "public");

    let admin = endpoint(&instance, "admin").expect("admin endpoint");
    assert_eq!(admin.protocol, "http");
    assert_eq!(admin.host, "127.0.0.1");
    assert_eq!(admin.port, 7101);
    assert_eq!(admin.visibility, "admin");
}
