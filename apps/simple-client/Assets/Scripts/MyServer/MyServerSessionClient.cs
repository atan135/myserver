using System;
using System.Threading;
using System.Threading.Tasks;

namespace MyServer.SimpleClient
{
    public sealed class MyServerSessionClient : IDisposable
    {
        private readonly MyServerClientConfig _config;

        public MyServerSessionClient(MyServerClientConfig config, SynchronizationContext syncContext = null)
        {
            _config = config ?? throw new ArgumentNullException(nameof(config));
            http = new MyServerHttpClient(_config);
            game = new MyServerGameClient(_config.requestTimeoutMs, syncContext);

            game.roomStatePushed += push =>
            {
                latestRoomSnapshot = push != null ? push.snapshot : null;
                roomStatePushed?.Invoke(push);
            };
            game.gameMessagePushed += push => gameMessagePushed?.Invoke(push);
            game.frameBundlePushed += push => frameBundlePushed?.Invoke(push);
            game.roomFrameRatePushed += push => roomFrameRatePushed?.Invoke(push);
            game.errorReceived += error => errorReceived?.Invoke(error);
            game.disconnected += reason => disconnected?.Invoke(reason);
        }

        public MyServerHttpClient http { get; }
        public MyServerGameClient game { get; }
        public GuestLoginResponse currentLogin { get; private set; }
        public AuthResponse currentGameAuth { get; private set; }
        public RoomSnapshot latestRoomSnapshot { get; private set; }

        public event Action<RoomStatePush> roomStatePushed;
        public event Action<GamePushMessage> gameMessagePushed;
        public event Action<FrameBundlePush> frameBundlePushed;
        public event Action<RoomFrameRatePush> roomFrameRatePushed;
        public event Action<ErrorResponse> errorReceived;
        public event Action<string> disconnected;

        public async Task<AuthResponse> GuestLoginAndConnectAsync(
            string guestId = "",
            CancellationToken cancellationToken = default)
        {
            currentLogin = await http.GuestLoginAsync(guestId, cancellationToken);
            return await ConnectWithTicketAsync(currentLogin.ticket, cancellationToken);
        }

        public async Task<AuthResponse> RefreshTicketAndReconnectAsync(CancellationToken cancellationToken = default)
        {
            EnsureLoggedIn();

            var newTicket = await http.IssueGameTicketAsync(currentLogin.accessToken, cancellationToken);

            currentLogin.ticket = newTicket.ticket;
            currentLogin.ticketExpiresAt = newTicket.ticketExpiresAt;

            return await ConnectWithTicketAsync(currentLogin.ticket, cancellationToken);
        }

        public async Task<AuthResponse> ConnectWithTicketAsync(
            string ticket,
            CancellationToken cancellationToken = default)
        {
            if (game.isConnected)
            {
                game.Disconnect();
            }

            await game.ConnectAsync(_config.gameHost, _config.gamePort, cancellationToken);
            latestRoomSnapshot = null;
            currentGameAuth = await game.AuthenticateAsync(ticket, cancellationToken);
            return currentGameAuth;
        }

        public Task<AuthMeResponse> GetMeAsync(CancellationToken cancellationToken = default)
        {
            EnsureLoggedIn();
            return http.GetMeAsync(currentLogin.accessToken, cancellationToken);
        }

        public Task<IssueGameTicketResponse> IssueGameTicketAsync(CancellationToken cancellationToken = default)
        {
            EnsureLoggedIn();
            return http.IssueGameTicketAsync(currentLogin.accessToken, cancellationToken);
        }

        public Task<PingResponse> PingAsync(CancellationToken cancellationToken = default)
        {
            return game.PingAsync(DateTimeOffset.UtcNow.ToUnixTimeMilliseconds(), cancellationToken);
        }

        public Task<RoomJoinResponse> JoinRoomAsync(string roomId, CancellationToken cancellationToken = default)
        {
            return game.JoinRoomAsync(roomId, cancellationToken);
        }

        public Task<RoomLeaveResponse> LeaveRoomAsync(CancellationToken cancellationToken = default)
        {
            return game.LeaveRoomAsync(cancellationToken);
        }

        public Task<RoomReadyResponse> SetReadyAsync(bool ready, CancellationToken cancellationToken = default)
        {
            return game.SetReadyAsync(ready, cancellationToken);
        }

        public Task<RoomStartResponse> StartGameAsync(CancellationToken cancellationToken = default)
        {
            return game.StartGameAsync(cancellationToken);
        }

        public Task<PlayerInputResponse> SendPlayerInputAsync(
            uint frameId,
            string action,
            string payloadJson,
            CancellationToken cancellationToken = default)
        {
            return game.SendPlayerInputAsync(frameId, action, payloadJson, cancellationToken);
        }

        public Task<PlayerInputResponse> SendPlayerInputAsync(
            string action,
            string payloadJson,
            CancellationToken cancellationToken = default)
        {
            return game.SendPlayerInputAsync(action, payloadJson, cancellationToken);
        }

        public Task<RoomEndResponse> EndGameAsync(string reason, CancellationToken cancellationToken = default)
        {
            return game.EndGameAsync(reason, cancellationToken);
        }

        public Task<GetRoomDataResponse> GetRoomDataAsync(int idStart, int idEnd, CancellationToken cancellationToken = default)
        {
            return game.GetRoomDataAsync(idStart, idEnd, cancellationToken);
        }

        public void Dispose()
        {
            game.Dispose();
        }

        private void EnsureLoggedIn()
        {
            if (currentLogin == null || string.IsNullOrWhiteSpace(currentLogin.accessToken))
            {
                throw new InvalidOperationException("Guest login must be completed before calling this API.");
            }
        }
    }
}
