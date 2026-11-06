<script setup lang="ts">
import type { ClientProfilePreview, CustomClientProfileSelection } from '@/api/modules/settings/profiles'

import { ZFormItem, ZInput, ZTextarea } from '@codex-proxy/ui'
import { computed, shallowRef } from 'vue'
import { useI18n } from 'vue-i18n'

const props = defineProps<{ preview?: ClientProfilePreview, disabled: boolean }>()
const { t } = useI18n({ useScope: 'global' })
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
    model.value = {
      mode: 'custom',
      userAgent: value,
      ...(hasKnownPrefix(value)
        ? {}
        : manualHeaders.value),
    }
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
    <ZFormItem :label="t('settings.clientProfile.userAgent')" required>
      <ZTextarea v-model="userAgent" :rows="3" :disabled="disabled" :aria-label="t('settings.clientProfile.userAgent')" :placeholder="t('settings.clientProfile.userAgentPlaceholder')" spellcheck="false" autocomplete="off" class="font-mono" />
    </ZFormItem>
    <div v-if="needsManualHeaders" class="grid gap-3">
      <p class="m-0 text-z-sm text-z-text-secondary">
        {{ t('settings.clientProfile.custom.headersRequired') }}
      </p>
      <div class="grid gap-4 sm:grid-cols-2">
        <ZFormItem :label="t('settings.clientProfile.custom.originator')" required>
          <ZInput :model-value="model.originator ?? ''" :disabled="disabled" :aria-label="t('settings.clientProfile.custom.originator')" @update:model-value="updateHeader('originator', $event)" />
        </ZFormItem>
        <ZFormItem :label="t('settings.clientProfile.custom.coreVersion')" required>
          <ZInput :model-value="model.codexVersion ?? ''" :disabled="disabled" :aria-label="t('settings.clientProfile.custom.coreVersion')" :placeholder="t('settings.clientProfile.custom.coreVersionPlaceholder')" @update:model-value="updateHeader('codexVersion', $event)" />
        </ZFormItem>
      </div>
    </div>
  </div>
</template>
