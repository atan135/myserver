import { log } from "./logger.js";
import { encodeSubjectToken } from "./nats-client.js";

export class PubSubClient {
  constructor(nats) {
    this.nats = nats;
  }

  async publishMailNotification(playerId, mail) {
    const subject = `myserver.mail.notify.${encodeSubjectToken(playerId)}`;
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
        mailId: mail.mail_id
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
