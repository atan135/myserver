use std::collections::HashMap;

use crate::route_store::RoomMigrationState;

const MAX_ID_LEN: usize = 128;

pub(super) fn required<'a>(query: &'a HashMap<String, String>, key: &str) -> Option<&'a str> {
    query
        .get(key)
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

pub(super) fn required_identifier<'a>(
    query: &'a HashMap<String, String>,
    key: &'static str,
) -> Result<&'a str, &'static str> {
    let Some(value) = required(query, key) else {
        return Err(missing_field_error(key));
    };
    validate_identifier(key, value)
}

pub(super) fn optional_identifier(
    query: &HashMap<String, String>,
    key: &'static str,
) -> Result<Option<String>, &'static str> {
    let Some(value) = query.get(key).map(String::as_str).map(str::trim) else {
        return Ok(None);
    };
    if value.is_empty() {
        return Ok(None);
    }
    validate_identifier(key, value).map(|value| Some(value.to_string()))
}

pub(super) fn validate_identifier<'a>(
    key: &'static str,
    value: &'a str,
) -> Result<&'a str, &'static str> {
    if value.is_empty() {
        return Err(missing_field_error(key));
    }
    if value.len() > MAX_ID_LEN {
        return Err(field_too_long_error(key));
    }
    if !value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':' | b'@')
    }) {
        return Err(invalid_identifier_error(key));
    }
    Ok(value)
}

pub(super) fn optional_bounded_text(
    query: &HashMap<String, String>,
    key: &'static str,
    max_len: usize,
) -> Result<Option<String>, &'static str> {
    let Some(value) = query.get(key).map(String::as_str).map(str::trim) else {
        return Ok(None);
    };
    if value.is_empty() {
        return Ok(None);
    }
    if value.len() > max_len {
        return Err(field_too_long_error(key));
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_graphic() && !matches!(byte, b'&' | b'?' | b'#'))
    {
        return Err(invalid_identifier_error(key));
    }
    Ok(Some(value.to_string()))
}

pub(super) fn optional_u32(
    query: &HashMap<String, String>,
    key: &'static str,
) -> Result<Option<u32>, &'static str> {
    let Some(value) = query.get(key).map(String::as_str).map(str::trim) else {
        return Ok(None);
    };
    if value.is_empty() {
        return Ok(None);
    }
    value
        .parse::<u32>()
        .map(Some)
        .map_err(|_| invalid_number_error(key))
}

pub(super) fn optional_u64(
    query: &HashMap<String, String>,
    key: &'static str,
) -> Result<Option<u64>, &'static str> {
    let Some(value) = query.get(key).map(String::as_str).map(str::trim) else {
        return Ok(None);
    };
    if value.is_empty() {
        return Ok(None);
    }
    value
        .parse::<u64>()
        .map(Some)
        .map_err(|_| invalid_number_error(key))
}

pub(super) fn optional_migration_state(
    query: &HashMap<String, String>,
    key: &'static str,
) -> Result<Option<RoomMigrationState>, &'static str> {
    let Some(value) = query.get(key).map(String::as_str).map(str::trim) else {
        return Ok(None);
    };
    if value.is_empty() {
        return Ok(None);
    }
    RoomMigrationState::parse(value)
        .map(Some)
        .ok_or("invalid migration_state")
}

fn missing_field_error(key: &str) -> &'static str {
    match key {
        "rollout_epoch" => "missing rollout_epoch",
        "old_server_id" => "missing old_server_id",
        "new_server_id" => "missing new_server_id",
        "server_id" => "missing server_id",
        "room_id" => "missing room_id",
        "owner_server_id" => "missing owner_server_id",
        "character_id" => "missing character_id",
        _ => "missing required field",
    }
}

fn field_too_long_error(key: &str) -> &'static str {
    match key {
        "rollout_epoch" => "rollout_epoch too long",
        "old_server_id" => "old_server_id too long",
        "new_server_id" => "new_server_id too long",
        "server_id" => "server_id too long",
        "room_id" => "room_id too long",
        "owner_server_id" => "owner_server_id too long",
        "character_id" => "character_id too long",
        "current_room_id" => "current_room_id too long",
        "preferred_server_id" => "preferred_server_id too long",
        "last_transfer_checksum" => "last_transfer_checksum too long",
        "expected_last_transfer_checksum" => "expected_last_transfer_checksum too long",
        _ => "field too long",
    }
}

fn invalid_identifier_error(key: &str) -> &'static str {
    match key {
        "rollout_epoch" => "invalid rollout_epoch",
        "old_server_id" => "invalid old_server_id",
        "new_server_id" => "invalid new_server_id",
        "server_id" => "invalid server_id",
        "room_id" => "invalid room_id",
        "owner_server_id" => "invalid owner_server_id",
        "character_id" => "invalid character_id",
        "current_room_id" => "invalid current_room_id",
        "preferred_server_id" => "invalid preferred_server_id",
        "last_transfer_checksum" => "invalid last_transfer_checksum",
        "expected_last_transfer_checksum" => "invalid expected_last_transfer_checksum",
        _ => "invalid identifier",
    }
}

fn invalid_number_error(key: &str) -> &'static str {
    match key {
        "member_count" => "invalid member_count",
        "online_member_count" => "invalid online_member_count",
        "empty_since_ms" => "invalid empty_since_ms",
        "room_version" => "invalid room_version",
        "expected_room_version" => "invalid expected_room_version",
        _ => "invalid number",
    }
}
