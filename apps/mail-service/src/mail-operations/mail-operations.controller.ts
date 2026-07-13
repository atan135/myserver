import { Body, Controller, Get, Headers, HttpCode, HttpStatus, Inject, Param, Post, Query } from "@nestjs/common";
import { ApiOperation, ApiTags } from "@nestjs/swagger";

import { forbidden, serviceUnavailable, unauthorized } from "../common/http-exception.js";
import { validateMailHighRiskToken, validateMailOperationsToken } from "../mail-auth.js";
import { MAIL_CONFIG } from "../tokens.js";
import { MailOperationsService } from "./mail-operations.service.js";

@ApiTags("mail-operations")
@Controller("/api/v1/internal/mail-operations")
export class MailOperationsController {
  constructor(
    private readonly operations: MailOperationsService,
    @Inject(MAIL_CONFIG) private readonly config: any
  ) {}

  private authorize(headers: any, highRisk = false) {
    try {
      validateMailOperationsToken(headers, this.config);
      if (highRisk) validateMailHighRiskToken(headers, this.config);
    } catch (error: any) {
      if (error?.statusCode === 403) throw forbidden(error.code, error.message);
      if (error?.statusCode === 503) throw serviceUnavailable(error.code, error.message);
      throw unauthorized(error?.code || "MAIL_OPERATIONS_TOKEN_REQUIRED", error?.message);
    }
  }

  @Get("claims")
  @ApiOperation({ summary: "Query bounded mail claim workflows" })
  queryClaims(@Headers() headers: any, @Query() query: any) {
    this.authorize(headers);
    return this.operations.queryClaims(query);
  }

  @Post("claims/:mailId/reconcile")
  @HttpCode(HttpStatus.ACCEPTED)
  reconcile(@Param("mailId") mailId: string, @Headers() headers: any, @Body() body: any) {
    this.authorize(headers);
    return this.operations.scheduleClaim(mailId, "reconcile", body);
  }

  @Post("claims/:mailId/retry-original")
  @HttpCode(HttpStatus.ACCEPTED)
  retryOriginal(@Param("mailId") mailId: string, @Headers() headers: any, @Body() body: any) {
    this.authorize(headers);
    return this.operations.scheduleClaim(mailId, "retry_original", body);
  }

  @Post("claims/:mailId/manual-recover")
  @HttpCode(HttpStatus.ACCEPTED)
  manualRecover(@Param("mailId") mailId: string, @Headers() headers: any, @Body() body: any) {
    this.authorize(headers, true);
    return this.operations.scheduleClaim(mailId, "manual_recover", body, true);
  }

  @Post("outbox/:eventId/replay")
  @HttpCode(HttpStatus.ACCEPTED)
  replayOutbox(@Param("eventId") eventId: string, @Headers() headers: any, @Body() body: any) {
    this.authorize(headers);
    return this.operations.replayOutbox(eventId, body);
  }
}
