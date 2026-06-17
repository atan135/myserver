import { ApiHttpException, badRequest } from "../common/http-exception.js";

type DecodeGlobalIdInput = (id: string) => unknown;

const GLOBAL_ID_DECODER_MODULE = "../../../../packages/global-id/node/index.js";
let decoderPromise: Promise<DecodeGlobalIdInput> | null = null;

async function loadDecoder(): Promise<DecodeGlobalIdInput> {
  if (!decoderPromise) {
    decoderPromise = import(GLOBAL_ID_DECODER_MODULE)
      .then((module) => {
        if (typeof module.decodeGlobalIdInput !== "function") {
          throw new Error("packages/global-id/node/index.js does not export decodeGlobalIdInput");
        }
        return module.decodeGlobalIdInput as DecodeGlobalIdInput;
      })
      .catch((error) => {
        decoderPromise = null;
        throw error;
      });
  }

  return decoderPromise;
}

export async function decodeGlobalId(id: string) {
  let decoder: DecodeGlobalIdInput;
  try {
    decoder = await loadDecoder();
  } catch (error: any) {
    throw new ApiHttpException(503, {
      ok: false,
      error: "GLOBAL_ID_DECODER_UNAVAILABLE",
      message: error?.message || "Global ID decoder is unavailable"
    });
  }

  try {
    return await decoder(id);
  } catch (error: any) {
    throw badRequest(error?.code || "INVALID_GLOBAL_ID", error?.message || "Invalid global ID");
  }
}
