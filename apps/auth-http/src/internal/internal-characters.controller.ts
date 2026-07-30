import { Body, Controller, Inject, Post, Req } from "@nestjs/common";
import { ApiOperation, ApiTags } from "@nestjs/swagger";

import { CharactersService } from "../characters/characters.service.js";
import { AUTH_CONFIG } from "../tokens.js";
import { verifyInternalToken } from "./internal-auth.js";

@ApiTags("internal-characters")
@Controller("/api/v1/internal/characters")
export class InternalCharactersController {
  constructor(
    @Inject(AUTH_CONFIG) private readonly config: any,
    private readonly charactersService: CharactersService
  ) {}

  @Post()
  @ApiOperation({ summary: "Create a character on behalf of the admin control plane" })
  async create(@Req() req: any, @Body() body: any) {
    verifyInternalToken(req, this.config);
    return this.charactersService.createForAdmin(body);
  }
}
