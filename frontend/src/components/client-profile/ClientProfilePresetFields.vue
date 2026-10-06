<script setup lang="ts">
import type { ClientProfilePreset, PresetClientProfileSelection } from '@/api/modules/settings/profiles'

import { ZFormItem, ZInput, ZSelect } from '@codex-proxy/ui'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

const props = withDefaults(defineProps<{
  presets: ClientProfilePreset[]
  disabled: boolean
  maxVersionLag?: number
}>(), {
  maxVersionLag: 10,
})

const { t } = useI18n({ useScope: 'global' })

const model = defineModel<PresetClientProfileSelection | null>({ required: true })
const preset = computed(() => model.value ?? undefined)
const platforms = { macos: 'MacOS', linux: 'Linux', windows: 'Windows' }
const presetOptions = computed(() => props.presets.map(({ configuration }) => ({
  value: `${configuration.platform}-${configuration.client}`,
  label: `${platforms[configuration.platform]} · ${configuration.client === 'desktop' ? 'Desktop' : 'CLI'}`,
})))
const currentPreset = computed(() => props.presets.find(({ configuration }) =>
  configuration.client === preset.value?.client && configuration.platform === preset.value?.platform,
))
const selectedPreset = computed({
  get: () => {
    const selection = preset.value
    return selection ? `${selection.platform}-${selection.client}` : ''
  },
  set: (value: string) => {
    const selected = props.presets.find(({ configuration }) => `${configuration.platform}-${configuration.client}` === value)
    if (selected) {
      model.value = { ...selected.configuration, versionMode: selected.automaticAvailable ? 'latest' : 'fixed' }
    }
  },
})
const cliEntry = computed({
  get: () => preset.value?.cliEntry ?? 'core',
  set: (value: string) => {
    if (preset.value && (value === 'core' || value === 'tui' || value === 'exec')) {
      model.value = { ...preset.value, cliEntry: value === 'core' ? null : value }
    }
  },
})
const versionLag = computed({
  get: () => preset.value?.versionLag == null ? '' : String(preset.value.versionLag),
  set: (value: string) => {
    if (preset.value)
      model.value = { ...preset.value, versionLag: value === '' ? null : Number(value) }
  },
})
const versionLagError = computed(() => {
  const value = preset.value?.versionLag
  return value != null && (!Number.isInteger(value) || value < 1 || value > props.maxVersionLag)
    ? t('settings.clientProfile.enterIntegerFromTo', { version: props.maxVersionLag })
    : ''
})
const terminal = computed({
  get: () => preset.value?.terminal ?? '',
  set: (value: string) => {
    if (preset.value)
      model.value = { ...preset.value, terminal: value === '' ? null : value }
  },
})
const osVersion = computed({
  get: () => preset.value?.osVersion ?? '',
  set: (value: string) => {
    if (preset.value)
      model.value = { ...preset.value, osVersion: value === '' ? null : value }
  },
})
</script>

<template>
  <div class="grid gap-4">
    <div class="grid gap-4" :class="preset?.client === 'cli' ? 'sm:grid-cols-2' : undefined">
      <ZFormItem :label="t('settings.clientProfile.clientDefault')">
        <ZSelect v-model="selectedPreset" class="w-full" :options="presetOptions" :disabled="disabled || !presets.length" />
      </ZFormItem>
      <ZFormItem v-if="preset?.client === 'cli'" :label="t('settings.clientProfile.cliEntry')">
        <ZSelect
          v-model="cliEntry"
          class="w-full"
          :options="[
            { label: t('settings.clientProfile.coreDefaultIdentity'), value: 'core' },
            { label: t('settings.clientProfile.tuiInteractiveTerminal'), value: 'tui' },
            { label: t('settings.clientProfile.execNonInteractiveExecution'), value: 'exec' },
          ]"
          :disabled="disabled"
        />
      </ZFormItem>
    </div>
    <p v-if="currentPreset?.reason" class="m-0 text-cp-sm text-cp-text-secondary">
      {{ currentPreset.reason }}
    </p>
    <div class="grid gap-4 sm:grid-cols-2">
      <ZFormItem :label="t('settings.clientProfile.versionLag')" :error="versionLagError">
        <ZInput
          v-model="versionLag"
          :aria-label="t('settings.clientProfile.versionLag')"
          type="number"
          inputmode="numeric"
          min="1"
          :max="maxVersionLag"
          step="1"
          :placeholder="t('settings.clientProfile.versionLagHelp', { version: maxVersionLag })"
          :disabled="disabled"
        />
      </ZFormItem>
      <div class="grid gap-4" :class="{ 'sm:grid-cols-2': preset?.client === 'desktop' }">
        <ZFormItem :label="t('settings.clientProfile.terminalIdentification')">
          <ZInput
            v-model="terminal"
            :disabled="disabled"
            :aria-label="t('settings.clientProfile.terminalIdentification')"
            :placeholder="t('settings.clientProfile.leaveBlankToUseUnknown')"
            maxlength="128"
            spellcheck="false"
            autocomplete="off"
            class="font-mono"
          />
        </ZFormItem>
        <ZFormItem v-if="preset?.client === 'desktop'" :label="t('settings.clientProfile.systemVersion')">
          <ZInput
            v-model="osVersion"
            :disabled="disabled"
            :aria-label="t('settings.clientProfile.systemVersion')"
            :placeholder="currentPreset?.defaults.osVersion ?? undefined"
            maxlength="128"
            spellcheck="false"
            autocomplete="off"
            class="font-mono"
          />
        </ZFormItem>
      </div>
    </div>
  </div>
</template>
