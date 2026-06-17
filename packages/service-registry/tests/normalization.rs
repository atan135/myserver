use service_registry::{SERVICE_INSTANCE_SCHEMA_VERSION, ServiceEndpoint, ServiceInstance};

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
