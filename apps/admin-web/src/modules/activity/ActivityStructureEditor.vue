<template>
  <section class="structure-editor">
    <div class="section-heading"><h4>阶段</h4><el-button size="small" @click="addStage">新增阶段</el-button></div>
    <el-empty v-if="!stageRows.length" description="暂无阶段" :image-size="56" />
    <div v-for="(stage, index) in stageRows" :key="stage.stageId || index" class="stage-row">
      <el-input-number v-model="stage.stageNo" :min="1" controls-position="right" class="stage-no" @change="emitStages" />
      <el-input v-model="stage.stageId" placeholder="阶段标识" @change="emitStages" />
      <el-input v-model="stage.rewardGroupKey" placeholder="奖励组" @change="emitStages" />
      <el-input v-model="stage.qualificationJson" placeholder="资格条件 JSON" @change="emitStages" />
      <el-button type="danger" link @click="removeStage(index)">删除</el-button>
    </div>

    <div class="section-heading groups-heading"><h4>奖励组</h4><el-button size="small" @click="addGroup">新增奖励组</el-button></div>
    <el-empty v-if="!groupRows.length" description="暂无奖励组" :image-size="56" />
    <div v-for="(group, groupIndex) in groupRows" :key="group.key || groupIndex" class="group-row">
      <div class="group-header"><el-input v-model="group.key" placeholder="奖励组标识" @change="emitGroups" /><el-select v-model="group.selectionMode" @change="emitGroups"><el-option label="固定" value="fixed" /><el-option label="按权重" value="weighted" /></el-select><el-button type="danger" link @click="removeGroup(groupIndex)">删除奖励组</el-button></div>
      <div v-for="(item, itemIndex) in group.items" :key="itemIndex" class="item-row">
        <el-input-number v-model="item.item_id" :min="1" controls-position="right" placeholder="物品 ID" @change="emitGroups" />
        <el-input-number v-model="item.quantity" :min="1" controls-position="right" placeholder="数量" @change="emitGroups" />
        <el-input-number v-model="item.weight" :min="1" controls-position="right" placeholder="权重" @change="emitGroups" />
        <el-button type="danger" link @click="removeItem(groupIndex, itemIndex)">删除</el-button>
      </div>
      <el-button size="small" text @click="addItem(groupIndex)">新增奖励项</el-button>
    </div>
  </section>
</template>

<script setup>
import { computed } from "vue";
import { appendRewardGroup, appendRewardItem, appendStage } from "./structure-utils.js";

const props = defineProps({ stages: { type: Array, default: () => [] }, rewardGroups: { type: Array, default: () => [] } });
const emit = defineEmits(["update:stages", "update:rewardGroups"]);

const stageRows = computed(() => props.stages.map((stage) => ({ ...stage, qualificationJson: JSON.stringify(stage.qualification || {}) })));
const groupRows = computed(() => props.rewardGroups.map((group) => ({ ...group, items: (group.items || []).map((item) => ({ ...item })) })));
function parseQualification(value) { try { const parsed = JSON.parse(value || "{}"); return parsed && typeof parsed === "object" && !Array.isArray(parsed) ? parsed : {}; } catch { return {}; } }
function emitStages() { emit("update:stages", stageRows.value.map(({ qualificationJson, requiredCount: _requiredCount, ...stage }) => ({ ...stage, stageNo: Number(stage.stageNo), qualification: parseQualification(qualificationJson) }))); }
function emitGroups() { emit("update:rewardGroups", groupRows.value.map((group) => ({ ...group, items: group.items.map((item) => ({ ...item, item_id: Number(item.item_id), quantity: Number(item.quantity), weight: Number(item.weight) })) }))); }
function addStage() { emit("update:stages", appendStage(props.stages, props.rewardGroups)); }
function removeStage(index) { emit("update:stages", props.stages.filter((_, itemIndex) => itemIndex !== index)); }
function addGroup() { emit("update:rewardGroups", appendRewardGroup(props.rewardGroups)); }
function removeGroup(index) { emit("update:rewardGroups", props.rewardGroups.filter((_, itemIndex) => itemIndex !== index)); }
function addItem(groupIndex) { emit("update:rewardGroups", appendRewardItem(props.rewardGroups, groupIndex)); }
function removeItem(groupIndex, itemIndex) { const groups = groupRows.value.map((group) => ({ ...group, items: group.items.map((item) => ({ ...item })) })); groups[groupIndex].items.splice(itemIndex, 1); emit("update:rewardGroups", groups); }
</script>

<style scoped>
.structure-editor { display: grid; gap: 10px; } .section-heading { display: flex; justify-content: space-between; align-items: center; margin-top: 8px; } h4 { font-size: 15px; } .stage-row, .group-header, .item-row { display: grid; grid-template-columns: 100px minmax(120px, 1fr) minmax(120px, 1fr) minmax(160px, 1.5fr) 56px; gap: 8px; align-items: center; } .stage-no { width: 100%; } .groups-heading { margin-top: 18px; } .group-row { display: grid; gap: 8px; border: 1px solid var(--el-border-color-lighter); padding: 10px; } .group-header { grid-template-columns: minmax(120px, 1fr) 120px 100px; } .item-row { grid-template-columns: repeat(3, minmax(100px, 1fr)) 56px; } @media (max-width: 720px) { .stage-row, .group-header, .item-row { grid-template-columns: 1fr 1fr; } .stage-row .el-button, .group-header .el-button, .item-row .el-button { justify-self: start; } }
</style>
