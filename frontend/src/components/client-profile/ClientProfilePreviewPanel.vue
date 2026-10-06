<script setup lang="ts">
import type { ClientProfilePreview } from '@/api/modules/settings/profiles'
import { ZSkeleton } from '@codex-proxy/ui'

withDefaults(defineProps<{
  preview?: Pick<ClientProfilePreview, 'userAgent' | 'versionSource' | 'checkedAtDisplay' | 'error'> & Partial<Pick<ClientProfilePreview, 'originator' | 'codexVersion' | 'versionLag'>>
  previewing: boolean
  needsVersionInput: boolean
  error: string
  policy?: string
  showHeaders?: boolean
  showUserAgent?: boolean
  emptyLabel?: string
}>(), { showUserAgent: true, emptyLabel: '填写版本后预览' })
</script>

<template>
  <p v-if="needsVersionInput" role="status" class="m-0 text-cp-sm text-cp-text-tertiary">
    {{ emptyLabel }}
  </p>
  <p v-else-if="error && !previewing" role="alert" class="m-0 text-cp-sm text-cp-error">
    {{ error }}
  </p>
  <div
    v-else-if="previewing || preview"
    class="grid min-h-20 min-w-0 content-center gap-2 rounded-cp bg-cp-fill-quaternary p-4"
    aria-live="polite"
    :aria-busy="previewing"
  >
    <template v-if="previewing">
      <div class="flex h-lh items-center text-cp-sm" role="status" aria-label="正在解析客户端身份">
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
        {{ policy ?? (preview.versionSource === 'custom' ? '固定身份' : '自动更新') }}
        <template v-if="preview.versionSource === 'official'">
          <template v-if="preview.versionLag">
            · 滞后 {{ preview.versionLag }} 版
          </template>
          · {{ preview.checkedAtDisplay ? `检查于 ${preview.checkedAtDisplay}` : '待检查' }}
        </template>
      </p>
      <p v-if="preview.error && preview.versionSource !== 'custom'" :title="preview.error" class="m-0 text-cp-sm text-cp-warning">
        {{ preview.versionSource === 'catalog' ? '更新失败 · 沿用已保存身份' : '更新失败 · 沿用上次版本' }}
      </p>
      <div v-if="$slots.actions" class="flex flex-wrap items-center gap-2">
        <slot name="actions" />
      </div>
    </template>
  </div>
</template>
