<script setup lang="ts">
import type { ClientProfilePreset, PresetClientProfileSelection } from '@/api/modules/settings/profiles'
import { ZFormItem, ZInput, ZSelect } from '@codex-proxy/ui'
import { computed } from 'vue'

const props = withDefaults(defineProps<{
  presets: ClientProfilePreset[]
  disabled: boolean
  maxVersionLag?: number
}>(), {
  maxVersionLag: 10,
})
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
    ? `请输入 1～${props.maxVersionLag} 的正整数`
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
      <ZFormItem label="客户端预设">
        <ZSelect v-model="selectedPreset" class="w-full" :options="presetOptions" :disabled="disabled || !presets.length" />
      </ZFormItem>
      <ZFormItem v-if="preset?.client === 'cli'" label="CLI 入口">
        <ZSelect
          v-model="cliEntry"
          class="w-full"
          :options="[
            { label: 'Core · 默认身份', value: 'core' },
            { label: 'TUI · 交互式终端', value: 'tui' },
            { label: 'Exec · 非交互执行', value: 'exec' },
          ]"
          :disabled="disabled"
        />
      </ZFormItem>
    </div>
    <p v-if="currentPreset?.reason" class="m-0 text-cp-sm text-cp-text-secondary">
      {{ currentPreset.reason }}
    </p>
    <div class="grid gap-4 sm:grid-cols-2">
      <ZFormItem label="版本滞后" :error="versionLagError">
        <ZInput
          v-model="versionLag"
          aria-label="版本滞后"
          type="number"
          inputmode="numeric"
          min="1"
          :max="maxVersionLag"
          step="1"
          :placeholder="`滞后 1～${maxVersionLag} 个版本，留空不滞后`"
          :disabled="disabled"
        />
      </ZFormItem>
      <div class="grid gap-4" :class="{ 'sm:grid-cols-2': preset?.client === 'desktop' }">
        <ZFormItem label="终端标识">
          <ZInput
            v-model="terminal"
            :disabled="disabled"
            aria-label="终端标识"
            placeholder="留空使用 unknown"
            maxlength="128"
            spellcheck="false"
            autocomplete="off"
            class="font-mono"
          />
        </ZFormItem>
        <ZFormItem v-if="preset?.client === 'desktop'" label="系统版本">
          <ZInput
            v-model="osVersion"
            :disabled="disabled"
            aria-label="系统版本"
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
