import {
  encodeActivityClaimReq,
  encodeActivityDetailReq,
  encodeActivityListReq
} from "../messages.js";
import { MESSAGE_TYPE } from "../constants.js";

/**
 * Build the player activity request sequence without connecting to a server. The repeated claim
 * deliberately reuses the same opaque request id to exercise client retry behavior.
 */
export async function runActivityScenario(send) {
  const requests = [
    [MESSAGE_TYPE.ACTIVITY_LIST_REQ, encodeActivityListReq()],
    [MESSAGE_TYPE.ACTIVITY_DETAIL_REQ, encodeActivityDetailReq("activity-demo", 0)],
    [
      MESSAGE_TYPE.ACTIVITY_CLAIM_REQ,
      encodeActivityClaimReq("activity-demo", 1, "stage-1", "activity-retry-1")
    ],
    [
      MESSAGE_TYPE.ACTIVITY_CLAIM_REQ,
      encodeActivityClaimReq("activity-demo", 1, "stage-1", "activity-retry-1")
    ]
  ];
  for (const [messageType, body] of requests) {
    await send(messageType, body);
  }
  return requests;
}
