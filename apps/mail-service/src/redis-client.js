import Redis from "ioredis";

export async function createRedisClient(config) {
  const client = new Redis(config.redisUrl, {
    lazyConnect: true,
    maxRetriesPerRequest: 1,
    enableReadyCheck: true
  });

  await client.connect();
  await client.ping();

  return client;
}
