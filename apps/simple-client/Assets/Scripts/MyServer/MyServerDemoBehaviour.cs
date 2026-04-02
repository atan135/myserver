using System;
using System.Text;
using System.Threading.Tasks;
using UnityEngine;

namespace MyServer.SimpleClient
{
    public sealed class MyServerDemoBehaviour : MonoBehaviour
    {
        [Header("Server")]
        [SerializeField] private string httpBaseUrl = "http://127.0.0.1:3000";
        [SerializeField] private string gameHost = "127.0.0.1";
        [SerializeField] private int gamePort = 7000;
        [SerializeField] private int requestTimeoutMs = 5000;

        [Header("Session")]
        [SerializeField] private string guestId = "demo-user";
        [SerializeField] private string roomId = "room-demo";
        [SerializeField] private uint inputFrameId = 0;
        [SerializeField] private string inputAction = "move";
        [SerializeField] private string inputPayloadJson = "{\"x\":1,\"y\":0}";
        [SerializeField] private string endReason = "manual_end";
        [SerializeField] private int dataIdStart = 1000;
        [SerializeField] private int dataIdEnd = 1002;

        private readonly StringBuilder _logBuilder = new StringBuilder(2048);
        private Vector2 _scrollPosition;
        private MyServerSessionClient _client;
        private bool _busy;

        private void Awake()
        {
            CreateClient();
        }

        private void OnDestroy()
        {
            _client?.Dispose();
            _client = null;
        }

        private void OnGUI()
        {
            const float width = 520f;
            var area = new Rect(16f, 16f, width, Screen.height - 32f);
            GUILayout.BeginArea(area, GUI.skin.box);

            GUILayout.Label("MyServer Simple Client Demo");
            GUILayout.Space(8f);

            GUILayout.Label("HTTP Base URL");
            httpBaseUrl = GUILayout.TextField(httpBaseUrl);
            GUILayout.Label("Game Host");
            gameHost = GUILayout.TextField(gameHost);
            GUILayout.Label("Game Port");
            gamePort = ParseIntField(gamePort);
            GUILayout.Label("Timeout(ms)");
            requestTimeoutMs = ParseIntField(requestTimeoutMs);

            GUILayout.Space(8f);
            GUILayout.Label("Guest ID");
            guestId = GUILayout.TextField(guestId);
            GUILayout.Label("Room ID");
            roomId = GUILayout.TextField(roomId);
            GUILayout.Label("Input Frame ID");
            inputFrameId = ParseUIntField(inputFrameId);
            GUILayout.Label("Input Action");
            inputAction = GUILayout.TextField(inputAction);
            GUILayout.Label("Input Payload JSON");
            inputPayloadJson = GUILayout.TextField(inputPayloadJson);
            GUILayout.Label("End Reason");
            endReason = GUILayout.TextField(endReason);
            GUILayout.Label("Config ID Start");
            dataIdStart = ParseIntField(dataIdStart);
            GUILayout.Label("Config ID End");
            dataIdEnd = ParseIntField(dataIdEnd);

            GUILayout.Space(10f);

            using (new GUIEnabledScope(!_busy))
            {
                if (GUILayout.Button("1. Login + Connect"))
                {
                    Run(async () =>
                    {
                        RecreateClient();
                        var auth = await _client.GuestLoginAndConnectAsync(guestId);
                        Log("login/connect", $"ok={auth.ok} playerId={auth.playerId} error={auth.errorCode}");
                    });
                }

                if (GUILayout.Button("2. Get Me"))
                {
                    Run(async () =>
                    {
                        var me = await _client.GetMeAsync();
                        Log("auth/me", $"ok={me.ok} playerId={me.playerId} guestId={me.guestId}");
                    });
                }

                if (GUILayout.Button("3. Refresh Ticket + Reconnect"))
                {
                    Run(async () =>
                    {
                        var auth = await _client.RefreshTicketAndReconnectAsync();
                        Log("refresh/reconnect", $"ok={auth.ok} playerId={auth.playerId} error={auth.errorCode}");
                    });
                }

                if (GUILayout.Button("4. Join Room"))
                {
                    Run(async () =>
                    {
                        var response = await _client.JoinRoomAsync(roomId);
                        Log("join", $"ok={response.ok} roomId={response.roomId} error={response.errorCode}");
                    });
                }

                if (GUILayout.Button("5. Ready"))
                {
                    Run(async () =>
                    {
                        var response = await _client.SetReadyAsync(true);
                        Log("ready", $"ok={response.ok} roomId={response.roomId} ready={response.ready} error={response.errorCode}");
                    });
                }

                if (GUILayout.Button("6. Start Game"))
                {
                    Run(async () =>
                    {
                        var response = await _client.StartGameAsync();
                        Log("start", $"ok={response.ok} roomId={response.roomId} error={response.errorCode}");
                    });
                }

                if (GUILayout.Button("7. Send Input"))
                {
                    Run(async () =>
                    {
                        var response = await _client.SendPlayerInputAsync(inputFrameId, inputAction, inputPayloadJson);
                        Log("input", $"frameId={inputFrameId} ok={response.ok} roomId={response.roomId} error={response.errorCode}");
                    });
                }

                if (GUILayout.Button("8. End Game"))
                {
                    Run(async () =>
                    {
                        var response = await _client.EndGameAsync(endReason);
                        Log("end", $"ok={response.ok} roomId={response.roomId} error={response.errorCode}");
                    });
                }

                if (GUILayout.Button("9. Leave Room"))
                {
                    Run(async () =>
                    {
                        var response = await _client.LeaveRoomAsync();
                        Log("leave", $"ok={response.ok} roomId={response.roomId} error={response.errorCode}");
                    });
                }

                if (GUILayout.Button("10. Ping"))
                {
                    Run(async () =>
                    {
                        var response = await _client.PingAsync();
                        Log("ping", $"serverTime={response.serverTime}");
                    });
                }

                if (GUILayout.Button("11. Get Room Data"))
                {
                    Run(async () =>
                    {
                        var response = await _client.GetRoomDataAsync(dataIdStart, dataIdEnd);
                        Log("room-data", $"ok={response.ok} count={response.field0List.Count} error={response.errorCode} values=[{string.Join(",", response.field0List.ToArray())}]");
                    });
                }
            }

            if (GUILayout.Button("Clear Log"))
            {
                _logBuilder.Clear();
            }

            GUILayout.Space(10f);
            GUILayout.Label(BuildStatusText());

            _scrollPosition = GUILayout.BeginScrollView(_scrollPosition, GUILayout.ExpandHeight(true));
            GUILayout.TextArea(_logBuilder.ToString(), GUILayout.ExpandHeight(true));
            GUILayout.EndScrollView();

            GUILayout.EndArea();
        }

        private void CreateClient()
        {
            var config = new MyServerClientConfig
            {
                httpBaseUrl = httpBaseUrl,
                gameHost = gameHost,
                gamePort = gamePort,
                requestTimeoutMs = requestTimeoutMs
            };

            _client = new MyServerSessionClient(config);
            _client.roomStatePushed += OnRoomStatePushed;
            _client.gameMessagePushed += OnGameMessagePushed;
            _client.frameBundlePushed += OnFrameBundlePushed;
            _client.roomFrameRatePushed += OnRoomFrameRatePushed;
            _client.errorReceived += OnErrorReceived;
            _client.disconnected += OnDisconnected;
        }

        private void RecreateClient()
        {
            _client?.Dispose();
            CreateClient();
        }

        private async void Run(Func<Task> action)
        {
            if (_busy)
            {
                return;
            }

            _busy = true;
            try
            {
                await action();
            }
            catch (Exception ex)
            {
                Log("exception", ex.ToString());
            }
            finally
            {
                _busy = false;
            }
        }

        private void OnRoomStatePushed(RoomStatePush push)
        {
            var snapshot = push != null && push.snapshot != null
                ? $"roomId={push.snapshot.roomId} state={push.snapshot.state} owner={push.snapshot.ownerPlayerId} members={push.snapshot.members.Count}"
                : "snapshot=null";
            Log("room-push", $"event={push?.eventName} {snapshot}");
        }

        private void OnGameMessagePushed(GamePushMessage push)
        {
            Log("game-push", $"event={push?.eventName} roomId={push?.roomId} playerId={push?.playerId} action={push?.action} payload={push?.payloadJson}");
        }

        private void OnFrameBundlePushed(FrameBundlePush push)
        {
            Log("frame-bundle", $"roomId={push?.roomId} frameId={push?.frameId} fps={push?.fps} silent={push?.isSilentFrame} inputs={push?.inputs?.Count ?? 0}");
        }

        private void OnRoomFrameRatePushed(RoomFrameRatePush push)
        {
            Log("frame-rate", $"roomId={push?.roomId} fps={push?.fps} reason={push?.reason}");
        }

        private void OnErrorReceived(ErrorResponse error)
        {
            Log("game-error", $"code={error?.errorCode} message={error?.message}");
        }

        private void OnDisconnected(string reason)
        {
            Log("disconnect", reason);
        }

        private void Log(string category, string message)
        {
            var line = $"[{DateTime.Now:HH:mm:ss}] {category}: {message}";
            Debug.Log(line, this);
            _logBuilder.AppendLine(line);
        }

        private string BuildStatusText()
        {
            if (_client == null)
            {
                return "client=null";
            }

            var playerId = _client.currentLogin != null ? _client.currentLogin.playerId : "-";
            var roomState = _client.latestRoomSnapshot != null ? _client.latestRoomSnapshot.state : "-";
            var connected = _client.game.isConnected ? "connected" : "disconnected";
            return $"status={connected} playerId={playerId} roomState={roomState}";
        }

        private static int ParseIntField(int currentValue)
        {
            var raw = GUILayout.TextField(currentValue.ToString());
            return int.TryParse(raw, out var parsed) ? parsed : currentValue;
        }

        private static uint ParseUIntField(uint currentValue)
        {
            var raw = GUILayout.TextField(currentValue.ToString());
            return uint.TryParse(raw, out var parsed) ? parsed : currentValue;
        }

        private readonly struct GUIEnabledScope : IDisposable
        {
            private readonly bool _previousState;

            public GUIEnabledScope(bool enabled)
            {
                _previousState = GUI.enabled;
                GUI.enabled = enabled;
            }

            public void Dispose()
            {
                GUI.enabled = _previousState;
            }
        }
    }
}
