import { log } from "./logger.js";
import { encodeSubjectToken } from "./nats-client.js";

export function buildLegacyMailSubject(playerId) {
  return `myserver.mail.notify.${encodeSubjectToken(playerId)}`;
}

export function buildInstanceMailSubject(instanceId) {
  return `myserver.mail.notify.instance.${encodeSubjectToken(instanceId)}`;
}

export function buildChatOnlineRouteKey(playerId, prefix = "") {
  return `${prefix}chat:online:${playerId}`;
}

export class PubSubClient {
  constructor(nats, redis = null, options = {}) {
    this.nats = nats;
    this.redis = redis;
    this.redisKeyPrefix = options.redisKeyPrefix || "";
  }

  async resolveMailNotificationSubject(playerId) {
    const fallbackSubject = buildLegacyMailSubject(playerId);
    if (!this.redis) {
      return {
        subject: fallbackSubject,
        routed: false,
        reason: "redis_unavailable"
      };
    }

    try {
      const key = buildChatOnlineRouteKey(playerId, this.redisKeyPrefix);
      const instanceId = await this.redis.get(key);
      if (instanceId) {
        return {
          subject: buildInstanceMailSubject(instanceId),
          routed: true,
          instanceId
        };
      }
      return {
        subject: fallbackSubject,
        routed: false,
        reason: "route_not_found"
      };
    } catch (error) {
      log("warn", "mail.online_route_lookup_failed", {
        playerId,
        error: error.message
      });
      return {
        subject: fallbackSubject,
        routed: false,
        reason: "lookup_failed"
      };
    }
  }

  async publishMailNotification(playerId, mail) {
    const route = await this.resolveMailNotificationSubject(playerId);
    const subject = route.subject;
    const senderId = typeof mail.sender_id === "string" && mail.sender_id.toLowerCase() === "system"
      ? "system"
      : (mail.sender_id || mail.from_player_id);
    const payload = {
      player_id: playerId,
      mail_id: mail.mail_id,
      title: mail.title,
      from: senderId,
      from_name: mail.sender_name || (senderId === "system" ? "系统" : senderId),
      type: mail.mail_type,
      created_at: mail.created_at
    };

    try {
      await this.nats.publishJson(subject, payload);
      log("info", "nats.mail_notification", {
        subject,
        playerId,
        mailId: mail.mail_id,
        routed: route.routed,
        instanceId: route.instanceId || null,
        routeReason: route.reason || null
      });
      return 0;
    } catch (error) {
      log("error", "nats.publish_failed", {
        error: error.message,
        playerId,
        mailId: mail.mail_id
      });
      throw error;
    }
  }
}
