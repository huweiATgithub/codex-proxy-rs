<script setup lang="ts">
import type { ClientProfilePreview, CustomClientProfileSelection } from '@/api/modules/settings/profiles'
import { ZFormItem, ZInput, ZTextarea } from '@codex-proxy/ui'
import { computed, shallowRef } from 'vue'

const props = defineProps<{ preview?: ClientProfilePreview, disabled: boolean }>()
const model = defineModel<CustomClientProfileSelection>({ required: true })
const manualHeaders = shallowRef<Pick<CustomClientProfileSelection, 'originator' | 'codexVersion'>>({})
const needsManualHeaders = computed(() => model.value.userAgent.trim()
  && (props.preview?.recognized === false || !hasKnownPrefix(model.value.userAgent)))
const userAgent = computed({
  get: () => model.value.userAgent,
  set: (value: string) => {
    if (!hasKnownPrefix(model.value.userAgent))
      manualHeaders.value = { originator: model.value.originator, codexVersion: model.value.codexVersion }
    // 已知客户端的配套头由后端派生，手填值仅用于未知身份的草稿
    model.value = { mode: 'custom', userAgent: value, ...(hasKnownPrefix(value) ? {} : manualHeaders.value) }
  },
})

function hasKnownPrefix(value: string) {
  return /^(?:Codex Desktop|codex-tui|codex_cli_rs|codex_exec)\/\S+(?:\s|$)/.test(value)
}

function updateHeader(key: 'originator' | 'codexVersion', value: string) {
  model.value = { ...model.value, [key]: value }
}
</script>

<template>
  <div class="grid min-w-0 gap-4">
    <ZFormItem label="用户代理" required>
      <ZTextarea v-model="userAgent" :rows="3" :disabled="disabled" aria-label="用户代理" placeholder="填写完整用户代理" spellcheck="false" autocomplete="off" class="font-mono" />
    </ZFormItem>
    <div v-if="needsManualHeaders" class="grid gap-3">
      <p class="m-0 text-cp-sm text-cp-text-secondary">
        未识别配套请求头，请补充客户端标识和 Core 版本
      </p>
      <div class="grid gap-4 sm:grid-cols-2">
        <ZFormItem label="客户端标识 originator" required>
          <ZInput :model-value="model.originator ?? ''" :disabled="disabled" aria-label="客户端标识 originator" @update:model-value="updateHeader('originator', $event)" />
        </ZFormItem>
        <ZFormItem label="Core 版本" required>
          <ZInput :model-value="model.codexVersion ?? ''" :disabled="disabled" aria-label="Core 版本" placeholder="例如 0.158.0" @update:model-value="updateHeader('codexVersion', $event)" />
        </ZFormItem>
      </div>
    </div>
  </div>
</template>
