<script setup lang="ts">
import type { ClientProfilePreview } from '@/api/modules/client-profiles'
import { BaseSkeleton } from '@codex-proxy/ui'
import { formatDateTime } from '@/utils/date'

withDefaults(defineProps<{
  preview?: Pick<ClientProfilePreview, 'userAgent' | 'versionSource' | 'checkedAt' | 'error'> & Partial<Pick<ClientProfilePreview, 'originator' | 'codexVersion' | 'desktopVersion'>>
  previewing: boolean
  needsVersionInput: boolean
  error: string
  label?: string
  policy?: string
  showUserAgent?: boolean
  showHeaders?: boolean
  emptyLabel?: string
}>(), {
  showUserAgent: true,
  showHeaders: false,
  emptyLabel: '填写版本后预览',
})
</script>

<template>
  <div class="grid min-h-20 min-w-0 content-center gap-3 rounded-cp bg-cp-fill-quaternary p-4" aria-live="polite" :aria-busy="previewing">
    <div v-if="label && showUserAgent" class="flex flex-wrap items-center justify-between gap-2">
      <span class="text-cp-sm text-cp-text-secondary">{{ label }}</span>
      <slot name="actions" />
    </div>
    <p v-if="needsVersionInput" class="m-0 text-cp-sm text-cp-text-tertiary">
      {{ emptyLabel }}
    </p>
    <div v-else-if="previewing" class="grid gap-2" role="status" aria-label="正在解析客户端身份">
      <div class="flex h-lh items-center text-cp-sm" aria-hidden="true">
        <BaseSkeleton shape="text" class="w-4/5" />
      </div>
      <div class="flex h-lh items-center text-cp-xs" aria-hidden="true">
        <BaseSkeleton shape="text" class="w-52 max-w-full" />
      </div>
    </div>
    <p v-else-if="error" role="alert" class="m-0 text-cp-sm text-cp-error">
      {{ error }}
    </p>
    <template v-else-if="preview">
      <code v-if="showUserAgent" class="break-all text-cp-sm text-cp-text">{{ preview.userAgent }}</code>
      <dl v-if="showHeaders && preview.originator" class="m-0 flex flex-wrap gap-x-5 gap-y-2 text-cp-xs">
        <div class="flex min-w-0 flex-wrap gap-x-2">
          <dt class="text-cp-text-tertiary">
            originator
          </dt>
          <dd class="m-0 break-all font-mono text-cp-text-secondary">
            {{ preview.originator }}
          </dd>
        </div>
        <div class="flex min-w-0 flex-wrap gap-x-2">
          <dt class="text-cp-text-tertiary">
            Core version
          </dt>
          <dd class="m-0 break-all font-mono text-cp-text-secondary">
            {{ preview.codexVersion }}
          </dd>
        </div>
        <div v-if="preview.desktopVersion" class="flex min-w-0 flex-wrap gap-x-2">
          <dt class="text-cp-text-tertiary">
            Desktop
          </dt>
          <dd class="m-0 break-all font-mono text-cp-text-secondary">
            {{ preview.desktopVersion }}
          </dd>
        </div>
      </dl>
      <p v-if="policy" class="m-0 text-cp-xs text-cp-text-tertiary">
        {{ policy }}
      </p>
      <p v-else class="m-0 text-cp-xs text-cp-text-tertiary">
        {{ preview.versionSource === 'custom' ? '固定身份' : '官方版本自动更新' }}
        <template v-if="preview.versionSource === 'official'">
          · {{ preview.checkedAt ? `检查于 ${formatDateTime(preview.checkedAt)}` : '待检查' }}
        </template>
      </p>
      <p v-if="preview.error && preview.versionSource !== 'custom'" :title="preview.error" class="m-0 text-cp-sm text-cp-warning">
        更新失败 · 沿用上次版本
      </p>
    </template>
  </div>
</template>
