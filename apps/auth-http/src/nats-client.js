import { connect, StringCodec } from "nats";

const codec = StringCodec();

export function encodeSubjectToken(value) {
  return Buffer.from(String(value), "utf8").toString("base64url");
}

export async function createNatsClient(config) {
  const connection = await connect({
    servers: config.natsUrl,
    name: config.appName
  });

  connection.closed().then((error) => {
    if (error) {
      console.error("[nats] connection closed:", error.message);
    }
  });

  return {
    async publishJson(subject, payload) {
      connection.publish(subject, codec.encode(JSON.stringify(payload)));
      await connection.flush();
    },

    async close() {
      try {
        await connection.drain();
      } catch {
        connection.close();
      }
    }
  };
}
