import { Inject, Injectable } from "@nestjs/common";

import { AuthService } from "../auth/auth.service.js";
import { getClientIp } from "../common/client-ip.js";
import { badRequest, forbidden, serviceUnavailable, unauthorized } from "../common/http-exception.js";
import { AUTH_CHARACTER_STORE, AUTH_CONFIG, AUTH_STORE } from "../tokens.js";

const LOGINABLE_CHARACTER_STATUSES = new Set(["active"]);
const CHARACTER_ID_PATTERN = /^chr_[0-9a-hjkmnp-tv-z]+$/;
const CHARACTER_NAME_PATTERN = /^[\p{Script=Han}A-Za-z0-9_-]+$/u;
const APPEARANCE_KEY_PATTERN = /^[A-Za-z][A-Za-z0-9_]{0,31}$/;
const MAX_APPEARANCE_DEPTH = 4;
const MAX_APPEARANCE_ARRAY_ITEMS = 16;
const MAX_APPEARANCE_OBJECT_KEYS = 32;
const MAX_APPEARANCE_STRING_LENGTH = 64;

function getBearerToken(req: any): string | null {
  const authorization = req.headers.authorization;
  if (!authorization?.startsWith("Bearer ")) {
    return null;
  }

  return authorization.slice("Bearer ".length).trim();
}

function toFiniteNumber(value: unknown, fallback: number) {
  const numeric = Number(value);
  return Number.isFinite(numeric) ? numeric : fallback;
}

function toInteger(value: unknown, fallback: number) {
  const numeric = Number.parseInt(String(value), 10);
  return Number.isFinite(numeric) ? numeric : fallback;
}

function shortCharacterId(characterId: string) {
  const suffix = String(characterId || "").split("_").pop() || characterId;
  return suffix.length <= 8 ? suffix : suffix.slice(-8);
}

function toSnakeCharacter(character: any) {
  return {
    character_id: character.characterId,
    character_id_short: shortCharacterId(character.characterId),
    name: character.name,
    world_id: character.worldId,
    status: character.status,
    appearance_json: character.appearance,
    last_login_at: character.lastLoginAt || null,
    position: {
      scene_id: character.position.sceneId,
      x: character.position.x,
      y: character.position.y,
      dir_x: character.position.dirX,
      dir_y: character.position.dirY
    },
    attributes: {
      affinity: character.affinity,
      mastery: character.mastery
    }
  };
}

@Injectable()
export class CharactersService {
  constructor(
    @Inject(AUTH_CONFIG) private readonly config: any,
    @Inject(AUTH_STORE) private readonly authStore: any,
    @Inject(AUTH_CHARACTER_STORE) private readonly characterStore: any,
    private readonly authService: AuthService
  ) {}

  async list(req: any) {
    const session = await this.requireSession(req);
    this.assertCharacterStoreEnabled();

    const characters = await this.characterStore.listByAccountPlayerId(session.playerId);

    return {
      ok: true,
      playerId: session.playerId,
      characters: characters.map((character: any) => toSnakeCharacter(character))
    };
  }

  async create(req: any, body: any) {
    const session = await this.requireSession(req);
    this.assertCharacterStoreEnabled();

    await this.authService.assertNotInMaintenance();

    const clientIp = getClientIp(req, this.config);
    await this.assertAccountCanUseCharacters(session.playerId, clientIp, "character_create");

    const name = this.normalizeCharacterName(body?.name);
    const appearance = this.normalizeAppearance(body?.appearance ?? body?.appearance_json ?? {});
    const defaults = this.getCreateDefaults();

    try {
      const character = await this.characterStore.createCharacter({
        accountPlayerId: session.playerId,
        worldId: defaults.worldId,
        name,
        appearance,
        position: defaults.position,
        affinity: defaults.affinity,
        mastery: defaults.mastery
      });

      return {
        ok: true,
        character: toSnakeCharacter(character)
      };
    } catch (error: any) {
      if (error?.code === "CHARACTER_LIMIT_EXCEEDED") {
        throw forbidden(
          "CHARACTER_LIMIT_EXCEEDED",
          `ordinary accounts can create at most ${error.limit || 6} effective characters`
        );
      }
      if (error?.code === "CHARACTER_STORE_DISABLED") {
        throw serviceUnavailable("CHARACTER_STORE_UNAVAILABLE", "character store is unavailable");
      }
      throw error;
    }
  }

  async select(req: any, body: any) {
    const session = await this.requireSession(req);
    this.assertCharacterStoreEnabled();

    const characterId = this.normalizeCharacterId(body?.character_id ?? body?.characterId);
    const clientIp = getClientIp(req, this.config);

    await this.authService.assertNotInMaintenance();
    await this.assertAccountCanUseCharacters(session.playerId, clientIp, "character_select");

    const character = await this.characterStore.getByCharacterId(characterId);
    if (!character) {
      throw forbidden("CHARACTER_NOT_FOUND", "character is not available to the current account");
    }

    if (character.accountPlayerId !== session.playerId) {
      throw forbidden("CHARACTER_OWNER_MISMATCH", "character does not belong to current account");
    }

    if (!LOGINABLE_CHARACTER_STATUSES.has(character.status)) {
      throw forbidden("CHARACTER_NOT_LOGINABLE", "character status does not allow login");
    }

    const updated = await this.characterStore.updateLastLoginAt(character.characterId);
    if (!updated) {
      throw forbidden("CHARACTER_NOT_LOGINABLE", "character status does not allow login");
    }

    const refreshedCharacter = await this.characterStore.getByCharacterId(character.characterId) || character;
    const ticket = await this.authStore.issueGameTicket(session.playerId, clientIp, {
      characterId: refreshedCharacter.characterId,
      worldId: refreshedCharacter.worldId
    });
    const services = await this.authService.buildServicePayload();
    const gameProxy = this.authService.getGameProxyDescriptor(services);
    if (!gameProxy) {
      throw serviceUnavailable("SERVICE_DISCOVERY_UNAVAILABLE", "game-proxy client endpoint is unavailable");
    }

    return {
      ok: true,
      playerId: session.playerId,
      character: toSnakeCharacter(refreshedCharacter),
      ticket: ticket.value,
      ticketExpiresAt: ticket.expiresAt,
      gameProxyHost: gameProxy.host,
      gameProxyPort: gameProxy.port,
      services
    };
  }

  async requireSession(req: any) {
    const accessToken = getBearerToken(req);
    if (!accessToken) {
      throw unauthorized("MISSING_BEARER_TOKEN");
    }

    const session = await this.authStore.getSessionByAccessToken(accessToken);
    if (!session) {
      throw unauthorized("INVALID_ACCESS_TOKEN");
    }

    return session;
  }

  assertCharacterStoreEnabled() {
    if (!this.characterStore?.enabled) {
      throw serviceUnavailable("CHARACTER_STORE_UNAVAILABLE", "character store is unavailable");
    }
  }

  async assertAccountCanUseCharacters(playerId: string, clientIp: string | null, source: string) {
    try {
      await this.authStore.assertPlayerCanIssueTicket(playerId, clientIp);
      await this.authStore.assertPlayerNotBlocked?.(playerId, clientIp, source);
    } catch (error: any) {
      if (error?.code === "ACCOUNT_DISABLED") {
        throw forbidden("ACCOUNT_DISABLED", "Account is disabled");
      }
      if (error?.code === "PLAYER_BLOCKED") {
        throw forbidden("PLAYER_BLOCKED", "player is blocked");
      }
      if (error?.code === "BLOCKLIST_UNAVAILABLE") {
        throw serviceUnavailable("BLOCKLIST_UNAVAILABLE", "redis blocklist is unavailable");
      }
      throw error;
    }
  }

  normalizeCharacterName(input: unknown) {
    if (typeof input !== "string") {
      throw badRequest("INVALID_CHARACTER_NAME", "name must be a string");
    }

    const name = input.trim();
    if (name.length === 0) {
      throw badRequest("INVALID_CHARACTER_NAME", "name must not be blank");
    }

    if (/\s/u.test(name)) {
      throw badRequest("INVALID_CHARACTER_NAME", "name must not contain whitespace");
    }

    const minLength = toInteger(this.config.characterNameMinLength, 2);
    const maxLength = toInteger(this.config.characterNameMaxLength, 16);
    if (Array.from(name).length < minLength || Array.from(name).length > maxLength) {
      throw badRequest("INVALID_CHARACTER_NAME", `name must be between ${minLength} and ${maxLength} characters`);
    }

    if (!CHARACTER_NAME_PATTERN.test(name)) {
      throw badRequest("INVALID_CHARACTER_NAME", "name may only contain Chinese characters, letters, numbers, underscore, and hyphen");
    }

    const lowered = name.toLowerCase();
    const forbiddenWords = Array.isArray(this.config.characterNameForbiddenWords)
      ? this.config.characterNameForbiddenWords
      : [];
    if (forbiddenWords.some((word: string) => word && lowered.includes(String(word).toLowerCase()))) {
      throw badRequest("CHARACTER_NAME_RESERVED", "name is reserved");
    }

    return name;
  }

  normalizeCharacterId(input: unknown) {
    if (typeof input !== "string" || input.trim().length === 0) {
      throw badRequest("INVALID_CHARACTER_ID", "character_id must be a non-empty string");
    }

    const characterId = input.trim();
    if (!CHARACTER_ID_PATTERN.test(characterId)) {
      throw badRequest("INVALID_CHARACTER_ID", "character_id has invalid format");
    }

    return characterId;
  }

  normalizeAppearance(input: unknown) {
    if (input === undefined || input === null) {
      return {};
    }

    if (!isPlainObject(input)) {
      throw badRequest("INVALID_APPEARANCE", "appearance must be a JSON object");
    }

    this.assertAppearanceValue(input, 0);

    const jsonBytes = Buffer.byteLength(JSON.stringify(input), "utf8");
    const maxJsonBytes = toInteger(this.config.characterAppearanceMaxJsonBytes, 4096);
    if (jsonBytes > maxJsonBytes) {
      throw badRequest("INVALID_APPEARANCE", `appearance JSON must be at most ${maxJsonBytes} bytes`);
    }

    return input;
  }

  assertAppearanceValue(value: unknown, depth: number) {
    if (depth > MAX_APPEARANCE_DEPTH) {
      throw badRequest("INVALID_APPEARANCE", "appearance JSON is too deeply nested");
    }

    if (value === null) {
      return;
    }

    if (typeof value === "string") {
      if (value.length > MAX_APPEARANCE_STRING_LENGTH) {
        throw badRequest("INVALID_APPEARANCE", "appearance string values must be at most 64 characters");
      }
      if (!/^[A-Za-z0-9_.:-]*$/.test(value)) {
        throw badRequest("INVALID_APPEARANCE", "appearance string values contain unsupported characters");
      }
      return;
    }

    if (typeof value === "number") {
      if (!Number.isFinite(value)) {
        throw badRequest("INVALID_APPEARANCE", "appearance numbers must be finite");
      }
      return;
    }

    if (typeof value === "boolean") {
      return;
    }

    if (Array.isArray(value)) {
      if (value.length > MAX_APPEARANCE_ARRAY_ITEMS) {
        throw badRequest("INVALID_APPEARANCE", "appearance arrays are too large");
      }
      for (const item of value) {
        this.assertAppearanceValue(item, depth + 1);
      }
      return;
    }

    if (isPlainObject(value)) {
      const entries = Object.entries(value);
      if (entries.length > MAX_APPEARANCE_OBJECT_KEYS) {
        throw badRequest("INVALID_APPEARANCE", "appearance objects have too many fields");
      }
      for (const [key, item] of entries) {
        if (!APPEARANCE_KEY_PATTERN.test(key)) {
          throw badRequest("INVALID_APPEARANCE", "appearance field names are invalid");
        }
        this.assertAppearanceValue(item, depth + 1);
      }
      return;
    }

    throw badRequest("INVALID_APPEARANCE", "appearance contains unsupported values");
  }

  getCreateDefaults() {
    const affinity = {
      earth: 2500,
      fire: 2500,
      water: 2500,
      wind: 2500
    };
    const mastery = {
      earth: 0,
      fire: 0,
      water: 0,
      wind: 0
    };

    return {
      worldId: toInteger(this.config.characterDefaultWorldId, 0),
      position: {
        sceneId: toInteger(this.config.characterDefaultSceneId, 100),
        x: toFiniteNumber(this.config.characterDefaultX, 0),
        y: toFiniteNumber(this.config.characterDefaultY, 0),
        dirX: toFiniteNumber(this.config.characterDefaultDirX, 0),
        dirY: toFiniteNumber(this.config.characterDefaultDirY, 1)
      },
      affinity,
      mastery
    };
  }

}

function isPlainObject(value: unknown): value is Record<string, unknown> {
  if (typeof value !== "object" || value === null || Array.isArray(value)) {
    return false;
  }

  const prototype = Object.getPrototypeOf(value);
  return prototype === Object.prototype || prototype === null;
}

export { toSnakeCharacter, shortCharacterId };
