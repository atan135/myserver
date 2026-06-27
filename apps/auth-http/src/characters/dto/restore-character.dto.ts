import { ApiProperty } from "@nestjs/swagger";

export class RestoreCharacterDto {
  @ApiProperty({
    description: "Soft-deleted character identifier owned by the current account.",
    example: "chr_0000000000001"
  })
  character_id: string;

  @ApiProperty({
    description: "camelCase alias accepted for clients that do not use snake_case.",
    example: "chr_0000000000001",
    required: false
  })
  characterId?: string;
}