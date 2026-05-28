import { ApiProperty } from "@nestjs/swagger";

export class ClientServiceDto {
  @ApiProperty({ example: "127.0.0.1" })
  host: string;

  @ApiProperty({ example: 4000 })
  port: number;

  @ApiProperty({ example: "kcp" })
  protocol: string;
}

export class LoginServicesDto {
  @ApiProperty({ type: ClientServiceDto })
  game: ClientServiceDto;

  @ApiProperty({ type: ClientServiceDto, nullable: true })
  chat: ClientServiceDto | null;

  @ApiProperty({ type: ClientServiceDto, nullable: true })
  mail: ClientServiceDto | null;

  @ApiProperty({ type: ClientServiceDto, nullable: true })
  announce: ClientServiceDto | null;
}

export class LoginResponseDto {
  @ApiProperty({ example: true })
  ok: boolean;

  @ApiProperty({ example: "player-4e2fe4d6-5f8e-49e7-96cf-47d52efc3264" })
  playerId: string;

  @ApiProperty({ example: "guest-device-001", nullable: true })
  guestId: string | null;

  @ApiProperty({ example: "test001", nullable: true })
  loginName: string | null;

  @ApiProperty({ example: "a3f01b8f9e8a46ad930b7ff9b91b6c2e" })
  accessToken: string;

  @ApiProperty({ example: "eyJwbGF5ZXJJZCI6InBsYXllci0xIn0.signature" })
  ticket: string;

  @ApiProperty({ example: "2026-05-28T12:00:00.000Z" })
  ticketExpiresAt: string;

  @ApiProperty({ example: "127.0.0.1" })
  gameProxyHost: string;

  @ApiProperty({ example: 4000 })
  gameProxyPort: number;

  @ApiProperty({ type: LoginServicesDto })
  services: LoginServicesDto;
}
