<template>
  <AdminLayout>
    <h3>玩家管理</h3>

    <el-card style="margin-top: 20px">
      <el-form :inline="true" @submit.prevent="handleSearch">
        <el-form-item label="登录名">
          <el-input v-model="filters.loginName" placeholder="模糊搜索" clearable />
        </el-form-item>
        <el-form-item label="Guest ID">
          <el-input v-model="filters.guestId" placeholder="模糊搜索" clearable />
        </el-form-item>
        <el-form-item label="状态">
          <el-select v-model="filters.status" placeholder="全部" clearable>
            <el-option label="正常" value="active" />
            <el-option label="待审核" value="pending_review" />
            <el-option label="禁用" value="disabled" />
            <el-option label="封禁" value="banned" />
          </el-select>
        </el-form-item>
        <el-form-item>
          <el-button type="primary" @click="handleSearch">查询</el-button>
        </el-form-item>
      </el-form>

      <el-table :data="players" v-loading="loading" stripe style="margin-top: 16px">
        <el-table-column prop="player_id" label="Player ID" width="220" />
        <el-table-column prop="login_name" label="登录名" width="120">
          <template #default="{ row }">
            {{ row.login_name || "-" }}
          </template>
        </el-table-column>
        <el-table-column prop="guest_id" label="Guest ID" width="180">
          <template #default="{ row }">
            {{ row.guest_id || "-" }}
          </template>
        </el-table-column>
        <el-table-column prop="account_type" label="账号类型" width="100" />
        <el-table-column prop="status" label="状态" width="100">
          <template #default="{ row }">
            <el-tag size="small" :type="statusType(row.status)">
              {{ statusLabel(row.status) }}
            </el-tag>
          </template>
        </el-table-column>
        <el-table-column prop="last_login_at" label="最后登录" width="170">
          <template #default="{ row }">
            {{ formatTime(row.last_login_at) }}
          </template>
        </el-table-column>
        <el-table-column label="操作" min-width="300">
          <template #default="{ row }">
            <el-button type="primary" size="small" link @click="openCharacters(row)">
              角色
            </el-button>
            <template v-if="canManagePlayers">
              <el-button
                v-if="row.status === 'pending_review' && authStore.hasPermission(P.PLAYERS_STATUS_UPDATE)"
                type="primary"
                size="small"
                link
                @click="handleApprove(row)"
              >
                通过
              </el-button>
              <el-button
                v-if="row.status === 'pending_review' && authStore.hasPermission(P.PLAYERS_STATUS_UPDATE)"
                type="warning"
                size="small"
                link
                @click="handleReject(row)"
              >
                拒绝
              </el-button>
              <el-button
                v-if="row.status === 'active' && authStore.hasPermission(P.PLAYERS_STATUS_UPDATE)"
                type="warning"
                size="small"
                link
                @click="handleDisable(row)"
              >
                禁用
              </el-button>
              <el-button
                v-if="(row.status === 'disabled' || row.status === 'banned') && authStore.hasPermission(P.PLAYERS_STATUS_UPDATE)"
                type="success"
                size="small"
                link
                @click="handleEnable(row)"
              >
                解禁
              </el-button>
              <el-button
                v-if="row.status !== 'banned' && authStore.hasPermission(P.PLAYERS_BAN)"
                type="danger"
                size="small"
                link
                @click="handleBan(row)"
              >
                封禁
              </el-button>
            </template>
          </template>
        </el-table-column>
      </el-table>

      <el-pagination
        v-model:current-page="pagination.page"
        v-model:page-size="pagination.limit"
        :total="pagination.total"
        :page-sizes="[20, 50, 100]"
        layout="total, sizes, prev, pager, next"
        style="margin-top: 16px"
        @size-change="fetchPlayers"
        @current-change="fetchPlayers"
      />
    </el-card>

    <el-drawer v-model="characterDrawer.visible" size="78%" :title="characterDrawerTitle" destroy-on-close>
      <el-alert
        v-if="characterDrawer.error"
        :title="characterDrawer.error"
        type="error"
        show-icon
        style="margin-bottom: 12px"
      />

      <el-row :gutter="16">
        <el-col :span="8">
          <el-card shadow="never" class="panel-card">
            <template #header>
              <span>角色列表</span>
            </template>
            <el-skeleton v-if="characterDrawer.loading" :rows="4" animated />
            <el-empty v-else-if="characters.length === 0" description="暂无角色" />
            <el-table
              v-else
              :data="characters"
              size="small"
              highlight-current-row
              @row-click="selectCharacter"
            >
              <el-table-column prop="name" label="名称" min-width="100" />
              <el-table-column prop="status" label="状态" width="86">
                <template #default="{ row }">
                  <el-tag size="small" :type="characterStatusType(row)">
                    {{ row.status }}
                  </el-tag>
                </template>
              </el-table-column>
              <el-table-column label="ID" min-width="120">
                <template #default="{ row }">
                  <span class="mono">{{ shortId(row.character_id) }}</span>
                </template>
              </el-table-column>
            </el-table>
          </el-card>
        </el-col>

        <el-col :span="16">
          <el-card shadow="never" class="panel-card">
            <template #header>
              <div class="detail-header">
                <span>角色详情</span>
                <div class="detail-actions">
                  <el-button
                    size="small"
                    :disabled="!selectedCharacterId"
                    :loading="profile.loading"
                    @click="refreshProfile"
                  >
                    刷新
                  </el-button>
                  <el-dropdown
                    v-if="selectedCharacterId && canUseCharacterGm"
                    trigger="click"
                    @command="openGmDialog"
                  >
                    <el-button size="small" type="primary">
                      GM 操作
                    </el-button>
                    <template #dropdown>
                      <el-dropdown-menu>
                        <el-dropdown-item
                          v-if="authStore.hasPermission(P.GM_CHARACTER_ELEMENTS_WRITE)"
                          command="elements"
                        >
                          调整四属性
                        </el-dropdown-item>
                        <el-dropdown-item
                          v-if="authStore.hasPermission(P.GM_CHARACTER_TITLES_WRITE)"
                          command="title"
                        >
                          称号操作
                        </el-dropdown-item>
                        <el-dropdown-item
                          v-if="authStore.hasPermission(P.GM_CHARACTER_DISCIPLINES_WRITE)"
                          command="discipline"
                        >
                          修改职业
                        </el-dropdown-item>
                        <el-dropdown-item
                          v-if="canRunUnlockCheck"
                          command="unlock"
                        >
                          解锁检查
                        </el-dropdown-item>
                      </el-dropdown-menu>
                    </template>
                  </el-dropdown>
                </div>
              </div>
            </template>

            <el-empty v-if="!selectedCharacterId && !profile.loading" description="请选择角色" />
            <el-skeleton v-else-if="profile.loading" :rows="8" animated />
            <el-alert
              v-else-if="profile.error"
              :title="profile.error"
              type="error"
              show-icon
            />
            <template v-else-if="profile.data">
              <el-descriptions :column="2" border size="small">
                <el-descriptions-item label="角色ID">
                  <span class="mono">{{ profile.data.character.character_id }}</span>
                </el-descriptions-item>
                <el-descriptions-item label="名称">
                  {{ profile.data.character.name }}
                </el-descriptions-item>
                <el-descriptions-item label="世界">
                  {{ profile.data.character.world_id }}
                </el-descriptions-item>
                <el-descriptions-item label="状态">
                  {{ profile.data.character.status }}
                </el-descriptions-item>
                <el-descriptions-item label="创建时间">
                  {{ formatTime(profile.data.character.created_at) }}
                </el-descriptions-item>
                <el-descriptions-item label="最后登录">
                  {{ formatTime(profile.data.character.last_login_at) }}
                </el-descriptions-item>
              </el-descriptions>

              <el-tabs v-model="activeTab" style="margin-top: 16px">
                <el-tab-pane label="四属性" name="elements">
                  <div class="element-grid">
                    <div v-for="element in elementKeys" :key="element.key" class="element-row">
                      <span>{{ element.label }}</span>
                      <el-progress
                        :percentage="affinityPercent(element.key)"
                        :stroke-width="12"
                        :show-text="false"
                      />
                      <span class="element-value">
                        affinity {{ attrValue("affinity", element.key) }} / mastery {{ attrValue("mastery", element.key) }}
                      </span>
                    </div>
                  </div>
                </el-tab-pane>

                <el-tab-pane label="称号" name="titles">
                  <el-empty v-if="profile.data.titles.length === 0" description="暂无称号" />
                  <el-table v-else :data="profile.data.titles" size="small">
                    <el-table-column prop="title_id" label="ID" width="90" />
                    <el-table-column label="名称" min-width="120">
                      <template #default="{ row }">
                        {{ row.name || row.title_id }}
                      </template>
                    </el-table-column>
                    <el-table-column label="状态" width="150">
                      <template #default="{ row }">
                        <el-tag v-if="row.is_equipped && !row.expired" type="success" size="small">已装备</el-tag>
                        <el-tag v-else-if="row.expired" type="danger" size="small">已过期</el-tag>
                        <el-tag v-else size="small">拥有</el-tag>
                        <el-tag v-if="row.hidden" type="info" size="small" style="margin-left: 4px">隐藏</el-tag>
                      </template>
                    </el-table-column>
                    <el-table-column label="来源" min-width="140">
                      <template #default="{ row }">
                        {{ row.source_type }} / {{ row.source_id || "-" }}
                      </template>
                    </el-table-column>
                    <el-table-column label="过期时间" min-width="160">
                      <template #default="{ row }">
                        {{ formatTime(row.expires_at) }}
                      </template>
                    </el-table-column>
                  </el-table>
                </el-tab-pane>

                <el-tab-pane label="职业" name="disciplines">
                  <el-empty v-if="profile.data.disciplines.length === 0" description="暂无职业阶位" />
                  <el-table v-else :data="profile.data.disciplines" size="small">
                    <el-table-column prop="discipline_id" label="职业" min-width="140" />
                    <el-table-column prop="tier" label="阶位" width="120" />
                    <el-table-column prop="points" label="点数" width="110" />
                    <el-table-column label="激活" width="90">
                      <template #default="{ row }">
                        <el-tag :type="row.active ? 'success' : 'info'" size="small">
                          {{ row.active ? "是" : "否" }}
                        </el-tag>
                      </template>
                    </el-table-column>
                    <el-table-column label="更新时间" min-width="160">
                      <template #default="{ row }">
                        {{ formatTime(row.updated_at) }}
                      </template>
                    </el-table-column>
                  </el-table>
                </el-tab-pane>

                <el-tab-pane label="日志" name="logs">
                  <el-tabs tab-position="left">
                    <el-tab-pane label="四属性">
                      <LogTable :rows="profile.data.logs.elements" type="element" />
                    </el-tab-pane>
                    <el-tab-pane label="称号">
                      <LogTable :rows="profile.data.logs.titles" type="title" />
                    </el-tab-pane>
                    <el-tab-pane label="职业">
                      <LogTable :rows="profile.data.logs.disciplines" type="discipline" />
                    </el-tab-pane>
                  </el-tabs>
                </el-tab-pane>
              </el-tabs>
            </template>
          </el-card>
        </el-col>
      </el-row>
    </el-drawer>

    <el-dialog v-model="gmDialog.visible" :title="gmDialogTitle" width="520px" destroy-on-close>
      <el-alert
        v-if="gmDialog.permissionDenied"
        title="权限不足"
        type="error"
        show-icon
        style="margin-bottom: 12px"
      />
      <el-alert
        v-if="gmDialog.error"
        :title="gmDialog.error"
        type="error"
        show-icon
        style="margin-bottom: 12px"
      />

      <el-form v-if="gmDialog.type === 'elements'" :model="elementForm" label-width="110px">
        <el-form-item v-for="element in elementKeys" :key="element.key" :label="element.label">
          <el-input-number v-model="elementForm.affinity[element.key]" :min="0" :max="10000" />
          <el-input-number v-model="elementForm.mastery[element.key]" :min="0" style="margin-left: 8px" />
        </el-form-item>
        <el-form-item label="原因">
          <el-input v-model="gmDialog.reason" maxlength="255" show-word-limit />
        </el-form-item>
      </el-form>

      <el-form v-else-if="gmDialog.type === 'title'" :model="titleForm" label-width="110px">
        <el-form-item label="操作">
          <el-select v-model="titleForm.action">
            <el-option label="授予" value="grant" />
            <el-option label="撤销" value="revoke" />
            <el-option label="装备" value="equip" />
            <el-option label="卸下" value="unequip" />
          </el-select>
        </el-form-item>
        <el-form-item label="称号ID">
          <el-input v-model="titleForm.titleId" placeholder="9001" />
        </el-form-item>
        <el-form-item label="过期时间">
          <el-date-picker
            v-model="titleForm.expiresAt"
            type="datetime"
            value-format="YYYY-MM-DDTHH:mm:ss.SSS[Z]"
            placeholder="限时称号必填"
          />
        </el-form-item>
        <el-form-item label="原因">
          <el-input v-model="gmDialog.reason" maxlength="255" show-word-limit />
        </el-form-item>
      </el-form>

      <el-form v-else-if="gmDialog.type === 'discipline'" :model="disciplineForm" label-width="110px">
        <el-form-item label="职业ID">
          <el-input v-model="disciplineForm.disciplineId" placeholder="forging" />
        </el-form-item>
        <el-form-item label="阶位">
          <el-select v-model="disciplineForm.tier">
            <el-option v-for="tier in disciplineTiers" :key="tier" :label="tier" :value="tier" />
          </el-select>
        </el-form-item>
        <el-form-item label="点数">
          <el-input-number v-model="disciplineForm.points" :min="0" />
        </el-form-item>
        <el-form-item label="激活">
          <el-switch v-model="disciplineForm.active" />
        </el-form-item>
        <el-form-item label="原因">
          <el-input v-model="gmDialog.reason" maxlength="255" show-word-limit />
        </el-form-item>
      </el-form>

      <el-form v-else-if="gmDialog.type === 'unlock'" label-width="110px">
        <el-form-item label="原因">
          <el-input v-model="gmDialog.reason" maxlength="255" show-word-limit />
        </el-form-item>
      </el-form>

      <template #footer>
        <el-button @click="gmDialog.visible = false">取消</el-button>
        <el-button
          type="primary"
          :loading="gmDialog.submitting"
          :disabled="gmDialog.permissionDenied"
          @click="submitGmDialog"
        >
          确认提交
        </el-button>
      </template>
    </el-dialog>
  </AdminLayout>
</template>

<script setup>
import { computed, defineComponent, h, onMounted, reactive, ref } from "vue";
import { ElMessage, ElMessageBox, ElEmpty, ElTable, ElTableColumn } from "element-plus";
import AdminLayout from "../components/AdminLayout.vue";
import { useAuthStore } from "../stores/auth";
import { gmApi, playerApi } from "../api";
import { ADMIN_PERMISSIONS as P } from "../auth/permissions";

const elementKeys = [
  { key: "earth", label: "地" },
  { key: "fire", label: "火" },
  { key: "water", label: "水" },
  { key: "wind", label: "风" }
];
const disciplineTiers = ["novice", "apprentice", "adept", "expert", "master", "grandmaster"];

const LogTable = defineComponent({
  props: {
    rows: { type: Array, default: () => [] },
    type: { type: String, default: "element" }
  },
  setup(props) {
    return () => {
      if (!props.rows.length) {
        return h(ElEmpty, { description: "暂无日志" });
      }
      return h(ElTable, { data: props.rows, size: "small" }, () => [
        h(ElTableColumn, { prop: "created_at", label: "时间", minWidth: 160, formatter: (_row, _column, value) => formatTime(value) }),
        h(ElTableColumn, {
          label: "动作",
          minWidth: 120,
          formatter: (row) => row.action || row.source_type || "-"
        }),
        h(ElTableColumn, {
          label: "对象",
          minWidth: 130,
          formatter: (row) => row.title_id || row.discipline_id || row.source_id || "-"
        }),
        h(ElTableColumn, {
          label: "操作者",
          minWidth: 120,
          formatter: (row) => row.operator_id || row.operator?.id || "-"
        }),
        h(ElTableColumn, {
          label: "原因",
          minWidth: 180,
          formatter: (row) => row.reason || "-"
        }),
        h(ElTableColumn, {
          label: "结果",
          minWidth: 110,
          formatter: (row) => {
            if (props.type === "element") {
              const delta = row.affinity_delta || {};
              return `火${signed(delta.fire || 0)} 地${signed(delta.earth || 0)}`;
            }
            return row.action || "-";
          }
        })
      ]);
    };
  }
});

const authStore = useAuthStore();

const players = ref([]);
const loading = ref(false);
const filters = reactive({
  loginName: "",
  guestId: "",
  status: ""
});
const pagination = ref({
  page: 1,
  limit: 50,
  total: 0
});
const characterDrawer = reactive({
  visible: false,
  player: null,
  loading: false,
  error: ""
});
const characters = ref([]);
const selectedCharacterId = ref("");
const profile = reactive({
  loading: false,
  error: "",
  data: null
});
const activeTab = ref("elements");
const gmDialog = reactive({
  visible: false,
  type: "",
  reason: "",
  error: "",
  submitting: false,
  permissionDenied: false
});
const elementForm = reactive({
  affinity: { earth: 2500, fire: 2500, water: 2500, wind: 2500 },
  mastery: { earth: 0, fire: 0, water: 0, wind: 0 }
});
const titleForm = reactive({
  action: "grant",
  titleId: "",
  expiresAt: ""
});
const disciplineForm = reactive({
  disciplineId: "forging",
  points: 0,
  tier: "novice",
  active: false
});

const canManagePlayers = computed(() => authStore.hasAnyPermission([
  P.PLAYERS_STATUS_UPDATE,
  P.PLAYERS_BAN
]));
const canUseCharacterGm = computed(() => authStore.hasAnyPermission([
  P.GM_CHARACTER_ELEMENTS_WRITE,
  P.GM_CHARACTER_TITLES_WRITE,
  P.GM_CHARACTER_DISCIPLINES_WRITE
]));
const canRunUnlockCheck = computed(() => authStore.hasPermission(P.GM_CHARACTER_TITLES_WRITE) &&
  authStore.hasPermission(P.GM_CHARACTER_DISCIPLINES_WRITE));
const characterDrawerTitle = computed(() => {
  const player = characterDrawer.player;
  return player ? `角色详情 - ${player.login_name || player.guest_id || player.player_id}` : "角色详情";
});
const gmDialogTitle = computed(() => {
  switch (gmDialog.type) {
    case "elements":
      return "调整四属性";
    case "title":
      return "称号操作";
    case "discipline":
      return "修改职业";
    case "unlock":
      return "触发解锁检查";
    default:
      return "GM 操作";
  }
});

function signed(value) {
  const numeric = Number(value) || 0;
  return numeric >= 0 ? `+${numeric}` : String(numeric);
}

function formatTime(time) {
  if (!time) return "-";
  return new Date(time).toLocaleString("zh-CN");
}

function shortId(value) {
  if (!value) return "-";
  return value.length > 12 ? `${value.slice(0, 8)}...${value.slice(-4)}` : value;
}

function statusType(status) {
  switch (status) {
    case "active":
      return "success";
    case "pending_review":
      return "warning";
    case "disabled":
      return "info";
    case "banned":
      return "danger";
    default:
      return "";
  }
}

function statusLabel(status) {
  switch (status) {
    case "active":
      return "正常";
    case "pending_review":
      return "待审核";
    case "disabled":
      return "禁用";
    case "banned":
      return "封禁";
    default:
      return status;
  }
}

function characterStatusType(row) {
  if (row.deleted_at || row.deletedAt || row.status === "deleted") return "danger";
  return row.status === "active" ? "success" : "info";
}

function attrValue(group, key) {
  return profile.data?.attributes?.[group]?.[key] ?? 0;
}

function affinityPercent(key) {
  return Math.round((attrValue("affinity", key) / 10000) * 100);
}

async function fetchPlayers() {
  loading.value = true;
  try {
    const params = {
      limit: pagination.value.limit,
      offset: (pagination.value.page - 1) * pagination.value.limit
    };
    if (filters.loginName) params.login_name = filters.loginName;
    if (filters.guestId) params.guest_id = filters.guestId;
    if (filters.status) params.status = filters.status;

    const { data } = await playerApi.getPlayers(params);
    players.value = data.players;
    pagination.value.total = data.total;
  } catch (err) {
    ElMessage.error("获取玩家列表失败");
  } finally {
    loading.value = false;
  }
}

function handleSearch() {
  pagination.value.page = 1;
  fetchPlayers();
}

async function openCharacters(row) {
  characterDrawer.visible = true;
  characterDrawer.player = row;
  characterDrawer.loading = true;
  characterDrawer.error = "";
  characters.value = [];
  profile.data = null;
  selectedCharacterId.value = "";

  try {
    const { data } = await playerApi.getPlayerCharacters(row.player_id, {
      includeDeleted: true,
      limit: 100
    });
    characters.value = data.characters || [];
    if (characters.value.length > 0) {
      await selectCharacter(characters.value[0]);
    }
  } catch (err) {
    characterDrawer.error = err.response?.data?.message || "角色列表加载失败";
  } finally {
    characterDrawer.loading = false;
  }
}

async function selectCharacter(row) {
  selectedCharacterId.value = row.character_id || row.characterId;
  activeTab.value = "elements";
  await refreshProfile();
}

async function refreshProfile() {
  if (!selectedCharacterId.value) return;
  profile.loading = true;
  profile.error = "";
  try {
    const { data } = await playerApi.getCharacterProfile(selectedCharacterId.value, { logLimit: 50 });
    profile.data = data;
  } catch (err) {
    profile.error = err.response?.data?.message || "角色详情加载失败";
  } finally {
    profile.loading = false;
  }
}

function openGmDialog(type) {
  gmDialog.type = type;
  gmDialog.reason = "";
  gmDialog.error = "";
  gmDialog.permissionDenied = !hasGmPermission(type);
  if (type === "elements" && profile.data?.attributes) {
    for (const element of elementKeys) {
      elementForm.affinity[element.key] = attrValue("affinity", element.key);
      elementForm.mastery[element.key] = attrValue("mastery", element.key);
    }
  }
  gmDialog.visible = true;
}

function hasGmPermission(type) {
  if (type === "elements") return authStore.hasPermission(P.GM_CHARACTER_ELEMENTS_WRITE);
  if (type === "title") return authStore.hasPermission(P.GM_CHARACTER_TITLES_WRITE);
  if (type === "discipline") return authStore.hasPermission(P.GM_CHARACTER_DISCIPLINES_WRITE);
  if (type === "unlock") return canRunUnlockCheck.value;
  return false;
}

function validateGmDialog() {
  if (!selectedCharacterId.value) return "未选择角色";
  if (!gmDialog.reason.trim()) return "请填写操作原因";
  if (gmDialog.type === "elements") {
    const total = elementKeys.reduce((sum, element) => sum + Number(elementForm.affinity[element.key] || 0), 0);
    return total === 10000 ? "" : "affinity 总和必须为 10000";
  }
  if (gmDialog.type === "title" && !titleForm.titleId.trim()) return "请填写称号ID";
  if (gmDialog.type === "discipline" && !disciplineForm.disciplineId.trim()) return "请填写职业ID";
  return "";
}

async function submitGmDialog() {
  const validationError = validateGmDialog();
  if (validationError) {
    gmDialog.error = validationError;
    return;
  }

  try {
    await ElMessageBox.confirm("确认提交该 GM 操作吗？", gmDialogTitle.value, { type: "warning" });
  } catch {
    return;
  }

  gmDialog.submitting = true;
  gmDialog.error = "";
  try {
    const characterId = selectedCharacterId.value;
    const reason = gmDialog.reason.trim();
    if (gmDialog.type === "elements") {
      await gmApi.setCharacterElements(characterId, {
        affinity: { ...elementForm.affinity },
        mastery: { ...elementForm.mastery },
        reason
      });
    } else if (gmDialog.type === "title") {
      await gmApi.applyCharacterTitle(characterId, {
        action: titleForm.action,
        titleId: titleForm.titleId.trim(),
        expiresAt: titleForm.expiresAt || undefined,
        reason
      });
    } else if (gmDialog.type === "discipline") {
      await gmApi.setCharacterDiscipline(characterId, {
        disciplineId: disciplineForm.disciplineId.trim(),
        points: disciplineForm.points,
        tier: disciplineForm.tier,
        active: disciplineForm.active,
        reason
      });
    } else if (gmDialog.type === "unlock") {
      await gmApi.runCharacterUnlockCheck(characterId, { reason });
    }

    ElMessage.success("操作已提交");
    gmDialog.visible = false;
    await refreshProfile();
  } catch (err) {
    gmDialog.error = err.response?.data?.message || err.response?.data?.error || "操作失败";
  } finally {
    gmDialog.submitting = false;
  }
}

async function handleDisable(row) {
  try {
    await ElMessageBox.confirm(
      `确定禁用玩家 ${row.login_name || row.guest_id || row.player_id} 吗？`,
      "禁用玩家",
      { type: "warning" }
    );

    await playerApi.updatePlayerStatus(row.player_id, "disabled");
    ElMessage.success("已禁用");
    fetchPlayers();
  } catch (err) {
    if (err !== "cancel") {
      ElMessage.error("操作失败");
    }
  }
}

async function handleApprove(row) {
  try {
    await ElMessageBox.confirm(
      `确定通过玩家 ${row.login_name || row.guest_id || row.player_id} 的注册审核吗？`,
      "通过审核",
      { type: "info" }
    );

    await playerApi.updatePlayerStatus(row.player_id, "active");
    ElMessage.success("已通过审核");
    fetchPlayers();
  } catch (err) {
    if (err !== "cancel") {
      ElMessage.error("操作失败");
    }
  }
}

async function handleReject(row) {
  try {
    await ElMessageBox.confirm(
      `确定拒绝玩家 ${row.login_name || row.guest_id || row.player_id} 的注册审核吗？`,
      "拒绝审核",
      { type: "warning" }
    );

    await playerApi.updatePlayerStatus(row.player_id, "disabled");
    ElMessage.success("已拒绝");
    fetchPlayers();
  } catch (err) {
    if (err !== "cancel") {
      ElMessage.error("操作失败");
    }
  }
}

async function handleEnable(row) {
  try {
    await ElMessageBox.confirm(
      `确定解禁玩家 ${row.login_name || row.guest_id || row.player_id} 吗？`,
      "解禁玩家",
      { type: "info" }
    );

    await playerApi.updatePlayerStatus(row.player_id, "active");
    ElMessage.success("已解禁");
    fetchPlayers();
  } catch (err) {
    if (err !== "cancel") {
      ElMessage.error("操作失败");
    }
  }
}

async function handleBan(row) {
  try {
    await ElMessageBox.confirm(
      `确定封禁玩家 ${row.login_name || row.guest_id || row.player_id} 吗？`,
      "封禁玩家",
      { type: "warning" }
    );

    await playerApi.updatePlayerStatus(row.player_id, "banned");
    ElMessage.success("已封禁");
    fetchPlayers();
  } catch (err) {
    if (err !== "cancel") {
      ElMessage.error("操作失败");
    }
  }
}

onMounted(() => {
  fetchPlayers();
});
</script>

<style scoped>
.panel-card {
  min-height: 520px;
}

.detail-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: 12px;
}

.detail-actions {
  display: flex;
  gap: 8px;
}

.mono {
  font-family: Consolas, "Courier New", monospace;
  font-size: 12px;
}

.element-grid {
  display: grid;
  gap: 12px;
  max-width: 760px;
}

.element-row {
  display: grid;
  grid-template-columns: 42px minmax(160px, 1fr) 260px;
  align-items: center;
  gap: 12px;
}

.element-value {
  color: #606266;
  font-size: 13px;
}
</style>
