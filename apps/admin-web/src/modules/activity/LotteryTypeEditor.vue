<template>
  <section class="type-editor">
    <el-form label-width="100px" size="small">
      <el-form-item label="免费次数"><el-input-number v-model="draft.free_draw_count" :min="0" @change="emitConfig" /></el-form-item>
      <el-form-item label="兑换物品"><el-input-number v-model="draft.voucher_item_id" :min="1" @change="emitConfig" /></el-form-item>
      <el-form-item label="每日上限"><el-input-number v-model="draft.daily_draw_limit" :min="0" @change="emitConfig" /></el-form-item>
      <el-form-item label="总次数上限"><el-input-number v-model="draft.total_draw_limit" :min="0" @change="emitConfig" /></el-form-item>
      <el-form-item label="奖池版本"><el-input-number v-model="draft.pool_version" :min="1" @change="emitConfig" /></el-form-item>
    </el-form>
    <div class="pool-heading"><strong>奖池项目</strong><el-button size="small" @click="addPoolItem">新增项目</el-button></div>
    <el-empty v-if="!(draft.pool_items || []).length" description="暂无奖池项目" :image-size="48" />
    <div v-for="(item, index) in draft.pool_items || []" :key="index" class="pool-row">
      <el-input-number v-model="item.item_id" :min="1" controls-position="right" placeholder="物品 ID" @change="emitConfig" />
      <el-input-number v-model="item.quantity" :min="1" controls-position="right" placeholder="数量" @change="emitConfig" />
      <el-input-number v-model="item.weight" :min="1" controls-position="right" placeholder="权重" @change="emitConfig" />
      <el-button type="danger" link @click="removePoolItem(index)">删除</el-button>
    </div>
  </section>
</template>

<script setup>
import { reactive, watch } from "vue";
import { serializeLotteryConfig } from "./types/lottery.ts";

const props = defineProps({ modelValue: { type: Object, required: true } });
const emit = defineEmits(["update:modelValue"]);
const draft = reactive({ ...props.modelValue });
function emitConfig() { try { emit("update:modelValue", serializeLotteryConfig(draft)); } catch { emit("update:modelValue", { ...draft }); } }
function addPoolItem() { draft.pool_items = [...(draft.pool_items || []), { item_id: 1001, quantity: 1, weight: 1 }]; emitConfig(); }
function removePoolItem(index) { draft.pool_items = (draft.pool_items || []).filter((_, itemIndex) => itemIndex !== index); emitConfig(); }
watch(() => props.modelValue, (value) => { Object.keys(draft).forEach((key) => delete draft[key]); Object.assign(draft, value || {}); }, { deep: true });
</script>

<style scoped>.type-editor { padding: 8px 0; } .type-editor :deep(.el-input-number) { width: 180px; } .pool-heading { display: flex; justify-content: space-between; align-items: center; margin: 8px 0; } .pool-row { display: grid; grid-template-columns: repeat(3, minmax(120px, 1fr)) 56px; gap: 8px; margin-bottom: 8px; } @media (max-width: 720px) { .pool-row { grid-template-columns: 1fr 1fr; } }</style>
