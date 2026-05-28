import { ApiProperty } from "@nestjs/swagger";

export class LoginDto {
  @ApiProperty({
    description: "Admin account username.",
    example: "admin"
  })
  username: string;

  @ApiProperty({
    description: "Admin account password.",
    example: "AdminPass123!"
  })
  password: string;
}
