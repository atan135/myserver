use std::collections::HashMap;

use tracing::info;

use crate::core::character_discipline::{
    CharacterDiscipline, DisciplineDefinitionSummary, DisciplineItemCost,
    DisciplineOperationContext, DisciplineUpsert, LearnDisciplineRequest,
};
use crate::core::character_push::{CharacterPushSource, queue_character_push};
use crate::core::character_title::{
    CharacterTitle, EquipTitleOptions, GrantTitleRequest, TitleOperationContext,
};
use crate::core::character_title_unlock::{
    TitleUnlockCheckResult, TitleUnlockGrant, TitleUnlockTrigger,
};
use crate::core::context::{ConnectionContext, ServiceContext};
use crate::csv_code::titletable::{TitleTable, TitleTableRow};
use crate::pb::{
    AddCharacterDisciplinePointsReq, AddCharacterDisciplinePointsRes,
    CharacterDisciplineChangePush, CharacterDisciplineSummary, CharacterTitleChangePush,
    CharacterTitleDefinitionSummary, CharacterTitleSummary, DebugCharacterTitleReq,
    DebugCharacterTitleRes, DisciplineItemCost as PbDisciplineItemCost, EquipCharacterTitleReq,
    EquipCharacterTitleRes, GetCharacterDisciplinesRes, GetCharacterTitlesRes,
    LearnCharacterDisciplineReq, LearnCharacterDisciplineRes, SetCharacterDisciplineActiveReq,
    SetCharacterDisciplineActiveRes, SwitchCharacterDisciplineReq, SwitchCharacterDisciplineRes,
};
use crate::protocol::{MessageType, Packet};
use crate::session::AuthenticatedSessionIdentity;

const TITLE_DEBUG_SOURCE_TYPE: &str = "gm";
const TITLE_DEBUG_SOURCE_ID: &str = "debug-character-titles";
const TITLE_DEBUG_OPERATOR_TYPE: &str = "player_debug";
const TITLE_PLAYER_SOURCE_TYPE: &str = "player";
const TITLE_PLAYER_SOURCE_ID: &str = "character_title_protocol";
const DISCIPLINE_PLAYER_SOURCE_ID: &str = "character_discipline_learn_protocol";
const DISCIPLINE_ACTIVE_SOURCE_ID: &str = "character_discipline_active_protocol";
const DISCIPLINE_SWITCH_SOURCE_ID: &str = "character_discipline_switch_protocol";
const DISCIPLINE_POINTS_SOURCE_ID: &str = "character_discipline_points_protocol";
const DISCIPLINE_DEBUG_SOURCE_TYPE: &str = "gm";
const DISCIPLINE_DEBUG_SOURCE_ID: &str = "debug-character-disciplines";
const DISCIPLINE_DEBUG_OPERATOR_TYPE: &str = "player_debug";
const DEFAULT_TITLE_DEBUG_REASON: &str = "mock-client character title debug";

pub async fn handle_get_character_titles(
    services: &ServiceContext,
    connection: &ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(identity) = connection.ensure_authenticated_identity(packet.header.seq)? else {
        return Ok(());
    };

    info!(
        session_id = connection.session.id,
        account_player_id = %identity.account_player_id,
        player_id = %identity.account_player_id,
        character_id = %identity.character_id,
        world_id = ?identity.world_id,
        "handle_get_character_titles"
    );

    queue_expired_title_pushes(services, connection, &identity).await?;

    let response = match services
        .title_service
        .list_for_identity(&identity, player_title_context(&identity, "list"))
        .await
    {
        Ok(owned) => {
            let table = services
                .config_tables
                .tables_snapshot()
                .await
                .titletable
                .clone();
            let titles = to_title_summaries(&table, &owned);
            let equipped_title = titles.iter().find(|title| title.equipped).cloned();
            GetCharacterTitlesRes {
                ok: true,
                error_code: String::new(),
                character_id: identity.character_id,
                titles,
                equipped_title,
            }
        }
        Err(error) => GetCharacterTitlesRes {
            ok: false,
            error_code: error.error_code().to_string(),
            character_id: identity.character_id,
            titles: Vec::new(),
            equipped_title: None,
        },
    };

    connection.queue_message(
        MessageType::GetCharacterTitlesRes,
        packet.header.seq,
        response,
    )?;
    Ok(())
}

pub async fn handle_equip_character_title(
    services: &ServiceContext,
    connection: &ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(identity) = connection.ensure_authenticated_identity(packet.header.seq)? else {
        return Ok(());
    };

    let request = match packet.decode_body::<EquipCharacterTitleReq>("INVALID_EQUIP_TITLE_BODY") {
        Ok(value) => value,
        Err(error_code) => {
            queue_equip_title_response(
                connection,
                packet.header.seq,
                false,
                error_code,
                &identity.character_id,
                None,
            )?;
            return Ok(());
        }
    };

    let title_id = request.title_id.trim();
    if title_id.is_empty() {
        queue_equip_title_response(
            connection,
            packet.header.seq,
            false,
            "TITLE_ID_REQUIRED",
            &identity.character_id,
            None,
        )?;
        return Ok(());
    }

    info!(
        session_id = connection.session.id,
        account_player_id = %identity.account_player_id,
        player_id = %identity.account_player_id,
        character_id = %identity.character_id,
        world_id = ?identity.world_id,
        title_id,
        "handle_equip_character_title"
    );

    queue_expired_title_pushes(services, connection, &identity).await?;
    let before_equipped = services
        .title_service
        .equipped_for_identity(&identity, player_title_context(&identity, "equip_before"))
        .await?;

    let result = services
        .title_service
        .equip_for_identity(
            &identity,
            title_id,
            EquipTitleOptions::visible_only(),
            player_title_context(&identity, "equip"),
        )
        .await;

    match result {
        Ok(title) => {
            let table = services
                .config_tables
                .tables_snapshot()
                .await
                .titletable
                .clone();
            let title_summary = to_title_summary(&table, Some(&title), &title.title_id);
            queue_equip_title_response(
                connection,
                packet.header.seq,
                true,
                "",
                &identity.character_id,
                Some(title_summary.clone()),
            )?;
            let owned = services
                .title_service
                .list_for_identity(&identity, player_title_context(&identity, "push_snapshot"))
                .await?;
            if let Some(before) = before_equipped
                .as_ref()
                .filter(|before| before.title_id != title.title_id)
            {
                let unequipped_summary = to_title_summary(&table, Some(before), &before.title_id);
                queue_title_push_with_owned_snapshot(
                    services,
                    connection,
                    &identity,
                    "unequip",
                    TITLE_PLAYER_SOURCE_TYPE,
                    TITLE_PLAYER_SOURCE_ID,
                    "title unequipped",
                    Some(unequipped_summary),
                    &owned,
                    &table,
                )
                .await?;
            }
            queue_title_push_with_owned_snapshot(
                services,
                connection,
                &identity,
                "equip",
                TITLE_PLAYER_SOURCE_TYPE,
                TITLE_PLAYER_SOURCE_ID,
                "title equipped",
                Some(title_summary),
                &owned,
                &table,
            )
            .await?;
        }
        Err(error) => {
            queue_equip_title_response(
                connection,
                packet.header.seq,
                false,
                error.error_code(),
                &identity.character_id,
                None,
            )?;
        }
    }

    Ok(())
}

pub async fn handle_get_character_disciplines(
    services: &ServiceContext,
    connection: &ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(identity) = connection.ensure_authenticated_identity(packet.header.seq)? else {
        return Ok(());
    };

    info!(
        session_id = connection.session.id,
        account_player_id = %identity.account_player_id,
        player_id = %identity.account_player_id,
        character_id = %identity.character_id,
        world_id = ?identity.world_id,
        "handle_get_character_disciplines"
    );

    let response = match services
        .discipline_service
        .list_for_identity(&identity)
        .await
    {
        Ok(disciplines) => GetCharacterDisciplinesRes {
            ok: true,
            error_code: String::new(),
            character_id: identity.character_id,
            disciplines: disciplines.iter().map(to_discipline_summary).collect(),
        },
        Err(error) => GetCharacterDisciplinesRes {
            ok: false,
            error_code: error.error_code().to_string(),
            character_id: identity.character_id,
            disciplines: Vec::new(),
        },
    };

    connection.queue_message(
        MessageType::GetCharacterDisciplinesRes,
        packet.header.seq,
        response,
    )?;
    Ok(())
}

pub async fn handle_learn_character_discipline(
    services: &ServiceContext,
    connection: &ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(identity) = connection.ensure_authenticated_identity(packet.header.seq)? else {
        return Ok(());
    };

    let request =
        match packet.decode_body::<LearnCharacterDisciplineReq>("INVALID_LEARN_DISCIPLINE_BODY") {
            Ok(value) => value,
            Err(error_code) => {
                queue_learn_discipline_response(
                    connection,
                    packet.header.seq,
                    false,
                    error_code,
                    &identity.character_id,
                    None,
                    None,
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                )?;
                return Ok(());
            }
        };

    let discipline_id = request.discipline_id.trim();
    if discipline_id.is_empty() {
        queue_learn_discipline_response(
            connection,
            packet.header.seq,
            false,
            "DISCIPLINE_ID_REQUIRED",
            &identity.character_id,
            None,
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )?;
        return Ok(());
    }

    info!(
        session_id = connection.session.id,
        account_player_id = %identity.account_player_id,
        player_id = %identity.account_player_id,
        character_id = %identity.character_id,
        world_id = ?identity.world_id,
        discipline_id,
        "handle_learn_character_discipline"
    );

    let config_tables = services.config_tables.tables_snapshot().await;
    let definition = config_tables
        .disciplinetable
        .all()
        .iter()
        .find(|row| {
            config_tables
                .disciplinetable
                .resolve_string(row.disciplineid)
                .is_some_and(|value| value == discipline_id)
        })
        .map(|row| DisciplineDefinitionSummary::from_row(&config_tables.disciplinetable, row));
    let mut player_data = services
        .player_manager
        .get_or_create_player(&identity.character_id)
        .await;

    let result = services
        .discipline_service
        .learn_for_identity(
            &identity,
            LearnDisciplineRequest::new(discipline_id.to_string()),
            &config_tables.disciplinetable,
            &config_tables.item_table,
            &services.character_element_compatibility_service,
            &services.title_service,
            &mut player_data,
            services.config.max_learned_disciplines,
            player_discipline_context(&identity, "learn"),
        )
        .await;

    match result {
        Ok(result) => {
            if let Err(error) = services
                .player_manager
                .save_player(&identity.character_id, player_data)
                .await
            {
                queue_learn_discipline_response(
                    connection,
                    packet.header.seq,
                    false,
                    error.error_code(),
                    &identity.character_id,
                    None,
                    definition.map(to_discipline_definition_summary),
                    Vec::new(),
                    Vec::new(),
                    Vec::new(),
                )?;
                return Ok(());
            }
            let unlocked = run_discipline_unlock_check(
                services,
                &identity,
                result.discipline.discipline_id.as_str(),
            )
            .await?;
            let active_skill_pool = active_skill_pool(services, &identity).await?;
            let discipline_summary = to_discipline_summary(&result.discipline);
            queue_learn_discipline_response(
                connection,
                packet.header.seq,
                true,
                "",
                &identity.character_id,
                Some(discipline_summary.clone()),
                definition.map(to_discipline_definition_summary),
                result
                    .consumed_items
                    .iter()
                    .map(to_pb_discipline_item_cost)
                    .collect(),
                active_skill_pool.clone(),
                unlocked.clone(),
            )?;
            queue_discipline_push(
                services,
                connection,
                &identity,
                "learn",
                TITLE_PLAYER_SOURCE_TYPE,
                DISCIPLINE_PLAYER_SOURCE_ID,
                "discipline learned",
                Some(discipline_summary),
                Vec::new(),
                active_skill_pool,
                unlocked.clone(),
            )
            .await?;
            queue_unlocked_title_pushes(
                services,
                connection,
                &identity,
                &unlocked,
                DISCIPLINE_PLAYER_SOURCE_ID,
            )
            .await?;
        }
        Err(error) => {
            queue_learn_discipline_response(
                connection,
                packet.header.seq,
                false,
                error.error_code(),
                &identity.character_id,
                None,
                definition.map(to_discipline_definition_summary),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )?;
        }
    }

    Ok(())
}

pub async fn handle_set_character_discipline_active(
    services: &ServiceContext,
    connection: &ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(identity) = connection.ensure_authenticated_identity(packet.header.seq)? else {
        return Ok(());
    };

    let request = match packet
        .decode_body::<SetCharacterDisciplineActiveReq>("INVALID_SET_DISCIPLINE_ACTIVE_BODY")
    {
        Ok(value) => value,
        Err(error_code) => {
            queue_set_discipline_active_response(
                connection,
                packet.header.seq,
                false,
                error_code,
                &identity.character_id,
                None,
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )?;
            return Ok(());
        }
    };

    let discipline_id = request.discipline_id.trim();
    if discipline_id.is_empty() {
        queue_set_discipline_active_response(
            connection,
            packet.header.seq,
            false,
            "DISCIPLINE_ID_REQUIRED",
            &identity.character_id,
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )?;
        return Ok(());
    }

    info!(
        session_id = connection.session.id,
        account_player_id = %identity.account_player_id,
        player_id = %identity.account_player_id,
        character_id = %identity.character_id,
        world_id = ?identity.world_id,
        discipline_id,
        active = request.active,
        "handle_set_character_discipline_active"
    );

    let context = player_discipline_context_with_source(
        &identity,
        if request.active {
            "activate"
        } else {
            "deactivate"
        },
        DISCIPLINE_ACTIVE_SOURCE_ID,
    );
    let result = if request.active {
        services
            .discipline_service
            .activate_for_identity(
                &identity,
                discipline_id,
                services.config.max_active_disciplines,
                context,
            )
            .await
    } else {
        services
            .discipline_service
            .deactivate_for_identity(&identity, discipline_id, context)
            .await
    };

    match result {
        Ok(result) => {
            let unlocked = run_discipline_unlock_check(services, &identity, discipline_id).await?;
            let active_skill_pool = active_skill_pool(services, &identity).await?;
            let discipline_summary = to_discipline_summary(&result.discipline);
            let discipline_summaries = to_discipline_summaries(&result.disciplines);
            queue_set_discipline_active_response(
                connection,
                packet.header.seq,
                true,
                "",
                &identity.character_id,
                Some(discipline_summary.clone()),
                discipline_summaries.clone(),
                active_skill_pool.clone(),
                unlocked.clone(),
            )?;
            let action = if request.active {
                "activate"
            } else {
                "deactivate"
            };
            queue_discipline_push(
                services,
                connection,
                &identity,
                action,
                TITLE_PLAYER_SOURCE_TYPE,
                DISCIPLINE_ACTIVE_SOURCE_ID,
                if request.active {
                    "discipline activated"
                } else {
                    "discipline deactivated"
                },
                Some(discipline_summary),
                discipline_summaries,
                active_skill_pool,
                unlocked.clone(),
            )
            .await?;
            queue_unlocked_title_pushes(
                services,
                connection,
                &identity,
                &unlocked,
                DISCIPLINE_ACTIVE_SOURCE_ID,
            )
            .await?;
        }
        Err(error) => queue_set_discipline_active_response(
            connection,
            packet.header.seq,
            false,
            error.error_code(),
            &identity.character_id,
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )?,
    }

    Ok(())
}

pub async fn handle_switch_character_discipline(
    services: &ServiceContext,
    connection: &ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(identity) = connection.ensure_authenticated_identity(packet.header.seq)? else {
        return Ok(());
    };

    let request = match packet
        .decode_body::<SwitchCharacterDisciplineReq>("INVALID_SWITCH_DISCIPLINE_BODY")
    {
        Ok(value) => value,
        Err(error_code) => {
            queue_switch_discipline_response(
                connection,
                packet.header.seq,
                false,
                error_code,
                &identity.character_id,
                None,
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )?;
            return Ok(());
        }
    };

    let discipline_id = request.discipline_id.trim();
    if discipline_id.is_empty() {
        queue_switch_discipline_response(
            connection,
            packet.header.seq,
            false,
            "DISCIPLINE_ID_REQUIRED",
            &identity.character_id,
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )?;
        return Ok(());
    }

    info!(
        session_id = connection.session.id,
        account_player_id = %identity.account_player_id,
        player_id = %identity.account_player_id,
        character_id = %identity.character_id,
        world_id = ?identity.world_id,
        discipline_id,
        "handle_switch_character_discipline"
    );

    match services
        .discipline_service
        .switch_active_for_identity(
            &identity,
            discipline_id,
            services.config.max_active_disciplines,
            player_discipline_context_with_source(&identity, "switch", DISCIPLINE_SWITCH_SOURCE_ID),
        )
        .await
    {
        Ok(result) => {
            let unlocked = run_discipline_unlock_check(services, &identity, discipline_id).await?;
            let active_skill_pool = active_skill_pool(services, &identity).await?;
            let discipline_summary = to_discipline_summary(&result.discipline);
            let discipline_summaries = to_discipline_summaries(&result.disciplines);
            queue_switch_discipline_response(
                connection,
                packet.header.seq,
                true,
                "",
                &identity.character_id,
                Some(discipline_summary.clone()),
                discipline_summaries.clone(),
                active_skill_pool.clone(),
                unlocked.clone(),
            )?;
            queue_discipline_push(
                services,
                connection,
                &identity,
                "switch",
                TITLE_PLAYER_SOURCE_TYPE,
                DISCIPLINE_SWITCH_SOURCE_ID,
                "discipline switched",
                Some(discipline_summary),
                discipline_summaries,
                active_skill_pool,
                unlocked.clone(),
            )
            .await?;
            queue_unlocked_title_pushes(
                services,
                connection,
                &identity,
                &unlocked,
                DISCIPLINE_SWITCH_SOURCE_ID,
            )
            .await?;
        }
        Err(error) => queue_switch_discipline_response(
            connection,
            packet.header.seq,
            false,
            error.error_code(),
            &identity.character_id,
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )?,
    }

    Ok(())
}

pub async fn handle_add_character_discipline_points(
    services: &ServiceContext,
    connection: &ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(identity) = connection.ensure_authenticated_identity(packet.header.seq)? else {
        return Ok(());
    };

    let request = match packet
        .decode_body::<AddCharacterDisciplinePointsReq>("INVALID_ADD_DISCIPLINE_POINTS_BODY")
    {
        Ok(value) => value,
        Err(error_code) => {
            queue_add_discipline_points_response(
                connection,
                packet.header.seq,
                false,
                error_code,
                &identity.character_id,
                None,
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )?;
            return Ok(());
        }
    };

    let discipline_id = request.discipline_id.trim();
    if discipline_id.is_empty() {
        queue_add_discipline_points_response(
            connection,
            packet.header.seq,
            false,
            "DISCIPLINE_ID_REQUIRED",
            &identity.character_id,
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )?;
        return Ok(());
    }

    info!(
        session_id = connection.session.id,
        account_player_id = %identity.account_player_id,
        player_id = %identity.account_player_id,
        character_id = %identity.character_id,
        world_id = ?identity.world_id,
        discipline_id,
        points_delta = request.points_delta,
        "handle_add_character_discipline_points"
    );

    let config_tables = services.config_tables.tables_snapshot().await;
    match services
        .discipline_service
        .add_points_for_identity(
            &identity,
            discipline_id,
            request.points_delta,
            &config_tables.disciplinetable,
            player_discipline_context_with_source(
                &identity,
                "points_add",
                DISCIPLINE_POINTS_SOURCE_ID,
            ),
        )
        .await
    {
        Ok(result) => {
            let unlocked = run_discipline_unlock_check(services, &identity, discipline_id).await?;
            let active_skill_pool = active_skill_pool(services, &identity).await?;
            let discipline_summary = to_discipline_summary(&result.discipline);
            let discipline_summaries = to_discipline_summaries(&result.disciplines);
            queue_add_discipline_points_response(
                connection,
                packet.header.seq,
                true,
                "",
                &identity.character_id,
                Some(discipline_summary.clone()),
                discipline_summaries.clone(),
                active_skill_pool.clone(),
                unlocked.clone(),
            )?;
            queue_discipline_push(
                services,
                connection,
                &identity,
                "points_change",
                TITLE_PLAYER_SOURCE_TYPE,
                DISCIPLINE_POINTS_SOURCE_ID,
                "discipline points changed",
                Some(discipline_summary),
                discipline_summaries,
                active_skill_pool,
                unlocked.clone(),
            )
            .await?;
            queue_unlocked_title_pushes(
                services,
                connection,
                &identity,
                &unlocked,
                DISCIPLINE_POINTS_SOURCE_ID,
            )
            .await?;
        }
        Err(error) => queue_add_discipline_points_response(
            connection,
            packet.header.seq,
            false,
            error.error_code(),
            &identity.character_id,
            None,
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )?,
    }

    Ok(())
}

pub async fn handle_debug_character_title(
    services: &ServiceContext,
    connection: &ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(identity) = connection.ensure_authenticated_identity(packet.header.seq)? else {
        return Ok(());
    };

    let request = match packet.decode_body::<DebugCharacterTitleReq>("INVALID_TITLE_DEBUG_BODY") {
        Ok(value) => value,
        Err(error_code) => {
            queue_debug_title_response(
                connection,
                packet.header.seq,
                false,
                error_code,
                &identity.character_id,
                "",
                None,
                None,
                Vec::new(),
            )?;
            return Ok(());
        }
    };

    let action = normalize_action(&request.action);
    if !debug_token_matches(&request.debug_token) {
        queue_debug_title_response(
            connection,
            packet.header.seq,
            false,
            "CHARACTER_TITLE_DEBUG_FORBIDDEN",
            &identity.character_id,
            &action,
            None,
            None,
            Vec::new(),
        )?;
        return Ok(());
    }

    info!(
        session_id = connection.session.id,
        account_player_id = %identity.account_player_id,
        player_id = %identity.account_player_id,
        character_id = %identity.character_id,
        world_id = ?identity.world_id,
        action,
        title_id = %request.title_id,
        discipline_id = %request.discipline_id,
        discipline_tier = %request.discipline_tier,
        trigger_unlock_check = request.trigger_unlock_check,
        "handle_debug_character_title"
    );

    match action.as_str() {
        "grant_title" | "grant" => {
            handle_debug_grant_title(services, connection, packet.header.seq, &identity, &request)
                .await?
        }
        "revoke_title" | "revoke" => {
            handle_debug_revoke_title(services, connection, packet.header.seq, &identity, &request)
                .await?
        }
        "set_discipline" => {
            handle_debug_set_discipline(
                services,
                connection,
                packet.header.seq,
                &identity,
                &request,
            )
            .await?
        }
        "check_unlock" | "trigger_unlock_check" => {
            handle_debug_check_unlock(services, connection, packet.header.seq, &identity, &request)
                .await?
        }
        _ => queue_debug_title_response(
            connection,
            packet.header.seq,
            false,
            "INVALID_TITLE_DEBUG_ACTION",
            &identity.character_id,
            &action,
            None,
            None,
            Vec::new(),
        )?,
    }

    Ok(())
}

async fn handle_debug_grant_title(
    services: &ServiceContext,
    connection: &ConnectionContext,
    seq: u32,
    identity: &AuthenticatedSessionIdentity,
    request: &DebugCharacterTitleReq,
) -> Result<(), Box<dyn std::error::Error>> {
    let title_id = request.title_id.trim();
    if title_id.is_empty() {
        queue_debug_title_response(
            connection,
            seq,
            false,
            "TITLE_ID_REQUIRED",
            &identity.character_id,
            "grant_title",
            None,
            None,
            Vec::new(),
        )?;
        return Ok(());
    }

    let mut grant = GrantTitleRequest::new(title_id.to_string());
    if !request.expires_at.trim().is_empty() {
        grant = grant.with_expires_at(request.expires_at.trim().to_string());
    }

    let result = services
        .title_service
        .grant_for_identity(identity, grant, debug_title_context(identity, request))
        .await;

    match result {
        Ok(result) => {
            let table = services
                .config_tables
                .tables_snapshot()
                .await
                .titletable
                .clone();
            let mut unlocked = Vec::new();
            if request.trigger_unlock_check {
                unlocked = run_unlock_check(services, identity, request).await?;
            }
            queue_debug_title_response(
                connection,
                seq,
                true,
                "",
                &identity.character_id,
                "grant_title",
                Some(to_title_summary(
                    &table,
                    Some(&result.title),
                    &result.title.title_id,
                )),
                None,
                unlocked,
            )?;
            queue_title_push(
                services,
                connection,
                identity,
                "grant",
                TITLE_DEBUG_SOURCE_TYPE,
                TITLE_DEBUG_SOURCE_ID,
                "title granted",
                Some(to_title_summary(
                    &table,
                    Some(&result.title),
                    &result.title.title_id,
                )),
            )
            .await?;
        }
        Err(error) => queue_debug_title_response(
            connection,
            seq,
            false,
            error.error_code(),
            &identity.character_id,
            "grant_title",
            None,
            None,
            Vec::new(),
        )?,
    }

    Ok(())
}

async fn handle_debug_revoke_title(
    services: &ServiceContext,
    connection: &ConnectionContext,
    seq: u32,
    identity: &AuthenticatedSessionIdentity,
    request: &DebugCharacterTitleReq,
) -> Result<(), Box<dyn std::error::Error>> {
    let title_id = request.title_id.trim();
    if title_id.is_empty() {
        queue_debug_title_response(
            connection,
            seq,
            false,
            "TITLE_ID_REQUIRED",
            &identity.character_id,
            "revoke_title",
            None,
            None,
            Vec::new(),
        )?;
        return Ok(());
    }

    queue_expired_title_pushes(services, connection, identity).await?;
    let before_equipped = services
        .title_service
        .equipped_for_identity(identity, player_title_context(identity, "revoke_before"))
        .await?;

    match services
        .title_service
        .revoke_for_identity(identity, title_id, debug_title_context(identity, request))
        .await
    {
        Ok(()) => {
            queue_debug_title_response(
                connection,
                seq,
                true,
                "",
                &identity.character_id,
                "revoke_title",
                None,
                None,
                Vec::new(),
            )?;
            let table = services
                .config_tables
                .tables_snapshot()
                .await
                .titletable
                .clone();
            let owned = services
                .title_service
                .list_for_identity(identity, player_title_context(identity, "push_snapshot"))
                .await?;
            if before_equipped
                .as_ref()
                .is_some_and(|before| before.title_id == title_id)
            {
                let unequipped_summary =
                    to_title_summary(&table, before_equipped.as_ref(), title_id);
                queue_title_push_with_owned_snapshot(
                    services,
                    connection,
                    identity,
                    "unequip",
                    TITLE_DEBUG_SOURCE_TYPE,
                    TITLE_DEBUG_SOURCE_ID,
                    "title unequipped",
                    Some(unequipped_summary),
                    &owned,
                    &table,
                )
                .await?;
            }
            queue_title_push_with_owned_snapshot(
                services,
                connection,
                identity,
                "revoke",
                TITLE_DEBUG_SOURCE_TYPE,
                TITLE_DEBUG_SOURCE_ID,
                "title revoked",
                None,
                &owned,
                &table,
            )
            .await?;
        }
        Err(error) => queue_debug_title_response(
            connection,
            seq,
            false,
            error.error_code(),
            &identity.character_id,
            "revoke_title",
            None,
            None,
            Vec::new(),
        )?,
    }

    Ok(())
}

async fn handle_debug_set_discipline(
    services: &ServiceContext,
    connection: &ConnectionContext,
    seq: u32,
    identity: &AuthenticatedSessionIdentity,
    request: &DebugCharacterTitleReq,
) -> Result<(), Box<dyn std::error::Error>> {
    let discipline_id = request.discipline_id.trim();
    let discipline_tier = request.discipline_tier.trim();
    let upsert = DisciplineUpsert::new(
        discipline_id.to_string(),
        request.discipline_points,
        discipline_tier.to_string(),
        request.discipline_active,
    );

    match services
        .discipline_service
        .upsert_for_identity_with_context(
            identity,
            upsert,
            debug_discipline_context(identity, request),
        )
        .await
    {
        Ok(discipline) => {
            let unlocked = if request.trigger_unlock_check {
                run_unlock_check(services, identity, request).await?
            } else {
                Vec::new()
            };
            let active_skill_pool = active_skill_pool(services, identity).await?;
            let disciplines = services
                .discipline_service
                .list_for_identity(identity)
                .await?;
            let discipline_summary = to_discipline_summary(&discipline);
            let discipline_summaries = to_discipline_summaries(&disciplines);
            queue_debug_title_response(
                connection,
                seq,
                true,
                "",
                &identity.character_id,
                "set_discipline",
                None,
                Some(discipline_summary.clone()),
                unlocked.clone(),
            )?;
            queue_discipline_push(
                services,
                connection,
                identity,
                "upsert",
                DISCIPLINE_DEBUG_SOURCE_TYPE,
                DISCIPLINE_DEBUG_SOURCE_ID,
                "discipline debug updated",
                Some(discipline_summary),
                discipline_summaries,
                active_skill_pool,
                unlocked.clone(),
            )
            .await?;
            queue_unlocked_title_pushes(
                services,
                connection,
                identity,
                &unlocked,
                DISCIPLINE_DEBUG_SOURCE_ID,
            )
            .await?;
        }
        Err(error) => queue_debug_title_response(
            connection,
            seq,
            false,
            error.error_code(),
            &identity.character_id,
            "set_discipline",
            None,
            None,
            Vec::new(),
        )?,
    }

    Ok(())
}

async fn handle_debug_check_unlock(
    services: &ServiceContext,
    connection: &ConnectionContext,
    seq: u32,
    identity: &AuthenticatedSessionIdentity,
    request: &DebugCharacterTitleReq,
) -> Result<(), Box<dyn std::error::Error>> {
    match run_unlock_check(services, identity, request).await {
        Ok(unlocked) => {
            queue_debug_title_response(
                connection,
                seq,
                true,
                "",
                &identity.character_id,
                "check_unlock",
                None,
                None,
                unlocked.clone(),
            )?;
            queue_unlocked_title_pushes(
                services,
                connection,
                identity,
                &unlocked,
                TITLE_DEBUG_SOURCE_ID,
            )
            .await?;
        }
        Err(error) => queue_debug_title_response(
            connection,
            seq,
            false,
            error,
            &identity.character_id,
            "check_unlock",
            None,
            None,
            Vec::new(),
        )?,
    }

    Ok(())
}

async fn run_unlock_check(
    services: &ServiceContext,
    identity: &AuthenticatedSessionIdentity,
    request: &DebugCharacterTitleReq,
) -> Result<Vec<CharacterTitleSummary>, &'static str> {
    let trigger = if request.discipline_id.trim().is_empty() {
        TitleUnlockTrigger::Gm {
            operator_id: Some(identity.account_player_id.clone()),
        }
    } else {
        TitleUnlockTrigger::Discipline {
            discipline_id: Some(request.discipline_id.trim().to_string()),
        }
    };
    let result = services
        .title_unlock_service
        .check_for_identity(identity, trigger)
        .await
        .map_err(|error| error.error_code())?;
    let table = services
        .config_tables
        .tables_snapshot()
        .await
        .titletable
        .clone();
    Ok(unlocked_title_summaries(&table, &result))
}

async fn run_discipline_unlock_check(
    services: &ServiceContext,
    identity: &AuthenticatedSessionIdentity,
    discipline_id: &str,
) -> Result<Vec<CharacterTitleSummary>, &'static str> {
    let result = run_discipline_unlock_check_with_service(
        &services.title_unlock_service,
        identity,
        discipline_id,
    )
    .await?;
    let table = services
        .config_tables
        .tables_snapshot()
        .await
        .titletable
        .clone();
    Ok(unlocked_title_summaries(&table, &result))
}

async fn run_discipline_unlock_check_with_service(
    title_unlock_service: &crate::core::character_title_unlock::TitleUnlockService,
    identity: &AuthenticatedSessionIdentity,
    discipline_id: &str,
) -> Result<TitleUnlockCheckResult, &'static str> {
    title_unlock_service
        .check_for_identity(
            identity,
            TitleUnlockTrigger::Discipline {
                discipline_id: Some(discipline_id.to_string()),
            },
        )
        .await
        .map_err(|error| error.error_code())
}

async fn active_skill_pool(
    services: &ServiceContext,
    identity: &AuthenticatedSessionIdentity,
) -> Result<Vec<String>, &'static str> {
    let tables = services.config_tables.tables_snapshot().await;
    services
        .discipline_service
        .active_skill_pool_for_identity(identity, &tables.disciplinetable)
        .await
        .map_err(|error| error.error_code())
}

fn unlocked_title_summaries(
    table: &TitleTable,
    result: &TitleUnlockCheckResult,
) -> Vec<CharacterTitleSummary> {
    result
        .unlocked
        .iter()
        .map(|grant: &TitleUnlockGrant| {
            to_title_summary(table, Some(&grant.title), &grant.title.title_id)
        })
        .collect()
}

fn player_title_context(
    identity: &AuthenticatedSessionIdentity,
    action: &str,
) -> TitleOperationContext {
    TitleOperationContext::new(TITLE_PLAYER_SOURCE_TYPE)
        .with_source_id(TITLE_PLAYER_SOURCE_ID)
        .with_operator(TITLE_PLAYER_SOURCE_TYPE, identity.account_player_id.clone())
        .with_reason(format!("character title {action}"))
}

fn debug_title_context(
    identity: &AuthenticatedSessionIdentity,
    request: &DebugCharacterTitleReq,
) -> TitleOperationContext {
    TitleOperationContext::new(TITLE_DEBUG_SOURCE_TYPE)
        .with_source_id(TITLE_DEBUG_SOURCE_ID)
        .with_operator(
            TITLE_DEBUG_OPERATOR_TYPE,
            identity.account_player_id.clone(),
        )
        .with_reason(normalize_debug_reason(&request.reason))
}

fn player_discipline_context(
    identity: &AuthenticatedSessionIdentity,
    action: &str,
) -> DisciplineOperationContext {
    player_discipline_context_with_source(identity, action, DISCIPLINE_PLAYER_SOURCE_ID)
}

fn player_discipline_context_with_source(
    identity: &AuthenticatedSessionIdentity,
    action: &str,
    source_id: &str,
) -> DisciplineOperationContext {
    DisciplineOperationContext::new(TITLE_PLAYER_SOURCE_TYPE)
        .with_source_id(source_id)
        .with_operator(TITLE_PLAYER_SOURCE_TYPE, identity.account_player_id.clone())
        .with_reason(format!("character discipline {action}"))
}

fn debug_discipline_context(
    identity: &AuthenticatedSessionIdentity,
    request: &DebugCharacterTitleReq,
) -> DisciplineOperationContext {
    DisciplineOperationContext::new(DISCIPLINE_DEBUG_SOURCE_TYPE)
        .with_source_id(DISCIPLINE_DEBUG_SOURCE_ID)
        .with_operator(
            DISCIPLINE_DEBUG_OPERATOR_TYPE,
            identity.account_player_id.clone(),
        )
        .with_reason(normalize_debug_reason(&request.reason))
}

fn normalize_action(raw: &str) -> String {
    raw.trim().to_ascii_lowercase()
}

fn normalize_debug_reason(raw: &str) -> String {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return DEFAULT_TITLE_DEBUG_REASON.to_string();
    }
    trimmed.chars().take(255).collect()
}

fn debug_token_matches(actual: &str) -> bool {
    let actual = actual.trim();
    if actual.is_empty() {
        return false;
    }

    std::env::var("MYSERVER_CHARACTER_TITLE_DEBUG_TOKEN")
        .ok()
        .is_some_and(|expected| !expected.trim().is_empty() && expected.trim() == actual)
}

fn to_title_summaries(
    table: &TitleTable,
    owned_titles: &[CharacterTitle],
) -> Vec<CharacterTitleSummary> {
    let owned_by_id: HashMap<&str, &CharacterTitle> = owned_titles
        .iter()
        .map(|title| (title.title_id.as_str(), title))
        .collect();
    let mut summaries: Vec<_> = table
        .all()
        .iter()
        .map(|row| {
            let title_id = row.titleid.to_string();
            to_title_summary(
                table,
                owned_by_id.get(title_id.as_str()).copied(),
                &title_id,
            )
        })
        .collect();
    summaries.sort_by(|left, right| {
        let left_sort = left
            .definition
            .as_ref()
            .map(|definition| definition.sort_order)
            .unwrap_or_default();
        let right_sort = right
            .definition
            .as_ref()
            .map(|definition| definition.sort_order)
            .unwrap_or_default();
        left_sort
            .cmp(&right_sort)
            .then_with(|| title_id(left).cmp(title_id(right)))
    });
    summaries
}

fn title_id(summary: &CharacterTitleSummary) -> &str {
    summary
        .definition
        .as_ref()
        .map(|definition| definition.id.as_str())
        .unwrap_or("")
}

pub(crate) fn to_title_summary(
    table: &TitleTable,
    title: Option<&CharacterTitle>,
    title_id: &str,
) -> CharacterTitleSummary {
    CharacterTitleSummary {
        definition: Some(to_title_definition_summary(table, title_id)),
        owned: title.is_some(),
        equipped: title.map(|value| value.is_equipped).unwrap_or(false),
        source_type: title
            .map(|value| value.source_type.clone())
            .unwrap_or_default(),
        source_id: title
            .and_then(|value| value.source_id.clone())
            .unwrap_or_default(),
        unlocked_at: title
            .map(|value| value.unlocked_at.clone())
            .unwrap_or_default(),
        expires_at: title
            .and_then(|value| value.expires_at.clone())
            .unwrap_or_default(),
        expired: title.map(|value| value.expired).unwrap_or(false),
    }
}

fn to_title_definition_summary(
    table: &TitleTable,
    title_id: &str,
) -> CharacterTitleDefinitionSummary {
    let row = title_id.parse::<i32>().ok().and_then(|id| table.get(id));
    match row {
        Some(row) => to_title_definition_summary_from_row(table, row),
        None => CharacterTitleDefinitionSummary {
            id: title_id.to_string(),
            name: String::new(),
            r#type: String::new(),
            rarity: String::new(),
            icon: String::new(),
            color: String::new(),
            tags: Vec::new(),
            hidden: false,
            limited: false,
            sort_order: 0,
        },
    }
}

fn to_title_definition_summary_from_row(
    table: &TitleTable,
    row: &TitleTableRow,
) -> CharacterTitleDefinitionSummary {
    CharacterTitleDefinitionSummary {
        id: row.titleid.to_string(),
        name: resolve_string(table, row.name),
        r#type: resolve_string(table, row.titletype),
        rarity: resolve_string(table, row.rarity),
        icon: resolve_string(table, row.icon),
        color: resolve_string(table, row.color),
        tags: row
            .tags
            .iter()
            .filter_map(|key| table.resolve_string(*key).map(ToString::to_string))
            .collect(),
        hidden: row.hidden != 0,
        limited: row.limited != 0,
        sort_order: row.sortorder,
    }
}

fn resolve_string(table: &TitleTable, key: u32) -> String {
    table
        .resolve_string(key)
        .map(ToString::to_string)
        .unwrap_or_default()
}

pub(crate) fn to_discipline_summary(
    discipline: &CharacterDiscipline,
) -> CharacterDisciplineSummary {
    CharacterDisciplineSummary {
        discipline_id: discipline.discipline_id.clone(),
        points: discipline.points,
        tier: discipline.tier.clone(),
        active: discipline.active,
        learned_at: discipline.learned_at.clone(),
        updated_at: discipline.updated_at.clone(),
    }
}

fn to_discipline_summaries(disciplines: &[CharacterDiscipline]) -> Vec<CharacterDisciplineSummary> {
    disciplines.iter().map(to_discipline_summary).collect()
}

fn to_discipline_definition_summary(
    definition: DisciplineDefinitionSummary,
) -> crate::pb::CharacterDisciplineDefinitionSummary {
    crate::pb::CharacterDisciplineDefinitionSummary {
        discipline_id: definition.discipline_id,
        name: definition.name,
        description: definition.description,
        initial_tier: definition.initial_tier,
        initial_points: definition.initial_points,
        skill_pool: definition.skill_pool,
        interaction_permissions: definition.interaction_permissions,
        display_fields_json: definition.display_fields_json,
    }
}

pub(crate) async fn queue_title_push(
    services: &ServiceContext,
    connection: &ConnectionContext,
    identity: &AuthenticatedSessionIdentity,
    action: &str,
    source_type: &str,
    source_id: &str,
    summary: &str,
    title: Option<CharacterTitleSummary>,
) -> Result<(), Box<dyn std::error::Error>> {
    let table = services
        .config_tables
        .tables_snapshot()
        .await
        .titletable
        .clone();
    let owned = services
        .title_service
        .list_for_identity(identity, player_title_context(identity, "push_snapshot"))
        .await?;
    queue_title_push_with_owned_snapshot(
        services,
        connection,
        identity,
        action,
        source_type,
        source_id,
        summary,
        title,
        &owned,
        &table,
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn queue_title_push_with_owned_snapshot(
    services: &ServiceContext,
    connection: &ConnectionContext,
    identity: &AuthenticatedSessionIdentity,
    action: &str,
    source_type: &str,
    source_id: &str,
    summary: &str,
    title: Option<CharacterTitleSummary>,
    owned: &[CharacterTitle],
    table: &TitleTable,
) -> Result<(), Box<dyn std::error::Error>> {
    let record = record_title_push_with_owned_snapshot(
        &services.character_push_service,
        &identity.character_id,
        action,
        source_type,
        source_id,
        summary,
        title,
        owned,
        table,
    )
    .await;
    queue_character_push(connection, &identity.character_id, &record)?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn record_title_push_with_owned_snapshot(
    character_push_service: &crate::core::character_push::CharacterPushService,
    character_id: &str,
    action: &str,
    source_type: &str,
    source_id: &str,
    summary: &str,
    title: Option<CharacterTitleSummary>,
    owned: &[CharacterTitle],
    table: &TitleTable,
) -> crate::core::character_push::CharacterPushRecord {
    let titles = to_title_summaries(table, owned);
    let equipped_title = titles.iter().find(|title| title.equipped).cloned();
    character_push_service
        .record_title_change(
            character_id,
            CharacterPushSource::new(source_type, source_id, action, summary),
            CharacterTitleChangePush {
                meta: None,
                title,
                titles,
                equipped_title,
            },
        )
        .await
}

async fn queue_expired_title_pushes(
    services: &ServiceContext,
    connection: &ConnectionContext,
    identity: &AuthenticatedSessionIdentity,
) -> Result<(), Box<dyn std::error::Error>> {
    let expired = services
        .title_service
        .process_expired(
            &identity.character_id,
            player_title_context(identity, "expire"),
        )
        .await?;
    if expired.is_empty() {
        return Ok(());
    }

    let table = services
        .config_tables
        .tables_snapshot()
        .await
        .titletable
        .clone();
    let owned = services
        .title_service
        .list_for_identity(identity, player_title_context(identity, "push_snapshot"))
        .await?;
    for title in expired {
        queue_title_push_with_owned_snapshot(
            services,
            connection,
            identity,
            "expire",
            "system",
            "character_title_expiry",
            "title expired",
            Some(to_title_summary(&table, Some(&title), &title.title_id)),
            &owned,
            &table,
        )
        .await?;
    }

    Ok(())
}

pub(crate) async fn queue_discipline_push(
    services: &ServiceContext,
    connection: &ConnectionContext,
    identity: &AuthenticatedSessionIdentity,
    action: &str,
    source_type: &str,
    source_id: &str,
    summary: &str,
    discipline: Option<CharacterDisciplineSummary>,
    disciplines: Vec<CharacterDisciplineSummary>,
    active_skill_pool: Vec<String>,
    unlocked_titles: Vec<CharacterTitleSummary>,
) -> Result<(), Box<dyn std::error::Error>> {
    let record = services
        .character_push_service
        .record_discipline_change(
            &identity.character_id,
            CharacterPushSource::new(source_type, source_id, action, summary),
            CharacterDisciplineChangePush {
                meta: None,
                discipline,
                disciplines,
                active_skill_pool,
                unlocked_titles,
            },
        )
        .await;
    queue_character_push(connection, &identity.character_id, &record)?;
    Ok(())
}

async fn queue_unlocked_title_pushes(
    services: &ServiceContext,
    connection: &ConnectionContext,
    identity: &AuthenticatedSessionIdentity,
    unlocked_titles: &[CharacterTitleSummary],
    source_id: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    for title in unlocked_titles {
        queue_title_push(
            services,
            connection,
            identity,
            "auto_unlock",
            "system",
            source_id,
            "title auto unlocked",
            Some(title.clone()),
        )
        .await?;
    }
    Ok(())
}

fn to_pb_discipline_item_cost(cost: &DisciplineItemCost) -> PbDisciplineItemCost {
    PbDisciplineItemCost {
        item_uid: cost.item_uid,
        item_id: cost.item_id,
        count: cost.count,
    }
}

fn queue_equip_title_response(
    connection: &ConnectionContext,
    seq: u32,
    ok: bool,
    error_code: &str,
    character_id: &str,
    equipped_title: Option<CharacterTitleSummary>,
) -> Result<(), std::io::Error> {
    connection.queue_message(
        MessageType::EquipCharacterTitleRes,
        seq,
        EquipCharacterTitleRes {
            ok,
            error_code: error_code.to_string(),
            character_id: character_id.to_string(),
            equipped_title,
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn queue_learn_discipline_response(
    connection: &ConnectionContext,
    seq: u32,
    ok: bool,
    error_code: &str,
    character_id: &str,
    discipline: Option<CharacterDisciplineSummary>,
    definition: Option<crate::pb::CharacterDisciplineDefinitionSummary>,
    consumed_items: Vec<PbDisciplineItemCost>,
    active_skill_pool: Vec<String>,
    unlocked_titles: Vec<CharacterTitleSummary>,
) -> Result<(), std::io::Error> {
    connection.queue_message(
        MessageType::LearnCharacterDisciplineRes,
        seq,
        LearnCharacterDisciplineRes {
            ok,
            error_code: error_code.to_string(),
            character_id: character_id.to_string(),
            discipline,
            definition,
            consumed_items,
            active_skill_pool,
            unlocked_titles,
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn queue_set_discipline_active_response(
    connection: &ConnectionContext,
    seq: u32,
    ok: bool,
    error_code: &str,
    character_id: &str,
    discipline: Option<CharacterDisciplineSummary>,
    disciplines: Vec<CharacterDisciplineSummary>,
    active_skill_pool: Vec<String>,
    unlocked_titles: Vec<CharacterTitleSummary>,
) -> Result<(), std::io::Error> {
    connection.queue_message(
        MessageType::SetCharacterDisciplineActiveRes,
        seq,
        SetCharacterDisciplineActiveRes {
            ok,
            error_code: error_code.to_string(),
            character_id: character_id.to_string(),
            discipline,
            disciplines,
            active_skill_pool,
            unlocked_titles,
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn queue_switch_discipline_response(
    connection: &ConnectionContext,
    seq: u32,
    ok: bool,
    error_code: &str,
    character_id: &str,
    discipline: Option<CharacterDisciplineSummary>,
    disciplines: Vec<CharacterDisciplineSummary>,
    active_skill_pool: Vec<String>,
    unlocked_titles: Vec<CharacterTitleSummary>,
) -> Result<(), std::io::Error> {
    connection.queue_message(
        MessageType::SwitchCharacterDisciplineRes,
        seq,
        SwitchCharacterDisciplineRes {
            ok,
            error_code: error_code.to_string(),
            character_id: character_id.to_string(),
            discipline,
            disciplines,
            active_skill_pool,
            unlocked_titles,
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn queue_add_discipline_points_response(
    connection: &ConnectionContext,
    seq: u32,
    ok: bool,
    error_code: &str,
    character_id: &str,
    discipline: Option<CharacterDisciplineSummary>,
    disciplines: Vec<CharacterDisciplineSummary>,
    active_skill_pool: Vec<String>,
    unlocked_titles: Vec<CharacterTitleSummary>,
) -> Result<(), std::io::Error> {
    connection.queue_message(
        MessageType::AddCharacterDisciplinePointsRes,
        seq,
        AddCharacterDisciplinePointsRes {
            ok,
            error_code: error_code.to_string(),
            character_id: character_id.to_string(),
            discipline,
            disciplines,
            active_skill_pool,
            unlocked_titles,
        },
    )
}

#[allow(clippy::too_many_arguments)]
fn queue_debug_title_response(
    connection: &ConnectionContext,
    seq: u32,
    ok: bool,
    error_code: &str,
    character_id: &str,
    action: &str,
    title: Option<CharacterTitleSummary>,
    discipline: Option<CharacterDisciplineSummary>,
    unlocked_titles: Vec<CharacterTitleSummary>,
) -> Result<(), std::io::Error> {
    connection.queue_message(
        MessageType::DebugCharacterTitleRes,
        seq,
        DebugCharacterTitleRes {
            ok,
            error_code: error_code.to_string(),
            character_id: character_id.to_string(),
            action: action.to_string(),
            title,
            discipline,
            unlocked_titles,
        },
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::core::character_discipline::DisciplineService;
    use crate::core::character_element::CharacterElementService;
    use crate::core::character_push::CharacterPushService;
    use crate::core::character_title::TitleService;
    use crate::core::character_title_unlock::TitleUnlockService;
    use crate::gameconfig::ConfigTables;
    use prost::Message;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn identity() -> AuthenticatedSessionIdentity {
        AuthenticatedSessionIdentity {
            account_player_id: "plr_0000000000001".to_string(),
            character_id: "chr_0000000000001".to_string(),
            world_id: Some(1),
        }
    }

    fn title_table() -> Arc<TitleTable> {
        let rows = vec![
            TitleTableRow {
                titleid: 1001,
                hidden: 0,
                limited: 0,
                ..TitleTableRow::default()
            },
            TitleTableRow {
                titleid: 9001,
                hidden: 1,
                limited: 0,
                ..TitleTableRow::default()
            },
        ];
        let by_id = rows
            .iter()
            .enumerate()
            .map(|(index, row)| (row.titleid, index))
            .collect();
        Arc::new(TitleTable {
            string_pool: HashMap::new(),
            rows,
            by_id,
        })
    }

    #[test]
    fn debug_token_accepts_only_the_dedicated_debug_token() {
        unsafe {
            std::env::set_var("GAME_ADMIN_TOKEN", "env-token");
            std::env::set_var("MYSERVER_CHARACTER_TITLE_DEBUG_TOKEN", "title-env-token");
        }
        assert!(debug_token_matches("title-env-token"));
        assert!(!debug_token_matches("env-token"));
        assert!(!debug_token_matches(""));
        assert!(!debug_token_matches("other"));
        unsafe {
            std::env::remove_var("GAME_ADMIN_TOKEN");
            std::env::remove_var("MYSERVER_CHARACTER_TITLE_DEBUG_TOKEN");
        }
    }

    #[test]
    fn empty_debug_reason_uses_controlled_default() {
        assert_eq!(
            normalize_debug_reason("   "),
            DEFAULT_TITLE_DEBUG_REASON.to_string()
        );
    }

    #[tokio::test]
    async fn formal_discipline_unlock_helper_triggers_title_check() {
        let identity = identity();
        let tables =
            ConfigTables::load_from_dir(std::path::Path::new("csv")).expect("csv tables load");
        let title_table = tables.titletable.clone();
        let discipline_service = DisciplineService::new_in_memory();
        discipline_service
            .upsert_for_identity(
                &identity,
                DisciplineUpsert::new("forging", 0, "novice", true),
            )
            .await
            .unwrap();
        let title_unlock_service = TitleUnlockService::new_for_test(
            TitleService::new_in_memory(title_table),
            discipline_service,
            CharacterElementService::new_in_memory(),
            tables.titletable.clone(),
        );

        let result =
            run_discipline_unlock_check_with_service(&title_unlock_service, &identity, "forging")
                .await
                .unwrap();

        assert!(
            result
                .unlocked
                .iter()
                .any(|grant| grant.title_id == "2001" && grant.source_type == "discipline")
        );
    }

    #[tokio::test]
    async fn title_push_records_unequip_with_changed_snapshot() {
        let identity = identity();
        let table = title_table();
        let title_service = TitleService::new_in_memory(table.clone());
        title_service
            .grant_for_identity(
                &identity,
                GrantTitleRequest::new("1001"),
                title_context("grant"),
            )
            .await
            .unwrap();
        title_service
            .grant_for_identity(
                &identity,
                GrantTitleRequest::new("9001"),
                title_context("grant"),
            )
            .await
            .unwrap();
        let before = title_service
            .equip_for_identity(
                &identity,
                "1001",
                EquipTitleOptions::visible_only(),
                title_context("equip"),
            )
            .await
            .unwrap();
        title_service
            .equip_for_identity(
                &identity,
                "9001",
                EquipTitleOptions::allow_hidden(),
                title_context("equip"),
            )
            .await
            .unwrap();
        let owned = title_service
            .list_for_identity(&identity, title_context("snapshot"))
            .await
            .unwrap();
        let push_service = CharacterPushService::new();

        let record = record_title_push_with_owned_snapshot(
            &push_service,
            &identity.character_id,
            "unequip",
            TITLE_PLAYER_SOURCE_TYPE,
            TITLE_PLAYER_SOURCE_ID,
            "title unequipped",
            Some(to_title_summary(&table, Some(&before), &before.title_id)),
            &owned,
            &table,
        )
        .await;

        let push = CharacterTitleChangePush::decode(record.body.as_slice()).unwrap();
        assert_eq!(push.meta.as_ref().unwrap().action, "unequip");
        assert_eq!(
            push.title
                .as_ref()
                .and_then(|title| title.definition.as_ref())
                .map(|definition| definition.id.as_str()),
            Some("1001")
        );
        assert_eq!(
            push.equipped_title
                .as_ref()
                .and_then(|title| title.definition.as_ref())
                .map(|definition| definition.id.as_str()),
            Some("9001")
        );
        assert!(push.titles.iter().any(|title| {
            title
                .definition
                .as_ref()
                .is_some_and(|definition| definition.id == "1001")
                && title.owned
                && !title.equipped
        }));
    }

    #[tokio::test]
    async fn title_push_records_expire_with_changed_snapshot() {
        let identity = identity();
        let table = title_table();
        let title_service = TitleService::new_in_memory(table.clone());
        title_service
            .grant_for_identity(
                &identity,
                GrantTitleRequest::new("1001"),
                title_context("grant"),
            )
            .await
            .unwrap();
        title_service
            .equip_for_identity(
                &identity,
                "1001",
                EquipTitleOptions::visible_only(),
                title_context("equip"),
            )
            .await
            .unwrap();
        title_service
            .mark_expired_for_test(&identity.character_id, "1001")
            .await;

        let expired = title_service
            .process_expired(&identity.character_id, title_context("expire"))
            .await
            .unwrap();
        let owned = title_service
            .list_for_identity(&identity, title_context("snapshot"))
            .await
            .unwrap();
        let push_service = CharacterPushService::new();

        let record = record_title_push_with_owned_snapshot(
            &push_service,
            &identity.character_id,
            "expire",
            "system",
            "character_title_expiry",
            "title expired",
            Some(to_title_summary(
                &table,
                Some(&expired[0]),
                &expired[0].title_id,
            )),
            &owned,
            &table,
        )
        .await;

        let push = CharacterTitleChangePush::decode(record.body.as_slice()).unwrap();
        assert_eq!(push.meta.as_ref().unwrap().action, "expire");
        assert!(push.equipped_title.is_none());
        assert_eq!(
            push.title
                .as_ref()
                .and_then(|title| title.definition.as_ref())
                .map(|definition| definition.id.as_str()),
            Some("1001")
        );
        assert!(push.title.as_ref().unwrap().expired);
        assert!(push.titles.iter().any(|title| {
            title
                .definition
                .as_ref()
                .is_some_and(|definition| definition.id == "1001")
                && title.owned
                && title.expired
                && !title.equipped
        }));
    }

    fn title_context(action: &str) -> TitleOperationContext {
        TitleOperationContext::new("unit-test")
            .with_source_id(action)
            .with_operator("account", "plr_0000000000001")
            .with_reason("test")
    }
}
