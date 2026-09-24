<script setup lang="ts">
import type {
  CatalogClientProfileSelection,
  ClientProfileCatalogEntry,
  ClientProfileOptions,
  ClientProfilePreview,
  ClientProfileSelection,
  CustomClientProfileSelection,
} from '@/api/modules/client-profiles'
import { BaseButton, BaseCheckbox, BaseFormItem, BaseInput, BaseSegmented, BaseSelect, BaseTextarea } from '@codex-proxy/ui'
import { computed, onMounted, shallowRef, watch } from 'vue'
import { getClientProfileOptions, previewClientProfile, refreshClientProfileCatalog } from '@/api/modules/client-profiles'
import { useCopyText } from '@/composables/useCopyText'
import { errorMessage } from '@/utils/async'
import { formatDateTime } from '@/utils/date'
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
const refreshing = shallowRef(false)
const loadError = shallowRef('')
const refreshError = shallowRef('')
const previewError = shallowRef('')
const previewing = shallowRef(false)
const previewRevision = shallowRef(0)
const independentDraft = shallowRef<ClientProfileSelection>()
const catalogDraft = shallowRef<CatalogClientProfileSelection>()
const customDraft = shallowRef<CustomClientProfileSelection>()
const manualHeaders = shallowRef<Pick<CustomClientProfileSelection, 'originator' | 'codexVersion'>>({})
const copyText = useCopyText()
const clients = [
  { label: 'Desktop', value: 'desktop' },
  { label: 'CLI', value: 'cli' },
  { label: 'Exec', value: 'exec' },
]
const effective = computed(() => model.value ?? options.value?.globalConfiguration)
const inherited = computed(() => props.allowInherit && model.value === null)
const catalog = computed(() => effective.value?.mode === 'catalog' ? effective.value : undefined)
const custom = computed(() => effective.value?.mode === 'custom' ? effective.value : undefined)
const entries = computed(() => options.value?.catalog.entries ?? [])
const clientOptions = computed(() => clients.map(client => ({
  ...client,
  disabled: !entries.value.some(entry => entry.client === client.value) && catalog.value?.entry.client !== client.value,
})))
const resolvedEntry = computed(() => preview.value?.configuration.mode === 'catalog'
  ? preview.value.configuration.entry
  : catalog.value?.entry)
const source = computed(() => catalog.value
  ? options.value?.catalog.sources.find(item => item.source === (catalog.value?.entry.client === 'desktop' ? 'desktop' : 'cli'))
  : undefined)
const releaseLimitReached = computed(() => {
  const limit = options.value?.catalog.releaseLimit
  if (!catalog.value || !limit)
    return false
  const desktop = catalog.value.entry.client === 'desktop'
  return new Set(entries.value.filter(entry => (entry.client === 'desktop') === desktop).map(entry => entry.release)).size >= limit
})
const catalogError = computed(() => refreshError.value || (catalog.value
  ? source.value?.error
  : options.value?.catalog.sources.filter(item => item.error).map(item => `${item.source === 'desktop' ? 'Desktop' : 'CLI / Exec'}: ${item.error}`).join(' · ')))
const environmentOptions = computed(() => {
  const environments = new Map<string, { value: string, label: string }>()
  for (const entry of entries.value) {
    if (entry.client === catalog.value?.entry.client && !environments.has(entry.environment))
      environments.set(entry.environment, { value: entry.environment, label: environmentLabel(entry) })
  }
  const current = resolvedEntry.value
  if (current)
    environments.set(current.environment, { value: current.environment, label: environmentLabel(current) })
  return [...environments.values()]
})
const releaseOptions = computed(() => {
  if (!catalog.value)
    return []
  const current = catalog.value.entry
  const matching = entries.value.filter(entry => entry.client === current.client && entry.environment === current.environment)
  const releases = [...new Set(matching.map(entry => entry.release))].map((release, index) => ({
    value: release,
    label: `${release}${index === 0 ? ' · 最新' : ''}`,
  }))
  if (!releases.some(release => release.value === current.release))
    releases.push({ value: current.release, label: `${current.release} · 已保存条目` })
  return releases
})
const needsManualHeaders = computed(() => custom.value && custom.value.userAgent.trim()
  && (preview.value?.recognized === false || !hasKnownPrefix(custom.value.userAgent)))
const needsInput = computed(() => !!custom.value && !custom.value.userAgent.trim())
const policy = computed(() => {
  if (inherited.value)
    return '使用全局设置中已保存的身份'
  if (custom.value)
    return '自定义身份不会自动更新'
  if (catalog.value) {
    return catalog.value.versionMode === 'latest'
      ? '保存后自动跟随最新发布，客户端与运行环境保持不变'
      : '保存后固定使用此完整条目，刷新列表不会更改身份'
  }
  return ''
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
const mode = computed({
  get: () => effective.value?.mode ?? 'legacy',
  set: (value: string) => {
    rememberDraft()
    if (value === 'custom') {
      model.value = { ...(customDraft.value ?? { mode: 'custom', userAgent: '' }) }
    }
    else {
      const entry = defaultEntry()
      if (catalogDraft.value)
        model.value = cloneProfile(catalogDraft.value)
      else if (entry)
        model.value = { mode: 'catalog', versionMode: 'latest', entry: { ...entry } }
    }
  },
})
const selectedClient = computed({
  get: () => catalog.value?.entry.client ?? '',
  set: (client: string) => selectEntry(entries.value.find(entry => entry.client === client && entry.environment === catalog.value?.entry.environment)
    ?? entries.value.find(entry => entry.client === client)),
})
const selectedEnvironment = computed({
  get: () => catalog.value?.entry.environment ?? '',
  set: (environment: string) => selectEntry(entries.value.find(entry => entry.client === catalog.value?.entry.client && entry.environment === environment
    && (catalog.value.versionMode === 'latest' || entry.release === catalog.value.entry.release))
  ?? entries.value.find(entry => entry.client === catalog.value?.entry.client && entry.environment === environment)),
})
const selectedRelease = computed({
  get: () => catalog.value?.entry.release ?? '',
  set: (release: string) => selectEntry(entries.value.find(entry => entry.client === catalog.value?.entry.client
    && entry.environment === catalog.value?.entry.environment && entry.release === release)),
})
const followLatest = computed({
  get: () => catalog.value?.versionMode === 'latest',
  set: (checked: boolean) => {
    if (catalog.value && resolvedEntry.value)
      model.value = { mode: 'catalog', versionMode: checked ? 'latest' : 'fixed', entry: { ...resolvedEntry.value } }
  },
})

function cloneProfile<T extends ClientProfileSelection>(configuration: T): T {
  return { ...configuration, ...(configuration.mode === 'catalog' ? { entry: { ...configuration.entry } } : {}) }
}

function rememberDraft() {
  if (catalog.value)
    catalogDraft.value = cloneProfile(catalog.value)
  if (custom.value)
    customDraft.value = cloneProfile(custom.value)
}

function defaultEntry() {
  const previous = effective.value
  if (previous && !previous.mode) {
    return entries.value.find(entry => entry.client === previous.client && entry.environment.startsWith(previous.platform))
      ?? entries.value.find(entry => entry.client === previous.client)
      ?? entries.value[0]
  }
  return entries.value[0]
}

function environmentLabel(entry: ClientProfileCatalogEntry) {
  const target = entry.userAgent.match(/\(([^;]+); ([^)]+)\)/)
  return target ? `${target[1]} · ${target[2]}` : entry.environment
}

function selectEntry(entry?: ClientProfileCatalogEntry) {
  if (entry && catalog.value)
    model.value = { ...catalog.value, entry: { ...entry } }
}

function hasKnownPrefix(userAgent: string) {
  return /^(?:Codex Desktop|codex-tui|codex_cli_rs|codex_exec)\/\S+(?:\s|$)/.test(userAgent)
}

function updateCustom(key: 'userAgent' | 'originator' | 'codexVersion', value: string) {
  const current = custom.value
  if (!current)
    return
  if (key !== 'userAgent') {
    manualHeaders.value = { originator: current.originator, codexVersion: current.codexVersion, [key]: value }
    model.value = { ...current, ...manualHeaders.value }
    return
  }
  if (!hasKnownPrefix(current.userAgent))
    manualHeaders.value = { originator: current.originator, codexVersion: current.codexVersion }
  // 已知 UA 的配套头由后端派生，手填值仅用于未知 UA 的草稿。
  model.value = { mode: 'custom', userAgent: value, ...(hasKnownPrefix(value) ? {} : manualHeaders.value) }
}

function customize() {
  if (!preview.value)
    return
  rememberDraft()
  model.value = {
    mode: 'custom',
    userAgent: preview.value.userAgent,
    ...(hasKnownPrefix(preview.value.userAgent)
      ? {}
      : {
          originator: preview.value.originator,
          codexVersion: preview.value.codexVersion,
        }),
  }
}

async function load(refresh = false) {
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
      <span>发布列表加载失败：{{ loadError }}</span>
      <BaseButton size="sm" :disabled="disabled || refreshing" @click="load()">
        重试
      </BaseButton>
    </div>
    <p v-if="loading" role="status" class="m-0 text-cp text-cp-text-secondary">
      正在加载客户端身份…
    </p>
    <template v-else-if="!inherited">
      <div class="flex flex-wrap items-center justify-between gap-3">
        <BaseSegmented
          v-model="mode"
          label="User-Agent 配置方式"
          :options="[
            { label: '发布列表', value: 'catalog' },
            { label: '自定义', value: 'custom' },
          ]"
          :disabled="disabled"
        />
        <BaseButton v-if="!custom" size="sm" :loading="refreshing" :disabled="disabled || refreshing" @click="load(true)">
          刷新列表
        </BaseButton>
      </div>
      <p v-if="!effective?.mode" class="m-0 text-cp-sm text-cp-text-secondary" role="status">
        当前仍使用原预设，下方为当前 User-Agent，选择发布列表或自定义并保存后更换
      </p>
      <p v-if="!custom && !entries.length" class="m-0 text-cp-sm text-cp-text-secondary" role="status">
        暂无发布条目，可刷新列表或填写自定义 User-Agent
      </p>
      <div v-if="catalog" class="grid min-w-0 gap-4">
        <div class="grid gap-4 sm:grid-cols-2">
          <BaseFormItem label="客户端">
            <BaseSelect v-model="selectedClient" class="w-full" :options="clientOptions" :disabled="disabled" />
          </BaseFormItem>
          <BaseFormItem label="运行环境">
            <BaseSelect v-model="selectedEnvironment" class="w-full" :options="environmentOptions" :disabled="disabled" />
          </BaseFormItem>
        </div>
        <BaseFormItem label="发布版本">
          <template #extra>
            <BaseCheckbox
              v-model="followLatest"
              label="自动跟随最新版本"
              show-label
              :disabled="disabled || previewing || !!previewError"
            />
          </template>
          <BaseInput v-if="followLatest" :model-value="resolvedEntry?.release ?? ''" aria-label="当前发布版本" readonly :disabled="disabled" />
          <BaseSelect v-else v-model="selectedRelease" class="w-full" :options="releaseOptions" :disabled="disabled" />
          <p v-if="catalog.entry.client === 'desktop'" class="mt-2 mb-0 text-cp-xs text-cp-text-tertiary">
            此处为 Desktop 应用版本，内嵌 Core 版本见下方详情
          </p>
        </BaseFormItem>
        <div class="flex flex-wrap items-center gap-x-3 gap-y-1 text-cp-xs text-cp-text-tertiary">
          <a
            class="text-cp-link hover:text-cp-link-hover"
            :href="`https://github.com/huweiATgithub/${catalog.entry.client === 'desktop' ? 'codex-desktop-ua' : 'codex-ua'}/releases`"
            target="_blank"
            rel="noopener noreferrer"
          >
            {{ catalog.entry.client === 'desktop' ? 'codex-desktop-ua' : 'codex-ua' }}
          </a>
          <span>{{ source?.checkedAt ? `检查于 ${formatDateTime(source.checkedAt)}` : '尚未检查发布列表' }}</span>
          <span v-if="releaseLimitReached">最近 {{ options?.catalog.releaseLimit }} 个发布</span>
        </div>
      </div>
      <p v-if="!custom && catalogError" role="alert" class="m-0 break-all text-cp-sm text-cp-warning">
        刷新失败，保留上次列表：{{ catalogError }}
      </p>
      <template v-if="custom">
        <BaseFormItem label="User-Agent" required>
          <template #extra>
            <BaseButton size="sm" :disabled="!custom.userAgent" @click="copyText(custom.userAgent, { successText: '已复制 User-Agent' })">
              复制
            </BaseButton>
          </template>
          <BaseTextarea
            :model-value="custom.userAgent"
            :rows="4"
            :disabled="disabled"
            placeholder="粘贴完整 User-Agent，或从发布列表基于某一项自定义"
            spellcheck="false"
            autocomplete="off"
            class="font-mono"
            @update:model-value="updateCustom('userAgent', $event)"
          />
        </BaseFormItem>
        <div v-if="needsManualHeaders" class="grid gap-3">
          <p class="m-0 text-cp-sm text-cp-text-secondary">
            未识别配套请求头，请补充客户端标识和 Core 版本
          </p>
          <div class="grid gap-4 sm:grid-cols-2">
            <BaseFormItem label="客户端标识 originator" required>
              <BaseInput :model-value="custom.originator ?? ''" :disabled="disabled" @update:model-value="updateCustom('originator', $event)" />
            </BaseFormItem>
            <BaseFormItem label="Core version" required>
              <BaseInput :model-value="custom.codexVersion ?? ''" :disabled="disabled" placeholder="例如 0.156.1" @update:model-value="updateCustom('codexVersion', $event)" />
            </BaseFormItem>
          </div>
        </div>
        <p v-else-if="preview?.recognized" class="m-0 text-cp-sm text-cp-success">
          已识别 {{ preview.originator }} · Core {{ preview.codexVersion }}，配套请求头已同步
        </p>
      </template>
    </template>
    <ClientProfilePreviewPanel
      :preview="preview"
      :previewing="previewing"
      :needs-version-input="needsInput"
      :error="previewError"
      :label="inherited ? '当前继承的 User-Agent' : 'User-Agent 预览'"
      :show-user-agent="!custom || inherited"
      show-headers
      :policy="policy"
      empty-label="填写 User-Agent 后预览"
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
