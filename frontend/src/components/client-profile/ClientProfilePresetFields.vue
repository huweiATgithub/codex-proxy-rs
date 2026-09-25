<script setup lang="ts">
import type { ClientProfilePreset, ClientProfilePreview, PresetClientProfileSelection } from '@/api/modules/client-profiles'
import { BaseFormItem, BaseInput, BaseSelect } from '@codex-proxy/ui'
import { computed } from 'vue'

const props = defineProps<{
  presets: ClientProfilePreset[]
  preview?: ClientProfilePreview
  previewing: boolean
  disabled: boolean
}>()
const model = defineModel<PresetClientProfileSelection>({ required: true })
const platforms = { macos: 'MacOS', linux: 'Linux', windows: 'Windows' }
const presetOptions = computed(() => props.presets.map(({ configuration }) => ({
  value: `${configuration.platform}-${configuration.client}`,
  label: `${platforms[configuration.platform]} · ${configuration.client === 'desktop' ? 'Desktop' : 'CLI'}`,
})))
const currentPreset = computed(() => props.presets.find(({ configuration }) =>
  configuration.client === model.value.client && configuration.platform === model.value.platform,
))
const selectedPreset = computed({
  get: () => `${model.value.platform}-${model.value.client}`,
  set: (value: string) => {
    const preset = props.presets.find(({ configuration }) => `${configuration.platform}-${configuration.client}` === value)
    if (preset)
      model.value = { ...preset.configuration, versionMode: preset.automaticAvailable ? 'latest' : 'fixed' }
  },
})
const versionMode = computed({
  get: () => model.value.versionMode,
  set: (value: string) => {
    const fixed = value === 'fixed'
    model.value = {
      ...model.value,
      versionMode: fixed ? 'fixed' : 'latest',
      codexVersion: fixed ? props.preview?.codexVersion ?? null : null,
      desktopVersion: fixed ? props.preview?.desktopVersion ?? null : null,
      desktopBuild: fixed ? props.preview?.desktopBuild ?? null : null,
    }
  },
})
const cliEntry = computed({
  get: () => model.value.cliEntry ?? 'core',
  set: (value: string) => {
    if (value === 'core' || value === 'tui' || value === 'exec')
      model.value = { ...model.value, cliEntry: value === 'core' ? null : value }
  },
})
const defaults = computed(() => ({
  ...currentPreset.value?.defaults,
  originator: model.value.cliEntry === 'tui'
    ? 'codex-tui'
    : model.value.cliEntry === 'exec' ? 'codex_exec' : currentPreset.value?.defaults.originator,
}))
const fields = [
  { key: 'originator', label: '客户端标识' },
  { key: 'osType', label: '系统名称' },
  { key: 'osVersion', label: '系统版本' },
  { key: 'arch', label: 'CPU 架构' },
  { key: 'terminal', label: '终端标记' },
] as const

function updateField(key: keyof PresetClientProfileSelection, value: string) {
  model.value = { ...model.value, [key]: value || null }
}
</script>

<template>
  <div class="grid gap-4">
    <div class="grid gap-4 sm:grid-cols-2">
      <BaseFormItem label="客户端预设">
        <BaseSelect v-model="selectedPreset" class="w-full" :options="presetOptions" :disabled="disabled || !presets.length" />
      </BaseFormItem>
      <BaseFormItem label="版本策略">
        <BaseSelect
          v-model="versionMode"
          class="w-full"
          :options="[
            { label: '跟随官方最新版本', value: 'latest', disabled: !currentPreset?.automaticAvailable },
            { label: '固定版本', value: 'fixed' },
          ]"
          :disabled="disabled || previewing"
        />
      </BaseFormItem>
      <BaseFormItem v-if="model.client === 'cli'" label="CLI 入口">
        <BaseSelect
          v-model="cliEntry"
          class="w-full"
          :options="[
            { label: 'Core · 默认身份', value: 'core' },
            { label: 'TUI · 交互式终端', value: 'tui' },
            { label: 'Exec · 非交互执行', value: 'exec' },
          ]"
          :disabled="disabled"
        />
      </BaseFormItem>
    </div>
    <p v-if="currentPreset?.reason" class="m-0 text-cp-sm text-cp-text-secondary">
      {{ currentPreset.reason }}
    </p>
    <div v-if="model.versionMode === 'fixed'" class="grid gap-4 sm:grid-cols-2">
      <BaseFormItem label="Codex Core 版本" required>
        <BaseInput :model-value="model.codexVersion ?? ''" :disabled="disabled" placeholder="例如 0.157.0" @update:model-value="updateField('codexVersion', $event)" />
      </BaseFormItem>
      <template v-if="model.client === 'desktop'">
        <BaseFormItem label="Desktop 版本" required>
          <BaseInput :model-value="model.desktopVersion ?? ''" :disabled="disabled" placeholder="填写该制品的应用版本" @update:model-value="updateField('desktopVersion', $event)" />
        </BaseFormItem>
        <BaseFormItem label="Desktop 构建号" required>
          <BaseInput :model-value="model.desktopBuild ?? ''" :disabled="disabled" placeholder="填写该制品的构建号" @update:model-value="updateField('desktopBuild', $event)" />
        </BaseFormItem>
      </template>
    </div>
    <div class="grid gap-4 sm:grid-cols-2">
      <BaseFormItem v-for="field in fields" :key="field.key" :label="field.label">
        <BaseInput
          :model-value="model[field.key] ?? ''"
          :placeholder="defaults[field.key] ?? ''"
          :disabled="disabled"
          @update:model-value="updateField(field.key, $event)"
        />
      </BaseFormItem>
    </div>
  </div>
</template>
