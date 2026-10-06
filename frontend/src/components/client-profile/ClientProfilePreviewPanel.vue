<script setup lang="ts">
import type { ClientProfilePreview } from '@/api/modules/settings/profiles'

import { ZEmpty, ZSkeleton } from '@codex-proxy/ui'
import { useI18n } from 'vue-i18n'

withDefaults(defineProps<{
  preview?: Pick<ClientProfilePreview, 'userAgent' | 'versionSource' | 'checkedAtDisplay' | 'error'> & Partial<Pick<ClientProfilePreview, 'originator' | 'codexVersion' | 'versionLag'>>
  previewing: boolean
  needsVersionInput: boolean
  error: string
  policy?: string
  showHeaders?: boolean
  showUserAgent?: boolean
  emptyLabel?: string
}>(), { showUserAgent: true })

const { t } = useI18n({ useScope: 'global' })
</script>

<template>
  <p v-if="needsVersionInput" role="status" class="m-0 text-cp-sm text-cp-text-tertiary">
    {{ emptyLabel ?? t('settings.clientProfile.previewAfterFillingInVersion') }}
  </p>
  <ZEmpty v-else-if="error && !previewing" :title="t('settings.clientProfile.previewIsNotAvailableYet')" :description="error" size="small" surface="none" role="alert" />
  <div
    v-else-if="previewing || preview"
    class="grid min-h-20 min-w-0 content-center gap-2 rounded-cp bg-cp-fill-quaternary p-4"
    aria-live="polite"
    :aria-busy="previewing"
  >
    <template v-if="previewing">
      <div class="flex h-lh items-center text-cp-sm" role="status" :aria-label="t('settings.clientProfile.resolvingClientIdentity')">
        <ZSkeleton shape="text" class="w-4/5" aria-hidden="true" />
      </div>
      <div class="flex h-lh items-center text-cp-xs" aria-hidden="true">
        <ZSkeleton shape="text" class="w-52 max-w-full" />
      </div>
    </template>
    <template v-else-if="preview">
      <code v-if="showUserAgent" class="break-all text-cp-sm text-cp-text">{{ preview.userAgent }}</code>
      <p v-if="showHeaders" class="m-0 break-all text-cp-xs text-cp-text-secondary">
        {{ preview.originator }} · Core {{ preview.codexVersion }}
      </p>
      <p class="m-0 text-cp-xs text-cp-text-tertiary">
        {{ policy ?? (preview.versionSource === 'custom' ? t('settings.clientProfile.fixedIdentity') : t('settings.clientProfile.automaticUpdates')) }}
        <template v-if="preview.versionSource === 'official'">
          <template v-if="preview.versionLag">
            {{ t('settings.clientProfile.versionsBehind', { version: preview.versionLag }) }}
          </template>
          · {{ preview.checkedAtDisplay ? t('settings.clientProfile.checked', { checkedAtDisplay: preview.checkedAtDisplay }) : t('settings.clientProfile.toBeChecked') }}
        </template>
      </p>
      <p v-if="preview.error && preview.versionSource !== 'custom'" :title="preview.error" class="m-0 text-cp-sm text-cp-warning">
        {{ preview.versionSource === 'catalog' ? t('settings.clientProfile.catalog.updateFailedUseSavedIdentity') : t('settings.clientProfile.updateFailedUseLastVersion') }}
      </p>
      <div v-if="$slots.actions" class="flex flex-wrap items-center gap-2">
        <slot name="actions" />
      </div>
    </template>
  </div>
</template>
