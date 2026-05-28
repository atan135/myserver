import { ApiProperty } from "@nestjs/swagger";

export class LoginDto {
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
}
