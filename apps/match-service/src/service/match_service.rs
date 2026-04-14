//! MatchService gRPC 实现

use std::pin::Pin;
use tokio::sync::mpsc;
use tonic::{Request, Response, Status};
use tracing::info;

use crate::matcher::SharedSimpleMatcher;
use crate::proto::myserver::matchservice::{
    match_service_server::MatchService,
    match_internal_server::MatchInternal,
    CreateRoomAndJoinReq, CreateRoomAndJoinRes, MatchCancelReq, MatchCancelRes, MatchEndReq,
    MatchEndRes, MatchEvent, MatchEventStreamReq, MatchStartReq, MatchStartRes,
    MatchStatusReq, MatchStatusRes, PlayerJoinedReq, PlayerJoinedRes, PlayerLeftReq,
    PlayerLeftRes,
};

/// MatchService 实现
#[derive(Clone)]
pub struct MatchServiceImpl {
    matcher: SharedSimpleMatcher,
}

impl MatchServiceImpl {
    pub fn new(matcher: SharedSimpleMatcher) -> Self {
        Self { matcher }
    }
}

#[tonic::async_trait]
impl MatchService for MatchServiceImpl {
    /// 客户端发起匹配
    async fn match_start(
        &self,
        request: Request<MatchStartReq>,
    ) -> Result<Response<MatchStartRes>, Status> {
        let req = request.into_inner();

        info!(
            player_id = %req.player_id,
            mode = %req.mode,
            "MatchStart request"
        );

        let mode = req.mode.clone();
        let player_id = req.player_id.clone();

        let result = self.matcher.start_match(player_id, mode).await;

        match result {
            Ok(match_id) => {
                Ok(Response::new(MatchStartRes {
                    ok: true,
                    match_id,
                    error_code: String::new(),
                }))
            }
            Err(e) => {
                tracing::error!(error = %e, "MatchStart failed");
                Ok(Response::new(MatchStartRes {
                    ok: false,
                    match_id: String::new(),
                    error_code: e.error_code().to_string(),
                }))
            }
        }
    }

    /// 客户端取消匹配
    async fn match_cancel(
        &self,
        request: Request<MatchCancelReq>,
    ) -> Result<Response<MatchCancelRes>, Status> {
        let req = request.into_inner();

        info!(
            player_id = %req.player_id,
            match_id = %req.match_id,
            "MatchCancel request"
        );

        let result = self.matcher.cancel_match(&req.player_id, &req.match_id).await;

        match result {
            Ok(()) => {
                Ok(Response::new(MatchCancelRes {
                    ok: true,
                    error_code: String::new(),
                }))
            }
            Err(e) => {
                tracing::error!(error = %e, "MatchCancel failed");
                Ok(Response::new(MatchCancelRes {
                    ok: false,
                    error_code: e.error_code().to_string(),
                }))
            }
        }
    }

    /// 客户端查询匹配状态
    async fn match_status(
        &self,
        request: Request<MatchStatusReq>,
    ) -> Result<Response<MatchStatusRes>, Status> {
        let req = request.into_inner();

        let result = self.matcher.get_status(&req.player_id).await;

        match result {
            Ok(res) => Ok(Response::new(res)),
            Err(e) => {
                tracing::error!(error = %e, "MatchStatus failed");
                Err(Status::internal(e.to_string()))
            }
        }
    }

    /// 客户端订阅匹配事件推送
    type MatchEventStreamStream = Pin<Box<dyn futures_core::Stream<Item = Result<MatchEvent, Status>> + Send>>;

    async fn match_event_stream(
        &self,
        request: Request<MatchEventStreamReq>,
    ) -> Result<Response<Self::MatchEventStreamStream>, Status> {
        let req = request.into_inner();
        let player_id = req.player_id.clone();

        info!(player_id = %player_id, "MatchEventStream connected");

        // 创建通道
        let (tx, rx) = mpsc::channel(128);

        // 注册到 player_state
        let player_state = self.matcher.player_state().clone();
        player_state.register_stream(&player_id, tx).await;

        Ok(Response::new(Box::pin(async_stream::stream! {
            let mut stream = rx;
            while let Some(event) = stream.recv().await {
                yield Ok(event);
            }
        })))
    }
}

/// MatchInternal 服务实现
#[derive(Clone)]
pub struct MatchInternalImpl {
    matcher: SharedSimpleMatcher,
}

impl MatchInternalImpl {
    pub fn new(matcher: SharedSimpleMatcher) -> Self {
        Self { matcher }
    }
}

#[tonic::async_trait]
impl MatchInternal for MatchInternalImpl {
    /// GameServer 创建房间成功后回调
    async fn create_room_and_join(
        &self,
        request: Request<CreateRoomAndJoinReq>,
    ) -> Result<Response<CreateRoomAndJoinRes>, Status> {
        let req = request.into_inner();

        info!(
            match_id = %req.match_id,
            room_id = %req.room_id,
            players = ?req.player_ids,
            mode = %req.mode,
            "CreateRoomAndJoin request"
        );

        Ok(Response::new(CreateRoomAndJoinRes {
            ok: true,
            error_code: String::new(),
        }))
    }

    /// GameServer 通知玩家已进入房间
    async fn player_joined(
        &self,
        request: Request<PlayerJoinedReq>,
    ) -> Result<Response<PlayerJoinedRes>, Status> {
        let req = request.into_inner();

        info!(
            match_id = %req.match_id,
            player_id = %req.player_id,
            room_id = %req.room_id,
            "PlayerJoined request"
        );

        let result = self
            .matcher
            .player_joined(&req.match_id, &req.player_id, &req.room_id)
            .await;

        match result {
            Ok(()) => Ok(Response::new(PlayerJoinedRes {
                ok: true,
                error_code: String::new(),
            })),
            Err(e) => Ok(Response::new(PlayerJoinedRes {
                ok: false,
                error_code: e.error_code().to_string(),
            })),
        }
    }

    /// GameServer 通知玩家已离开房间
    async fn player_left(
        &self,
        request: Request<PlayerLeftReq>,
    ) -> Result<Response<PlayerLeftRes>, Status> {
        let req = request.into_inner();

        info!(
            match_id = %req.match_id,
            player_id = %req.player_id,
            reason = %req.reason,
            "PlayerLeft request"
        );

        let result = self
            .matcher
            .player_left(&req.match_id, &req.player_id, &req.reason)
            .await;

        match result {
            Ok(should_abort) => Ok(Response::new(PlayerLeftRes {
                ok: true,
                match_should_abort: should_abort,
                error_code: String::new(),
            })),
            Err(e) => Ok(Response::new(PlayerLeftRes {
                ok: false,
                match_should_abort: false,
                error_code: e.error_code().to_string(),
            })),
        }
    }

    /// GameServer 通知对局结束
    async fn match_end(
        &self,
        request: Request<MatchEndReq>,
    ) -> Result<Response<MatchEndRes>, Status> {
        let req = request.into_inner();

        info!(
            match_id = %req.match_id,
            room_id = %req.room_id,
            reason = %req.reason,
            "MatchEnd request"
        );

        let result = self
            .matcher
            .match_end(&req.match_id, &req.room_id, &req.reason)
            .await;

        match result {
            Ok(()) => Ok(Response::new(MatchEndRes {
                ok: true,
                error_code: String::new(),
            })),
            Err(e) => Ok(Response::new(MatchEndRes {
                ok: false,
                error_code: e.error_code().to_string(),
            })),
        }
    }
}
