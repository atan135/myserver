using System;
using System.Collections.Generic;
using System.IO;
using System.Text;

namespace MyServer.SimpleClient
{
    internal static class MyServerGameProtocol
    {
        public const ushort Magic = 0xCAFE;
        public const byte Version = 1;
        public const int HeaderLength = 14;

        public static PacketHeader ParseHeader(byte[] bytes)
        {
            if (bytes == null || bytes.Length != HeaderLength)
            {
                throw new InvalidDataException("Invalid packet header length.");
            }

            var magic = ReadUInt16BigEndian(bytes, 0);
            if (magic != Magic)
            {
                throw new InvalidDataException("Invalid packet magic.");
            }

            if (bytes[2] != Version)
            {
                throw new InvalidDataException("Unsupported protocol version.");
            }

            if (bytes[3] != 0)
            {
                throw new InvalidDataException("Unsupported packet flags.");
            }

            return new PacketHeader(
                (GameMessageType)ReadUInt16BigEndian(bytes, 4),
                ReadUInt32BigEndian(bytes, 6),
                ReadUInt32BigEndian(bytes, 10));
        }

        public static byte[] EncodePacket(GameMessageType messageType, uint seq, byte[] body)
        {
            var payload = body ?? Array.Empty<byte>();
            var packet = new byte[HeaderLength + payload.Length];
            WriteUInt16BigEndian(packet, 0, Magic);
            packet[2] = Version;
            packet[3] = 0;
            WriteUInt16BigEndian(packet, 4, (ushort)messageType);
            WriteUInt32BigEndian(packet, 6, seq);
            WriteUInt32BigEndian(packet, 10, (uint)payload.Length);
            Buffer.BlockCopy(payload, 0, packet, HeaderLength, payload.Length);
            return packet;
        }

        public static byte[] EncodeAuthRequest(string ticket)
        {
            var writer = new ProtoWriter();
            writer.WriteString(1, ticket);
            return writer.ToArray();
        }

        public static byte[] EncodePingRequest(long clientTime)
        {
            var writer = new ProtoWriter();
            writer.WriteInt64(1, clientTime);
            return writer.ToArray();
        }

        public static byte[] EncodeRoomJoinRequest(string roomId)
        {
            var writer = new ProtoWriter();
            writer.WriteString(1, roomId);
            return writer.ToArray();
        }

        public static byte[] EncodeRoomLeaveRequest()
        {
            return Array.Empty<byte>();
        }

        public static byte[] EncodeRoomReadyRequest(bool ready)
        {
            var writer = new ProtoWriter();
            writer.WriteBool(1, ready);
            return writer.ToArray();
        }

        public static byte[] EncodeRoomStartRequest()
        {
            return Array.Empty<byte>();
        }

        public static byte[] EncodePlayerInputRequest(uint frameId, string action, string payloadJson)
        {
            var writer = new ProtoWriter();
            writer.WriteUInt32(1, frameId);
            writer.WriteString(2, action);
            writer.WriteString(3, payloadJson);
            return writer.ToArray();
        }

        public static byte[] EncodeRoomEndRequest(string reason)
        {
            var writer = new ProtoWriter();
            writer.WriteString(1, reason);
            return writer.ToArray();
        }

        public static byte[] EncodeGetRoomDataRequest(int idStart, int idEnd)
        {
            var writer = new ProtoWriter();
            writer.WriteInt32(1, idStart);
            writer.WriteInt32(2, idEnd);
            return writer.ToArray();
        }

        public static AuthResponse DecodeAuthResponse(byte[] body)
        {
            var response = new AuthResponse();
            ReadFields(body, (fieldNumber, reader) =>
            {
                switch (fieldNumber)
                {
                    case 1:
                        response.ok = reader.ReadBool();
                        return true;
                    case 2:
                        response.playerId = reader.ReadString();
                        return true;
                    case 3:
                        response.errorCode = reader.ReadString();
                        return true;
                    default:
                        return false;
                }
            });

            return response;
        }

        public static PingResponse DecodePingResponse(byte[] body)
        {
            var response = new PingResponse();
            ReadFields(body, (fieldNumber, reader) =>
            {
                switch (fieldNumber)
                {
                    case 1:
                        response.serverTime = reader.ReadInt64();
                        return true;
                    default:
                        return false;
                }
            });

            return response;
        }

        public static RoomJoinResponse DecodeRoomJoinResponse(byte[] body)
        {
            var response = new RoomJoinResponse();
            ReadFields(body, (fieldNumber, reader) =>
            {
                switch (fieldNumber)
                {
                    case 1:
                        response.ok = reader.ReadBool();
                        return true;
                    case 2:
                        response.roomId = reader.ReadString();
                        return true;
                    case 3:
                        response.errorCode = reader.ReadString();
                        return true;
                    default:
                        return false;
                }
            });

            return response;
        }

        public static RoomLeaveResponse DecodeRoomLeaveResponse(byte[] body)
        {
            var response = new RoomLeaveResponse();
            ReadFields(body, (fieldNumber, reader) =>
            {
                switch (fieldNumber)
                {
                    case 1:
                        response.ok = reader.ReadBool();
                        return true;
                    case 2:
                        response.roomId = reader.ReadString();
                        return true;
                    case 3:
                        response.errorCode = reader.ReadString();
                        return true;
                    default:
                        return false;
                }
            });

            return response;
        }

        public static RoomReadyResponse DecodeRoomReadyResponse(byte[] body)
        {
            var response = new RoomReadyResponse();
            ReadFields(body, (fieldNumber, reader) =>
            {
                switch (fieldNumber)
                {
                    case 1:
                        response.ok = reader.ReadBool();
                        return true;
                    case 2:
                        response.roomId = reader.ReadString();
                        return true;
                    case 3:
                        response.ready = reader.ReadBool();
                        return true;
                    case 4:
                        response.errorCode = reader.ReadString();
                        return true;
                    default:
                        return false;
                }
            });

            return response;
        }

        public static RoomStartResponse DecodeRoomStartResponse(byte[] body)
        {
            var response = new RoomStartResponse();
            ReadFields(body, (fieldNumber, reader) =>
            {
                switch (fieldNumber)
                {
                    case 1:
                        response.ok = reader.ReadBool();
                        return true;
                    case 2:
                        response.roomId = reader.ReadString();
                        return true;
                    case 3:
                        response.errorCode = reader.ReadString();
                        return true;
                    default:
                        return false;
                }
            });

            return response;
        }

        public static PlayerInputResponse DecodePlayerInputResponse(byte[] body)
        {
            var response = new PlayerInputResponse();
            ReadFields(body, (fieldNumber, reader) =>
            {
                switch (fieldNumber)
                {
                    case 1:
                        response.ok = reader.ReadBool();
                        return true;
                    case 2:
                        response.roomId = reader.ReadString();
                        return true;
                    case 3:
                        response.errorCode = reader.ReadString();
                        return true;
                    default:
                        return false;
                }
            });

            return response;
        }

        public static RoomEndResponse DecodeRoomEndResponse(byte[] body)
        {
            var response = new RoomEndResponse();
            ReadFields(body, (fieldNumber, reader) =>
            {
                switch (fieldNumber)
                {
                    case 1:
                        response.ok = reader.ReadBool();
                        return true;
                    case 2:
                        response.roomId = reader.ReadString();
                        return true;
                    case 3:
                        response.errorCode = reader.ReadString();
                        return true;
                    default:
                        return false;
                }
            });

            return response;
        }

        public static GetRoomDataResponse DecodeGetRoomDataResponse(byte[] body)
        {
            var response = new GetRoomDataResponse();
            ReadFields(body, (fieldNumber, reader) =>
            {
                switch (fieldNumber)
                {
                    case 1:
                        response.ok = reader.ReadBool();
                        return true;
                    case 2:
                        response.field0List.Add(reader.ReadString());
                        return true;
                    case 3:
                        response.errorCode = reader.ReadString();
                        return true;
                    default:
                        return false;
                }
            });

            return response;
        }

        public static ErrorResponse DecodeErrorResponse(byte[] body)
        {
            var response = new ErrorResponse();
            ReadFields(body, (fieldNumber, reader) =>
            {
                switch (fieldNumber)
                {
                    case 1:
                        response.errorCode = reader.ReadString();
                        return true;
                    case 2:
                        response.message = reader.ReadString();
                        return true;
                    default:
                        return false;
                }
            });

            return response;
        }

        public static RoomStatePush DecodeRoomStatePush(byte[] body)
        {
            var push = new RoomStatePush();
            ReadFields(body, (fieldNumber, reader) =>
            {
                switch (fieldNumber)
                {
                    case 1:
                        push.eventName = reader.ReadString();
                        return true;
                    case 2:
                        push.snapshot = DecodeRoomSnapshot(reader.ReadBytes());
                        return true;
                    default:
                        return false;
                }
            });

            return push;
        }

        public static GamePushMessage DecodeGamePushMessage(byte[] body)
        {
            var push = new GamePushMessage();
            ReadFields(body, (fieldNumber, reader) =>
            {
                switch (fieldNumber)
                {
                    case 1:
                        push.eventName = reader.ReadString();
                        return true;
                    case 2:
                        push.roomId = reader.ReadString();
                        return true;
                    case 3:
                        push.playerId = reader.ReadString();
                        return true;
                    case 4:
                        push.action = reader.ReadString();
                        return true;
                    case 5:
                        push.payloadJson = reader.ReadString();
                        return true;
                    default:
                        return false;
                }
            });

            return push;
        }

        public static FrameBundlePush DecodeFrameBundlePush(byte[] body)
        {
            var push = new FrameBundlePush();
            ReadFields(body, (fieldNumber, reader) =>
            {
                switch (fieldNumber)
                {
                    case 1:
                        push.roomId = reader.ReadString();
                        return true;
                    case 2:
                        push.frameId = reader.ReadUInt32();
                        return true;
                    case 3:
                        push.fps = reader.ReadUInt32();
                        return true;
                    case 4:
                        push.inputs.Add(DecodeFrameInput(reader.ReadBytes()));
                        return true;
                    case 5:
                        push.isSilentFrame = reader.ReadBool();
                        return true;
                    default:
                        return false;
                }
            });

            return push;
        }

        public static RoomFrameRatePush DecodeRoomFrameRatePush(byte[] body)
        {
            var push = new RoomFrameRatePush();
            ReadFields(body, (fieldNumber, reader) =>
            {
                switch (fieldNumber)
                {
                    case 1:
                        push.roomId = reader.ReadString();
                        return true;
                    case 2:
                        push.fps = reader.ReadUInt32();
                        return true;
                    case 3:
                        push.reason = reader.ReadString();
                        return true;
                    default:
                        return false;
                }
            });

            return push;
        }

        private static RoomSnapshot DecodeRoomSnapshot(byte[] body)
        {
            var snapshot = new RoomSnapshot();
            ReadFields(body, (fieldNumber, reader) =>
            {
                switch (fieldNumber)
                {
                    case 1:
                        snapshot.roomId = reader.ReadString();
                        return true;
                    case 2:
                        snapshot.ownerPlayerId = reader.ReadString();
                        return true;
                    case 3:
                        snapshot.state = reader.ReadString();
                        return true;
                    case 4:
                        snapshot.members.Add(DecodeRoomMember(reader.ReadBytes()));
                        return true;
                    default:
                        return false;
                }
            });

            return snapshot;
        }

        private static RoomMember DecodeRoomMember(byte[] body)
        {
            var member = new RoomMember();
            ReadFields(body, (fieldNumber, reader) =>
            {
                switch (fieldNumber)
                {
                    case 1:
                        member.playerId = reader.ReadString();
                        return true;
                    case 2:
                        member.ready = reader.ReadBool();
                        return true;
                    case 3:
                        member.isOwner = reader.ReadBool();
                        return true;
                    default:
                        return false;
                }
            });

            return member;
        }

        private static FrameInput DecodeFrameInput(byte[] body)
        {
            var input = new FrameInput();
            ReadFields(body, (fieldNumber, reader) =>
            {
                switch (fieldNumber)
                {
                    case 1:
                        input.playerId = reader.ReadString();
                        return true;
                    case 2:
                        input.action = reader.ReadString();
                        return true;
                    case 3:
                        input.payloadJson = reader.ReadString();
                        return true;
                    default:
                        return false;
                }
            });

            return input;
        }

        private static void ReadFields(byte[] body, Func<int, ProtoFieldReader, bool> readField)
        {
            if (body == null || body.Length == 0)
            {
                return;
            }

            var reader = new ProtoReader(body);
            while (reader.TryReadField(out var fieldNumber, out var wireType))
            {
                var fieldReader = new ProtoFieldReader(reader, wireType);
                if (!readField(fieldNumber, fieldReader))
                {
                    fieldReader.Skip();
                }
            }
        }

        private static ushort ReadUInt16BigEndian(byte[] buffer, int offset)
        {
            return (ushort)((buffer[offset] << 8) | buffer[offset + 1]);
        }

        private static uint ReadUInt32BigEndian(byte[] buffer, int offset)
        {
            return ((uint)buffer[offset] << 24)
                | ((uint)buffer[offset + 1] << 16)
                | ((uint)buffer[offset + 2] << 8)
                | buffer[offset + 3];
        }

        private static void WriteUInt16BigEndian(byte[] buffer, int offset, ushort value)
        {
            buffer[offset] = (byte)(value >> 8);
            buffer[offset + 1] = (byte)value;
        }

        private static void WriteUInt32BigEndian(byte[] buffer, int offset, uint value)
        {
            buffer[offset] = (byte)(value >> 24);
            buffer[offset + 1] = (byte)(value >> 16);
            buffer[offset + 2] = (byte)(value >> 8);
            buffer[offset + 3] = (byte)value;
        }

        internal readonly struct PacketHeader
        {
            public PacketHeader(GameMessageType messageType, uint seq, uint bodyLength)
            {
                this.messageType = messageType;
                this.seq = seq;
                this.bodyLength = bodyLength;
            }

            public GameMessageType messageType { get; }
            public uint seq { get; }
            public uint bodyLength { get; }
        }

        private sealed class ProtoWriter
        {
            private readonly List<byte> _buffer = new List<byte>(64);

            public void WriteString(int fieldNumber, string value)
            {
                var normalized = value ?? string.Empty;
                var bytes = Encoding.UTF8.GetBytes(normalized);
                WriteTag(fieldNumber, 2);
                WriteVarint((ulong)bytes.Length);
                _buffer.AddRange(bytes);
            }

            public void WriteBool(int fieldNumber, bool value)
            {
                WriteTag(fieldNumber, 0);
                WriteVarint(value ? 1UL : 0UL);
            }

            public void WriteInt32(int fieldNumber, int value)
            {
                WriteTag(fieldNumber, 0);
                WriteVarint(unchecked((ulong)value));
            }

            public void WriteUInt32(int fieldNumber, uint value)
            {
                WriteTag(fieldNumber, 0);
                WriteVarint(value);
            }

            public void WriteInt64(int fieldNumber, long value)
            {
                WriteTag(fieldNumber, 0);
                WriteVarint(unchecked((ulong)value));
            }

            public byte[] ToArray()
            {
                return _buffer.ToArray();
            }

            private void WriteTag(int fieldNumber, int wireType)
            {
                WriteVarint((ulong)((fieldNumber << 3) | wireType));
            }

            private void WriteVarint(ulong value)
            {
                while (value >= 0x80)
                {
                    _buffer.Add((byte)((value & 0x7F) | 0x80));
                    value >>= 7;
                }

                _buffer.Add((byte)value);
            }
        }

        private sealed class ProtoReader
        {
            private readonly byte[] _buffer;
            private int _offset;

            public ProtoReader(byte[] buffer)
            {
                _buffer = buffer ?? Array.Empty<byte>();
                _offset = 0;
            }

            public bool TryReadField(out int fieldNumber, out int wireType)
            {
                if (_offset >= _buffer.Length)
                {
                    fieldNumber = 0;
                    wireType = 0;
                    return false;
                }

                var tag = ReadVarint();
                fieldNumber = (int)(tag >> 3);
                wireType = (int)(tag & 0x07);
                return true;
            }

            public bool ReadBool(int wireType)
            {
                EnsureWireType(wireType, 0);
                return ReadVarint() != 0;
            }

            public int ReadInt32(int wireType)
            {
                EnsureWireType(wireType, 0);
                return unchecked((int)ReadVarint());
            }

            public uint ReadUInt32(int wireType)
            {
                EnsureWireType(wireType, 0);
                return checked((uint)ReadVarint());
            }

            public long ReadInt64(int wireType)
            {
                EnsureWireType(wireType, 0);
                return unchecked((long)ReadVarint());
            }

            public string ReadString(int wireType)
            {
                EnsureWireType(wireType, 2);
                return Encoding.UTF8.GetString(ReadLengthDelimited());
            }

            public byte[] ReadBytes(int wireType)
            {
                EnsureWireType(wireType, 2);
                return ReadLengthDelimited();
            }

            public void SkipField(int wireType)
            {
                switch (wireType)
                {
                    case 0:
                        ReadVarint();
                        return;
                    case 2:
                        ReadLengthDelimited();
                        return;
                    default:
                        throw new InvalidDataException("Unsupported protobuf wire type: " + wireType);
                }
            }

            private byte[] ReadLengthDelimited()
            {
                var length = checked((int)ReadVarint());
                if (length < 0 || _offset + length > _buffer.Length)
                {
                    throw new InvalidDataException("Invalid protobuf length-delimited field.");
                }

                var bytes = new byte[length];
                Buffer.BlockCopy(_buffer, _offset, bytes, 0, length);
                _offset += length;
                return bytes;
            }

            private ulong ReadVarint()
            {
                ulong result = 0;
                var shift = 0;
                while (_offset < _buffer.Length)
                {
                    var value = _buffer[_offset++];
                    result |= (ulong)(value & 0x7F) << shift;
                    if ((value & 0x80) == 0)
                    {
                        return result;
                    }

                    shift += 7;
                    if (shift > 63)
                    {
                        throw new InvalidDataException("Varint is too large.");
                    }
                }

                throw new InvalidDataException("Unexpected end of protobuf varint.");
            }

            private static void EnsureWireType(int actual, int expected)
            {
                if (actual != expected)
                {
                    throw new InvalidDataException("Unexpected protobuf wire type.");
                }
            }
        }

        private readonly struct ProtoFieldReader
        {
            private readonly ProtoReader _reader;
            private readonly int _wireType;

            public ProtoFieldReader(ProtoReader reader, int wireType)
            {
                _reader = reader;
                _wireType = wireType;
            }

            public bool ReadBool()
            {
                return _reader.ReadBool(_wireType);
            }

            public int ReadInt32()
            {
                return _reader.ReadInt32(_wireType);
            }

            public uint ReadUInt32()
            {
                return _reader.ReadUInt32(_wireType);
            }

            public long ReadInt64()
            {
                return _reader.ReadInt64(_wireType);
            }

            public string ReadString()
            {
                return _reader.ReadString(_wireType);
            }

            public byte[] ReadBytes()
            {
                return _reader.ReadBytes(_wireType);
            }

            public void Skip()
            {
                _reader.SkipField(_wireType);
            }
        }
    }
}
