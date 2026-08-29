import { toIsoString, toNumericId, nextParam } from "../formatters.js";

export function readTotal(rows) {
  return Number.parseInt(String(rows[0]?.total ?? "0"), 10);
}

export function assetLedgerFilters({ characterId, requestId, originType, originId, deliveryId, from, to } = {}) {
  let where = " WHERE 1=1";
  const params = [];
  const add = (column, value) => {
    if (!value) return;
    params.push(value);
    where += ` AND ${column} = ${nextParam(params)}`;
  };

  add("character_id", characterId);
  add("request_id", requestId);
  add("origin_type", originType);
  add("origin_id", originId);
  add("delivery_id", deliveryId);
  if (from) {
    params.push(from);
    where += ` AND created_at >= ${nextParam(params)}::timestamptz`;
  }
  if (to) {
    params.push(to);
    where += ` AND created_at <= ${nextParam(params)}::timestamptz`;
  }
  return { where, params };
}

export function assetLedgerQuery(filters) {
  const { where, params } = assetLedgerFilters(filters);
  return {
    query: `SELECT
              id,
              character_id,
              request_id,
              asset_type,
              item_id,
              COALESCE((binding_json ->> ''binded'')::boolean, false) AS is_bound,
              quantity_before,
              quantity_after,
              quantity_delta,
              container,
              source,
              origin_type,
              origin_id,
              delivery_method,
              delivery_id,
              mail_id,
              fallback_reason,
              rule_version,
              snapshot_revision,
              created_at
            FROM character_asset_ledger${where}`,
    params
  };
}

export function toAssetLedgerEntry(row) {
  return {
    id: toNumericId(row.id),
    characterId: row.character_id,
    requestId: row.request_id,
    assetType: row.asset_type,
    itemId: Number(row.item_id),
    isBound: row.is_bound === true,
    quantityBefore: Number(row.quantity_before),
    quantityAfter: Number(row.quantity_after),
    quantityDelta: Number(row.quantity_delta),
    container: row.container,
    source: row.source,
    originType: row.origin_type,
    originId: row.origin_id,
    deliveryMethod: row.delivery_method,
    deliveryId: row.delivery_id || null,
    mailId: row.mail_id || null,
    fallbackReason: row.fallback_reason || null,
    ruleVersion: row.rule_version,
    snapshotRevision: Number(row.snapshot_revision),
    createdAt: toIsoString(row.created_at)
  };
}
