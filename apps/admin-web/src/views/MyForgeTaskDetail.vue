<template>
  <AdminLayout>
    <main class="detail-page">
      <header class="page-header">
        <div class="title-group">
          <el-tooltip content="返回任务列表" placement="bottom">
            <el-button
              :icon="ArrowLeft"
              circle
              aria-label="返回任务列表"
              @click="goBack"
            />
          </el-tooltip>
          <div>
            <h1>MyForge 任务详情</h1>
            <span class="request-id mono">{{ requestId }}</span>
          </div>
        </div>
        <div class="header-actions">
          <el-tooltip content="刷新任务详情" placement="bottom">
            <el-button
              :icon="Refresh"
              circle
              aria-label="刷新任务详情"
              :loading="state.refreshing"
              :disabled="state.loading"
              @click="refreshTask"
            />
          </el-tooltip>
          <el-button
            v-if="canCancelTasks && cancellable"
            type="danger"
            plain
            :icon="CircleClose"
            :loading="state.cancelling"
            :disabled="state.cancelAccepted || Boolean(task?.cancelRequestedAt)"
            @click="cancelTask"
          >
            {{ state.cancelAccepted || task?.cancelRequestedAt ? "取消处理中" : "取消任务" }}
          </el-button>
        </div>
      </header>

      <el-alert
        v-if="state.error"
        class="page-alert"
        :title="state.error.title"
        type="error"
        :closable="false"
        show-icon
      >
        <div class="alert-content">
          <span>{{ state.error.description }}</span>
          <el-button size="small" @click="refreshTask">重试</el-button>
        </div>
      </el-alert>

      <div v-if="state.loading" class="loading-state">
        <el-skeleton :rows="10" animated />
      </div>

      <template v-else-if="task">
        <section class="status-band">
          <div class="status-main">
            <el-tag :type="taskStatusTagType(task.status)" size="large">
              {{ taskStatusLabel(task.status) }}
            </el-tag>
            <span v-if="isActiveTaskStatus(task.status)" class="poll-indicator">
              每 2.5 秒刷新
            </span>
          </div>
          <div class="status-meta">
            <span>{{ taskTypeLabel(task.taskType) }}</span>
            <span>{{ formatDuration(detailDurationMs) }}</span>
          </div>
        </section>

        <el-alert
          v-if="task.queueReason"
          class="page-alert"
          :title="queueReasonLabel(task.queueReason)"
          type="warning"
          :closable="false"
          show-icon
        />
        <el-alert
          v-if="task.dangerFullAccess === true"
          class="page-alert"
          title="本任务使用整机最高权限执行"
          description="Agent 在本机绕过 Codex 审批与沙箱；该权限来自 Agent 本机配置，不由管理后台切换。"
          type="error"
          :closable="false"
          show-icon
        />
        <el-alert
          v-if="task.cancelRequestedAt"
          class="page-alert"
          title="已请求取消任务"
          :description="task.cancelDeadlineAt
            ? `等待 Agent 确认，截止时间 ${formatDateTime(task.cancelDeadlineAt)}`
            : '取消请求正在处理。'"
          type="warning"
          :closable="false"
          show-icon
        />
        <el-alert
          v-if="task.errorCode || task.errorMessage"
          class="page-alert"
          :title="task.errorCode || '任务执行错误'"
          :description="task.errorMessage || '未提供错误详情。'"
          type="error"
          :closable="false"
          show-icon
        />

        <section class="detail-section">
          <h2>基本信息</h2>
          <dl class="metadata-grid">
            <div class="metadata-item wide">
              <dt>Request ID</dt>
              <dd class="copy-value">
                <span class="mono">{{ task.requestId }}</span>
                <CopyButton :value="task.requestId" label="复制 Request ID" @copy="copyText" />
              </dd>
            </div>
            <div class="metadata-item">
              <dt>Agent ID</dt>
              <dd class="mono">{{ formatValue(task.agentId) }}</dd>
            </div>
            <div class="metadata-item">
              <dt>Project ID</dt>
              <dd class="mono">{{ formatValue(task.projectId) }}</dd>
            </div>
            <div class="metadata-item">
              <dt>执行模式</dt>
              <dd>{{ formatValue(task.executionMode) }}</dd>
            </div>
            <div class="metadata-item">
              <dt>执行权限</dt>
              <dd>
                <el-tag :type="dangerFullAccessState(task.dangerFullAccess).tagType" size="small">
                  {{ dangerFullAccessState(task.dangerFullAccess).label }}
                </el-tag>
              </dd>
            </div>
            <div class="metadata-item">
              <dt>创建人</dt>
              <dd>{{ creatorLabel(task.createdBy) }}</dd>
            </div>
            <div class="metadata-item">
              <dt>Exit Code</dt>
              <dd class="mono">{{ formatValue(task.exitCode) }}</dd>
            </div>
          </dl>
        </section>

        <section class="detail-section">
          <h2>任务参数</h2>
          <dl class="path-list">
            <div>
              <dt>artifactFile</dt>
              <dd class="copy-value">
                <span class="mono path-value">{{ formatValue(task.artifactFile) }}</span>
                <CopyButton
                  v-if="task.artifactFile"
                  :value="task.artifactFile"
                  label="复制产物路径"
                  @copy="copyText"
                />
              </dd>
            </div>
            <div>
              <dt>consumerTargetFile</dt>
              <dd class="copy-value">
                <span class="mono path-value">{{ formatValue(task.consumerTargetFile) }}</span>
                <CopyButton
                  v-if="task.consumerTargetFile"
                  :value="task.consumerTargetFile"
                  label="复制消费端目标路径"
                  @copy="copyText"
                />
              </dd>
            </div>
            <div>
              <dt>rulesFile</dt>
              <dd class="copy-value">
                <span :class="task.rulesFile ? 'mono path-value' : 'path-value'">
                  {{ task.rulesFile ? task.rulesFile : "未提供（无规则执行）" }}
                </span>
                <CopyButton
                  v-if="task.rulesFile"
                  :value="task.rulesFile"
                  label="复制规则路径"
                  @copy="copyText"
                />
              </dd>
            </div>
          </dl>

          <div v-if="task.prompt" class="prompt-grid">
            <div class="prompt-item prompt-theme">
              <span class="field-label">主题</span>
              <span>{{ formatValue(task.prompt.theme) }}</span>
            </div>
            <div class="prompt-item">
              <span class="field-label">Primitive 上限</span>
              <span>{{ formatValue(task.prompt.primitiveLimit) }}</span>
            </div>
            <div class="prompt-item">
              <span class="field-label">Bounds</span>
              <span class="mono">{{ boundsLabel(task.prompt.bounds) }}</span>
            </div>
          </div>
          <div class="requirements-block">
            <span class="field-label">生成要求</span>
            <ol v-if="task.prompt?.requirements?.length">
              <li v-for="(requirement, index) in task.prompt.requirements" :key="index">
                {{ requirement }}
              </li>
            </ol>
            <span v-else>--</span>
          </div>
        </section>

        <section class="detail-section">
          <h2>命令与输出</h2>
          <div class="code-block-wrap">
            <div class="code-header">
              <span>Command Preview</span>
              <CopyButton
                v-if="task.commandPreview"
                :value="task.commandPreview"
                label="复制命令预览"
                @copy="copyText"
              />
            </div>
            <pre class="code-block">{{ formatValue(task.commandPreview) }}</pre>
          </div>

          <el-collapse v-model="expandedOutputs" class="output-collapse">
            <el-collapse-item name="stdout">
              <template #title>
                <span class="output-title">
                  stdout
                  <span>{{ outputSummary(task.stdoutBytes, task.stdoutTruncated) }}</span>
                </span>
              </template>
              <pre class="output-block">{{ formatOutput(task.stdoutPreview) }}</pre>
            </el-collapse-item>
            <el-collapse-item name="stderr">
              <template #title>
                <span class="output-title">
                  stderr
                  <span>{{ outputSummary(task.stderrBytes, task.stderrTruncated) }}</span>
                </span>
              </template>
              <pre class="output-block error-output">{{ formatOutput(task.stderrPreview) }}</pre>
            </el-collapse-item>
          </el-collapse>
        </section>

        <section class="detail-section result-grid">
          <div class="result-panel">
            <h2>Artifact</h2>
            <pre class="json-block">{{ formatJson(task.artifact) }}</pre>
          </div>
          <div class="result-panel">
            <h2>Audit</h2>
            <pre class="json-block">{{ formatJson(task.audit) }}</pre>
          </div>
        </section>

        <section class="detail-section">
          <h2>时间线</h2>
          <dl class="timeline-grid">
            <div v-for="entry in timeline" :key="entry.label" class="timeline-item">
              <dt>{{ entry.label }}</dt>
              <dd>{{ formatDateTime(entry.value) }}</dd>
            </div>
          </dl>
        </section>
      </template>
    </main>
  </AdminLayout>
</template>

<script setup>
import { computed, defineComponent, h, onBeforeUnmount, reactive, ref, watch } from "vue";
import { useRoute, useRouter } from "vue-router";
import { ElButton, ElMessage, ElMessageBox, ElTooltip } from "element-plus";
import { ArrowLeft, CircleClose, CopyDocument, Refresh } from "@element-plus/icons-vue";
import AdminLayout from "../components/AdminLayout.vue";
import { myforgeApi } from "../api";
import { normalizeMyforgeError } from "../api/myforge-errors";
import { ADMIN_PERMISSIONS as P } from "../auth/permissions";
import {
  dangerFullAccessState,
  formatDuration,
  formatJson,
  isActiveTaskStatus,
  queueReasonLabel,
  taskDurationMs,
  taskStatusLabel,
  taskStatusTagType,
  taskTypeLabel
} from "../myforge/task-utils";
import { useAuthStore } from "../stores/auth";

const CopyButton = defineComponent({
  name: "CopyButton",
  props: {
    value: { type: String, required: true },
    label: { type: String, required: true }
  },
  emits: ["copy"],
  setup(props, { emit }) {
    return () => h(ElTooltip, { content: props.label, placement: "top" }, {
      default: () => h(ElButton, {
        link: true,
        icon: CopyDocument,
        "aria-label": props.label,
        onClick: () => emit("copy", props.value)
      })
    });
  }
});

const route = useRoute();
const router = useRouter();
const authStore = useAuthStore();
const requestId = computed(() => String(route.params.requestId || ""));
const task = computed(() => state.task);
const canCancelTasks = computed(() => authStore.hasPermission(P.MYFORGE_TASK_CANCEL));
const cancellable = computed(() => isActiveTaskStatus(task.value?.status));
const detailDurationMs = computed(() => taskDurationMs(task.value));
const expandedOutputs = ref([]);
const state = reactive({
  loading: true,
  refreshing: false,
  cancelling: false,
  cancelAccepted: false,
  task: null,
  error: null
});
let destroyed = false;
let loadGeneration = 0;
let pollTimer = null;
let activeCancelAttempt = null;
const activeLoads = new Set();

const timeline = computed(() => [
  { label: "创建", value: task.value?.createdAt },
  { label: "下发", value: task.value?.dispatchedAt },
  { label: "开始", value: task.value?.startedAt },
  { label: "请求取消", value: task.value?.cancelRequestedAt },
  { label: "取消截止", value: task.value?.cancelDeadlineAt },
  { label: "完成", value: task.value?.completedAt }
]);

function clearPoll() {
  if (pollTimer !== null) {
    clearTimeout(pollTimer);
    pollTimer = null;
  }
}

function schedulePoll(generation) {
  clearPoll();
  if (destroyed || generation !== loadGeneration || !isActiveTaskStatus(state.task?.status)) return;
  pollTimer = setTimeout(() => {
    pollTimer = null;
    void loadTask({ generation, initial: false });
  }, 2500);
}

async function loadTask({ generation = loadGeneration, initial = false } = {}) {
  if (destroyed || generation !== loadGeneration || activeLoads.has(generation)) return;
  clearPoll();
  const id = requestId.value;
  activeLoads.add(generation);
  if (initial || !state.task) state.loading = true;
  else state.refreshing = true;
  state.error = null;
  try {
    const { data } = await myforgeApi.getTask(id);
    if (destroyed || generation !== loadGeneration || id !== requestId.value) return;
    state.task = data?.task || null;
    if (state.task?.errorCode === "MYFORGE_TARGET_FILE_MISSING" && expandedOutputs.value.length === 0) {
      expandedOutputs.value = ["stdout", "stderr"];
    }
    if (!state.task) {
      state.error = {
        title: "任务详情为空",
        description: "管理接口未返回任务详情，请稍后重试。"
      };
    }
  } catch (error) {
    if (destroyed || generation !== loadGeneration || id !== requestId.value) return;
    const notFound = error?.response?.status === 404 ||
      error?.response?.data?.error === "MYFORGE_TASK_NOT_FOUND";
    if (notFound) state.task = null;
    state.error = normalizeMyforgeError(error, {
      taskDetailLookup: true,
      notFoundMessage: "未找到该任务，请从任务列表重新选择。",
      title: "任务详情加载失败",
      fallbackMessage: "无法加载 MyForge 任务详情，请稍后重试。"
    });
  } finally {
    activeLoads.delete(generation);
    if (!destroyed && generation === loadGeneration && id === requestId.value) {
      state.loading = false;
      state.refreshing = false;
      schedulePoll(generation);
    }
  }
}

function resetForRoute() {
  loadGeneration += 1;
  activeCancelAttempt = null;
  clearPoll();
  state.loading = true;
  state.refreshing = false;
  state.cancelling = false;
  state.cancelAccepted = false;
  state.task = null;
  state.error = null;
  expandedOutputs.value = [];
  void loadTask({ generation: loadGeneration, initial: true });
}

function refreshTask() {
  if (state.loading || state.refreshing) return;
  void loadTask({ generation: loadGeneration, initial: false });
}

async function cancelTask() {
  if (!canCancelTasks.value || !cancellable.value || activeCancelAttempt ||
      state.cancelAccepted || task.value?.cancelRequestedAt) return;
  const attempt = {
    requestId: requestId.value,
    generation: loadGeneration
  };
  activeCancelAttempt = attempt;
  state.cancelling = true;
  try {
    try {
      await ElMessageBox.confirm(
        `确认取消任务 ${attempt.requestId}？`,
        "取消 MyForge 任务",
        {
          confirmButtonText: "确认取消",
          cancelButtonText: "返回",
          type: "warning"
        }
      );
    } catch {
      return;
    }

    if (!cancelAttemptIsCurrent(attempt) || !cancellable.value || task.value?.cancelRequestedAt) return;
    const { data } = await myforgeApi.cancelTask(attempt.requestId, {});
    if (!cancelAttemptIsCurrent(attempt)) return;
    state.cancelAccepted = true;
    const suffix = data?.cancelRequested && data?.status === "running" ? "，等待 Agent 确认" : "";
    ElMessage.success(`取消请求已受理：${taskStatusLabel(data?.status)}${suffix}`);
    await loadTask({ generation: attempt.generation, initial: false });
  } catch (error) {
    if (!cancelAttemptIsCurrent(attempt)) return;
    const normalized = normalizeMyforgeError(error, {
      taskDetailLookup: true,
      title: "取消任务失败",
      fallbackMessage: "取消请求未能完成，请刷新任务状态后重试。"
    });
    ElMessage.error(`${normalized.title}：${normalized.description}`);
    schedulePoll(attempt.generation);
  } finally {
    releaseCancelAttempt(attempt);
  }
}

function cancelAttemptIsCurrent(attempt) {
  return !destroyed && activeCancelAttempt === attempt &&
    attempt.generation === loadGeneration && attempt.requestId === requestId.value;
}

function releaseCancelAttempt(attempt) {
  if (activeCancelAttempt !== attempt) return;
  activeCancelAttempt = null;
  if (!destroyed && attempt.generation === loadGeneration && attempt.requestId === requestId.value) {
    state.cancelling = false;
  }
}

function goBack() {
  void router.push({ name: "MyForge" });
}

async function copyText(value) {
  try {
    await navigator.clipboard.writeText(value);
    ElMessage.success("已复制");
  } catch {
    const textarea = document.createElement("textarea");
    textarea.value = value;
    textarea.style.position = "fixed";
    textarea.style.opacity = "0";
    document.body.appendChild(textarea);
    textarea.select();
    const copied = document.execCommand("copy");
    textarea.remove();
    copied ? ElMessage.success("已复制") : ElMessage.error("复制失败");
  }
}

function formatValue(value) {
  return value === null || value === undefined || value === "" ? "--" : String(value);
}

function formatDateTime(value) {
  if (!value) return "--";
  const date = new Date(value);
  if (!Number.isFinite(date.getTime())) return "--";
  return date.toLocaleString("zh-CN", { hour12: false });
}

function creatorLabel(createdBy) {
  if (!createdBy?.username && !createdBy?.adminId) return "--";
  if (createdBy.username && createdBy.adminId) return `${createdBy.username} (${createdBy.adminId})`;
  return createdBy.username || createdBy.adminId;
}

function boundsLabel(bounds) {
  if (!bounds) return "--";
  return `${formatValue(bounds.width)} × ${formatValue(bounds.depth)} × ${formatValue(bounds.height)}`;
}

function formatBytes(bytes) {
  if (!Number.isFinite(bytes) || bytes < 0) return "未知大小";
  if (bytes < 1024) return `${bytes} B`;
  return `${(bytes / 1024).toFixed(1)} KiB`;
}

function outputSummary(bytes, truncated) {
  return `${formatBytes(bytes)}${truncated ? " · 已截断" : ""}`;
}

function formatOutput(value) {
  return value === null || value === undefined || value === "" ? "（无输出）" : String(value);
}

watch(requestId, resetForRoute, { immediate: true });

onBeforeUnmount(() => {
  destroyed = true;
  loadGeneration += 1;
  activeCancelAttempt = null;
  clearPoll();
});
</script>

<style scoped>
.detail-page {
  min-width: 0;
  padding: 20px 24px 32px;
  background: #fff;
  border: 1px solid #e4e7ed;
}

.page-header,
.title-group,
.header-actions,
.status-band,
.status-main,
.status-meta,
.copy-value,
.code-header,
.alert-content {
  display: flex;
  align-items: center;
}

.page-header {
  justify-content: space-between;
  gap: 16px;
  margin-bottom: 18px;
}

.title-group {
  gap: 12px;
  min-width: 0;
}

.title-group > div {
  min-width: 0;
}

.title-group h1 {
  margin: 0 0 3px;
  font-size: 20px;
  letter-spacing: 0;
}

.request-id {
  display: block;
  color: #909399;
  font-size: 12px;
  overflow-wrap: anywhere;
}

.header-actions {
  gap: 8px;
  flex: 0 0 auto;
}

.loading-state {
  padding: 24px 8px;
}

.page-alert {
  margin-bottom: 14px;
}

.alert-content {
  justify-content: space-between;
  gap: 12px;
  width: 100%;
}

.status-band {
  justify-content: space-between;
  gap: 16px;
  padding: 12px 0 16px;
  border-bottom: 1px solid #e4e7ed;
}

.status-main,
.status-meta {
  gap: 12px;
}

.status-meta {
  color: #606266;
  font-size: 13px;
}

.status-meta span + span::before {
  content: "·";
  margin-right: 12px;
  color: #c0c4cc;
}

.poll-indicator {
  color: #909399;
  font-size: 12px;
}

.detail-section {
  min-width: 0;
  padding: 20px 0;
  border-bottom: 1px solid #e4e7ed;
}

.detail-section:last-child {
  border-bottom: 0;
}

.detail-section h2 {
  margin: 0 0 14px;
  font-size: 15px;
  letter-spacing: 0;
}

.metadata-grid,
.timeline-grid,
.path-list {
  display: grid;
  margin: 0;
}

.metadata-grid {
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 1px;
  background: #ebeef5;
  border: 1px solid #ebeef5;
}

.metadata-item {
  min-width: 0;
  padding: 10px 12px;
  background: #fff;
}

.metadata-item.wide {
  grid-column: span 2;
}

dt,
.field-label {
  color: #909399;
  font-size: 12px;
}

dd {
  margin: 5px 0 0;
  color: #303133;
  overflow-wrap: anywhere;
}

.copy-value {
  justify-content: space-between;
  gap: 8px;
  min-width: 0;
}

.copy-value > span {
  min-width: 0;
  overflow-wrap: anywhere;
}

.path-list {
  grid-template-columns: minmax(0, 1fr);
  gap: 1px;
  margin-bottom: 14px;
  background: #ebeef5;
  border: 1px solid #ebeef5;
}

.path-list > div {
  display: grid;
  grid-template-columns: 180px minmax(0, 1fr);
  min-width: 0;
  padding: 9px 12px;
  background: #fff;
}

.path-list dd {
  margin: 0;
}

.prompt-grid {
  display: grid;
  grid-template-columns: minmax(0, 2fr) minmax(150px, 1fr) minmax(180px, 1fr);
  gap: 12px;
  margin-bottom: 14px;
}

.prompt-item {
  display: flex;
  flex-direction: column;
  gap: 5px;
  min-width: 0;
  padding: 10px 12px;
  background: #f5f7fa;
  border: 1px solid #ebeef5;
}

.requirements-block {
  padding: 10px 12px;
  background: #f5f7fa;
  border: 1px solid #ebeef5;
}

.requirements-block ol {
  margin: 8px 0 0 22px;
}

.requirements-block li {
  margin: 4px 0;
  padding-left: 3px;
  overflow-wrap: anywhere;
}

.code-block-wrap {
  border: 1px solid #dcdfe6;
}

.code-header {
  justify-content: space-between;
  min-height: 36px;
  padding: 0 12px;
  background: #f5f7fa;
  border-bottom: 1px solid #dcdfe6;
  color: #606266;
  font-size: 12px;
  font-weight: 600;
}

.code-block,
.output-block,
.json-block {
  margin: 0;
  font: 12px/1.6 ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", monospace;
  white-space: pre-wrap;
  overflow-wrap: anywhere;
}

.code-block {
  padding: 12px;
  background: #fff;
}

.output-collapse {
  margin-top: 14px;
  border: 1px solid #dcdfe6;
  border-bottom: 0;
}

.output-collapse :deep(.el-collapse-item__header) {
  padding: 0 12px;
}

.output-collapse :deep(.el-collapse-item__content) {
  padding: 0;
}

.output-title {
  display: flex;
  align-items: center;
  gap: 10px;
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", monospace;
}

.output-title span {
  color: #909399;
  font-family: inherit;
  font-size: 12px;
}

.output-block {
  max-height: 480px;
  overflow: auto;
  padding: 12px;
  background: #fafafa;
  color: #303133;
}

.error-output {
  color: #8f3c3c;
}

.result-grid {
  display: grid;
  grid-template-columns: repeat(2, minmax(0, 1fr));
  gap: 16px;
}

.result-panel {
  min-width: 0;
}

.json-block {
  max-height: 520px;
  overflow: auto;
  padding: 12px;
  background: #f5f7fa;
  border: 1px solid #e4e7ed;
  color: #303133;
}

.timeline-grid {
  grid-template-columns: repeat(3, minmax(0, 1fr));
  gap: 12px;
}

.timeline-item {
  padding-left: 10px;
  border-left: 2px solid #dcdfe6;
}

.mono {
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", monospace;
}

@media (max-width: 900px) {
  .detail-page {
    padding: 16px;
  }

  .metadata-grid,
  .timeline-grid {
    grid-template-columns: repeat(2, minmax(0, 1fr));
  }

  .metadata-item.wide {
    grid-column: span 2;
  }

  .prompt-grid,
  .result-grid {
    grid-template-columns: minmax(0, 1fr);
  }
}

@media (max-width: 600px) {
  .page-header,
  .status-band {
    align-items: flex-start;
  }

  .page-header {
    flex-wrap: wrap;
  }

  .page-header h1 {
    font-size: 18px;
  }

  .header-actions {
    justify-content: flex-end;
    width: 100%;
  }

  .status-band,
  .metadata-grid,
  .timeline-grid {
    grid-template-columns: minmax(0, 1fr);
  }

  .status-band {
    display: flex;
    flex-direction: column;
  }

  .metadata-item.wide {
    grid-column: span 1;
  }

  .path-list > div {
    grid-template-columns: minmax(0, 1fr);
    gap: 5px;
  }
}
</style>
