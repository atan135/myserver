import { ApiProperty } from "@nestjs/swagger";

export class AdminDto {
  @ApiProperty({ example: 1 })
  id: number;

  @ApiProperty({ example: "admin" })
  username: string;

  @ApiProperty({ example: "Administrator" })
  displayName: string;

  @ApiProperty({ example: "admin" })
  role: string;
}

export class LoginResponseDto {
  @ApiProperty({ example: true })
  ok: boolean;

  @ApiProperty({ example: "eyJhbGciOiJIUzI1NiIsInR5cCI6IkpXVCJ9.payload.signature" })
  accessToken: string;

  @ApiProperty({ example: "8h" })
  expiresIn: string;

  @ApiProperty({ type: AdminDto })
  admin: AdminDto;
}
