import { Body, Controller, Get, HttpCode, HttpStatus, Post, Req } from "@nestjs/common";
import { ApiBearerAuth, ApiBody, ApiCreatedResponse, ApiOkResponse, ApiOperation, ApiTags } from "@nestjs/swagger";

import { CharactersService } from "./characters.service.js";
import { CreateCharacterDto } from "./dto/create-character.dto.js";
import { SelectCharacterDto } from "./dto/select-character.dto.js";

@ApiTags("characters")
@ApiBearerAuth()
@Controller("/api/v1/characters")
export class CharactersController {
  constructor(private readonly charactersService: CharactersService) {}

  @Get()
  @ApiOperation({ summary: "List characters owned by the current account" })
  @ApiOkResponse({
    schema: {
      example: {
        ok: true,
        playerId: "plr_1j7qv8m4x2",
        characters: []
      }
    }
  })
  list(@Req() req: any) {
    return this.charactersService.list(req);
  }

  @Post()
  @ApiOperation({ summary: "Create a character for the current account" })
  @ApiBody({ type: CreateCharacterDto })
  @ApiCreatedResponse({
    schema: {
      example: {
        ok: true,
        character: {
          character_id: "chr_0000000000001",
          character_id_short: "00000001",
          name: "WindRunner",
          world_id: 0,
          status: "active",
          appearance_json: { body: "default" },
          last_login_at: null,
          position: { scene_id: 100, x: 0, y: 0, dir_x: 0, dir_y: 1 },
          attributes: {
            affinity: { earth: 2500, fire: 2500, water: 2500, wind: 2500 },
            mastery: { earth: 0, fire: 0, water: 0, wind: 0 }
          }
        }
      }
    }
  })
  create(@Req() req: any, @Body() dto: CreateCharacterDto) {
    return this.charactersService.create(req, dto);
  }

  @Post("select")
  @HttpCode(HttpStatus.OK)
  @ApiOperation({ summary: "Select a character and issue a character-bound game ticket" })
  @ApiBody({ type: SelectCharacterDto })
  @ApiOkResponse({
    schema: {
      example: {
        ok: true,
        playerId: "plr_1j7qv8m4x2",
        character: {
          character_id: "chr_0000000000001",
          character_id_short: "00000001",
          name: "WindRunner",
          world_id: 0,
          status: "active",
          appearance_json: { body: "default" },
          last_login_at: "2026-06-25T12:00:00.000Z",
          position: { scene_id: 100, x: 0, y: 0, dir_x: 0, dir_y: 1 },
          attributes: {
            affinity: { earth: 2500, fire: 2500, water: 2500, wind: 2500 },
            mastery: { earth: 0, fire: 0, water: 0, wind: 0 }
          }
        },
        ticket: "eyJwbGF5ZXJJZCI6InBscl8xajdxdjhtNHgyIn0.signature",
        ticketExpiresAt: "2026-06-25T12:15:00.000Z",
        gameProxyHost: "127.0.0.1",
        gameProxyPort: 4000
      }
    }
  })
  select(@Req() req: any, @Body() dto: SelectCharacterDto) {
    return this.charactersService.select(req, dto);
  }
}
