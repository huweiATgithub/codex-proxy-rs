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
import { useI18n } from 'vue-i18n'
import { getClientProfileOptions, previewClientProfile, refreshClientProfileCatalog } from '@/api/modules/settings/profiles'
import { notification as ZNotification } from '@/utils/notification'
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

const { t } = useI18n({ useScope: 'global' })

const model = defineModel<ClientProfileSelection | null>({ required: true })
const options = shallowRef<ClientProfileOptions>()
const preview = shallowRef<ClientProfilePreview>()
const loading = shallowRef(false)
const refreshing = shallowRef(false)
const loadError = shallowRef('')
const previewing = shallowRef(false)
const previewRevision = shallowRef(0)
const independentDraft = shallowRef<ClientProfileSelection>()
const presetDraft = shallowRef<PresetClientProfileSelection>()
const catalogDraft = shallowRef<CatalogClientProfileSelection>()
const customDraft = shallowRef<CustomClientProfileSelection>()
const effective = computed(() => model.value ?? options.value?.globalConfiguration)
const inherited = computed(() => props.allowInherit && model.value === null)
const preset = computed(() => effective.value && !effective.value.mode
  ? effective.value
  : undefined)
const catalog = computed(() => effective.value?.mode === 'catalog'
  ? effective.value
  : undefined)
const custom = computed(() => effective.value?.mode === 'custom'
  ? effective.value
  : preset.value?.versionMode === 'fixed'
    ? customSelection()
    : undefined)
const needsInput = computed(() => effective.value?.mode === 'custom'
  ? !effective.value.userAgent.trim()
  : preset.value?.versionMode === 'fixed'
    && (!preset.value.codexVersion || (preset.value.client === 'desktop' && (!preset.value.desktopVersion || !preset.value.desktopBuild))))
const catalogError = computed(() => options.value?.catalog.sources
  .filter(source => source.error && (!catalog.value || source.source === (catalog.value.entry.client === 'desktop'
    ? 'desktop'
    : 'cli')))
  .map(source => t('settings.clientProfile.catalog.sourceError', {
    source: source.source === 'desktop'
      ? 'Desktop'
      : 'CLI / Exec',
    error: source.error,
  }))
  .join(' · '))
const policy = computed(() => inherited.value
  ? t('settings.clientProfile.catalog.inheritedIdentity')
  : catalog.value
    ? catalog.value.versionMode === 'latest'
      ? t('settings.clientProfile.catalog.followLatest')
      : t('settings.clientProfile.catalog.fixedReleaseIdentity')
    : custom.value
      ? t('settings.clientProfile.fixedIdentity')
      : undefined)
const mode = computed({
  get: () => custom.value
    ? 'custom'
    : catalog.value ? 'catalog' : 'preset',
  set: (value: string) => {
    rememberDraft()
    if (value === 'preset') {
      const selection = presetDraft.value ?? options.value?.presets[0]?.configuration
      if (selection) {
        model.value = {
          ...selection,
          versionMode: 'latest',
          codexVersion: null,
          desktopVersion: null,
          desktopBuild: null,
        }
      }
    }
    else if (value === 'catalog') {
      const entry = defaultEntry()
      const selection = catalogDraft.value ?? (entry
        ? { mode: 'catalog' as const, versionMode: 'latest' as const, entry }
        : undefined)
      if (selection)
        model.value = cloneProfile(selection)
    }
    else if (value === 'custom') {
      model.value = {
        ...(customDraft.value ?? customSelection()),
      }
    }
  },
})
const profileSource = computed({
  get: () => model.value === null
    ? 'global'
    : 'independent',
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
  return {
    ...configuration,
    ...(configuration.mode === 'catalog'
      ? {
          entry: { ...configuration.entry },
        }
      : {}),
  }
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
  const client = preset.value?.client === 'cli' && preset.value.cliEntry === 'exec'
    ? 'exec'
    : preset.value?.client
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
    ...(current
      ? { originator: current.originator, codexVersion: current.codexVersion }
      : {}),
  }
}

async function load(refresh = false) {
  if (loading.value || refreshing.value)
    return
  if (refresh) {
    refreshing.value = true
  }
  else {
    loading.value = true
    loadError.value = ''
  }
  try {
    options.value = await (refresh
      ? refreshClientProfileCatalog()
      : getClientProfileOptions())
    loadError.value = ''
    previewRevision.value++
  }
  catch (error) {
    if (!refresh)
      loadError.value = errorMessage(error)
  }
  finally {
    loading.value = false
    refreshing.value = false
  }
}

watch([
  model,
  () => props.active,
  previewRevision,
], ([configuration, active], _, onCleanup) => {
  let cancelled = false
  preview.value = undefined
  previewing.value = active && !needsInput.value
  if (!active || needsInput.value)
    return
  const timer = setTimeout(async () => {
    try {
      const result = await previewClientProfile({ configuration })
      if (!cancelled)
        preview.value = result
    }
    catch (error) {
      if (!cancelled) {
        ZNotification.error({
          message: errorMessage(error).replaceAll('User-Agent', t('settings.clientProfile.userAgent')),
          deduplicate: true,
        })
      }
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
        :aria-label="t('settings.clientProfile.clientIdentitySource')"
        class="shrink-0"
        :options="[
          { label: t('settings.clientProfile.globalConfiguration'), value: 'global' },
          { label: t('settings.clientProfile.customConfiguration'), value: 'independent' },
        ]"
        :disabled="disabled || loading || !options"
      />
      <slot name="source-extra" />
    </div>
    <ZButton v-if="loadError" size="small" :disabled="disabled || loading || refreshing" @click="load()">
      {{ t('common.actions.refresh') }}
    </ZButton>
    <template v-if="!loadError && !inherited">
      <div class="flex flex-wrap items-center justify-between gap-3">
        <ZSegmented
          v-model="mode"
          :aria-label="t('settings.clientProfile.catalog.configurationMode')"
          :options="[
            { label: t('settings.clientProfile.catalog.presetMode'), value: 'preset', disabled: mode !== 'preset' && !options?.presets.length },
            { label: t('settings.clientProfile.catalog.catalogMode'), value: 'catalog', disabled: !options?.catalog.entries.length && !catalogDraft && !catalog },
            { label: t('settings.clientProfile.catalog.customMode'), value: 'custom' },
          ]"
          :disabled="disabled || loading"
        />
        <ZButton v-if="!custom" size="small" :loading="refreshing" :disabled="disabled || loading || refreshing" @click="load(true)">
          {{ t('settings.clientProfile.catalog.refresh') }}
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
        :disabled="disabled || loading"
        @update:model-value="model = $event"
      />
      <p v-if="!custom && options && !options.catalog.entries.length" role="status" class="m-0 text-z-sm text-z-text-secondary">
        {{ t('settings.clientProfile.catalog.empty') }}
      </p>
      <p v-if="!custom && catalogError" role="alert" class="m-0 break-all text-z-sm text-z-warning">
        {{ t('settings.clientProfile.catalog.refreshFailed', { error: catalogError }) }}
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
      v-if="!loadError"
      :preview="preview"
      :previewing="previewing"
      :needs-version-input="!!needsInput"
      :policy="policy"
      :show-user-agent="inherited || !custom"
      :empty-label="t('settings.clientProfile.catalog.previewAfterUserAgent')"
      show-headers
    >
      <template #actions>
        <ZButton v-if="!inherited && !custom" size="small" :disabled="disabled || previewing || !preview" @click="customize">
          {{ t('settings.clientProfile.catalog.customize') }}
        </ZButton>
      </template>
    </ClientProfilePreviewPanel>
  </div>
</template>
