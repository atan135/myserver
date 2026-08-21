<template>
  <section class="lottery-editor" aria-label="Lottery configuration">
    <div class="lottery-editor__limits">
      <el-input-number v-model="draft.free_draw_count" :min="0" controls-position="right" />
      <el-input-number v-model="draft.voucher_item_id" :min="1" controls-position="right" />
      <el-input-number v-model="draft.daily_draw_limit" :min="0" controls-position="right" />
      <el-input-number v-model="draft.total_draw_limit" :min="0" controls-position="right" />
    </div>
    <el-alert v-if="errors.length" type="error" :closable="false" :title="errors.join('; ')" />
    <el-table :data="rows" size="small" border>
      <el-table-column prop="item_id" label="Item" width="120" />
      <el-table-column label="Quantity" width="150">
        <template #default="{ row }"><el-input-number :model-value="row.quantity" :min="1" controls-position="right" @update:model-value="(value) => updatePoolItem(row.item_id, 'quantity', value)" /></template>
      </el-table-column>
      <el-table-column label="Weight" width="150">
        <template #default="{ row }"><el-input-number :model-value="row.weight" :min="1" controls-position="right" @update:model-value="(value) => updatePoolItem(row.item_id, 'weight', value)" /></template>
      </el-table-column>
      <el-table-column label="Catalog" width="110">
        <template #default="{ row }"><el-tag :type="row.reward_exists ? 'success' : 'danger'">{{ row.reward_exists ? 'OK' : 'Missing' }}</el-tag></template>
      </el-table-column>
    </el-table>
    <div class="lottery-editor__summary">{{ rows.length }} items / {{ totalWeight }} total weight</div>
  </section>
</template>

<script setup lang="ts">
import { computed, reactive, watch } from "vue";
import { ElAlert, ElInputNumber, ElTable, ElTableColumn, ElTag } from "element-plus";
import { buildLotteryPoolEditor, serializeLotteryConfig, type LotteryConfig, type LotteryPoolEditorRow } from "./types/lottery";

const props = defineProps<{ modelValue: LotteryConfig; rewardItemIds?: number[] }>();
const emit = defineEmits<{ (event: "update:modelValue", value: LotteryConfig): void }>();
const draft = reactive<LotteryConfig>(serializeLotteryConfig(props.modelValue));
const errors = computed(() => {
  try { serializeLotteryConfig(draft); return []; }
  catch (error) { return [error instanceof Error ? error.message : "Invalid lottery configuration"]; }
});
const rows = computed<LotteryPoolEditorRow[]>(() => {
  try { return buildLotteryPoolEditor(draft, props.rewardItemIds ?? []); }
  catch { return draft.pool_items.map((item) => ({ ...item, reward_exists: (props.rewardItemIds ?? []).includes(item.item_id) })); }
});
const totalWeight = computed(() => rows.value.reduce((sum, row) => sum + (Number.isSafeInteger(row.weight) && row.weight > 0 ? row.weight : 0), 0));
function updatePoolItem(itemId: number, field: "quantity" | "weight", value: number | undefined): void {
  const item = draft.pool_items.find((entry) => entry.item_id === itemId);
  if (item) item[field] = value ?? 0;
  emitConfig();
}
function emitConfig(): void { if (!errors.value.length) emit("update:modelValue", serializeLotteryConfig(draft)); }
function replaceDraft(value: LotteryConfig): void {
  const next = serializeLotteryConfig(value);
  for (const key of Object.keys(draft) as Array<keyof LotteryConfig>) delete draft[key];
  Object.assign(draft, next);
}
watch(() => props.modelValue, replaceDraft, { deep: true });
</script>

<style scoped>
.lottery-editor { display: grid; gap: 12px; }
.lottery-editor__limits { display: grid; grid-template-columns: repeat(4, minmax(120px, 1fr)); gap: 8px; }
.lottery-editor__summary { color: var(--el-text-color-secondary); font-size: 12px; }
@media (max-width: 720px) { .lottery-editor__limits { grid-template-columns: repeat(2, minmax(120px, 1fr)); } }
</style>
