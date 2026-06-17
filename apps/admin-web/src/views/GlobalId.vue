<template>
  <AdminLayout>
    <div class="global-id-page">
      <div class="page-header">
        <h3>全局 ID</h3>
      </div>

      <el-tabs v-model="activeTab">
        <el-tab-pane label="ID 解码" name="decode">
          <el-card class="tool-card">
            <el-form class="decode-form" :inline="true" @submit.prevent="handleDecode">
              <el-form-item label="业务 ID">
                <el-input
                  v-model="decodeForm.id"
                  class="id-input"
                  clearable
                  placeholder="plr_... / room_... / 纯数字"
                  @keyup.enter="handleDecode"
                />
              </el-form-item>
              <el-form-item>
                <el-button type="primary" :loading="decodeState.loading" @click="handleDecode">
                  解码
                </el-button>
              </el-form-item>
            </el-form>

            <el-empty v-if="!decodeState.result && !decodeState.loading" description="输入 ID 后查看解析结果" />

            <div v-if="decodeState.result" class="decode-result">
              <el-descriptions :column="2" border>
                <el-descriptions-item label="原始 ID">
                  <span class="mono">{{ decodeState.result.raw_id }}</span>
                </el-descriptions-item>
                <el-descriptions-item label="规范化 ID">
                  <span class="mono">{{ decodeState.result.normalized_id }}</span>
                </el-descriptions-item>
                <el-descriptions-item label="类型">
                  {{ valueOrDash(decodeState.result.id_kind) }}
                </el-descriptions-item>
                <el-descriptions-item label="数字 ID">
                  <span class="mono">{{ valueOrDash(decodeState.result.numeric_id) }}</span>
                </el-descriptions-item>
                <el-descriptions-item label="创建时间">
                  {{ formatTime(decodeState.result.created_at) }}
                </el-descriptions-item>
                <el-descriptions-item label="Origin">
                  {{ originText(decodeState.result) }}
                </el-descriptions-item>
                <el-descriptions-item label="Worker">
                  {{ valueOrDash(decodeState.result.worker_id) }}
                </el-descriptions-item>
                <el-descriptions-item label="Sequence">
                  {{ valueOrDash(decodeState.result.sequence) }}
                </el-descriptions-item>
                <el-descriptions-item label="生成时 World">
                  {{ worldMembershipText(decodeState.result.world_at_create) }}
                </el-descriptions-item>
                <el-descriptions-item label="当前 World">
                  {{ worldMembershipText(decodeState.result.current_world) }}
                </el-descriptions-item>
                <el-descriptions-item label="合服上下文" :span="2">
                  {{ mergeEventText(decodeState.result.merge_context) }}
                </el-descriptions-item>
              </el-descriptions>
            </div>
          </el-card>
        </el-tab-pane>

        <el-tab-pane label="Origins" name="origins">
          <el-card class="tool-card">
            <el-form :inline="true" @submit.prevent="searchOrigins">
              <el-form-item label="Origin ID">
                <el-input v-model="originFilters.originId" clearable placeholder="精确匹配" />
              </el-form-item>
              <el-form-item label="Origin Key">
                <el-input v-model="originFilters.originKey" clearable placeholder="模糊搜索" />
              </el-form-item>
              <el-form-item>
                <el-button type="primary" :loading="originsState.loading" @click="searchOrigins">
                  查询
                </el-button>
              </el-form-item>
            </el-form>

            <el-table :data="originsState.rows" v-loading="originsState.loading" stripe>
              <el-table-column prop="origin_id" label="Origin ID" width="120" />
              <el-table-column prop="origin_key" label="Origin Key" min-width="180" />
              <el-table-column prop="created_at" label="创建时间" width="180">
                <template #default="{ row }">
                  {{ formatTime(row.created_at) }}
                </template>
              </el-table-column>
              <el-table-column prop="retired_at" label="退役时间" width="180">
                <template #default="{ row }">
                  {{ formatTime(row.retired_at) }}
                </template>
              </el-table-column>
            </el-table>

            <el-pagination
              v-model:current-page="originsState.page"
              v-model:page-size="originsState.limit"
              :total="originsState.total"
              :page-sizes="[20, 50, 100]"
              layout="total, sizes, prev, pager, next"
              class="pagination"
              @size-change="fetchOrigins"
              @current-change="fetchOrigins"
            />
          </el-card>
        </el-tab-pane>

        <el-tab-pane label="Worlds" name="worlds">
          <el-card class="tool-card">
            <el-form :inline="true" @submit.prevent="searchWorlds">
              <el-form-item label="World ID">
                <el-input v-model="worldFilters.worldId" clearable placeholder="精确匹配" />
              </el-form-item>
              <el-form-item label="World Key">
                <el-input v-model="worldFilters.worldKey" clearable placeholder="模糊搜索" />
              </el-form-item>
              <el-form-item label="Origin ID">
                <el-input v-model="worldFilters.originId" clearable placeholder="active 或成员" />
              </el-form-item>
              <el-form-item>
                <el-button type="primary" :loading="worldsState.loading" @click="searchWorlds">
                  查询
                </el-button>
              </el-form-item>
            </el-form>

            <el-table :data="worldsState.rows" v-loading="worldsState.loading" stripe>
              <el-table-column prop="world_id" label="World ID" width="120" />
              <el-table-column prop="world_key" label="World Key" min-width="180" />
              <el-table-column label="Active Origin" min-width="180">
                <template #default="{ row }">
                  {{ idKeyText(row.active_origin_id, row.active_origin_key) }}
                </template>
              </el-table-column>
              <el-table-column label="成员 Origins" min-width="220">
                <template #default="{ row }">
                  {{ originListText(row) }}
                </template>
              </el-table-column>
              <el-table-column prop="created_at" label="创建时间" width="180">
                <template #default="{ row }">
                  {{ formatTime(row.created_at) }}
                </template>
              </el-table-column>
              <el-table-column prop="retired_at" label="退役时间" width="180">
                <template #default="{ row }">
                  {{ formatTime(row.retired_at) }}
                </template>
              </el-table-column>
            </el-table>

            <el-pagination
              v-model:current-page="worldsState.page"
              v-model:page-size="worldsState.limit"
              :total="worldsState.total"
              :page-sizes="[20, 50, 100]"
              layout="total, sizes, prev, pager, next"
              class="pagination"
              @size-change="fetchWorlds"
              @current-change="fetchWorlds"
            />
          </el-card>
        </el-tab-pane>

        <el-tab-pane label="合服事件" name="merge-events">
          <el-card class="tool-card">
            <el-form :inline="true" @submit.prevent="searchMergeEvents">
              <el-form-item label="World ID">
                <el-input v-model="mergeFilters.worldId" clearable placeholder="源或目标" />
              </el-form-item>
              <el-form-item label="Origin ID">
                <el-input v-model="mergeFilters.originId" clearable placeholder="active 或源" />
              </el-form-item>
              <el-form-item>
                <el-button type="primary" :loading="mergeState.loading" @click="searchMergeEvents">
                  查询
                </el-button>
              </el-form-item>
            </el-form>

            <el-table :data="mergeState.rows" v-loading="mergeState.loading" stripe>
              <el-table-column prop="merge_id" label="Merge ID" width="140" />
              <el-table-column label="目标 World" min-width="180">
                <template #default="{ row }">
                  {{ idKeyText(row.target_world_id, row.target_world_key) }}
                </template>
              </el-table-column>
              <el-table-column label="Active Origin" min-width="160">
                <template #default="{ row }">
                  {{ idKeyText(row.active_origin_id, row.active_origin_key) }}
                </template>
              </el-table-column>
              <el-table-column label="源 Worlds" min-width="220">
                <template #default="{ row }">
                  {{ listText(row.source_world_ids, row.source_world_keys) }}
                </template>
              </el-table-column>
              <el-table-column label="源 Origins" min-width="220">
                <template #default="{ row }">
                  {{ listText(row.source_origin_ids, row.source_origin_keys) }}
                </template>
              </el-table-column>
              <el-table-column prop="merged_at" label="合服时间" width="180">
                <template #default="{ row }">
                  {{ formatTime(row.merged_at) }}
                </template>
              </el-table-column>
              <el-table-column prop="operator" label="操作人" width="120">
                <template #default="{ row }">
                  {{ valueOrDash(row.operator) }}
                </template>
              </el-table-column>
            </el-table>

            <el-pagination
              v-model:current-page="mergeState.page"
              v-model:page-size="mergeState.limit"
              :total="mergeState.total"
              :page-sizes="[20, 50, 100]"
              layout="total, sizes, prev, pager, next"
              class="pagination"
              @size-change="fetchMergeEvents"
              @current-change="fetchMergeEvents"
            />
          </el-card>
        </el-tab-pane>
      </el-tabs>
    </div>
  </AdminLayout>
</template>

<script setup>
import { onMounted, reactive, ref } from "vue";
import { ElMessage } from "element-plus";
import AdminLayout from "../components/AdminLayout.vue";
import { globalIdApi } from "../api";

const activeTab = ref("decode");

const decodeForm = reactive({
  id: ""
});
const decodeState = reactive({
  loading: false,
  result: null
});

const originFilters = reactive({
  originId: "",
  originKey: ""
});
const worldFilters = reactive({
  worldId: "",
  worldKey: "",
  originId: ""
});
const mergeFilters = reactive({
  worldId: "",
  originId: ""
});

const originsState = reactive(pageState());
const worldsState = reactive(pageState());
const mergeState = reactive(pageState());

function pageState() {
  return {
    loading: false,
    rows: [],
    page: 1,
    limit: 50,
    total: 0
  };
}

function listParams(state) {
  return {
    limit: state.limit,
    offset: (state.page - 1) * state.limit
  };
}

function appendFilter(params, key, value) {
  if (value !== undefined && value !== null && String(value).trim() !== "") {
    params[key] = String(value).trim();
  }
}

function valueOrDash(value) {
  if (value === undefined || value === null || value === "") {
    return "-";
  }
  return String(value);
}

function formatTime(time) {
  if (!time) return "-";
  return new Date(time).toLocaleString("zh-CN");
}

function idKeyText(id, key) {
  if (id === undefined || id === null) {
    return "-";
  }
  return key ? `${id} / ${key}` : String(id);
}

function listText(ids = [], keys = []) {
  if (!Array.isArray(ids) || ids.length === 0) {
    return "-";
  }
  return ids.map((id, index) => idKeyText(id, keys?.[index])).join(", ");
}

function originText(decoded) {
  return idKeyText(decoded.origin_id, decoded.origin_key);
}

function originListText(row) {
  if (!Array.isArray(row.origins) || row.origins.length === 0) {
    return "-";
  }
  return row.origins.map((origin) => idKeyText(origin.origin_id, origin.origin_key)).join(", ");
}

function worldMembershipText(world) {
  if (!world) {
    return "-";
  }
  return idKeyText(world.world_id, world.world_key);
}

function mergeEventText(event) {
  if (!event) {
    return "-";
  }
  return `${idKeyText(event.merge_id, null)} -> ${idKeyText(event.target_world_id, event.target_world_key)} / ${formatTime(event.merged_at)}`;
}

async function handleDecode() {
  const id = decodeForm.id.trim();
  if (!id) {
    ElMessage.warning("请输入业务 ID");
    return;
  }

  decodeState.loading = true;
  try {
    const { data } = await globalIdApi.decode(id);
    decodeState.result = data.decoded;
  } catch (error) {
    decodeState.result = null;
    ElMessage.error(error.response?.data?.message || "解码失败");
  } finally {
    decodeState.loading = false;
  }
}

async function fetchOrigins() {
  originsState.loading = true;
  try {
    const params = listParams(originsState);
    appendFilter(params, "origin_id", originFilters.originId);
    appendFilter(params, "origin_key", originFilters.originKey);
    const { data } = await globalIdApi.getOrigins(params);
    originsState.rows = data.origins;
    originsState.total = data.total;
  } catch (error) {
    ElMessage.error(error.response?.data?.message || "获取 origins 失败");
  } finally {
    originsState.loading = false;
  }
}

async function fetchWorlds() {
  worldsState.loading = true;
  try {
    const params = listParams(worldsState);
    appendFilter(params, "world_id", worldFilters.worldId);
    appendFilter(params, "world_key", worldFilters.worldKey);
    appendFilter(params, "origin_id", worldFilters.originId);
    const { data } = await globalIdApi.getWorlds(params);
    worldsState.rows = data.worlds;
    worldsState.total = data.total;
  } catch (error) {
    ElMessage.error(error.response?.data?.message || "获取 worlds 失败");
  } finally {
    worldsState.loading = false;
  }
}

async function fetchMergeEvents() {
  mergeState.loading = true;
  try {
    const params = listParams(mergeState);
    appendFilter(params, "world_id", mergeFilters.worldId);
    appendFilter(params, "origin_id", mergeFilters.originId);
    const { data } = await globalIdApi.getMergeEvents(params);
    mergeState.rows = data.mergeEvents;
    mergeState.total = data.total;
  } catch (error) {
    ElMessage.error(error.response?.data?.message || "获取合服事件失败");
  } finally {
    mergeState.loading = false;
  }
}

function searchOrigins() {
  originsState.page = 1;
  fetchOrigins();
}

function searchWorlds() {
  worldsState.page = 1;
  fetchWorlds();
}

function searchMergeEvents() {
  mergeState.page = 1;
  fetchMergeEvents();
}

onMounted(() => {
  fetchOrigins();
  fetchWorlds();
  fetchMergeEvents();
});
</script>

<style scoped>
.global-id-page {
  min-width: 0;
}

.page-header {
  margin-bottom: 16px;
}

.tool-card {
  margin-top: 12px;
}

.decode-form {
  margin-bottom: 12px;
}

.id-input {
  width: min(520px, 62vw);
}

.decode-result {
  margin-top: 16px;
}

.mono {
  font-family: ui-monospace, SFMono-Regular, Menlo, Monaco, Consolas, "Liberation Mono", monospace;
  word-break: break-all;
}

.pagination {
  margin-top: 16px;
}
</style>
