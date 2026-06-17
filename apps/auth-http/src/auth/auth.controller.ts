import { Body, Controller, Get, HttpCode, HttpStatus, Post, Req, Res } from "@nestjs/common";
import { ApiBody, ApiCreatedResponse, ApiOkResponse, ApiOperation, ApiTags } from "@nestjs/swagger";

import { AuthService } from "./auth.service.js";
import { GuestLoginDto } from "./dto/guest-login.dto.js";
import { LoginDto } from "./dto/login.dto.js";
import { LoginResponseDto } from "./dto/login-response.dto.js";
import { RegisterDto } from "./dto/register.dto.js";

@ApiTags("auth")
@Controller("/api/v1/auth")
export class AuthController {
  constructor(private readonly authService: AuthService) {}

  @Post("login")
  @ApiOperation({ summary: "Password account login" })
  @ApiBody({ type: LoginDto })
  @ApiCreatedResponse({ type: LoginResponseDto })
  @HttpCode(HttpStatus.CREATED)
  login(@Body() dto: LoginDto, @Req() req: any, @Res({ passthrough: true }) res: any) {
    return this.authService.login(dto, req, res);
  }

  @Post("register")
  @ApiOperation({ summary: "Register password account" })
  @ApiBody({ type: RegisterDto })
  @ApiCreatedResponse({
    schema: {
      oneOf: [
        { $ref: "#/components/schemas/LoginResponseDto" },
        {
          example: {
            ok: true,
            playerId: "plr_1j7qv8m4x2",
            loginName: "test001",
            displayName: "Test Player",
            status: "pending_review",
            pendingReview: true,
            message: "Registration submitted for review"
          }
        }
      ]
    }
  })
  @HttpCode(HttpStatus.CREATED)
  register(@Body() dto: RegisterDto, @Req() req: any) {
    return this.authService.register(dto, req);
  }

  @Post("guest-login")
  @ApiOperation({ summary: "Guest login" })
  @ApiBody({ type: GuestLoginDto })
  @ApiCreatedResponse({ type: LoginResponseDto })
  @HttpCode(HttpStatus.CREATED)
  guestLogin(@Body() dto: GuestLoginDto, @Req() req: any) {
    return this.authService.guestLogin(dto, req);
  }

  @Get("me")
  @ApiOperation({ summary: "Get current player session" })
  @ApiOkResponse({ schema: { example: { ok: true, playerId: "plr_1j7qv8m4x2", guestId: null, loginName: "test001", createdAt: "2026-05-28T12:00:00.000Z" } } })
  me(@Req() req: any) {
    return this.authService.me(req);
  }

  @Post("logout")
  @HttpCode(HttpStatus.OK)
  @ApiOperation({ summary: "Logout and optionally revoke a game ticket" })
  @ApiOkResponse({ schema: { example: { ok: true, message: "Logged out" } } })
  logout(@Req() req: any, @Body() body: any) {
    return this.authService.logout(req, body);
  }

  @Post("change-password")
  @HttpCode(HttpStatus.OK)
  @ApiOperation({ summary: "Change password for password account" })
  @ApiOkResponse({ schema: { example: { ok: true, message: "Password changed successfully. Please login again." } } })
  changePassword(@Req() req: any, @Body() body: any) {
    return this.authService.changePassword(req, body);
  }
}
