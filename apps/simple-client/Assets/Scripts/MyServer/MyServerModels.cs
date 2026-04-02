using System;
using System.Collections.Generic;

namespace MyServer.SimpleClient
{
    [Serializable]
    public sealed class GuestLoginResponse
    {
        public bool ok;
        public string playerId = string.Empty;
        public string guestId = string.Empty;
        public string accessToken = string.Empty;
        public string ticket = string.Empty;
        public string ticketExpiresAt = string.Empty;
    }

    [Serializable]
    public sealed class AuthMeResponse
    {
        public bool ok;
        public string playerId = string.Empty;
        public string guestId = string.Empty;
        public string createdAt = string.Empty;
    }

    [Serializable]
    public sealed class IssueGameTicketResponse
    {
        public bool ok;
        public string playerId = string.Empty;
        public string ticket = string.Empty;
        public string ticketExpiresAt = string.Empty;
    }

    public enum GameMessageType : ushort
    {
        AuthReq = 1001,
        AuthRes = 1002,
        PingReq = 1003,
        PingRes = 1004,
        RoomJoinReq = 1101,
        RoomJoinRes = 1102,
        RoomLeaveReq = 1103,
        RoomLeaveRes = 1104,
        RoomReadyReq = 1105,
        RoomReadyRes = 1106,
        RoomStartReq = 1107,
        RoomStartRes = 1108,
        PlayerInputReq = 1111,
        PlayerInputRes = 1112,
        RoomEndReq = 1113,
        RoomEndRes = 1114,
        RoomStatePush = 1201,
        GameMessagePush = 1202,
        FrameBundlePush = 1203,
        RoomFrameRatePush = 1204,
        GetRoomDataReq = 1301,
        GetRoomDataRes = 1302,
        ErrorRes = 9000
    }

    [Serializable]
    public sealed class AuthResponse
    {
        public bool ok;
        public string playerId = string.Empty;
        public string errorCode = string.Empty;
    }

    [Serializable]
    public sealed class PingResponse
    {
        public long serverTime;
    }

    [Serializable]
    public sealed class RoomJoinResponse
    {
        public bool ok;
        public string roomId = string.Empty;
        public string errorCode = string.Empty;
    }

    [Serializable]
    public sealed class RoomLeaveResponse
    {
        public bool ok;
        public string roomId = string.Empty;
        public string errorCode = string.Empty;
    }

    [Serializable]
    public sealed class RoomReadyResponse
    {
        public bool ok;
        public string roomId = string.Empty;
        public bool ready;
        public string errorCode = string.Empty;
    }

    [Serializable]
    public sealed class RoomStartResponse
    {
        public bool ok;
        public string roomId = string.Empty;
        public string errorCode = string.Empty;
    }

    [Serializable]
    public sealed class PlayerInputResponse
    {
        public bool ok;
        public string roomId = string.Empty;
        public string errorCode = string.Empty;
    }

    [Serializable]
    public sealed class RoomEndResponse
    {
        public bool ok;
        public string roomId = string.Empty;
        public string errorCode = string.Empty;
    }

    [Serializable]
    public sealed class GetRoomDataResponse
    {
        public bool ok;
        public List<string> field0List = new List<string>();
        public string errorCode = string.Empty;
    }

    [Serializable]
    public sealed class ErrorResponse
    {
        public string errorCode = string.Empty;
        public string message = string.Empty;
    }

    [Serializable]
    public sealed class RoomMember
    {
        public string playerId = string.Empty;
        public bool ready;
        public bool isOwner;
    }

    [Serializable]
    public sealed class RoomSnapshot
    {
        public string roomId = string.Empty;
        public string ownerPlayerId = string.Empty;
        public string state = string.Empty;
        public List<RoomMember> members = new List<RoomMember>();
    }

    [Serializable]
    public sealed class RoomStatePush
    {
        public string eventName = string.Empty;
        public RoomSnapshot snapshot;
    }

    [Serializable]
    public sealed class GamePushMessage
    {
        public string eventName = string.Empty;
        public string roomId = string.Empty;
        public string playerId = string.Empty;
        public string action = string.Empty;
        public string payloadJson = string.Empty;
    }

    [Serializable]
    public sealed class FrameInput
    {
        public string playerId = string.Empty;
        public string action = string.Empty;
        public string payloadJson = string.Empty;
    }

    [Serializable]
    public sealed class FrameBundlePush
    {
        public string roomId = string.Empty;
        public uint frameId;
        public uint fps;
        public List<FrameInput> inputs = new List<FrameInput>();
        public bool isSilentFrame;
    }

    [Serializable]
    public sealed class RoomFrameRatePush
    {
        public string roomId = string.Empty;
        public uint fps;
        public string reason = string.Empty;
    }

    public sealed class MyServerHttpException : Exception
    {
        public long statusCode { get; }
        public string responseBody { get; }

        public MyServerHttpException(string message, long statusCode, string responseBody)
            : base(message)
        {
            this.statusCode = statusCode;
            this.responseBody = responseBody ?? string.Empty;
        }
    }

    public sealed class MyServerGameException : Exception
    {
        public string errorCode { get; }

        public MyServerGameException(string errorCode, string message)
            : base(message)
        {
            this.errorCode = errorCode ?? string.Empty;
        }
    }
}
