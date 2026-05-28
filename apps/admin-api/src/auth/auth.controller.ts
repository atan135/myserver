import { Body, Controller, Get, HttpCode, HttpStatus, Post, Req, UseGuards } from "@nestjs/common";
import { ApiBearerAuth, ApiBody, ApiOkResponse, ApiOperation, ApiTags } from "@nestjs/swagger";

import { AuthService } from "./auth.service.js";
import { JwtAuthGuard } from "./jwt-auth.guard.js";
import { LoginDto } from "./dto/login.dto.js";
import { LoginResponseDto } from "./dto/login-response.dto.js";

@ApiTags("auth")
@Controller("/api/v1/auth")
export class AuthController {
  constructor(private readonly authService: AuthService) {}

  @Post("login")
  @HttpCode(HttpStatus.OK)
  @ApiOperation({ summary: "Admin login" })
  @ApiBody({ type: LoginDto })
  @ApiOkResponse({ type: LoginResponseDto })
  login(@Body() dto: LoginDto, @Req() req: any) {
    return this.authService.login(dto, req);
  }

  @Get("me")
  @UseGuards(JwtAuthGuard)
  @ApiBearerAuth()
  @ApiOperation({ summary: "Get current admin account" })
  me(@Req() req: any) {
    return this.authService.me(req);
  }

  @Post("logout")
  @UseGuards(JwtAuthGuard)
  @ApiBearerAuth()
  @HttpCode(HttpStatus.OK)
  @ApiOperation({ summary: "Admin logout" })
  logout(@Req() req: any) {
    return this.authService.logout(req);
  }
}
