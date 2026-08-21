<script setup>
import { computed } from "vue";
import { buildLoginRewardStageEditor, sortLoginRewardStages } from "./types/login_reward.ts";

const props = defineProps({
  modelValue: { type: Object, required: true },
  rewardGroupKeys: { type: Array, default: () => [] }
});
const emit = defineEmits(["update:modelValue"]);
const rows = computed(() => buildLoginRewardStageEditor(props.modelValue, props.rewardGroupKeys));

function updateStage(index, field, value) {
  const stages = sortLoginRewardStages(props.modelValue.stages).map((stage) => ({ ...stage }));
  stages[index] = { ...stages[index], [field]: field === "stage_no" || field === "required_count" ? Number(value) : value };
  emit("update:modelValue", { ...props.modelValue, stages });
}
</script>

<template>
  <el-table :data="rows" row-key="stage_no" size="small">
    <el-table-column prop="stage_no" label="阶段" width="90" />
    <el-table-column label="达成天数" width="140">
      <template #default="scope"><el-input-number :model-value="scope.row.required_count" :min="1" @change="updateStage(scope.$index, 'required_count', $event)" /></template>
    </el-table-column>
    <el-table-column label="奖励组" min-width="180">
      <template #default="scope"><el-select :model-value="scope.row.reward_group_key" @change="updateStage(scope.$index, 'reward_group_key', $event)"><el-option v-for="key in rewardGroupKeys" :key="key" :label="key" :value="key" /></el-select></template>
    </el-table-column>
    <el-table-column label="状态" width="120"><template #default="scope">{{ scope.row.reward_group_exists ? "已配置" : "缺少奖励组" }}</template></el-table-column>
  </el-table>
</template>
