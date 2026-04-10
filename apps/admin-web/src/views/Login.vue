<template>
  <div class="login-container">
    <div class="login-box">
      <h1 class="title">MyServer 管理后台</h1>

      <el-form
        ref="formRef"
        :model="form"
        :rules="rules"
        label-width="0"
        @submit.prevent="handleLogin"
      >
        <el-form-item prop="username">
          <el-input
            v-model="form.username"
            placeholder="用户名"
            size="large"
            prefix-icon="User"
          />
        </el-form-item>

        <el-form-item prop="password">
          <el-input
            v-model="form.password"
            type="password"
            placeholder="密码"
            size="large"
            prefix-icon="Lock"
            show-password
            @keyup.enter="handleLogin"
          />
        </el-form-item>

        <el-form-item>
          <el-button
            type="primary"
            size="large"
            :loading="authStore.loading"
            style="width: 100%"
            @click="handleLogin"
          >
            登录
          </el-button>
        </el-form-item>
      </el-form>

      <el-alert
        v-if="errorMessage"
        :title="errorMessage"
        type="error"
        show-icon
        :closable="false"
        style="margin-top: 16px"
      />
    </div>
  </div>
</template>

<script setup>
import { ref, reactive } from "vue";
import { useRouter, useRoute } from "vue-router";
import { ElMessage } from "element-plus";
import { useAuthStore } from "../stores/auth";

const router = useRouter();
const route = useRoute();
const authStore = useAuthStore();

const formRef = ref(null);
const errorMessage = ref("");

const form = reactive({
  username: "",
  password: ""
});

const rules = {
  username: [{ required: true, message: "请输入用户名", trigger: "blur" }],
  password: [{ required: true, message: "请输入密码", trigger: "blur" }]
};

async function handleLogin() {
  if (!formRef.value) return;

  try {
    await formRef.value.validate();
    errorMessage.value = "";

    await authStore.login(form.username, form.password);

    ElMessage.success("登录成功");
    const redirect = route.query.redirect || "/";
    router.push(redirect);
  } catch (err) {
    if (err?.response?.data?.message) {
      errorMessage.value = err.response.data.message;
    } else {
      errorMessage.value = "登录失败，请检查用户名和密码";
    }
  }
}
</script>

<style scoped>
.login-container {
  min-height: 100vh;
  display: flex;
  align-items: center;
  justify-content: center;
  background: linear-gradient(135deg, #667eea 0%, #764ba2 100%);
}

.login-box {
  width: 400px;
  padding: 40px;
  background: white;
  border-radius: 8px;
  box-shadow: 0 4px 20px rgba(0, 0, 0, 0.15);
}

.title {
  text-align: center;
  margin-bottom: 32px;
  color: #333;
  font-size: 24px;
}
</style>
