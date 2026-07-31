import { Body, Controller, Get, Headers, HttpCode, HttpStatus, Inject, Param, Post, Put, Query, Req, Res } from "@nestjs/common";
import { ApiCreatedResponse, ApiOkResponse, ApiOperation, ApiTags } from "@nestjs/swagger";

import { forbidden, rateLimited, serviceUnavailable, unauthorized } from "../common/http-exception.js";
import { authenticatePlayerHeaders, validateServiceToken } from "../mail-auth.js";
import { MAIL_CONFIG, MAIL_METRICS, MAIL_PLAYER_AUTH, MAIL_PLAYER_RATE_LIMITER } from "../tokens.js";
import {
  publicResultClass,
  validateEmptyPlayerMutationBody,
  validateEmptyPlayerQuery,
  validateListQuery,
  validateMailId,
  validatePublicPlayerHeaders
} from "./public-mail-request.js";
import { MailsService } from "./mails.service.js";
import { log } from "../logger.js";

function responseStatus(error: any) {
  return typeof error?.getStatus === "function" ? error.getStatus() : 503;
}

function setHeader(response: any, name: string, value: string) {
  if (typeof response?.header === "function") {
    response.header(name, value);
  } else if (typeof response?.setHeader === "function") {
    response.setHeader(name, value);
  }
}

@ApiTags("mails")
@Controller("/api/v1/mails")
export class MailsController {
  constructor(
    private readonly mailsService: MailsService,
    @Inject(MAIL_CONFIG) private readonly config: any,
    @Inject(MAIL_PLAYER_AUTH) private readonly playerAuth: any,
    @Inject(MAIL_PLAYER_RATE_LIMITER) private readonly playerRateLimiter: any = null,
    @Inject(MAIL_METRICS) private readonly metrics: any = null
  ) {}

  private async authenticatePlayer(headers: any) {
    try {
      // Player mail identity always comes from the game ticket. The config flag is
      // retained for registry compatibility, but it cannot reopen a query/body identity path.
      return await authenticatePlayerHeaders(headers, this.playerAuth);
    } catch (error: any) {
      if (error?.code === "AUTH_BACKEND_UNAVAILABLE") {
        throw serviceUnavailable("MAIL_AUTH_UNAVAILABLE", "Player authentication is temporarily unavailable");
      }
      throw unauthorized("MAIL_PLAYER_TICKET_INVALID", "Player ticket is invalid");
    }
  }

  private authenticateService(headers: any) {
    try {
      validateServiceToken(headers, this.config);
    } catch (error: any) {
      if (error?.statusCode === 403) {
        throw forbidden(error.code || "MAIL_SERVICE_TOKEN_INVALID", error.message || "mail service token is invalid");
      }
      throw unauthorized(error?.code || "MAIL_SERVICE_TOKEN_REQUIRED", error?.message || "mail service token is required");
    }
  }

  private async executePlayerRequest(
    operation: "list" | "detail" | "read" | "claim",
    headers: any,
    request: any,
    response: any,
    validateInput: () => any,
    action: (auth: any, input: any) => Promise<any>
  ) {
    const startedAt = Date.now();
    let requestId: string | null = null;
    let clientSource = "unverified";
    let status = HttpStatus.SERVICE_UNAVAILABLE;
    let releaseClaim: (() => Promise<void>) | null = null;

    try {
      const requestContext = validatePublicPlayerHeaders(headers, request, operation, this.config);
      requestId = requestContext.requestId;
      clientSource = requestContext.trustedProxy ? "trusted_caddy" : "socket_peer";
      setHeader(response, "Cache-Control", "private, no-store");
      const input = validateInput();
      const auth = await this.authenticatePlayer(headers);

      if (this.playerRateLimiter?.check) {
        const limit = await this.playerRateLimiter.check(operation, auth.playerId, requestContext.clientIp);
        if (limit?.limited) {
          setHeader(response, "Retry-After", String(limit.retryAfterSeconds));
          this.metrics?.recordMailPublicRateLimited?.(limit.dimension);
          throw rateLimited("MAIL_RATE_LIMITED", "Too many mail requests");
        }
      }

      if (operation === "claim" && this.playerRateLimiter?.acquireClaim) {
        const claimLease = await this.playerRateLimiter.acquireClaim(auth.playerId);
        if (!claimLease?.acquired) {
          setHeader(response, "Retry-After", String(claimLease?.retryAfterSeconds || 1));
          this.metrics?.recordMailPublicRateLimited?.(claimLease?.dimension || "claim_concurrency");
          throw rateLimited("MAIL_CLAIM_CONCURRENCY_LIMITED", "Too many concurrent mail claims");
        }
        releaseClaim = claimLease.release;
      }

      const result = await action(auth, input);
      status = Number(result?._http_status) || HttpStatus.OK;
      return result;
    } catch (error: any) {
      status = responseStatus(error);
      throw error;
    } finally {
      await releaseClaim?.();
      const latencyMs = Math.max(0, Date.now() - startedAt);
      this.metrics?.recordMailPublicRequest?.(operation, status, latencyMs);
      log(status >= 500 ? "warn" : "info", "mail.public_request", {
        requestId,
        route: operation,
        clientSource,
        status,
        resultClass: publicResultClass(status),
        latencyMs
      });
    }
  }

  @Get()
  @ApiOperation({ summary: "List player mails" })
  @ApiOkResponse({ schema: { example: { ok: true, mails: [], unread_count: 0 } } })
  async list(@Headers() headers: any, @Query() query: any, @Req() request: any, @Res({ passthrough: true }) response: any) {
    return this.executePlayerRequest(
      "list",
      headers,
      request,
      response,
      () => validateListQuery(query),
      (auth, input) => this.mailsService.list(auth.playerId, input)
    );
  }

  @Get(":mailId")
  @ApiOperation({ summary: "Get mail detail" })
  async get(
    @Param("mailId") mailId: string,
    @Headers() headers: any,
    @Query() query: any,
    @Req() request: any,
    @Res({ passthrough: true }) response: any
  ) {
    return this.executePlayerRequest(
      "detail",
      headers,
      request,
      response,
      () => ({ mailId: validateMailId(mailId), query: validateEmptyPlayerQuery(query) }),
      (auth, input) => this.mailsService.get(input.mailId, auth.playerId, input.query)
    );
  }

  @Post()
  @ApiOperation({ summary: "Send mail" })
  @ApiCreatedResponse({ schema: { example: { ok: true, mail_id: "mail_1j7qv8m4x2" } } })
  create(@Headers() headers: any, @Body() body: any) {
    this.authenticateService(headers);
    return this.mailsService.create(body);
  }

  @Post("reward-deliveries")
  @ApiOperation({ summary: "Create an idempotent system reward mail from a trusted service" })
  @ApiCreatedResponse({ schema: { example: { ok: true, mail_id: "mail_1", delivery_request_id: "reward_mail:1" } } })
  createRewardDelivery(@Headers() headers: any, @Body() body: any) {
    this.authenticateService(headers);
    return this.mailsService.createRewardDelivery(body);
  }

  @Put(":mailId/read")
  @HttpCode(HttpStatus.OK)
  @ApiOperation({ summary: "Mark mail as read" })
  async markRead(
    @Param("mailId") mailId: string,
    @Headers() headers: any,
    @Body() body: any,
    @Res({ passthrough: true }) response: any,
    @Req() request: any = {}
  ) {
    return this.executePlayerRequest(
      "read",
      headers,
      request,
      response,
      () => ({ mailId: validateMailId(mailId), body: validateEmptyPlayerMutationBody(body) }),
      (auth, input) => this.mailsService.markRead(input.mailId, auth.playerId, input.body)
    );
  }

  @Post(":mailId/claim")
  @HttpCode(HttpStatus.OK)
  @ApiOperation({ summary: "Claim mail attachment" })
  async claim(
    @Param("mailId") mailId: string,
    @Headers() headers: any,
    @Body() body: any,
    @Res({ passthrough: true }) response: any,
    @Req() request: any = {}
  ) {
    const result = await this.executePlayerRequest(
      "claim",
      headers,
      request,
      response,
      () => ({ mailId: validateMailId(mailId), body: validateEmptyPlayerMutationBody(body) }),
      (auth, input) => this.mailsService.claim(input.mailId, auth.playerId, auth.characterId, input.body)
    );
    const { _http_status: httpStatus = HttpStatus.OK, ...bodyResult } = result;
    response.status(httpStatus);
    return bodyResult;
  }
}
