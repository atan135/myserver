using System;

namespace MyServer.SimpleClient
{
    [Serializable]
    public sealed class MyServerClientConfig
    {
        public string httpBaseUrl = "http://127.0.0.1:3000";
        public string gameHost = "127.0.0.1";
        public int gamePort = 7000;
        public int requestTimeoutMs = 5000;
    }
}
