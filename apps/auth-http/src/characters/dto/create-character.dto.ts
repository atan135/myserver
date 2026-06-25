import { ApiProperty } from "@nestjs/swagger";

export class CreateCharacterDto {
  @ApiProperty({
    description: "Character display name. Names are not unique.",
    example: "WindRunner"
  })
  name: string;

  @ApiProperty({
    description: "Client-selected cosmetic appearance. Server validates and stores it as JSON.",
    example: { body: "default", hair: "short_01", palette: "blue" },
    required: false
  })
  appearance?: Record<string, unknown>;
}
