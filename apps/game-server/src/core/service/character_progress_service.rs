use tracing::info;

use crate::core::character_progress::{
    ApplyCharacterProgressRequest, CharacterProgressOutcome, CharacterProgressRewardOutcome,
};
use crate::core::character_push::CharacterPushSource;
use crate::core::context::{ConnectionContext, ServiceContext};
use crate::core::service::character_title_service::{to_discipline_summary, to_title_summary};
use crate::pb::{
    ApplyCharacterProgressReq, ApplyCharacterProgressRes, CharacterProgressRewardSummary,
};
use crate::protocol::{MessageType, Packet};

pub async fn handle_apply_character_progress(
    services: &ServiceContext,
    connection: &ConnectionContext,
    packet: &Packet,
) -> Result<(), Box<dyn std::error::Error>> {
    let Some(identity) = connection.ensure_authenticated_identity(packet.header.seq)? else {
        return Ok(());
    };

    let request =
        match packet.decode_body::<ApplyCharacterProgressReq>("INVALID_CHARACTER_PROGRESS_BODY") {
            Ok(value) => value,
            Err(error_code) => {
                queue_apply_progress_response(
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

    let progress_id = request.progress_id.trim();
    if progress_id.is_empty() {
        queue_apply_progress_response(
            connection,
            packet.header.seq,
            false,
            "INVALID_CHARACTER_PROGRESS_ID",
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
        progress_id,
        "handle_apply_character_progress"
    );

    let config_tables = services.config_tables.tables_snapshot().await;
    let mut player_data = services
        .player_manager
        .get_or_create_player(&identity.character_id)
        .await;

    let result = services
        .character_progress_service
        .apply_for_identity(
            &identity,
            ApplyCharacterProgressRequest::new(progress_id.to_string()),
            &config_tables.characterprogresstable,
            &config_tables.disciplinetable,
            &mut player_data,
        )
        .await;

    match result {
        Ok(outcome) => {
            if outcome.applied {
                if let Err(error) = services
                    .player_manager
                    .save_player(&identity.character_id, player_data)
                    .await
                {
                    queue_apply_progress_response(
                        connection,
                        packet.header.seq,
                        false,
                        error.error_code(),
                        &identity.character_id,
                        None,
                    )?;
                    return Ok(());
                }
            }
            let rewards = outcome
                .rewards
                .iter()
                .map(|reward| to_reward_summary(&config_tables.titletable, reward))
                .collect();
            queue_apply_progress_response(
                connection,
                packet.header.seq,
                true,
                "",
                &identity.character_id,
                Some((&outcome, rewards)),
            )?;
            if outcome.applied {
                queue_progress_pushes(services, connection, &identity, &outcome).await?;
            }
        }
        Err(error) => queue_apply_progress_response(
            connection,
            packet.header.seq,
            false,
            error.error_code(),
            &identity.character_id,
            None,
        )?,
    }

    Ok(())
}

async fn queue_progress_pushes(
    services: &ServiceContext,
    connection: &ConnectionContext,
    identity: &crate::session::AuthenticatedSessionIdentity,
    outcome: &CharacterProgressOutcome,
) -> Result<(), Box<dyn std::error::Error>> {
    let title_table = services
        .config_tables
        .tables_snapshot()
        .await
        .titletable
        .clone();
    for reward in &outcome.rewards {
        match reward.reward_type.as_str() {
            "affinity" | "mastery" => {
                if let Some(changed) = reward.element_changed.as_ref() {
                    crate::gameservice::character_element::queue_character_element_push(
                        services,
                        connection,
                        identity,
                        changed,
                        CharacterPushSource::new(
                            outcome.source_type.as_str(),
                            outcome.source_id.as_str(),
                            "progress_reward",
                            format!(
                                "progress {} {} reward",
                                outcome.progress_id, reward.reward_type
                            ),
                        ),
                    )
                    .await?;
                }
            }
            "title" => {
                if let Some(title) = reward.title.as_ref() {
                    crate::core::service::character_title_service::queue_title_push(
                        services,
                        connection,
                        identity,
                        if reward.status == "renewed" {
                            "renew"
                        } else {
                            "grant"
                        },
                        outcome.source_type.as_str(),
                        outcome.source_id.as_str(),
                        "progress title reward",
                        Some(to_title_summary(&title_table, Some(title), &title.title_id)),
                    )
                    .await?;
                }
            }
            "discipline_points" => {
                if let Some(discipline) = reward.discipline.as_ref() {
                    let disciplines = services
                        .discipline_service
                        .list_for_identity(identity)
                        .await?;
                    let active_skill_pool =
                        active_skill_pool_for_progress_push(services, identity).await?;
                    crate::core::service::character_title_service::queue_discipline_push(
                        services,
                        connection,
                        identity,
                        "points_change",
                        outcome.source_type.as_str(),
                        outcome.source_id.as_str(),
                        "progress discipline points reward",
                        Some(to_discipline_summary(discipline)),
                        disciplines.iter().map(to_discipline_summary).collect(),
                        active_skill_pool,
                        Vec::new(),
                    )
                    .await?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

async fn active_skill_pool_for_progress_push(
    services: &ServiceContext,
    identity: &crate::session::AuthenticatedSessionIdentity,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let tables = services.config_tables.tables_snapshot().await;
    Ok(services
        .discipline_service
        .active_skill_pool_for_identity(identity, &tables.disciplinetable)
        .await?)
}

fn queue_apply_progress_response(
    connection: &ConnectionContext,
    seq: u32,
    ok: bool,
    error_code: &str,
    character_id: &str,
    outcome: Option<(
        &CharacterProgressOutcome,
        Vec<CharacterProgressRewardSummary>,
    )>,
) -> Result<(), std::io::Error> {
    let response = match outcome {
        Some((outcome, rewards)) => ApplyCharacterProgressRes {
            ok,
            error_code: error_code.to_string(),
            character_id: character_id.to_string(),
            applied: outcome.applied,
            progress_id: outcome.progress_id.clone(),
            source_type: outcome.source_type.clone(),
            source_id: outcome.source_id.clone(),
            rewards,
        },
        None => ApplyCharacterProgressRes {
            ok,
            error_code: error_code.to_string(),
            character_id: character_id.to_string(),
            applied: false,
            progress_id: String::new(),
            source_type: String::new(),
            source_id: String::new(),
            rewards: Vec::new(),
        },
    };

    connection.queue_message(MessageType::ApplyCharacterProgressRes, seq, response)
}

pub(crate) fn to_reward_summary(
    table: &crate::csv_code::titletable::TitleTable,
    reward: &CharacterProgressRewardOutcome,
) -> CharacterProgressRewardSummary {
    CharacterProgressRewardSummary {
        reward_type: reward.reward_type.clone(),
        reward_id: reward.reward_id.clone(),
        status: reward.status.clone(),
        title: reward
            .title
            .as_ref()
            .map(|title| to_title_summary(table, Some(title), &title.title_id)),
        discipline: reward.discipline.as_ref().map(to_discipline_summary),
        eligibility: reward.eligibility.clone().unwrap_or_default(),
    }
}
