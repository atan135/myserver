<template>
  <AdminLayout>
    <section class="activity-page">
      <div class="page-heading">
        <div>
          <h3>活动运营</h3>
          <p class="muted">管理活动草稿、版本和发布状态</p>
        </div>
        <el-button type="primary" @click="openCreate">新建活动</el-button>
      </div>

      <el-form class="filters" :inline="true" @submit.prevent="search">
        <el-form-item label="状态"><el-select v-model="filters.status" clearable placeholder="全部" @change="search"><el-option label="草稿" value="draft" /><el-option label="已发布" value="published" /><el-option label="已下线" value="offline" /></el-select></el-form-item>
        <el-form-item label="类型"><el-select v-model="filters.activityType" clearable placeholder="全部" @change="search"><el-option v-for="definition in typeDefinitions" :key="definition.type" :label="definition.label" :value="definition.type" /></el-select></el-form-item>
        <el-form-item label="活动标识"><el-input v-model="filters.key" clearable @keyup.enter="search" /></el-form-item>
        <el-form-item label="活动时间"><el-date-picker v-model="dateRange" type="daterange" value-format="YYYY-MM-DD" start-placeholder="开始日期" end-placeholder="结束日期" clearable @change="search" /></el-form-item>
        <el-form-item><el-button :loading="loading" @click="search">查询</el-button><el-button @click="resetFilters">重置</el-button></el-form-item>
      </el-form>

      <el-alert v-if="loadError" type="error" :title="loadError.message" show-icon closable @close="loadError = null" />
      <el-table :data="visibleItems" v-loading="loading" stripe empty-text="暂无活动">
        <el-table-column prop="key" label="活动标识" min-width="160" />
        <el-table-column label="类型" width="120"><template #default="{ row }">{{ typeLabel(row.activityType) }}</template></el-table-column>
        <el-table-column label="状态" width="100"><template #default="{ row }"><el-tag :type="statusTag(row.status)">{{ statusLabel(row.status) }}</el-tag></template></el-table-column>
        <el-table-column label="版本" width="90"><template #default="{ row }">v{{ row.version ?? "-" }}</template></el-table-column>
        <el-table-column label="活动时间" min-width="220"><template #default="{ row }">{{ formatWindow(row) }}</template></el-table-column>
        <el-table-column label="发布时间" min-width="160"><template #default="{ row }">{{ formatTime(row.publishedAt || row.published_at) }}</template></el-table-column>
        <el-table-column prop="publisher" label="发布人" width="120"><template #default="{ row }">{{ row.publisher || row.publishedBy || "-" }}</template></el-table-column>
        <el-table-column prop="offlineReason" label="下线原因" min-width="180"><template #default="{ row }">{{ row.offlineReason || "-" }}</template></el-table-column>
        <el-table-column label="操作" fixed="right" width="250"><template #default="{ row }"><el-button link type="primary" @click="openDetail(row)">查看/编辑</el-button><el-button v-if="row.status === 'published'" link type="warning" @click="takeOffline(row)">下线</el-button><el-button v-if="row.status === 'draft'" link type="success" @click="publish(row)">发布</el-button></template></el-table-column>
      </el-table>
      <el-pagination v-model:current-page="pagination.page" v-model:page-size="pagination.limit" :total="pagination.total" :page-sizes="[20, 50, 100]" layout="total, sizes, prev, pager, next" class="pagination" @size-change="search" @current-change="search" />
    </section>

    <el-drawer v-model="drawer.open" :title="drawer.title" size="min(720px, 92vw)" destroy-on-close :before-close="guardDrawerClose">
      <el-alert v-if="drawer.error" type="error" :title="drawer.error.message" show-icon />
      <el-skeleton v-if="drawer.loading" :rows="8" animated />
      <template v-else-if="drawer.detail">
        <el-descriptions :column="2" border class="summary">
          <el-descriptions-item label="活动标识">{{ drawer.detail.key }}</el-descriptions-item><el-descriptions-item label="版本">v{{ drawer.detail.version }}</el-descriptions-item>
          <el-descriptions-item label="类型">{{ typeLabel(drawer.detail.activityType) }}</el-descriptions-item><el-descriptions-item label="状态">{{ statusLabel(drawer.detail.status) }}</el-descriptions-item>
          <el-descriptions-item label="配置摘要" :span="2"><code>{{ configSummary(drawer.detail) }}</code></el-descriptions-item>
        </el-descriptions>
        <el-form ref="draftForm" :model="draft" label-width="100px" class="draft-form">
          <el-form-item label="标题"><el-input v-model="draft.publicConfig.title" :disabled="!isDraft" /></el-form-item>
          <el-form-item label="资源"><el-input v-model="resourcesText" type="textarea" :rows="2" :disabled="!isDraft" placeholder="JSON 数组或对象" @change="syncResources" /></el-form-item>
          <el-form-item label="开始时间"><el-input v-model="draft.startAt" :disabled="!isDraft" /></el-form-item><el-form-item label="结束时间"><el-input v-model="draft.endAt" :disabled="!isDraft" /></el-form-item>
          <el-form-item label="领取截止"><el-input v-model="draft.claimDeadline" :disabled="!isDraft" /></el-form-item><el-form-item label="时区"><el-input v-model="draft.timezone" :disabled="!isDraft" /></el-form-item>
          <el-form-item label="领取方式"><el-select v-model="draft.publicConfig.claimMode" clearable :disabled="!isDraft"><el-option label="手动" value="manual" /><el-option label="自动" value="automatic" /></el-select></el-form-item>
          <el-form-item label="状态"><el-tag :type="statusTag(drawer.detail.status)">{{ statusLabel(drawer.detail.status) }}</el-tag></el-form-item>
          <el-form-item label="修改原因" required><el-input v-model="reason" type="textarea" :rows="2" /></el-form-item>
        </el-form>
        <div class="editor-section"><h4>类型配置</h4><el-skeleton v-if="typeEditorLoading" :rows="3" animated /><el-alert v-else-if="typeEditorError" type="error" :title="typeEditorError" show-icon /><component v-else-if="typeEditor" :is="typeEditor" v-model="draft.typeConfig" :stages="draft.stages" /></div>
        <ActivityStructureEditor :stages="draft.stages" :reward-groups="draft.rewardGroups" @update:stages="updateStages" @update:reward-groups="updateRewardGroups" />
        <div class="drawer-actions"><el-button :disabled="!isDraft" :loading="drawer.saving" @click="saveDraft">保存草稿</el-button><el-button type="primary" :loading="preflight.loading" @click="runPreflight">预检</el-button><el-button v-if="isDraft" type="success" :loading="drawer.publishing" @click="publish(drawer.detail)">发布</el-button><el-button v-if="drawer.detail.status === 'published'" type="warning" :loading="drawer.offlining" @click="takeOffline(drawer.detail)">下线</el-button></div>
        <section class="preflight-panel"><div class="subheading"><h4>发布预检</h4><el-button v-if="preflight.error" link type="primary" @click="runPreflight">重试</el-button></div><el-alert v-if="preflight.error" type="error" :title="preflight.error.message" show-icon /><el-alert v-else-if="preflight.result" :type="preflight.result.valid ? 'success' : 'error'" :title="preflight.result.valid ? '预检通过，可以发布' : `预检发现 ${preflight.result.errors.length} 个问题`" show-icon /><el-empty v-if="preflight.result && !preflight.result.valid && !preflight.result.errors.length" description="服务端未返回具体错误" :image-size="48" /><div v-if="preflight.result && preflight.result.errors.length" class="preflight-errors"><div v-for="(item, index) in preflight.result.errors" :key="`${item.path}-${index}`" class="preflight-error"><code>{{ item.path || "活动配置" }}</code><el-tag size="small" type="danger">{{ item.code || "INVALID" }}</el-tag><span>{{ item.message || "配置不符合要求" }}</span><span class="suggestion">建议：{{ item.suggestion || item.fix || "请根据字段要求修正后重试" }}</span></div></div></section>
        <section class="records-panel"><div class="subheading"><h4>记录与审计</h4><el-button link type="primary" :loading="records.loading" @click="loadRecords">刷新</el-button></div><el-form :inline="true" class="records-filters" @submit.prevent="loadRecords"><el-input v-model="records.filters.status" placeholder="状态" clearable /><el-input v-model="records.filters.characterId" placeholder="角色 ID" clearable /><el-input v-model="records.filters.requestId" placeholder="请求 ID" clearable /><el-input-number v-model="records.filters.version" :min="1" controls-position="right" placeholder="版本" /><el-date-picker v-model="recordsRange" type="datetimerange" value-format="YYYY-MM-DDTHH:mm:ss[Z]" start-placeholder="开始" end-placeholder="结束" clearable @change="syncRecordDates" /><el-button native-type="submit" :loading="records.loading">查询</el-button></el-form><el-alert v-if="records.error" type="error" :title="records.error.message" show-icon /><el-table v-else :data="records.items" v-loading="records.loading" size="small" empty-text="暂无记录"><el-table-column prop="recordType" label="类型" width="90" /><el-table-column label="状态" width="110"><template #default="{ row }"><el-tag :type="recordStatusTag(row.status)" size="small">{{ recordStatusLabel(row.status) }}</el-tag></template></el-table-column><el-table-column prop="characterId" label="角色" min-width="120" /><el-table-column prop="requestId" label="请求" min-width="140" /><el-table-column prop="createdAt" label="时间" min-width="160"><template #default="{ row }">{{ formatTime(row.createdAt) }}</template></el-table-column><el-table-column label="详情" min-width="180"><template #default="{ row }"><code>{{ JSON.stringify(row.details || {}) }}</code></template></el-table-column></el-table><el-pagination v-model:current-page="records.page" v-model:page-size="records.limit" :total="records.total" :page-sizes="[20, 50]" layout="total, sizes, prev, pager, next" small @size-change="loadRecords" @current-change="loadRecords" /></section>
      </template>
    </el-drawer>

    <el-dialog v-model="create.open" title="新建活动" width="min(620px, 92vw)">
      <el-form :model="create.form" label-width="100px"><el-form-item label="活动标识" required><el-input v-model="create.form.key" /></el-form-item><el-form-item label="活动类型" required><el-select v-model="create.form.activityType"><el-option v-for="definition in typeDefinitions" :key="definition.type" :label="definition.label" :value="definition.type" /></el-select></el-form-item><el-form-item label="修改原因" required><el-input v-model="create.form.reason" type="textarea" /></el-form-item></el-form>
      <template #footer><el-button @click="create.open = false">取消</el-button><el-button type="primary" :loading="create.loading" @click="createDraft">创建草稿</el-button></template>
    </el-dialog>
  </AdminLayout>
</template>

<script setup>
import { computed, onMounted, reactive, ref } from "vue";
import { ElMessage, ElMessageBox } from "element-plus";
import AdminLayout from "../components/AdminLayout.vue";
import ActivityStructureEditor from "../modules/activity/ActivityStructureEditor.vue";
import { activityApi } from "../api";
import { activityError, buildVersionCommand, filterActivities, normalizeActivityListResponse } from "../api/activity.js";
import { normalizePreflightResponse, normalizeRecordsResponse, preflightErrorSuggestions, recordStatusLabel, recordStatusTag } from "../api/activity-records.js";
import { buildActivityDraftTemplate, listActivityTypeDefinitions, loadActivityTypeEditor, resolveActivityTypeDefinition } from "../modules/activity/type-registry.js";

const loading = ref(false); const loadError = ref(null); const items = ref([]); const dateRange = ref([]);
const filters = reactive({ status: "", activityType: "", key: "", from: "", to: "" });
const pagination = reactive({ page: 1, limit: 20, total: 0 });
const drawer = reactive({ open: false, title: "活动详情", loading: false, saving: false, publishing: false, offlining: false, detail: null, error: null });
const draft = reactive({}); const savedDraftSnapshot = ref(""); const reason = ref(""); const create = reactive({ open: false, loading: false, form: { key: "", activityType: listActivityTypeDefinitions()[0]?.type || "", reason: "" } });
const typeEditor = ref(null); const typeEditorLoading = ref(false); const typeEditorError = ref(""); const resourcesText = ref("");
const preflight = reactive({ loading: false, result: null, error: null });
const records = reactive({ loading: false, error: null, items: [], total: 0, page: 1, limit: 20, filters: { status: "", characterId: "", version: undefined, from: "", to: "", requestId: "" } });
const recordsRange = ref([]);
const isDraft = computed(() => drawer.detail?.status === "draft");
const isDirty = computed(() => isDraft.value && JSON.stringify(draft) !== savedDraftSnapshot.value);
const visibleItems = computed(() => filterActivities(items.value, filters));
const typeDefinitions = listActivityTypeDefinitions();

function statusLabel(value) { return ({ draft: "草稿", published: "已发布", offline: "已下线" }[value] || value || "-"); }
function statusTag(value) { return ({ draft: "info", published: "success", offline: "warning" }[value] || "info"); }
function typeLabel(value) { return resolveActivityTypeDefinition(value)?.label || value || "-"; }
function formatTime(value) { return value ? new Date(value).toLocaleString("zh-CN") : "-"; }
function formatWindow(row) { const start = row.startAt || row.draft?.startAt; const end = row.endAt || row.draft?.endAt; return start || end ? `${formatTime(start)} ~ ${formatTime(end)}` : "-"; }
function configSummary(detail) { const value = detail?.draft || detail?.snapshot || {}; return JSON.stringify({ publicConfig: value.publicConfig || {}, typeConfig: value.typeConfig || {}, stages: value.stages?.length || 0, rewardGroups: value.rewardGroups?.length || 0 }); }
function updateStages(value) { draft.stages = value; }
function updateRewardGroups(value) { draft.rewardGroups = value; }
function syncResources() { try { const parsed = JSON.parse(resourcesText.value || "[]"); draft.publicConfig.resources = parsed; } catch { ElMessage.warning("资源字段必须是合法 JSON"); } }
async function loadEditorForType(type) { typeEditorLoading.value = true; typeEditorError.value = ""; try { typeEditor.value = await loadActivityTypeEditor(type); } catch (error) { typeEditor.value = null; typeEditorError.value = error?.message || "类型编辑器加载失败"; } finally { typeEditorLoading.value = false; } }
async function runPreflight() { if (!drawer.detail) return; preflight.loading = true; preflight.error = null; preflight.result = null; try { const response = await activityApi.preflight(drawer.detail.activityId, buildVersionCommand(drawer.detail, reason.value || "发布前预检")); preflight.result = normalizePreflightResponse(response); } catch (error) { const normalized = activityError(error, "活动预检失败"); preflight.error = normalized; preflight.result = { valid: false, errors: preflightErrorSuggestions(error) }; } finally { preflight.loading = false; } }
function syncRecordDates(value) { records.filters.from = value?.[0] || ""; records.filters.to = value?.[1] || ""; }
async function loadRecords() { if (!drawer.detail) return; syncRecordDates(recordsRange.value); records.loading = true; records.error = null; try { const response = await activityApi.records(drawer.detail.activityId, { status: records.filters.status || undefined, characterId: records.filters.characterId || undefined, version: records.filters.version || undefined, from: records.filters.from || undefined, to: records.filters.to || undefined, requestId: records.filters.requestId || undefined, limit: records.limit, offset: (records.page - 1) * records.limit }); const result = normalizeRecordsResponse(response); records.items = result.items; records.total = result.total; } catch (error) { records.error = activityError(error, "获取活动记录失败"); } finally { records.loading = false; } }
async function guardDrawerClose(done) { if (!isDirty.value) return done(); try { await ElMessageBox.confirm("当前草稿有未保存修改，确定关闭并放弃吗？", "未保存变更", { type: "warning" }); done(); } catch { /* keep the editor open */ } }
function resetFilters() { Object.assign(filters, { status: "", activityType: "", key: "", from: "", to: "" }); dateRange.value = []; pagination.page = 1; search(); }
function syncDates(value) { filters.from = value?.[0] ? `${value[0]}T00:00:00Z` : ""; filters.to = value?.[1] ? `${value[1]}T23:59:59Z` : ""; }
async function search() { syncDates(dateRange.value); loading.value = true; loadError.value = null; try { const response = await activityApi.list({ status: filters.status || undefined, activityType: filters.activityType || undefined, key: filters.key || undefined, limit: pagination.limit, offset: (pagination.page - 1) * pagination.limit }); const result = normalizeActivityListResponse(response); items.value = result.items; pagination.total = result.total; } catch (error) { loadError.value = activityError(error, "获取活动列表失败"); } finally { loading.value = false; } }
function openCreate() { Object.assign(create.form, { key: "", activityType: typeDefinitions[0]?.type || "", reason: "" }); create.open = true; }
async function createDraft() { if (!create.form.key.trim() || !create.form.reason.trim()) return ElMessage.warning("请填写活动标识和修改原因"); create.loading = true; try { const template = buildActivityDraftTemplate(create.form.activityType, create.form.key.trim()); const response = await activityApi.createDraft({ key: create.form.key.trim(), activityType: create.form.activityType, schemaVersion: 1, startAt: new Date().toISOString(), endAt: new Date(Date.now() + 86400000).toISOString(), claimDeadline: new Date(Date.now() + 172800000).toISOString(), timezone: "UTC", ...template, reason: create.form.reason.trim() }); create.open = false; ElMessage.success("草稿已创建"); await openDetail(response.data || response); await search(); } catch (error) { ElMessage.error(activityError(error, "创建草稿失败").message); } finally { create.loading = false; } }
async function openDetail(row) { if (drawer.open && isDirty.value) { try { await ElMessageBox.confirm("当前草稿有未保存修改，确定切换活动并放弃吗？", "未保存变更", { type: "warning" }); } catch { return; } } drawer.open = true; drawer.loading = true; drawer.error = null; preflight.result = null; preflight.error = null; records.page = 1; records.items = []; recordsRange.value = []; try { const response = await activityApi.detail(row.activityId); const detail = response.data || response; drawer.detail = detail; Object.keys(draft).forEach((key) => delete draft[key]); Object.assign(draft, detail.draft || {}); draft.publicConfig = { ...(draft.publicConfig || {}) }; resourcesText.value = JSON.stringify(draft.publicConfig.resources || []); savedDraftSnapshot.value = JSON.stringify(draft); reason.value = ""; await loadEditorForType(detail.activityType); await loadRecords(); } catch (error) { drawer.error = activityError(error, "获取活动详情失败"); } finally { drawer.loading = false; } }
async function saveDraft() { if (!reason.value.trim()) return ElMessage.warning("请填写修改原因"); syncResources(); drawer.saving = true; try { const response = await activityApi.updateDraft(drawer.detail.activityId, { ...draft, reason: reason.value.trim(), ifMatch: drawer.detail.etag }); drawer.detail = response.data || response; Object.assign(draft, drawer.detail.draft || {}); resourcesText.value = JSON.stringify(draft.publicConfig?.resources || []); savedDraftSnapshot.value = JSON.stringify(draft); reason.value = ""; ElMessage.success("草稿已保存"); await search(); } catch (error) { drawer.error = activityError(error, "保存草稿失败"); ElMessage.error(drawer.error.message); } finally { drawer.saving = false; } }
async function publish(row) { const detail = row.activityId === drawer.detail?.activityId ? drawer.detail : row; if (isDirty.value) { try { await ElMessageBox.confirm("当前草稿有未保存修改，仍要发布服务器上的版本吗？", "未保存变更", { type: "warning" }); } catch { return; } } const command = buildVersionCommand(detail, reason.value || "发布活动"); if (!command.reason) return ElMessage.warning("请填写发布原因"); try { await ElMessageBox.confirm(`确认发布 ${detail.key} v${detail.version}？发布后该版本不可编辑。`, "发布确认", { type: "warning" }); drawer.publishing = true; const response = await activityApi.publish(detail.activityId, command); ElMessage.success("活动已发布"); drawer.detail = response.data || response; await search(); } catch (error) { if (error !== "cancel" && error !== "close") { const normalized = activityError(error, "发布活动失败"); drawer.error = normalized; ElMessage.error(normalized.message); } } finally { drawer.publishing = false; } }
async function takeOffline(row) { const detail = row.activityId === drawer.detail?.activityId ? drawer.detail : row; try { const prompt = await ElMessageBox.prompt("请输入下线原因", "下线确认", { inputPattern: /\\S+/, inputErrorMessage: "下线原因不能为空", inputValue: reason.value }); drawer.offlining = true; const response = await activityApi.offline(detail.activityId, buildVersionCommand(detail, prompt.value)); ElMessage.success("活动已下线"); drawer.detail = response.data || response; await search(); } catch (error) { if (error !== "cancel" && error !== "close") { const normalized = activityError(error, "活动下线失败"); drawer.error = normalized; ElMessage.error(normalized.message); } } finally { drawer.offlining = false; } }
onMounted(search);
</script>

<style scoped>
.activity-page { min-width: 0; } .page-heading { display: flex; justify-content: space-between; align-items: flex-start; gap: 16px; margin-bottom: 16px; } h3 { font-size: 20px; } .muted { color: #909399; margin-top: 6px; } .filters { margin-bottom: 16px; } .pagination { margin-top: 16px; justify-content: flex-end; } .summary { margin-bottom: 18px; } .draft-form { max-width: 680px; } .drawer-actions { display: flex; gap: 10px; justify-content: flex-end; margin-top: 18px; } .editor-section, .preflight-panel, .records-panel { margin-top: 18px; } .subheading { display: flex; align-items: center; justify-content: space-between; margin-bottom: 8px; } .preflight-errors { display: grid; gap: 8px; margin-top: 10px; } .preflight-error { display: grid; grid-template-columns: minmax(180px, 1fr) auto minmax(160px, 2fr) minmax(180px, 2fr); gap: 8px; align-items: center; padding: 8px; border: 1px solid var(--el-border-color-lighter); } .suggestion { color: var(--el-color-warning); } .records-filters { margin-bottom: 8px; } .records-filters :deep(.el-input) { width: 140px; } .records-filters :deep(.el-input-number) { width: 120px; } code { white-space: pre-wrap; word-break: break-word; } @media (max-width: 700px) { .page-heading { align-items: stretch; flex-direction: column; } .page-heading .el-button { align-self: flex-start; } .filters :deep(.el-form-item) { width: 100%; margin-right: 0; } .filters :deep(.el-select), .filters :deep(.el-input), .filters :deep(.el-date-editor) { width: 100%; } .preflight-error { grid-template-columns: 1fr; } .records-filters :deep(.el-form-item), .records-filters :deep(.el-input), .records-filters :deep(.el-input-number) { width: 100%; } }
</style>
