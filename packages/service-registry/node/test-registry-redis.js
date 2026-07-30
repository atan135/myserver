import {
  registryHeartbeatKey,
  registryInstanceIndexKey,
  registryInstanceKey
} from "./registry-schema.js";

// A command-level in-memory capture used by isolated registry tests. It does
// not emulate Redis scripting generally; it only implements this package's
// three lifecycle scripts and records if a forbidden SCAN is attempted.
export function createRegistryRedisCapture({ nowSeconds = () => Math.floor(Date.now() / 1000) } = {}) {
  const indexes = new Map();
  const hashTtls = new Map();
  const keys = new Map();
  const stats = { evalCount: 0, pipelineCount: 0, scanCount: 0 };

  const indexInstanceKey = (instanceKey) => {
    const matched = /^(.*)service:([^:]+):instances:([^:]+)$/.exec(instanceKey);
    if (!matched) return null;
    return {
      indexKey: registryInstanceIndexKey(matched[1], matched[2]),
      instanceId: matched[3]
    };
  };
  const indexesFor = (indexKey) => {
    let index = indexes.get(indexKey);
    if (!index) {
      index = new Map();
      indexes.set(indexKey, index);
    }
    return index;
  };
  const indexIdentity = (indexKey) => {
    const matched = /^(.*)service:([^:]+):instance-index$/.exec(indexKey);
    return matched
      ? { prefix: matched[1], serviceName: matched[2] }
      : null;
  };
  const hashes = new class RegistryHashMap extends Map {
    set(key, value) {
      super.set(key, value);
      if (key.endsWith(":data")) {
        const identity = indexInstanceKey(key.slice(0, -5));
        if (identity) {
          indexesFor(identity.indexKey).set(identity.instanceId, nowSeconds());
        }
      }
      return this;
    }
    delete(key) {
      if (key.endsWith(":data")) {
        const identity = indexInstanceKey(key.slice(0, -5));
        if (identity) {
          indexesFor(identity.indexKey).delete(identity.instanceId);
        }
      }
      return super.delete(key);
    }
  }();

  const redis = {
    hashes,
    hashTtls,
    keys,
    indexes,
    stats,
    failEval: null,
    failPipeline: null,
    async hset(key, field, value) {
      hashes.set(`${key}:${field}`, value);
    },
    async hget(key, field) {
      return hashes.get(`${key}:${field}`) ?? null;
    },
    async exists(key) {
      return keys.has(key) ? 1 : 0;
    },
    async setex(key, ttl, value) {
      keys.set(key, { ttl: Number(ttl), value });
    },
    async del(key) {
      hashes.delete(`${key}:data`);
      hashTtls.delete(key);
      keys.delete(key);
    },
    async scan() {
      stats.scanCount += 1;
      throw new Error("FORBIDDEN_SCAN");
    },
    async zrangebyscore(indexKey, minimum, _maximum, _limit, _offset, limit) {
      const lowerBound = Number(minimum);
      return [...indexesFor(indexKey).entries()]
        .filter(([, score]) => score >= lowerBound)
        .sort(([leftId, leftScore], [rightId, rightScore]) => leftScore - rightScore || leftId.localeCompare(rightId))
        .slice(0, Number(limit))
        .map(([instanceId]) => instanceId);
    },
    pipeline() {
      stats.pipelineCount += 1;
      const commands = [];
      const pipeline = {
        hget(key, field) {
          commands.push(() => redis.hget(key, field));
          return pipeline;
        },
        exists(key) {
          commands.push(() => redis.exists(key));
          return pipeline;
        },
        async exec() {
          if (redis.failPipeline) {
            throw new Error(redis.failPipeline);
          }
          return Promise.all(commands.map(async (command) => [null, await command()]));
        }
      };
      return pipeline;
    },
    async eval(script, keyCount, ...values) {
      stats.evalCount += 1;
      if (this.failEval) {
        throw new Error(this.failEval);
      }
      const [indexKey, instanceKey, heartbeatKey] = values.slice(0, keyCount);
      const args = values.slice(keyCount);
      const identity = indexInstanceKey(instanceKey);
      if (!identity) throw new Error("INVALID_REGISTRY_INSTANCE_KEY");

      if (script.includes("HSET")) {
        const [instanceId, payload, now, instanceTtl, heartbeatTtl, indexTtl, maximum] = args;
        cleanExpired(indexKey, Number(now) - Number(instanceTtl));
        const index = indexesFor(indexKey);
        if (!index.has(instanceId) && index.size >= Number(maximum)) {
          return ["REGISTRY_CAPACITY_EXCEEDED"];
        }
        hashes.set(`${instanceKey}:data`, payload);
        hashTtls.set(instanceKey, Number(instanceTtl));
        keys.set(heartbeatKey, { ttl: Number(heartbeatTtl), value: "1" });
        index.set(instanceId, Number(now));
        return ["OK", "0"];
      }

      if (script.includes("REGISTRY_INSTANCE_MISSING")) {
        const [instanceId, now, instanceTtl, heartbeatTtl, _indexTtl, maximum] = args;
        cleanExpired(indexKey, Number(now) - Number(instanceTtl));
        if (!hashes.has(`${instanceKey}:data`)) return ["REGISTRY_INSTANCE_MISSING"];
        const index = indexesFor(indexKey);
        if (!index.has(instanceId) && index.size >= Number(maximum)) {
          return ["REGISTRY_CAPACITY_EXCEEDED"];
        }
        hashTtls.set(instanceKey, Number(instanceTtl));
        keys.set(heartbeatKey, { ttl: Number(heartbeatTtl), value: "1" });
        index.set(instanceId, Number(now));
        return ["OK", "0"];
      }

      hashes.delete(`${instanceKey}:data`);
      hashTtls.delete(instanceKey);
      keys.delete(heartbeatKey);
      indexesFor(indexKey).delete(args[0]);
      return ["OK"];
    }
  };

  function cleanExpired(indexKey, exclusiveCutoff) {
    const identity = indexIdentity(indexKey);
    for (const [instanceId, score] of indexesFor(indexKey)) {
      if (score < exclusiveCutoff) {
        indexesFor(indexKey).delete(instanceId);
        if (identity) {
          const instanceKey = registryInstanceKey(identity.prefix, identity.serviceName, instanceId);
          hashes.delete(`${instanceKey}:data`);
          hashTtls.delete(instanceKey);
          keys.delete(registryHeartbeatKey(identity.prefix, identity.serviceName, instanceId));
        }
      }
    }
  }

  return redis;
}
