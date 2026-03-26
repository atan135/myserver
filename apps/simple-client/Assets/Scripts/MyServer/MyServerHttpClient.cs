using System;
using System.Text;
using System.Threading;
using System.Threading.Tasks;
using UnityEngine;
using UnityEngine.Networking;

namespace MyServer.SimpleClient
{
    public sealed class MyServerHttpClient
    {
        private readonly MyServerClientConfig _config;

        public MyServerHttpClient(MyServerClientConfig config)
        {
            _config = config ?? throw new ArgumentNullException(nameof(config));
        }

        public Task<GuestLoginResponse> GuestLoginAsync(string guestId = "", CancellationToken cancellationToken = default)
        {
            var body = string.IsNullOrWhiteSpace(guestId)
                ? "{}"
                : "{\"guestId\":\"" + EscapeJson(guestId) + "\"}";

            return SendJsonAsync<GuestLoginResponse>(
                UnityWebRequest.kHttpVerbPOST,
                "/api/v1/auth/guest-login",
                body,
                null,
                cancellationToken);
        }

        public Task<AuthMeResponse> GetMeAsync(string accessToken, CancellationToken cancellationToken = default)
        {
            if (string.IsNullOrWhiteSpace(accessToken))
            {
                throw new ArgumentException("accessToken is required", nameof(accessToken));
            }

            return SendJsonAsync<AuthMeResponse>(
                UnityWebRequest.kHttpVerbGET,
                "/api/v1/auth/me",
                null,
                accessToken,
                cancellationToken);
        }

        public Task<IssueGameTicketResponse> IssueGameTicketAsync(string accessToken, CancellationToken cancellationToken = default)
        {
            if (string.IsNullOrWhiteSpace(accessToken))
            {
                throw new ArgumentException("accessToken is required", nameof(accessToken));
            }

            return SendJsonAsync<IssueGameTicketResponse>(
                UnityWebRequest.kHttpVerbPOST,
                "/api/v1/game-ticket/issue",
                "{}",
                accessToken,
                cancellationToken);
        }

        private async Task<TResponse> SendJsonAsync<TResponse>(
            string method,
            string path,
            string jsonBody,
            string accessToken,
            CancellationToken cancellationToken)
        {
            using (var request = BuildRequest(method, path, jsonBody, accessToken))
            {
                await SendRequestAsync(request, cancellationToken);

                var responseText = request.downloadHandler != null ? request.downloadHandler.text : string.Empty;
                if (request.result != UnityWebRequest.Result.Success)
                {
                    throw new MyServerHttpException(
                        request.error ?? ("HTTP request failed: " + request.result),
                        request.responseCode,
                        responseText);
                }

                if (request.responseCode < 200 || request.responseCode >= 300)
                {
                    throw new MyServerHttpException(
                        "HTTP request returned non-success status: " + request.responseCode,
                        request.responseCode,
                        responseText);
                }

                if (string.IsNullOrWhiteSpace(responseText))
                {
                    throw new MyServerHttpException("HTTP response body is empty", request.responseCode, responseText);
                }

                var response = JsonUtility.FromJson<TResponse>(responseText);
                if (response == null)
                {
                    throw new MyServerHttpException("Failed to parse HTTP response JSON", request.responseCode, responseText);
                }

                return response;
            }
        }

        private UnityWebRequest BuildRequest(string method, string path, string jsonBody, string accessToken)
        {
            var url = BuildUrl(path);
            var request = new UnityWebRequest(url, method)
            {
                downloadHandler = new DownloadHandlerBuffer()
            };

            if (!string.IsNullOrEmpty(jsonBody))
            {
                var bodyBytes = Encoding.UTF8.GetBytes(jsonBody);
                request.uploadHandler = new UploadHandlerRaw(bodyBytes);
                request.SetRequestHeader("Content-Type", "application/json");
            }

            request.SetRequestHeader("Accept", "application/json");

            if (!string.IsNullOrWhiteSpace(accessToken))
            {
                request.SetRequestHeader("Authorization", "Bearer " + accessToken);
            }

            request.timeout = Mathf.Max(1, Mathf.CeilToInt(_config.requestTimeoutMs / 1000f));
            return request;
        }

        private string BuildUrl(string path)
        {
            var baseUrl = (_config.httpBaseUrl ?? string.Empty).TrimEnd('/');
            var normalizedPath = path.StartsWith("/") ? path : "/" + path;
            return baseUrl + normalizedPath;
        }

        private static async Task SendRequestAsync(UnityWebRequest request, CancellationToken cancellationToken)
        {
            var tcs = new TaskCompletionSource<object>(TaskCreationOptions.RunContinuationsAsynchronously);
            var operation = request.SendWebRequest();
            operation.completed += _ => tcs.TrySetResult(null);

            CancellationTokenRegistration registration = default;
            if (cancellationToken.CanBeCanceled)
            {
                registration = cancellationToken.Register(() =>
                {
                    request.Abort();
                    tcs.TrySetCanceled();
                });
            }

            try
            {
                await tcs.Task;
            }
            finally
            {
                registration.Dispose();
            }
        }

        private static string EscapeJson(string value)
        {
            if (string.IsNullOrEmpty(value))
            {
                return string.Empty;
            }

            var builder = new StringBuilder(value.Length + 8);
            foreach (var ch in value)
            {
                switch (ch)
                {
                    case '\\':
                        builder.Append("\\\\");
                        break;
                    case '"':
                        builder.Append("\\\"");
                        break;
                    case '\b':
                        builder.Append("\\b");
                        break;
                    case '\f':
                        builder.Append("\\f");
                        break;
                    case '\n':
                        builder.Append("\\n");
                        break;
                    case '\r':
                        builder.Append("\\r");
                        break;
                    case '\t':
                        builder.Append("\\t");
                        break;
                    default:
                        if (ch < 32)
                        {
                            builder.Append("\\u");
                            builder.Append(((int)ch).ToString("x4"));
                        }
                        else
                        {
                            builder.Append(ch);
                        }

                        break;
                }
            }

            return builder.ToString();
        }
    }
}

