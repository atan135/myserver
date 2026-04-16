import { log } from "./logger.js";

export class PubSubClient {
  constructor(redis) {
    this.redis = redis;
  }

  async publishMailNotification(playerId, mail) {
    const channel = `mail:notify:${playerId}`;
    const senderId = typeof mail.sender_id === "string" && mail.sender_id.toLowerCase() === "system"
      ? "system"
      : (mail.sender_id || mail.from_player_id);
    const payload = {
      mail_id: mail.mail_id,
      title: mail.title,
      from: senderId,
      from_name: mail.sender_name || (senderId === "system" ? "系统" : senderId),
      type: mail.mail_type,
      created_at: mail.created_at
    };

    try {
      const count = await this.redis.publish(channel, JSON.stringify(payload));
      log("info", "pubsub.mail_notification", {
        channel,
        playerId,
        mailId: mail.mail_id,
        subscribers: count
      });
      return count;
    } catch (error) {
      log("error", "pubsub.publish_failed", {
        error: error.message,
        playerId,
        mailId: mail.mail_id
      });
      throw error;
    }
  }
}
