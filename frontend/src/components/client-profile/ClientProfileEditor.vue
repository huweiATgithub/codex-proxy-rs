<script setup lang="ts">
import type {
  CatalogClientProfileSelection,
  ClientProfileOptions,
  ClientProfilePreview,
  ClientProfileSelection,
  CustomClientProfileSelection,
  PresetClientProfileSelection,
} from '@/api/modules/settings/profiles'
import { ZButton, ZSegmented } from '@codex-proxy/ui'
import { computed, onMounted, shallowRef, watch } from 'vue'
import { getClientProfileOptions, previewClientProfile, refreshClientProfileCatalog } from '@/api/modules/settings/profiles'
import { errorMessage } from '@/utils/operation'
import ClientProfileCatalogFields from './ClientProfileCatalogFields.vue'
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
const loading = shallowRef(false)
const refreshing = shallowRef(false)
const loadError = shallowRef('')
const refreshError = shallowRef('')
const previewError = shallowRef('')
const previewing = shallowRef(false)
const previewRevision = shallowRef(0)
const independentDraft = shallowRef<ClientProfileSelection>()
const presetDraft = shallowRef<PresetClientProfileSelection>()
const catalogDraft = shallowRef<CatalogClientProfileSelection>()
const customDraft = shallowRef<CustomClientProfileSelection>()
const effective = computed(() => model.value ?? options.value?.globalConfiguration)
const inherited = computed(() => props.allowInherit && model.value === null)
const preset = computed(() => effective.value && !effective.value.mode ? effective.value : undefined)
const catalog = computed(() => effective.value?.mode === 'catalog' ? effective.value : undefined)
const custom = computed(() => effective.value?.mode === 'custom'
  ? effective.value
  : preset.value?.versionMode === 'fixed'
    ? customSelection()
    : undefined)
const needsInput = computed(() => effective.value?.mode === 'custom'
  ? !effective.value.userAgent.trim()
  : preset.value?.versionMode === 'fixed'
    && (!preset.value.codexVersion || (preset.value.client === 'desktop' && (!preset.value.desktopVersion || !preset.value.desktopBuild))))
const catalogError = computed(() => refreshError.value || options.value?.catalog.sources
  .filter(source => source.error && (!catalog.value || source.source === (catalog.value.entry.client === 'desktop' ? 'desktop' : 'cli')))
  .map(source => `${source.source === 'desktop' ? 'Desktop' : 'CLI / Exec'}：${source.error}`)
  .join(' · '))
const policy = computed(() => inherited.value
  ? '使用全局设置中已保存的身份'
  : catalog.value
    ? catalog.value.versionMode === 'latest' ? '跟随最新收录' : '固定发布身份'
    : custom.value ? '固定身份' : undefined)
const mode = computed({
  get: () => custom.value ? 'custom' : catalog.value ? 'catalog' : 'preset',
  set: (value: string) => {
    rememberDraft()
    if (value === 'preset') {
      const selection = presetDraft.value ?? options.value?.presets[0]?.configuration
      if (selection)
        model.value = { ...selection, versionMode: 'latest', codexVersion: null, desktopVersion: null, desktopBuild: null }
    }
    else if (value === 'catalog') {
      const entry = defaultEntry()
      const selection = catalogDraft.value ?? (entry ? { mode: 'catalog' as const, versionMode: 'latest' as const, entry } : undefined)
      if (selection)
        model.value = cloneProfile(selection)
    }
    else if (value === 'custom') {
      model.value = { ...(customDraft.value ?? customSelection()) }
    }
  },
})
const profileSource = computed({
  get: () => model.value === null ? 'global' : 'independent',
  set: (value: string) => {
    if (value === 'global') {
      if (model.value)
        independentDraft.value = cloneProfile(model.value)
      model.value = null
    }
    else if (independentDraft.value ?? options.value?.globalConfiguration) {
      model.value = cloneProfile(independentDraft.value ?? options.value!.globalConfiguration)
    }
  },
})

function cloneProfile<T extends ClientProfileSelection>(configuration: T): T {
  return { ...configuration, ...(configuration.mode === 'catalog' ? { entry: { ...configuration.entry } } : {}) }
}

function rememberDraft() {
  if (preset.value)
    presetDraft.value = { ...preset.value }
  if (catalog.value)
    catalogDraft.value = cloneProfile(catalog.value)
  if (custom.value)
    customDraft.value = { ...custom.value }
}

function defaultEntry() {
  const entries = options.value?.catalog.entries ?? []
  const client = preset.value?.client === 'cli' && preset.value.cliEntry === 'exec' ? 'exec' : preset.value?.client
  return entries.find(entry => entry.client === client && entry.environment.startsWith(preset.value?.platform ?? ''))
    ?? entries.find(entry => entry.client === client)
    ?? entries[0]
}

function customize() {
  if (!preview.value)
    return
  rememberDraft()
  model.value = customSelection()
}

function customSelection(): CustomClientProfileSelection {
  const current = preview.value
  return {
    mode: 'custom',
    userAgent: current?.userAgent ?? '',
    ...(current ? { originator: current.originator, codexVersion: current.codexVersion } : {}),
  }
}

async function load(refresh = false) {
  if (loading.value || refreshing.value)
    return
  if (refresh) {
    refreshing.value = true
    refreshError.value = ''
  }
  else {
    loading.value = true
    loadError.value = ''
  }
  try {
    options.value = await (refresh ? refreshClientProfileCatalog() : getClientProfileOptions())
    loadError.value = ''
    refreshError.value = ''
    previewRevision.value++
  }
  catch (error) {
    if (refresh)
      refreshError.value = errorMessage(error)
    else
      loadError.value = errorMessage(error)
  }
  finally {
    loading.value = false
    refreshing.value = false
  }
}

watch([model, () => props.active, previewRevision], ([configuration, active], _, onCleanup) => {
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
        previewError.value = errorMessage(error).replaceAll('User-Agent', '用户代理')
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
      <ZSegmented
        v-model="profileSource"
        aria-label="客户端身份来源"
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
      <span>客户端身份加载失败：{{ loadError }}</span>
      <ZButton size="small" :disabled="disabled || loading || refreshing" @click="load()">
        重试
      </ZButton>
    </div>
    <template v-if="!inherited">
      <div class="flex flex-wrap items-center justify-between gap-3">
        <ZSegmented
          v-model="mode"
          aria-label="客户端身份配置方式"
          :options="[
            { label: '版本预设', value: 'preset', disabled: mode !== 'preset' && !options?.presets.length },
            { label: '发布列表', value: 'catalog', disabled: !options?.catalog.entries.length && !catalogDraft && !catalog },
            { label: '自定义', value: 'custom' },
          ]"
          :disabled="disabled || loading"
        />
        <ZButton v-if="!custom" size="small" :loading="refreshing" :disabled="disabled || loading || refreshing" @click="load(true)">
          刷新列表
        </ZButton>
      </div>
      <ClientProfilePresetFields
        v-if="mode === 'preset'"
        :model-value="preset ?? null"
        :presets="options?.presets ?? []"
        :disabled="disabled || loading"
        :max-version-lag="options?.maxVersionLag"
        :aria-busy="loading || undefined"
        @update:model-value="model = $event"
      />
      <ClientProfileCatalogFields
        v-if="catalog && options"
        :model-value="catalog"
        :catalog="options.catalog"
        :preview="preview"
        :previewing="previewing"
        :error="previewError"
        :disabled="disabled || loading"
        @update:model-value="model = $event"
      />
      <p v-if="!custom && options && !options.catalog.entries.length" role="status" class="m-0 text-cp-sm text-cp-text-secondary">
        暂无发布条目，可刷新列表或填写自定义用户代理
      </p>
      <p v-if="!custom && catalogError" role="alert" class="m-0 break-all text-cp-sm text-cp-warning">
        刷新失败，保留上次列表：{{ catalogError }}
      </p>
      <ClientProfileCustomFields
        v-if="custom"
        :model-value="custom"
        :preview="preview"
        :disabled="disabled || loading || (!!preset && previewing)"
        @update:model-value="model = $event"
      />
    </template>
    <ClientProfilePreviewPanel
      :preview="preview"
      :previewing="previewing"
      :needs-version-input="!!needsInput"
      :error="previewError"
      :policy="policy"
      :show-user-agent="inherited || !custom"
      empty-label="填写用户代理后预览"
      show-headers
    >
      <template #actions>
        <ZButton v-if="!inherited && !custom" size="small" :disabled="disabled || previewing || !preview" @click="customize">
          基于此项自定义
        </ZButton>
      </template>
    </ClientProfilePreviewPanel>
  </div>
</template>
