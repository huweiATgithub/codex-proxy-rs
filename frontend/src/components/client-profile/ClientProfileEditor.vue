<script setup lang="ts">
import type {
  ClientProfileOptions,
  ClientProfilePreview,
  ClientProfileSelection,
  CustomClientProfileSelection,
  PresetClientProfileSelection,
} from '@/api/modules/client-profiles'
import { BaseButton, BaseSegmented } from '@codex-proxy/ui'
import { computed, onMounted, shallowRef, watch } from 'vue'
import { getClientProfileOptions, previewClientProfile } from '@/api/modules/client-profiles'
import { useCopyText } from '@/composables/useCopyText'
import { errorMessage } from '@/utils/async'
import ClientProfileCustomFields from './ClientProfileCustomFields.vue'
import ClientProfilePresetFields from './ClientProfilePresetFields.vue'
import ClientProfilePreviewPanel from './ClientProfilePreviewPanel.vue'

const props = withDefaults(defineProps<{ active?: boolean, disabled?: boolean, allowInherit?: boolean }>(), {
  active: true,
  disabled: false,
  allowInherit: false,
})
const model = defineModel<ClientProfileSelection | null>({ required: true })
const options = shallowRef<ClientProfileOptions>()
const preview = shallowRef<ClientProfilePreview>()
const loading = shallowRef(true)
const loadError = shallowRef('')
const previewError = shallowRef('')
const previewing = shallowRef(false)
const independentDraft = shallowRef<ClientProfileSelection>()
const presetDraft = shallowRef<PresetClientProfileSelection>()
const customDraft = shallowRef<CustomClientProfileSelection>()
const copyText = useCopyText()
const effective = computed(() => model.value ?? options.value?.globalConfiguration)
const inherited = computed(() => props.allowInherit && model.value === null)
const preset = computed(() => effective.value && !effective.value.mode ? effective.value : undefined)
const custom = computed(() => effective.value?.mode === 'custom' ? effective.value : undefined)
const needsInput = computed(() => custom.value
  ? !custom.value.userAgent.trim()
  : preset.value?.versionMode === 'fixed'
    && (!preset.value.codexVersion || (preset.value.client === 'desktop' && (!preset.value.desktopVersion || !preset.value.desktopBuild))))
const policy = computed(() => {
  if (inherited.value)
    return '使用全局设置中已保存的身份'
  if (custom.value)
    return '自定义身份不会自动更新'
  return preset.value?.versionMode === 'latest'
    ? '每 24 小时检查官方版本，保留所选入口和运行环境'
    : '固定版本不受后台更新影响'
})
const profileSource = computed({
  get: () => model.value === null ? 'global' : 'independent',
  set: (value: string) => {
    if (value === 'global') {
      if (model.value)
        independentDraft.value = { ...model.value }
      model.value = null
    }
    else if (independentDraft.value ?? options.value?.globalConfiguration) {
      model.value = { ...(independentDraft.value ?? options.value!.globalConfiguration) }
    }
  },
})
const mode = computed({
  get: () => effective.value?.mode ?? 'preset',
  set: (value: string) => {
    rememberDraft()
    if (value === 'custom') {
      model.value = { ...(customDraft.value ?? { mode: 'custom', userAgent: '' }) }
    }
    else {
      const global = options.value?.globalConfiguration
      const draft = presetDraft.value ?? (global && !global.mode ? global : options.value?.presets[0]?.configuration)
      if (draft)
        model.value = { ...draft }
    }
  },
})
function rememberDraft() {
  if (preset.value)
    presetDraft.value = { ...preset.value }
  if (custom.value)
    customDraft.value = { ...custom.value }
}

function customize() {
  if (!preview.value)
    return
  rememberDraft()
  model.value = {
    mode: 'custom',
    userAgent: preview.value.userAgent,
    originator: preview.value.originator,
    codexVersion: preview.value.codexVersion,
  }
}

async function load() {
  loading.value = true
  loadError.value = ''
  try {
    options.value = await getClientProfileOptions()
  }
  catch (error) {
    loadError.value = errorMessage(error)
  }
  finally {
    loading.value = false
  }
}

watch([model, () => props.active], ([configuration, active], _, onCleanup) => {
  let cancelled = false
  preview.value = undefined
  previewError.value = ''
  previewing.value = active && !needsInput.value
  if (!active || needsInput.value)
    return
  const timer = setTimeout(async () => {
    try {
      const result = await previewClientProfile(configuration)
      if (!cancelled)
        preview.value = result
    }
    catch (error) {
      if (!cancelled)
        previewError.value = errorMessage(error)
    }
    finally {
      if (!cancelled)
        previewing.value = false
    }
  }, 300)
  onCleanup(() => {
    cancelled = true
    clearTimeout(timer)
  })
}, { immediate: true })

onMounted(() => load())
</script>

<template>
  <div class="grid min-w-0 gap-4">
    <div v-if="allowInherit" class="flex flex-wrap items-center justify-between gap-3">
      <BaseSegmented
        v-model="profileSource"
        label="客户端身份来源"
        class="shrink-0"
        :options="[
          { label: '全局配置', value: 'global' },
          { label: '独立配置', value: 'independent' },
        ]"
        :disabled="disabled || loading || !options"
      />
      <slot name="source-extra" />
    </div>
    <div v-if="loadError" role="alert" class="flex flex-wrap items-center justify-between gap-3 text-cp text-cp-error">
      <span>预设加载失败：{{ loadError }}</span>
      <BaseButton size="sm" :disabled="disabled || loading" @click="load()">
        重试
      </BaseButton>
    </div>
    <p v-if="loading" role="status" class="m-0 text-cp text-cp-text-secondary">
      正在加载客户端身份…
    </p>
    <template v-else-if="!inherited">
      <BaseSegmented
        v-model="mode"
        label="User-Agent 配置方式"
        :options="[
          { label: '客户端预设', value: 'preset', disabled: !options },
          { label: '完整自定义', value: 'custom' },
        ]"
        :disabled="disabled"
      />
      <ClientProfilePresetFields
        v-if="preset"
        :model-value="preset"
        :presets="options?.presets ?? []"
        :preview="preview"
        :previewing="previewing"
        :disabled="disabled"
        @update:model-value="model = $event"
      />
      <ClientProfileCustomFields
        v-else-if="custom"
        :model-value="custom"
        :preview="preview"
        :disabled="disabled"
        @update:model-value="model = $event"
      />
    </template>
    <ClientProfilePreviewPanel
      :preview="preview"
      :previewing="previewing"
      :needs-version-input="!!needsInput"
      :error="previewError"
      :label="inherited ? '当前继承的 User-Agent' : 'User-Agent 预览'"
      :show-user-agent="!custom || inherited"
      show-headers
      :policy="policy"
      :empty-label="custom ? '填写 User-Agent 后预览' : '填写版本后预览'"
    >
      <template #actions>
        <div class="flex flex-wrap items-center gap-2">
          <BaseButton size="sm" :disabled="previewing || !preview" @click="preview && copyText(preview.userAgent, { successText: '已复制 User-Agent' })">
            复制
          </BaseButton>
          <BaseButton v-if="!inherited && !custom" size="sm" :disabled="disabled || previewing || !preview" @click="customize">
            基于此项自定义
          </BaseButton>
        </div>
      </template>
    </ClientProfilePreviewPanel>
  </div>
</template>
