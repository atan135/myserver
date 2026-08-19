<template>
  <AdminLayout>
    <div class="approval-page">
      <div class="page-header">
        <div>
          <h2>高风险操作审批</h2>
          <p>审批仅确认摘要与影响；服务端仍会在原申请人使用原预检凭据执行时重新校验身份、范围和状态。</p>
        </div>
        <el-button :loading="loading" @click="refresh">刷新</el-button>
      </div>

      <el-alert v-if="loadError" type="error" :title="loadError" :closable="false" show-icon class="page-alert" />

      <div class="approval-layout">
        <el-card class="operation-list" v-loading="loading && !operations.length">
          <template #header>
            <div class="card-header">
              <span>待批准操作</span>
              <el-tag type="warning" size="small">{{ operations.length }}</el-tag>
            </div>
          </template>
          <el-table
            v-if="operations.length"
            :data="operations"
            :row-class-name="rowClass"
            @row-click="selectOperation"
          >
            <el-table-column prop="requestId" label="请求 ID" min-width="170" show-overflow-tooltip />
            <el-table-column prop="permissionKey" label="权限" min-width="150" />
            <el-table-column label="申请人" min-width="110">
              <template #default="{ row }">{{ row.requester?.subject || '--' }}</template>
            </el-table-column>
            <el-table-column label="状态" width="92">
              <template #default="{ row }">
                <el-tag :type="approvalStatusType(row.approvalStatus)" size="small">{{ row.approvalStatus }}</el-tag>
              </template>
            </el-table-column>
          </el-table>
          <el-empty v-else description="暂无待批准操作" />
        </el-card>

        <el-card class="operation-detail" v-loading="detailLoading">
          <template #header>
            <div class="card-header">
              <span>操作详情</span>
              <el-tag v-if="detail" :type="operationStatusType(detail.status)" size="small">{{ detail.status }}</el-tag>
            </div>
          </template>

          <el-empty v-if="!detail" description="从待批准列表中选择操作" />
          <template v-else>
            <el-descriptions :column="1" border class="detail-descriptions">
              <el-descriptions-item label="请求 ID">{{ detail.requestId }}</el-descriptions-item>
              <el-descriptions-item label="申请人">{{ detail.requester?.subject || '--' }}</el-descriptions-item>
              <el-descriptions-item label="权限">{{ detail.permissionKey }}</el-descriptions-item>
              <el-descriptions-item label="风险级别">{{ detail.riskLevel }}</el-descriptions-item>
              <el-descriptions-item label="申请原因">{{ detail.reason || '--' }}</el-descriptions-item>
              <el-descriptions-item label="目标摘要"><pre>{{ formatSummary(detail.targetSummary) }}</pre></el-descriptions-item>
              <el-descriptions-item label="影响摘要"><pre>{{ formatSummary(detail.impactSummary) }}</pre></el-descriptions-item>
              <el-descriptions-item label="预检有效期">{{ formatTime(detail.preview?.expiresAt) }}</el-descriptions-item>
            </el-descriptions>

            <el-alert
              v-if="selfApproval"
              type="warning"
              title="申请人不能审批自己的操作"
              :closable="false"
              show-icon
              class="detail-alert"
            />
            <el-alert
              v-else-if="detail.approvalStatus !== 'pending'"
              type="info"
              title="该操作已不处于待审批状态，请以审计事件为准。"
              :closable="false"
              show-icon
              class="detail-alert"
            />

            <el-form v-if="canDecide" label-position="top" class="decision-form">
              <el-form-item label="审批证据摘要" required>
                <el-input
                  v-model="evidence"
                  type="textarea"
                  :rows="3"
                  maxlength="512"
                  show-word-limit
                  placeholder="例如：已核对变更单、排空指标和回退窗口"
                  :disabled="decisionLoading"
                />
                <div v-if="evidence && !evidenceSummary" class="validation-error">证据摘要不能为空，且不得包含令牌、密码或其他凭据。</div>
              </el-form-item>
              <el-form-item label="拒绝原因">
                <el-input
                  v-model="rejection"
                  type="textarea"
                  :rows="2"
                  maxlength="512"
                  show-word-limit
                  placeholder="拒绝时必填"
                  :disabled="decisionLoading"
                />
              </el-form-item>
              <div class="decision-actions">
                <el-button
                  type="success"
                  :loading="decisionLoading && decisionType === 'approved'"
                  :disabled="decisionLoading || !canApprove"
                  @click="decide('approved')"
                >批准</el-button>
                <el-button
                  type="danger"
                  plain
                  :loading="decisionLoading && decisionType === 'rejected'"
                  :disabled="decisionLoading || !canReject"
                  @click="decide('rejected')"
                >拒绝</el-button>
              </div>
            </el-form>

            <section class="audit-section">
              <div class="section-heading">关联审计事件</div>
              <el-alert
                v-if="auditUnavailable"
                type="info"
                title="当前账号没有审计读取权限，无法显示关联事件。"
                :closable="false"
                show-icon
              />
              <el-timeline v-else-if="events.length" class="event-timeline">
                <el-timeline-item v-for="event in events" :key="event.id" :timestamp="formatTime(event.createdAt)">
                  <strong>{{ event.eventType }}</strong>
                  <span class="event-result">{{ event.result || '--' }}</span>
                </el-timeline-item>
              </el-timeline>
              <el-empty v-else-if="!auditLoading" description="暂无关联审计事件" :image-size="56" />
            </section>
          </template>
        </el-card>
      </div>
    </div>
  </AdminLayout>
</template>

<script setup>
import { computed, onMounted, ref } from "vue";
import { ElMessage, ElMessageBox } from "element-plus";
import AdminLayout from "../../components/AdminLayout.vue";
import { adminOperationApi, auditApi } from "../../api";
import { ADMIN_PERMISSIONS as P } from "../../auth/permissions";
import { useAuthStore } from "../../stores/auth";
import {
  approvalEvidenceSummary,
  approvalDecisionPayload,
  approvalStatusType,
  canDecideApproval,
  isSelfApproval,
  operationStatusType
} from "../../operations/operation-approval";

const authStore = useAuthStore();
const operations = ref([]);
const detail = ref(null);
const events = ref([]);
const loading = ref(false);
const detailLoading = ref(false);
const auditLoading = ref(false);
const auditUnavailable = ref(false);
const loadError = ref("");
const evidence = ref("");
const rejection = ref("");
const decisionLoading = ref(false);
const decisionType = ref("");

const evidenceSummary = computed(() => approvalEvidenceSummary(evidence.value));
const selfApproval = computed(() => isSelfApproval(detail.value, authStore.user?.id));
const canDecide = computed(() => detail.value?.approvalStatus === "pending" && !selfApproval.value);
const canApprove = computed(() => canDecideApproval(detail.value, authStore.user?.id, evidence.value));
const canReject = computed(() => canDecideApproval(detail.value, authStore.user?.id, evidence.value, rejection.value, "rejected"));

function formatTime(value) {
  return value ? new Date(value).toLocaleString("zh-CN") : "--";
}

function formatSummary(value) {
  return JSON.stringify(value && typeof value === "object" ? value : {}, null, 2);
}

function rowClass({ row }) {
  return row.requestId === detail.value?.requestId ? "selected-row" : "";
}

async function loadAudit(requestId) {
  events.value = [];
  auditUnavailable.value = !authStore.hasPermission(P.AUDIT_READ);
  if (auditUnavailable.value) return;
  auditLoading.value = true;
  try {
    const { data } = await auditApi.getOperationEvents({
      request_id: requestId,
      from: new Date(Date.now() - 7 * 24 * 60 * 60 * 1000).toISOString(),
      to: new Date().toISOString(),
      limit: 100
    });
    events.value = Array.isArray(data?.events) ? data.events : [];
  } catch (error) {
    if (error?.response?.status === 403) {
      auditUnavailable.value = true;
      return;
    }
    ElMessage.warning("无法加载关联审计事件");
  } finally {
    auditLoading.value = false;
  }
}

async function selectOperation(row) {
  if (!row?.requestId) return;
  detailLoading.value = true;
  evidence.value = "";
  rejection.value = "";
  try {
    const { data } = await adminOperationApi.get(row.requestId);
    detail.value = data?.operation || null;
    if (detail.value?.requestId) await loadAudit(detail.value.requestId);
  } catch (error) {
    detail.value = null;
    ElMessage.error(error?.response?.data?.message || "无法加载操作详情");
  } finally {
    detailLoading.value = false;
  }
}

async function refresh() {
  loading.value = true;
  loadError.value = "";
  try {
    const { data } = await adminOperationApi.getPendingApprovals({ limit: 100 });
    operations.value = Array.isArray(data?.operations) ? data.operations : [];
    const selected = operations.value.find((item) => item.requestId === detail.value?.requestId) || operations.value[0];
    if (selected) await selectOperation(selected);
    else {
      detail.value = null;
      events.value = [];
    }
  } catch (error) {
    loadError.value = error?.response?.data?.message || "无法加载待批准操作。";
  } finally {
    loading.value = false;
  }
}

async function decide(status) {
  if (!detail.value || decisionLoading.value) return;
  if (status === "approved" && !canApprove.value) return;
  if (status === "rejected" && !canReject.value) return;
  const payload = approvalDecisionPayload(status, evidence.value, rejection.value);
  if (!payload) return;
  decisionLoading.value = true;
  decisionType.value = status;
  try {
    await ElMessageBox.confirm(
      status === "approved" ? "确认批准这项高风险操作？" : "确认拒绝这项高风险操作？",
      status === "approved" ? "批准确认" : "拒绝确认",
      { type: status === "approved" ? "warning" : "error", confirmButtonText: "确认", cancelButtonText: "取消" }
    );
    if (status === "approved") {
      await adminOperationApi.approve(detail.value.requestId, payload.evidenceSummary);
    } else {
      await adminOperationApi.reject(detail.value.requestId, payload.rejectionReason, payload.evidenceSummary);
    }
    ElMessage.success(status === "approved" ? "已批准操作" : "已拒绝操作");
    await refresh();
  } catch (error) {
    if (error !== "cancel" && error !== "close") {
      ElMessage.error(error?.response?.data?.message || "审批操作失败");
    }
  } finally {
    decisionLoading.value = false;
    decisionType.value = "";
  }
}

onMounted(refresh);
</script>

<style scoped>
.approval-page { max-width: 1320px; margin: 0 auto; padding: 24px; }
.page-header, .card-header, .decision-actions { display: flex; align-items: center; }
.page-header { justify-content: space-between; gap: 16px; margin-bottom: 18px; }
.page-header h2 { margin: 0 0 6px; font-size: 22px; }
.page-header p { color: #606266; font-size: 13px; line-height: 1.6; }
.page-alert { margin-bottom: 16px; }
.approval-layout { display: grid; grid-template-columns: minmax(360px, .9fr) minmax(0, 1.4fr); gap: 16px; align-items: start; }
.card-header { justify-content: space-between; gap: 12px; font-weight: 600; }
.operation-list :deep(.selected-row > td) { background: #ecf5ff; }
.operation-list :deep(.el-table__row) { cursor: pointer; }
.detail-descriptions pre { margin: 0; white-space: pre-wrap; overflow-wrap: anywhere; font: 12px/1.45 ui-monospace, SFMono-Regular, Menlo, monospace; }
.detail-alert, .decision-form, .audit-section { margin-top: 16px; }
.decision-actions { gap: 10px; flex-wrap: wrap; }
.validation-error { margin-top: 6px; color: #f56c6c; font-size: 12px; line-height: 1.4; }
.section-heading { margin-bottom: 10px; font-weight: 600; }
.event-timeline { padding-left: 6px; }
.event-result { margin-left: 8px; color: #909399; font-size: 12px; }
@media (max-width: 900px) {
  .approval-page { padding: 12px; }
  .page-header { align-items: flex-start; flex-direction: column; }
  .approval-layout { grid-template-columns: 1fr; }
  .operation-list { overflow-x: auto; }
}
</style>
