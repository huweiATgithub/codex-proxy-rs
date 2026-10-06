<script setup lang="ts">
import type { CatalogClientProfileSelection, ClientProfileCatalogEntry, ClientProfileOptions, ClientProfilePreview } from '@/api/modules/client-profiles'
import { BaseCheckbox, BaseFormItem, BaseInput, BaseSelect } from '@codex-proxy/ui'
import { computed } from 'vue'

const props = defineProps<{
  catalog: ClientProfileOptions['catalog']
  preview?: ClientProfilePreview
  disabled: boolean
  previewing: boolean
  error: string
}>()
const model = defineModel<CatalogClientProfileSelection>({ required: true })
const entries = computed(() => props.catalog.entries)
const resolvedEntry = computed(() => props.preview?.configuration.mode === 'catalog'
  ? props.preview.configuration.entry
  : model.value.entry)
const clients = [
  { label: 'Desktop', value: 'desktop' },
  { label: 'CLI · TUI', value: 'cli' },
  { label: 'Exec', value: 'exec' },
]
const clientOptions = computed(() => clients.map(client => ({
  ...client,
  disabled: !entries.value.some(entry => entry.client === client.value) && model.value.entry.client !== client.value,
})))
const environmentOptions = computed(() => {
  const environments = new Map<string, { value: string, label: string }>()
  for (const entry of entries.value) {
    if (entry.client === model.value.entry.client && !environments.has(entry.environment))
      environments.set(entry.environment, { value: entry.environment, label: environmentLabel(entry) })
  }
  const current = resolvedEntry.value
  environments.set(current.environment, { value: current.environment, label: environmentLabel(current) })
  return [...environments.values()]
})
const releaseOptions = computed(() => {
  const current = model.value.entry
  const matching = entries.value.filter(entry => entry.client === current.client && entry.environment === current.environment)
  const releases = [...new Set(matching.map(entry => entry.release))].map((release, index) => ({
    value: release,
    label: `${release}${index === 0 ? ' · 最新收录' : ''}`,
  }))
  if (!releases.some(release => release.value === current.release))
    releases.push({ value: current.release, label: `${current.release} · 已保存条目` })
  return releases
})
const selectedClient = computed({
  get: () => model.value.entry.client,
  set: (client: string) => selectEntry(entries.value.find(entry => entry.client === client && entry.environment === model.value.entry.environment)
    ?? entries.value.find(entry => entry.client === client)),
})
const selectedEnvironment = computed({
  get: () => model.value.entry.environment,
  set: (environment: string) => selectEntry(entries.value.find(entry => entry.client === model.value.entry.client && entry.environment === environment)),
})
const selectedRelease = computed({
  get: () => model.value.entry.release,
  set: (release: string) => selectEntry(entries.value.find(entry => entry.client === model.value.entry.client
    && entry.environment === model.value.entry.environment && entry.release === release)),
})
const followLatest = computed({
  get: () => model.value.versionMode === 'latest',
  set: (checked: boolean) => {
    model.value = { mode: 'catalog', versionMode: checked ? 'latest' : 'fixed', entry: { ...resolvedEntry.value } }
  },
})
const source = computed(() => props.catalog.sources.find(item => item.source === (model.value.entry.client === 'desktop' ? 'desktop' : 'cli')))

function environmentLabel(entry: ClientProfileCatalogEntry) {
  const target = entry.userAgent.match(/\(([^;]+); ([^)]+)\)/)
  return target ? `${target[1]} · ${target[2]}` : entry.environment
}

function selectEntry(entry?: ClientProfileCatalogEntry) {
  if (entry)
    model.value = { ...model.value, entry: { ...entry } }
}
</script>

<template>
  <div class="grid min-w-0 gap-4">
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
        <BaseCheckbox v-model="followLatest" label="自动跟随最新收录" show-label :disabled="disabled || previewing || !!error" />
      </template>
      <BaseInput v-if="followLatest" :model-value="resolvedEntry.release" aria-label="当前发布版本" readonly :disabled="disabled" />
      <BaseSelect v-else v-model="selectedRelease" class="w-full" :options="releaseOptions" :disabled="disabled" />
      <p v-if="model.entry.client === 'desktop'" class="mt-2 mb-0 text-cp-xs text-cp-text-tertiary">
        此处为 Desktop 应用版本，内嵌 Core 版本见下方详情
      </p>
    </BaseFormItem>
    <div class="flex flex-wrap items-center gap-x-3 gap-y-1 text-cp-xs text-cp-text-tertiary">
      <a
        class="text-cp-link hover:text-cp-link-hover"
        :href="`https://github.com/huweiATgithub/${model.entry.client === 'desktop' ? 'codex-desktop-ua' : 'codex-ua'}/releases`"
        target="_blank"
        rel="noopener noreferrer"
      >
        {{ model.entry.client === 'desktop' ? 'codex-desktop-ua' : 'codex-ua' }}
      </a>
      <span>{{ source?.checkedAtDisplay ? `检查于 ${source.checkedAtDisplay}` : '尚未检查发布列表' }}</span>
      <span v-if="source?.updatedAtDisplay">收录于 {{ source.updatedAtDisplay }}</span>
    </div>
  </div>
</template>
