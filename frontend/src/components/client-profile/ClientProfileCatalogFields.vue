<script setup lang="ts">
import type { CatalogClientProfileSelection, ClientProfileCatalogEntry, ClientProfileOptions, ClientProfilePreview } from '@/api/modules/settings/profiles'

import { ZCheckbox, ZFormItem, ZInput, ZSelect } from '@codex-proxy/ui'
import { computed, useId } from 'vue'
import { useI18n } from 'vue-i18n'

const props = defineProps<{
  catalog: ClientProfileOptions['catalog']
  preview?: ClientProfilePreview
  disabled: boolean
  previewing: boolean
  error: string
}>()
const { t } = useI18n({ useScope: 'global' })
const model = defineModel<CatalogClientProfileSelection>({ required: true })
const releaseInputId = useId()
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
const profiles = [
  { value: 'xterm-256color', label: 'xterm-256color' },
  { value: 'WindowsTerminal', label: 'Windows Terminal' },
  { value: 'vscode', label: 'VS Code' },
  { value: 'herdr', label: 'Herdr' },
]
const profileOptions = computed(() => profiles.filter(profile =>
  profile.value === entryProfile(model.value.entry)
  || entries.value.some(entry => entry.client === model.value.entry.client
    && entry.environment === model.value.entry.environment && entryProfile(entry) === profile.value),
))
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
  const matching = entries.value.filter(entry => entry.client === current.client && entry.environment === current.environment
    && entryProfile(entry) === entryProfile(current))
  const releases = [...new Set(matching.map(entry => entry.release))].map((release, index) => ({
    value: release,
    label: index === 0 ? t('settings.clientProfile.catalog.latestRelease', { release }) : release,
  }))
  if (!releases.some(release => release.value === current.release))
    releases.push({ value: current.release, label: t('settings.clientProfile.catalog.savedRelease', { release: current.release }) })
  return releases
})
const selectedClient = computed({
  get: () => model.value.entry.client,
  set: (client: string) => selectEntry(entries.value.find(entry => entry.client === client && entry.environment === model.value.entry.environment
    && entryProfile(entry) === entryProfile(model.value.entry))
  ?? entries.value.find(entry => entry.client === client && entry.environment === model.value.entry.environment)
  ?? entries.value.find(entry => entry.client === client)),
})
const selectedEnvironment = computed({
  get: () => model.value.entry.environment,
  set: (environment: string) => selectEntry(entries.value.find(entry => entry.client === model.value.entry.client && entry.environment === environment
    && entryProfile(entry) === entryProfile(model.value.entry))
  ?? entries.value.find(entry => entry.client === model.value.entry.client && entry.environment === environment)),
})
const selectedProfile = computed({
  get: () => entryProfile(model.value.entry) ?? 'xterm-256color',
  set: (profile: string) => selectEntry(entries.value.find(entry => entry.client === model.value.entry.client
    && entry.environment === model.value.entry.environment && entryProfile(entry) === profile)),
})
const selectedRelease = computed({
  get: () => model.value.entry.release,
  set: (release: string) => selectEntry(entries.value.find(entry => entry.client === model.value.entry.client
    && entry.environment === model.value.entry.environment && entryProfile(entry) === entryProfile(model.value.entry)
    && entry.release === release)),
})
const followLatest = computed({
  get: () => model.value.versionMode === 'latest',
  set: (checked: boolean) => {
    model.value = { mode: 'catalog', versionMode: checked ? 'latest' : 'fixed', entry: { ...resolvedEntry.value } }
  },
})
const source = computed(() => props.catalog.sources.find(item => item.source === (model.value.entry.client === 'desktop' ? 'desktop' : 'cli')))

function entryProfile(entry: ClientProfileCatalogEntry) {
  return entry.client === 'desktop' ? undefined : entry.profile ?? 'xterm-256color'
}

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
    <div class="grid gap-4" :class="model.entry.client === 'desktop' ? 'sm:grid-cols-2' : 'sm:grid-cols-3'">
      <ZFormItem :label="t('settings.clientProfile.catalog.client')">
        <ZSelect v-model="selectedClient" class="w-full" :options="clientOptions" :disabled="disabled" />
      </ZFormItem>
      <ZFormItem :label="t('settings.clientProfile.catalog.environment')">
        <ZSelect v-model="selectedEnvironment" class="w-full" :options="environmentOptions" :disabled="disabled" />
      </ZFormItem>
      <ZFormItem v-if="model.entry.client !== 'desktop'" label="Profile">
        <ZSelect v-model="selectedProfile" class="w-full" :options="profileOptions" :disabled="disabled" />
      </ZFormItem>
    </div>
    <ZFormItem :label="t('settings.clientProfile.catalog.release')" :for="releaseInputId">
      <template #extra>
        <ZCheckbox :id="`${releaseInputId}-follow-latest`" v-model="followLatest" :label="t('settings.clientProfile.catalog.followLatestAutomatically')" :disabled="disabled || previewing || !!error" />
      </template>
      <ZInput v-if="followLatest" :id="releaseInputId" :model-value="resolvedEntry.release" :aria-label="t('settings.clientProfile.catalog.currentRelease')" readonly :disabled="disabled" />
      <ZSelect v-else :id="releaseInputId" v-model="selectedRelease" class="w-full" :aria-label="t('settings.clientProfile.catalog.release')" :options="releaseOptions" :disabled="disabled" />
      <p v-if="model.entry.client === 'desktop'" class="mt-2 mb-0 text-cp-xs text-cp-text-tertiary">
        {{ t('settings.clientProfile.catalog.desktopReleaseHelp') }}
      </p>
    </ZFormItem>
    <div class="flex flex-wrap items-center gap-x-3 gap-y-1 text-cp-xs text-cp-text-tertiary">
      <a
        class="text-cp-link hover:text-cp-link-hover"
        :href="`https://github.com/huweiATgithub/${model.entry.client === 'desktop' ? 'codex-desktop-ua' : 'codex-ua'}/releases`"
        target="_blank"
        rel="noopener noreferrer"
      >
        {{ model.entry.client === 'desktop' ? 'codex-desktop-ua' : 'codex-ua' }}
      </a>
      <span>{{ source?.checkedAtDisplay ? t('settings.clientProfile.checked', { checkedAtDisplay: source.checkedAtDisplay }) : t('settings.clientProfile.catalog.notChecked') }}</span>
      <span v-if="source?.updatedAtDisplay">{{ t('settings.clientProfile.catalog.collectedAt', { updatedAtDisplay: source.updatedAtDisplay }) }}</span>
    </div>
  </div>
</template>
