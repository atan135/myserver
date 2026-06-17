import { ApiProperty } from "@nestjs/swagger";

export class RegisterDto {
  @ApiProperty({
    description: "Password account login name.",
    example: "test001"
  })
  loginName: string;

  @ApiProperty({
    description: "Password for the password account.",
    example: "Passw0rd!",
    minLength: 6,
    maxLength: 128
  })
  password: string;

  @ApiProperty({
    description: "Optional player display name.",
    example: "Test Player",
    required: false,
    nullable: true
  })
  displayName?: string | null;
}
