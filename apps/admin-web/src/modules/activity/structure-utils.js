export function appendStage(stages = [], rewardGroups = []) {
  const max = stages.reduce((value, stage) => Math.max(value, Number(stage.stageNo) || 0), 0);
  return [...stages, { stageId: `stage-${max + 1}`, stageNo: max + 1, rewardGroupKey: rewardGroups[0]?.key || "default", qualification: {} }];
}

export function appendRewardGroup(groups = []) {
  const used = new Set(groups.map((group) => group.key));
  const base = "group";
  let key = base;
  let index = 1;
  while (used.has(key)) key = `${base}-${index++}`;
  return [...groups, { key, selectionMode: "fixed", items: [{ item_id: 1001, quantity: 1, weight: 1 }] }];
}

export function appendRewardItem(groups = [], groupIndex) {
  return groups.map((group, index) => index === groupIndex ? { ...group, items: [...(group.items || []), { item_id: 1001, quantity: 1, weight: 1 }] } : { ...group, items: [...(group.items || [])] });
}
