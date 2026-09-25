<script setup lang="ts">
import type { ClientProfilePreview, CustomClientProfileSelection } from '@/api/modules/client-profiles'
import { BaseButton, BaseFormItem, BaseInput, BaseTextarea } from '@codex-proxy/ui'
import { computed, shallowRef } from 'vue'
import { useCopyText } from '@/composables/useCopyText'

const props = defineProps<{ preview?: ClientProfilePreview, disabled: boolean }>()
const model = defineModel<CustomClientProfileSelection>({ required: true })
const manualHeaders = shallowRef<Pick<CustomClientProfileSelection, 'originator' | 'codexVersion'>>({})
const copyText = useCopyText()
const needsManualHeaders = computed(() => model.value.userAgent.trim()
  && (props.preview?.recognized === false || !hasKnownPrefix(model.value.userAgent)))

function hasKnownPrefix(userAgent: string) {
  return /^(?:Codex Desktop|codex-tui|codex_cli_rs|codex_exec)\/\S+(?:\s|$)/.test(userAgent)
}

function updateCustom(key: 'userAgent' | 'originator' | 'codexVersion', value: string) {
  const current = model.value
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
</script>

<template>
  <div class="grid gap-4">
    <BaseFormItem label="User-Agent" required>
      <template #extra>
        <BaseButton size="sm" :disabled="!model.userAgent" @click="copyText(model.userAgent, { successText: '已复制 User-Agent' })">
          复制
        </BaseButton>
      </template>
      <BaseTextarea
        :model-value="model.userAgent"
        :rows="4"
        :disabled="disabled"
        placeholder="粘贴完整 User-Agent，或从客户端预设基于当前身份自定义"
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
          <BaseInput :model-value="model.originator ?? ''" :disabled="disabled" @update:model-value="updateCustom('originator', $event)" />
        </BaseFormItem>
        <BaseFormItem label="Core version" required>
          <BaseInput :model-value="model.codexVersion ?? ''" :disabled="disabled" placeholder="例如 0.157.0" @update:model-value="updateCustom('codexVersion', $event)" />
        </BaseFormItem>
      </div>
    </div>
    <p v-else-if="preview?.recognized" class="m-0 text-cp-sm text-cp-success">
      已识别 {{ preview.originator }} · Core {{ preview.codexVersion }}，配套请求头已同步
    </p>
  </div>
</template>
