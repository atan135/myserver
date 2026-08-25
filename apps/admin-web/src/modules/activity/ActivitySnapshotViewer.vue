<template>
  <section class="snapshot-viewer">
    <el-alert title="当前版本为只读快照，不能直接修改。需要调整时请创建新草稿。" type="info" :closable="false" show-icon />
    <h4>公共配置</h4>
    <el-descriptions :column="2" border size="small">
      <el-descriptions-item label="标题">{{ snapshot.publicConfig?.title || "-" }}</el-descriptions-item>
      <el-descriptions-item label="领取方式">{{ claimModeLabel(snapshot.publicConfig?.claimMode || snapshot.publicConfig?.claim_mode) }}</el-descriptions-item>
      <el-descriptions-item label="开始时间">{{ formatTime(snapshot.startAt) }}</el-descriptions-item>
      <el-descriptions-item label="结束时间">{{ formatTime(snapshot.endAt) }}</el-descriptions-item>
      <el-descriptions-item label="领取截止">{{ formatTime(snapshot.claimDeadline) }}</el-descriptions-item>
      <el-descriptions-item label="时区">{{ snapshot.timezone || "-" }}</el-descriptions-item>
      <el-descriptions-item label="资源" :span="2"><code>{{ formatJson(snapshot.publicConfig?.resources || []) }}</code></el-descriptions-item>
      <el-descriptions-item label="变更原因" :span="2">{{ snapshot.reason || "-" }}</el-descriptions-item>
    </el-descriptions>

    <h4>类型配置</h4>
    <el-descriptions :column="2" border size="small" class="config-table">
      <el-descriptions-item v-for="([key, value]) in configEntries(snapshot.typeConfig)" :key="key" :label="key"><code>{{ formatJson(value) }}</code></el-descriptions-item>
    </el-descriptions>

    <h4>阶段</h4>
    <el-table :data="snapshot.stages || []" size="small" border empty-text="暂无阶段">
      <el-table-column prop="stageNo" label="阶段" width="90" />
      <el-table-column prop="stageId" label="阶段标识" min-width="130" />
      <el-table-column prop="rewardGroupKey" label="奖励组" min-width="130" />
      <el-table-column label="资格条件" min-width="180"><template #default="{ row }"><code>{{ formatJson(row.qualification || {}) }}</code></template></el-table-column>
    </el-table>

    <h4>奖励组</h4>
    <el-empty v-if="!(snapshot.rewardGroups || []).length" description="暂无奖励组" :image-size="48" />
    <div v-for="group in snapshot.rewardGroups || []" :key="group.key" class="reward-group">
      <div class="reward-group__heading"><strong>{{ group.key }}</strong><el-tag size="small">{{ selectionModeLabel(group.selectionMode) }}</el-tag></div>
      <el-table :data="group.items || []" size="small" border empty-text="暂无奖励项">
        <el-table-column prop="item_id" label="物品 ID" width="120" />
        <el-table-column prop="quantity" label="数量" width="100" />
        <el-table-column prop="weight" label="权重" width="100" />
        <el-table-column prop="binding" label="绑定" min-width="120" />
      </el-table>
    </div>
  </section>
</template>

<script setup>
defineProps({ snapshot: { type: Object, required: true } });
function formatTime(value) { return value ? new Date(value).toLocaleString("zh-CN") : "-"; }
function formatJson(value) { return typeof value === "string" ? value : JSON.stringify(value ?? {}, null, 2); }
function configEntries(value) { return Object.entries(value || {}); }
function claimModeLabel(value) { return ({ manual: "手动", automatic: "自动" }[value] || value || "-"); }
function selectionModeLabel(value) { return ({ fixed: "固定", weighted: "按权重" }[value] || value || "-"); }
</script>

<style scoped>
.snapshot-viewer { display: grid; gap: 10px; }
h4 { margin: 8px 0 0; font-size: 15px; }
code { white-space: pre-wrap; word-break: break-word; overflow-wrap: anywhere; }
.reward-group { display: grid; gap: 6px; border: 1px solid var(--el-border-color-lighter); padding: 10px; }
.reward-group__heading { display: flex; align-items: center; justify-content: space-between; gap: 8px; }
</style>
