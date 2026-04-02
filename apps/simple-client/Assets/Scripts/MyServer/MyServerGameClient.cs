using System;
using System.Collections.Concurrent;
using System.IO;
using System.Net.Sockets;
using System.Threading;
using System.Threading.Tasks;

namespace MyServer.SimpleClient
{
    public sealed class MyServerGameClient : IDisposable
    {
        private readonly int _requestTimeoutMs;
        private readonly SynchronizationContext _syncContext;
        private readonly SemaphoreSlim _sendLock = new SemaphoreSlim(1, 1);
        private readonly ConcurrentDictionary<uint, PendingRequest> _pendingRequests =
            new ConcurrentDictionary<uint, PendingRequest>();

        private TcpClient _tcpClient;
        private NetworkStream _stream;
        private CancellationTokenSource _receiveCts;
        private Task _receiveLoopTask;
        private long _nextSeq;
        private int _disconnectRaised;
        private bool _disposed;

        public MyServerGameClient(int requestTimeoutMs = 5000, SynchronizationContext syncContext = null)
        {
            _requestTimeoutMs = Math.Max(1000, requestTimeoutMs);
            _syncContext = syncContext ?? SynchronizationContext.Current;
        }

        public bool isConnected => _tcpClient != null && _tcpClient.Connected && _stream != null;

        public event Action<RoomStatePush> roomStatePushed;
        public event Action<GamePushMessage> gameMessagePushed;
        public event Action<FrameBundlePush> frameBundlePushed;
        public event Action<RoomFrameRatePush> roomFrameRatePushed;
        public event Action<ErrorResponse> errorReceived;
        public event Action<string> disconnected;

        public async Task ConnectAsync(string host, int port, CancellationToken cancellationToken = default)
        {
            ThrowIfDisposed();

            if (isConnected)
            {
                return;
            }

            if (string.IsNullOrWhiteSpace(host))
            {
                throw new ArgumentException("host is required", nameof(host));
            }

            _tcpClient = new TcpClient
            {
                NoDelay = true
            };

            using (cancellationToken.Register(() => SafeCloseSocket()))
            {
                await _tcpClient.ConnectAsync(host, port).ConfigureAwait(false);
            }

            _stream = _tcpClient.GetStream();
            _receiveCts = new CancellationTokenSource();
            _disconnectRaised = 0;
            _receiveLoopTask = ReceiveLoopAsync(_receiveCts.Token);
        }

        public Task<AuthResponse> AuthenticateAsync(string ticket, CancellationToken cancellationToken = default)
        {
            if (string.IsNullOrWhiteSpace(ticket))
            {
                throw new ArgumentException("ticket is required", nameof(ticket));
            }

            return SendRequestAsync(
                GameMessageType.AuthReq,
                GameMessageType.AuthRes,
                MyServerGameProtocol.EncodeAuthRequest(ticket),
                MyServerGameProtocol.DecodeAuthResponse,
                cancellationToken);
        }

        public Task<PingResponse> PingAsync(long clientTime, CancellationToken cancellationToken = default)
        {
            return SendRequestAsync(
                GameMessageType.PingReq,
                GameMessageType.PingRes,
                MyServerGameProtocol.EncodePingRequest(clientTime),
                MyServerGameProtocol.DecodePingResponse,
                cancellationToken);
        }

        public Task<RoomJoinResponse> JoinRoomAsync(string roomId, CancellationToken cancellationToken = default)
        {
            return SendRequestAsync(
                GameMessageType.RoomJoinReq,
                GameMessageType.RoomJoinRes,
                MyServerGameProtocol.EncodeRoomJoinRequest(roomId),
                MyServerGameProtocol.DecodeRoomJoinResponse,
                cancellationToken);
        }

        public Task<RoomLeaveResponse> LeaveRoomAsync(CancellationToken cancellationToken = default)
        {
            return SendRequestAsync(
                GameMessageType.RoomLeaveReq,
                GameMessageType.RoomLeaveRes,
                MyServerGameProtocol.EncodeRoomLeaveRequest(),
                MyServerGameProtocol.DecodeRoomLeaveResponse,
                cancellationToken);
        }

        public Task<RoomReadyResponse> SetReadyAsync(bool ready, CancellationToken cancellationToken = default)
        {
            return SendRequestAsync(
                GameMessageType.RoomReadyReq,
                GameMessageType.RoomReadyRes,
                MyServerGameProtocol.EncodeRoomReadyRequest(ready),
                MyServerGameProtocol.DecodeRoomReadyResponse,
                cancellationToken);
        }

        public Task<RoomStartResponse> StartGameAsync(CancellationToken cancellationToken = default)
        {
            return SendRequestAsync(
                GameMessageType.RoomStartReq,
                GameMessageType.RoomStartRes,
                MyServerGameProtocol.EncodeRoomStartRequest(),
                MyServerGameProtocol.DecodeRoomStartResponse,
                cancellationToken);
        }

        public Task<PlayerInputResponse> SendPlayerInputAsync(
            uint frameId,
            string action,
            string payloadJson,
            CancellationToken cancellationToken = default)
        {
            return SendRequestAsync(
                GameMessageType.PlayerInputReq,
                GameMessageType.PlayerInputRes,
                MyServerGameProtocol.EncodePlayerInputRequest(frameId, action, payloadJson),
                MyServerGameProtocol.DecodePlayerInputResponse,
                cancellationToken);
        }

        public Task<PlayerInputResponse> SendPlayerInputAsync(
            string action,
            string payloadJson,
            CancellationToken cancellationToken = default)
        {
            return SendPlayerInputAsync(0, action, payloadJson, cancellationToken);
        }

        public Task<RoomEndResponse> EndGameAsync(string reason, CancellationToken cancellationToken = default)
        {
            return SendRequestAsync(
                GameMessageType.RoomEndReq,
                GameMessageType.RoomEndRes,
                MyServerGameProtocol.EncodeRoomEndRequest(reason),
                MyServerGameProtocol.DecodeRoomEndResponse,
                cancellationToken);
        }

        public Task<GetRoomDataResponse> GetRoomDataAsync(int idStart, int idEnd, CancellationToken cancellationToken = default)
        {
            return SendRequestAsync(
                GameMessageType.GetRoomDataReq,
                GameMessageType.GetRoomDataRes,
                MyServerGameProtocol.EncodeGetRoomDataRequest(idStart, idEnd),
                MyServerGameProtocol.DecodeGetRoomDataResponse,
                cancellationToken);
        }

        public void Disconnect()
        {
            if (_disposed || (!isConnected && _receiveCts == null))
            {
                return;
            }

            CleanupConnection("Disconnected by client.");
        }

        public void Dispose()
        {
            if (_disposed)
            {
                return;
            }

            _disposed = true;
            CleanupConnection("Disposed.");
            _sendLock.Dispose();
        }

        private async Task<TResponse> SendRequestAsync<TResponse>(
            GameMessageType requestType,
            GameMessageType responseType,
            byte[] body,
            Func<byte[], TResponse> decoder,
            CancellationToken cancellationToken)
        {
            ThrowIfDisposed();
            EnsureConnected();

            var seq = (uint)Interlocked.Increment(ref _nextSeq);
            var pending = new PendingRequest(
                responseType,
                payload => decoder(payload),
                new TaskCompletionSource<object>(TaskCreationOptions.RunContinuationsAsynchronously));

            using (var timeoutCts = new CancellationTokenSource(_requestTimeoutMs))
            using (var linkedCts = CancellationTokenSource.CreateLinkedTokenSource(cancellationToken, timeoutCts.Token))
            {
                pending.registration = linkedCts.Token.Register(() =>
                {
                    if (cancellationToken.IsCancellationRequested)
                    {
                        FailPending(seq, new OperationCanceledException(cancellationToken));
                        return;
                    }

                    FailPending(seq, new TimeoutException("Game request timed out after " + _requestTimeoutMs + "ms"));
                });

                if (!_pendingRequests.TryAdd(seq, pending))
                {
                    pending.registration.Dispose();
                    throw new InvalidOperationException("Failed to register pending request.");
                }

                try
                {
                    await WritePacketAsync(requestType, seq, body, linkedCts.Token).ConfigureAwait(false);
                    var result = await pending.completion.Task.ConfigureAwait(false);
                    return (TResponse)result;
                }
                catch
                {
                    FailPending(seq, null);
                    throw;
                }
            }
        }

        private async Task WritePacketAsync(
            GameMessageType messageType,
            uint seq,
            byte[] body,
            CancellationToken cancellationToken)
        {
            EnsureConnected();
            var packet = MyServerGameProtocol.EncodePacket(messageType, seq, body);

            await _sendLock.WaitAsync(cancellationToken).ConfigureAwait(false);
            try
            {
                await _stream.WriteAsync(packet, 0, packet.Length, cancellationToken).ConfigureAwait(false);
                await _stream.FlushAsync(cancellationToken).ConfigureAwait(false);
            }
            finally
            {
                _sendLock.Release();
            }
        }

        private async Task ReceiveLoopAsync(CancellationToken cancellationToken)
        {
            try
            {
                while (!cancellationToken.IsCancellationRequested)
                {
                    var headerBytes = await ReadExactAsync(MyServerGameProtocol.HeaderLength, cancellationToken)
                        .ConfigureAwait(false);
                    var header = MyServerGameProtocol.ParseHeader(headerBytes);
                    var body = header.bodyLength == 0
                        ? Array.Empty<byte>()
                        : await ReadExactAsync((int)header.bodyLength, cancellationToken).ConfigureAwait(false);

                    HandleInboundPacket(header.messageType, header.seq, body);
                }
            }
            catch (OperationCanceledException)
            {
            }
            catch (Exception ex)
            {
                CleanupConnection("Game connection closed: " + ex.Message);
                return;
            }

            CleanupConnection("Game connection closed.");
        }

        private async Task<byte[]> ReadExactAsync(int length, CancellationToken cancellationToken)
        {
            var buffer = new byte[length];
            var offset = 0;
            while (offset < length)
            {
                var read = await _stream.ReadAsync(buffer, offset, length - offset, cancellationToken).ConfigureAwait(false);
                if (read <= 0)
                {
                    throw new EndOfStreamException("Unexpected end of stream.");
                }

                offset += read;
            }

            return buffer;
        }

        private void HandleInboundPacket(GameMessageType messageType, uint seq, byte[] body)
        {
            if (messageType == GameMessageType.RoomStatePush)
            {
                var push = MyServerGameProtocol.DecodeRoomStatePush(body);
                Dispatch(() => roomStatePushed?.Invoke(push));
                return;
            }

            if (messageType == GameMessageType.GameMessagePush)
            {
                var push = MyServerGameProtocol.DecodeGamePushMessage(body);
                Dispatch(() => gameMessagePushed?.Invoke(push));
                return;
            }

            if (messageType == GameMessageType.FrameBundlePush)
            {
                var push = MyServerGameProtocol.DecodeFrameBundlePush(body);
                Dispatch(() => frameBundlePushed?.Invoke(push));
                return;
            }

            if (messageType == GameMessageType.RoomFrameRatePush)
            {
                var push = MyServerGameProtocol.DecodeRoomFrameRatePush(body);
                Dispatch(() => roomFrameRatePushed?.Invoke(push));
                return;
            }

            if (messageType == GameMessageType.ErrorRes)
            {
                var error = MyServerGameProtocol.DecodeErrorResponse(body);
                Dispatch(() => errorReceived?.Invoke(error));

                if (seq != 0)
                {
                    FailPending(seq, new MyServerGameException(error.errorCode, error.message));
                }

                return;
            }

            if (_pendingRequests.TryRemove(seq, out var pending))
            {
                pending.registration.Dispose();

                if (pending.responseType != messageType)
                {
                    pending.completion.TrySetException(
                        new InvalidDataException(
                            "Unexpected response type. expected=" + pending.responseType + " actual=" + messageType));
                    return;
                }

                try
                {
                    pending.completion.TrySetResult(pending.decoder(body));
                }
                catch (Exception ex)
                {
                    pending.completion.TrySetException(ex);
                }
            }
        }

        private void FailPending(uint seq, Exception error)
        {
            if (_pendingRequests.TryRemove(seq, out var pending))
            {
                pending.registration.Dispose();

                if (error == null)
                {
                    pending.completion.TrySetCanceled();
                }
                else
                {
                    pending.completion.TrySetException(error);
                }
            }
        }

        private void CleanupConnection(string reason)
        {
            _receiveCts?.Cancel();
            _receiveCts?.Dispose();
            _receiveCts = null;

            SafeCloseSocket();

            foreach (var pair in _pendingRequests)
            {
                FailPending(pair.Key, new IOException(reason));
            }

            if (Interlocked.Exchange(ref _disconnectRaised, 1) == 0)
            {
                Dispatch(() => disconnected?.Invoke(reason));
            }
        }

        private void EnsureConnected()
        {
            if (!isConnected)
            {
                throw new InvalidOperationException("Game client is not connected.");
            }
        }

        private void SafeCloseSocket()
        {
            try
            {
                _stream?.Close();
            }
            catch
            {
            }

            try
            {
                _tcpClient?.Close();
            }
            catch
            {
            }

            _stream = null;
            _tcpClient = null;
        }

        private void Dispatch(Action action)
        {
            if (action == null)
            {
                return;
            }

            if (_syncContext == null)
            {
                action();
                return;
            }

            _syncContext.Post(_ => action(), null);
        }

        private void ThrowIfDisposed()
        {
            if (_disposed)
            {
                throw new ObjectDisposedException(nameof(MyServerGameClient));
            }
        }

        private sealed class PendingRequest
        {
            public PendingRequest(
                GameMessageType responseType,
                Func<byte[], object> decoder,
                TaskCompletionSource<object> completion)
            {
                this.responseType = responseType;
                this.decoder = decoder;
                this.completion = completion;
                this.registration = default;
            }

            public GameMessageType responseType;
            public Func<byte[], object> decoder;
            public TaskCompletionSource<object> completion;
            public CancellationTokenRegistration registration;
        }
    }
}
