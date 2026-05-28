import { ApiProperty } from "@nestjs/swagger";

export class GuestLoginDto {
  @ApiProperty({
    description: "Client-side guest identifier. When omitted, the server creates one.",
    example: "guest-device-001",
    required: false,
    nullable: true
  })
  guestId?: string | null;
}
