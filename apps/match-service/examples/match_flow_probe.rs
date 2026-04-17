#![allow(dead_code)]

#[path = "../src/proto/mod.rs"]
mod proto;

use std::env;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use proto::myserver::matchservice::{
    CreateRoomAndJoinReq, MatchEndReq, MatchEvent, MatchEventStreamReq, MatchStartReq,
    MatchStatusReq, PlayerJoinedReq, PlayerLeftReq, match_internal_client::MatchInternalClient,
    match_service_client::MatchServiceClient,
};
use tonic::transport::Channel;

#[derive(Debug)]
struct Options {
    scenario: String,
    addr: String,
    mode: String,
    timeout_secs: u64,
    player_ids: Vec<String>,
}

fn parse_options() -> Result<Options, String> {
    let mut scenario = "matched".to_string();
    let mut addr = env::var("MATCH_SERVICE_ADDR").unwrap_or_else(|_| "http://127.0.0.1:9002".to_string());
    let mut mode = "1v1".to_string();
    let mut timeout_secs = 15_u64;
    let mut player_ids = Vec::new();

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--scenario" => {
                scenario = args
                    .next()
                    .ok_or_else(|| "--scenario requires a value".to_string())?;
            }
            "--addr" => {
                addr = args.next().ok_or_else(|| "--addr requires a value".to_string())?;
            }
            "--mode" => {
                mode = args.next().ok_or_else(|| "--mode requires a value".to_string())?;
            }
            "--timeout-secs" => {
                let raw = args
                    .next()
                    .ok_or_else(|| "--timeout-secs requires a value".to_string())?;
                timeout_secs = raw
                    .parse::<u64>()
                    .map_err(|_| format!("invalid --timeout-secs value: {raw}"))?;
            }
            "--player-id" => {
                player_ids.push(
                    args.next()
                        .ok_or_else(|| "--player-id requires a value".to_string())?,
                );
            }
            "--help" | "-h" => {
                return Err(
                    "usage: cargo run --example match_flow_probe -- [--scenario matched|timeout] [--addr http://127.0.0.1:9002] [--mode 1v1] [--timeout-secs 15] [--player-id PLAYER_A --player-id PLAYER_B]"
                        .to_string(),
                );
            }
            other => {
                return Err(format!("unknown argument: {other}"));
            }
        }
    }

    if player_ids.is_empty() {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| format!("system clock error: {error}"))?
            .as_millis();
        player_ids.push(format!("probe-{seed}-a"));
        if scenario == "matched" || scenario == "player-left" {
            player_ids.push(format!("probe-{seed}-b"));
        }
    }

    let expected_players = match scenario.as_str() {
        "matched" => 2,
        "player-left" => 2,
        "timeout" => 1,
        other => return Err(format!("unsupported --scenario value: {other}")),
    };

    if player_ids.len() != expected_players {
        return Err(format!(
            "scenario {scenario} expects exactly {expected_players} player ids, got {}",
            player_ids.len(),
        ));
    }

    Ok(Options {
        scenario,
        addr,
        mode,
        timeout_secs,
        player_ids,
    })
}

async fn connect(addr: &str) -> Result<MatchServiceClient<Channel>, Box<dyn std::error::Error>> {
    Ok(MatchServiceClient::connect(addr.to_string()).await?)
}

async fn connect_internal(
    addr: &str,
) -> Result<MatchInternalClient<Channel>, Box<dyn std::error::Error>> {
    Ok(MatchInternalClient::connect(addr.to_string()).await?)
}

async fn wait_for_event(
    player_id: &str,
    mut stream: tonic::Streaming<MatchEvent>,
    timeout_secs: u64,
) -> Result<MatchEvent, Box<dyn std::error::Error>> {
    let event = tokio::time::timeout(Duration::from_secs(timeout_secs), stream.message()).await??;
    let event = event.ok_or_else(|| format!("event stream closed for player {player_id}"))?;
    Ok(event)
}

async fn run_matched_probe(options: &Options) -> Result<(), Box<dyn std::error::Error>> {
    let player_a = options.player_ids[0].clone();
    let player_b = options.player_ids[1].clone();

    println!(
        "match_flow_probe matched: addr={} mode={} players={:?}",
        options.addr, options.mode, options.player_ids
    );

    let mut stream_client_a = connect(&options.addr).await?;
    let mut stream_client_b = connect(&options.addr).await?;
    let mut action_client_a = connect(&options.addr).await?;
    let mut action_client_b = connect(&options.addr).await?;
    let mut status_client = connect(&options.addr).await?;

    let stream_a = stream_client_a
        .match_event_stream(MatchEventStreamReq {
            player_id: player_a.clone(),
        })
        .await?
        .into_inner();
    let stream_b = stream_client_b
        .match_event_stream(MatchEventStreamReq {
            player_id: player_b.clone(),
        })
        .await?
        .into_inner();

    let (start_a, start_b) = tokio::try_join!(
        action_client_a.match_start(MatchStartReq {
            player_id: player_a.clone(),
            mode: options.mode.clone(),
            rank_tier: 0,
        }),
        action_client_b.match_start(MatchStartReq {
            player_id: player_b.clone(),
            mode: options.mode.clone(),
            rank_tier: 0,
        })
    )?;

    let start_a = start_a.into_inner();
    let start_b = start_b.into_inner();

    println!(
        "match_start responses: A={{ ok: {}, match_id: {}, error: {} }} B={{ ok: {}, match_id: {}, error: {} }}",
        start_a.ok,
        start_a.match_id,
        start_a.error_code,
        start_b.ok,
        start_b.match_id,
        start_b.error_code
    );

    if !start_a.ok || !start_b.ok {
        return Err(format!(
            "match_start failed: A ok={} error={} ; B ok={} error={}",
            start_a.ok, start_a.error_code, start_b.ok, start_b.error_code
        )
        .into());
    }

    let (event_a, event_b) = tokio::try_join!(
        wait_for_event(&player_a, stream_a, options.timeout_secs),
        wait_for_event(&player_b, stream_b, options.timeout_secs)
    )?;

    println!(
        "match events: A={{ event: {}, match_id: {}, room_id: {}, token_len: {} }} B={{ event: {}, match_id: {}, room_id: {}, token_len: {} }}",
        event_a.event,
        event_a.match_id,
        event_a.room_id,
        event_a.token.len(),
        event_b.event,
        event_b.match_id,
        event_b.room_id,
        event_b.token.len()
    );

    if event_a.event != "matched" || event_b.event != "matched" {
        return Err(format!(
            "expected matched events, got A={} B={}",
            event_a.event, event_b.event
        )
        .into());
    }
    if event_a.room_id.is_empty() || event_b.room_id.is_empty() {
        return Err("matched event returned empty room_id".into());
    }
    if event_a.token.is_empty() || event_b.token.is_empty() {
        return Err("matched event returned empty token".into());
    }
    if event_a.room_id != event_b.room_id {
        return Err(format!(
            "players matched into different rooms: {} vs {}",
            event_a.room_id, event_b.room_id
        )
        .into());
    }
    if event_a.match_id != event_b.match_id {
        return Err(format!(
            "players matched into different match ids: {} vs {}",
            event_a.match_id, event_b.match_id
        )
        .into());
    }

    let status_a = status_client
        .match_status(MatchStatusReq {
            player_id: player_a.clone(),
        })
        .await?
        .into_inner();
    let status_b = status_client
        .match_status(MatchStatusReq {
            player_id: player_b.clone(),
        })
        .await?
        .into_inner();

    println!(
        "match status: A={{ status: {}, match_id: {}, room_id: {} }} B={{ status: {}, match_id: {}, room_id: {} }}",
        status_a.status, status_a.match_id, status_a.room_id, status_b.status, status_b.match_id, status_b.room_id
    );

    if status_a.status != "matched" || status_b.status != "matched" {
        return Err(format!(
            "expected matched status, got A={} B={}",
            status_a.status, status_b.status
        )
        .into());
    }
    if status_a.room_id != event_a.room_id || status_b.room_id != event_b.room_id {
        return Err("match_status room_id does not match event room_id".into());
    }
    if status_a.token.is_empty() || status_b.token.is_empty() {
        return Err("match_status token is empty".into());
    }

    println!("match_flow_probe matched: success");
    Ok(())
}

async fn run_timeout_probe(options: &Options) -> Result<(), Box<dyn std::error::Error>> {
    let player_id = options.player_ids[0].clone();

    println!(
        "match_flow_probe timeout: addr={} mode={} player={} timeout_secs={}",
        options.addr, options.mode, player_id, options.timeout_secs
    );

    let mut stream_client = connect(&options.addr).await?;
    let mut action_client = connect(&options.addr).await?;
    let mut status_client = connect(&options.addr).await?;

    let stream = stream_client
        .match_event_stream(MatchEventStreamReq {
            player_id: player_id.clone(),
        })
        .await?
        .into_inner();

    let start = action_client
        .match_start(MatchStartReq {
            player_id: player_id.clone(),
            mode: options.mode.clone(),
            rank_tier: 0,
        })
        .await?
        .into_inner();

    println!(
        "match_start response: ok={} match_id={} error={}",
        start.ok, start.match_id, start.error_code
    );

    if !start.ok {
        return Err(format!("match_start failed: {}", start.error_code).into());
    }

    let event = wait_for_event(&player_id, stream, options.timeout_secs).await?;
    println!(
        "timeout event: event={} match_id={} room_id={} error={}",
        event.event, event.match_id, event.room_id, event.error_code
    );

    if event.event != "match_failed" {
        return Err(format!("expected match_failed event, got {}", event.event).into());
    }
    if event.error_code != "MATCH_TIMEOUT" {
        return Err(format!(
            "expected MATCH_TIMEOUT error code, got {}",
            event.error_code
        )
        .into());
    }

    let status = status_client
        .match_status(MatchStatusReq {
            player_id: player_id.clone(),
        })
        .await?
        .into_inner();

    println!(
        "timeout status: status={} match_id={} room_id={}",
        status.status, status.match_id, status.room_id
    );

    if status.status != "idle" {
        return Err(format!("expected idle status after timeout, got {}", status.status).into());
    }

    println!("match_flow_probe timeout: success");
    Ok(())
}

async fn run_player_left_probe(options: &Options) -> Result<(), Box<dyn std::error::Error>> {
    let player_a = options.player_ids[0].clone();
    let player_b = options.player_ids[1].clone();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)?
        .as_millis();
    let match_id = format!("probe-player-left-{now}");
    let room_id = format!("room-probe-player-left-{now}");

    println!(
        "match_flow_probe player-left: addr={} mode={} players={:?} match_id={} room_id={}",
        options.addr, options.mode, options.player_ids, match_id, room_id
    );

    let mut internal_client = connect_internal(&options.addr).await?;
    let mut status_client = connect(&options.addr).await?;

    let create = internal_client
        .create_room_and_join(CreateRoomAndJoinReq {
            match_id: match_id.clone(),
            room_id: room_id.clone(),
            player_ids: vec![player_a.clone(), player_b.clone()],
            mode: options.mode.clone(),
        })
        .await?
        .into_inner();

    println!(
        "create_room_and_join: ok={} error={}",
        create.ok, create.error_code
    );
    if !create.ok {
        return Err(format!("create_room_and_join failed: {}", create.error_code).into());
    }

    let joined_a = internal_client
        .player_joined(PlayerJoinedReq {
            match_id: match_id.clone(),
            player_id: player_a.clone(),
            room_id: room_id.clone(),
        })
        .await?
        .into_inner();
    let joined_b = internal_client
        .player_joined(PlayerJoinedReq {
            match_id: match_id.clone(),
            player_id: player_b.clone(),
            room_id: room_id.clone(),
        })
        .await?
        .into_inner();

    println!(
        "player_joined: A={{ ok: {}, error: {} }} B={{ ok: {}, error: {} }}",
        joined_a.ok, joined_a.error_code, joined_b.ok, joined_b.error_code
    );
    if !joined_a.ok || !joined_b.ok {
        return Err(format!(
            "player_joined failed: A ok={} error={} ; B ok={} error={}",
            joined_a.ok, joined_a.error_code, joined_b.ok, joined_b.error_code
        )
        .into());
    }

    let status_joined_a = status_client
        .match_status(MatchStatusReq {
            player_id: player_a.clone(),
        })
        .await?
        .into_inner();
    let status_joined_b = status_client
        .match_status(MatchStatusReq {
            player_id: player_b.clone(),
        })
        .await?
        .into_inner();

    println!(
        "status after player_joined: A={{ status: {}, room_id: {} }} B={{ status: {}, room_id: {} }}",
        status_joined_a.status, status_joined_a.room_id, status_joined_b.status, status_joined_b.room_id
    );
    if status_joined_a.status != "in_room" || status_joined_b.status != "in_room" {
        return Err("expected both players to be in_room after player_joined".into());
    }

    let left_a = internal_client
        .player_left(PlayerLeftReq {
            match_id: match_id.clone(),
            player_id: player_a.clone(),
            reason: "normal".to_string(),
        })
        .await?
        .into_inner();
    let left_b = internal_client
        .player_left(PlayerLeftReq {
            match_id: match_id.clone(),
            player_id: player_b.clone(),
            reason: "normal".to_string(),
        })
        .await?
        .into_inner();

    println!(
        "player_left: A={{ ok: {}, abort: {}, error: {} }} B={{ ok: {}, abort: {}, error: {} }}",
        left_a.ok,
        left_a.match_should_abort,
        left_a.error_code,
        left_b.ok,
        left_b.match_should_abort,
        left_b.error_code
    );
    if !left_a.ok || left_a.match_should_abort {
        return Err("expected first player_left to succeed without abort".into());
    }
    if !left_b.ok || !left_b.match_should_abort {
        return Err("expected second player_left to request abort".into());
    }

    let status_left_a = status_client
        .match_status(MatchStatusReq {
            player_id: player_a.clone(),
        })
        .await?
        .into_inner();
    let status_left_b = status_client
        .match_status(MatchStatusReq {
            player_id: player_b.clone(),
        })
        .await?
        .into_inner();

    println!(
        "status after player_left: A={{ status: {}, room_id: {} }} B={{ status: {}, room_id: {} }}",
        status_left_a.status, status_left_a.room_id, status_left_b.status, status_left_b.room_id
    );
    if status_left_a.status != "matched" || status_left_b.status != "matched" {
        return Err("expected both players to fall back to matched after player_left".into());
    }

    let match_end = internal_client
        .match_end(MatchEndReq {
            match_id: match_id.clone(),
            room_id: room_id.clone(),
            reason: "aborted".to_string(),
        })
        .await?
        .into_inner();

    println!(
        "match_end: ok={} error={}",
        match_end.ok, match_end.error_code
    );
    if !match_end.ok {
        return Err(format!("match_end failed: {}", match_end.error_code).into());
    }

    let status_end_a = status_client
        .match_status(MatchStatusReq {
            player_id: player_a.clone(),
        })
        .await?
        .into_inner();
    let status_end_b = status_client
        .match_status(MatchStatusReq {
            player_id: player_b.clone(),
        })
        .await?
        .into_inner();

    println!(
        "status after match_end: A={{ status: {}, room_id: {} }} B={{ status: {}, room_id: {} }}",
        status_end_a.status, status_end_a.room_id, status_end_b.status, status_end_b.room_id
    );
    if status_end_a.status != "idle" || status_end_b.status != "idle" {
        return Err("expected both players to be idle after match_end".into());
    }

    println!("match_flow_probe player-left: success");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let options = match parse_options() {
        Ok(value) => value,
        Err(message) => {
            eprintln!("{message}");
            std::process::exit(2);
        }
    };

    match options.scenario.as_str() {
        "matched" => run_matched_probe(&options).await?,
        "player-left" => run_player_left_probe(&options).await?,
        "timeout" => run_timeout_probe(&options).await?,
        other => return Err(format!("unsupported scenario: {other}").into()),
    }

    Ok(())
}
