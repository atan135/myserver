<template>
  <section class="type-editor">
    <el-form label-width="100px" size="small">
      <el-form-item label="进度模式"><el-select v-model="draft.progression" @change="emitConfig"><el-option label="连续" value="consecutive" /><el-option label="累计" value="cumulative" /></el-select></el-form-item>
      <el-form-item label="漏签策略"><el-select v-model="draft.miss_policy" @change="emitConfig"><el-option label="重置" value="reset" /><el-option label="延续" value="carry" /></el-select></el-form-item>
      <el-form-item label="领取方式"><el-select v-model="draft.claim_mode" @change="emitConfig"><el-option label="手动" value="manual" /><el-option label="自动" value="automatic" /></el-select></el-form-item>
      <el-form-item label="阶段达成"><div class="stage-counts"><div v-for="(stage, index) in draft.stages || []" :key="stage.stage_no" class="stage-count"><span>阶段 {{ stage.stage_no }}</span><el-input-number :model-value="stage.required_count" :min="1" controls-position="right" @update:model-value="(value) => updateStageCount(index, value)" /></div></div></el-form-item>
    </el-form>
  </section>
</template>

<script setup>
import { reactive, watch } from "vue";
import { validateLoginReward } from "./types/login_reward.ts";

const props = defineProps({ modelValue: { type: Object, required: true }, stages: { type: Array, default: () => [] } });
const emit = defineEmits(["update:modelValue"]);
const draft = reactive({ ...props.modelValue });
function emitConfig() { const previous = new Map((draft.stages || []).map((stage) => [Number(stage.stage_no), stage])); const value = { ...draft, stages: props.stages.map((stage) => ({ stage_no: Number(stage.stageNo), required_count: Number(previous.get(Number(stage.stageNo))?.required_count) || 1, reward_group_key: stage.rewardGroupKey })) }; try { emit("update:modelValue", validateLoginReward(value)); } catch { emit("update:modelValue", value); } }
function updateStageCount(index, value) { draft.stages[index] = { ...draft.stages[index], required_count: Number(value) || 1 }; emitConfig(); }
watch(() => props.modelValue, (value) => { Object.keys(draft).forEach((key) => delete draft[key]); Object.assign(draft, value || {}); }, { deep: true });
watch(() => props.stages, emitConfig, { deep: true });
</script>

<style scoped>.type-editor { padding: 8px 0; } .type-editor :deep(.el-select) { width: 180px; } .stage-counts { display: grid; gap: 8px; } .stage-count { display: flex; align-items: center; gap: 10px; }</style>
