<script setup lang="ts">
import type { Account } from '@/api'

import { BasePopover } from '@codex-proxy/ui'
import { CircleAlert } from '@lucide/vue'
import { computed, toRef } from 'vue'
import AccountUsageWindow from '@/components/account/account-usage-window/index.vue'
import { quotaWindowPresentation } from '@/components/account/account-usage-window/presenter'
import { useAccountQuotaForecast } from '../../composables/useAccountQuotaForecast'
import { useAccountResetCreditsSnapshot } from '../../composables/useAccountResetCredits'
import { groupedAccountQuotaWindows, visibleSummaryQuotaWindows } from '../../constants'
import { useStatusClock } from '../account-status-badge/useStatusClock'
import AccountCapacityIndicator from '../AccountCapacityIndicator.vue'
import AccountQuotaSummaryEntry from './Entry.vue'
import { recentlyUsedQuotaEntry, representativeQuotaWindow, soonestResetCredit } from './presenter'

const props = defineProps<{
  account: Account
}>()

const quotaWindows = computed(() => props.account.quota.windows.filter(window => window.windowSeconds !== 5 * 60 * 60))
const visibleQuotaWindows = computed(() => visibleSummaryQuotaWindows(quotaWindows.value))
const summaryEntries = computed(() => groupedAccountQuotaWindows(visibleQuotaWindows.value))
const recentUsageEntry = computed(() => recentlyUsedQuotaEntry(
  summaryEntries.value,
  props.account.usage.models,
))
const currentUsageWindow = computed(() => representativeQuotaWindow(recentUsageEntry.value))
const currentUsageDisplay = computed(() => currentUsageWindow.value?.usedPercentDisplay ?? '—')
const currentUsageTextClass = computed(() => currentUsageWindow.value
  ? quotaWindowPresentation(currentUsageWindow.value, '2px').percentTextClass
  : 'text-cp-text-quaternary')
const additionalEntryCount = computed(() => Math.max(summaryEntries.value.length - 1, 0))
const billing = computed(() => props.account.usage.costs.find(cost => cost.currency.toUpperCase() === 'USD'))
const hasForecast = computed(() => props.account.authenticationKind !== 'api_key')
const { report, loading, error } = useAccountQuotaForecast(toRef(() => props.account), hasForecast)
const forecast = computed(() => report.value?.forecasts.find(item => item.period === 'weekly'))
const now = useStatusClock()
const forecastAvailable = computed(() => forecast.value?.source
  && !forecast.value.unavailableReason
  && Date.parse(forecast.value.source.resetAt) > now.value.getTime()
  && (forecast.value.estimatedTokens !== null || forecast.value.estimatedUsd !== null))
const forecastCaveat = computed(() => [
  forecast.value?.lowSample ? '样本较少' : '',
  forecast.value?.incompleteTokens || forecast.value?.incompleteCost ? '数据不全' : '',
].filter(Boolean).join('，'))
const {
  snapshot: resetCredits,
  loading: resetLoading,
  consuming: resetConsuming,
  loadError: resetError,
  ambiguous: resetPending,
  loadCredits,
} = useAccountResetCreditsSnapshot(() => props.account)
const nextCredit = computed(() => soonestResetCredit(resetCredits.value?.credits ?? []))
</script>

<template>
  <div class="box-border grid min-h-16.5 w-full min-w-0 content-center gap-1 py-1.5 text-cp-xs leading-4">
    <div class="grid grid-cols-[auto_minmax(0,1fr)_auto] items-baseline gap-x-1.5 gap-y-0.5">
      <span class="text-cp-text-tertiary" :title="account.usage.windowLabelDisplay">Tokens</span>
      <strong class="font-mono font-heavy tabular-nums text-cp-text" :title="account.usage.windowLabelDisplay">{{ account.usage.totalTokensDisplay }}</strong>
      <span class="inline-flex items-baseline gap-1 justify-self-end" :title="account.usage.windowLabelDisplay">
        <BasePopover v-if="billing && account.usage.costEstimateStatus === 'partial'" trigger="hover-click" placement="top" :hover-delay="240" class="self-center">
          <template #trigger="{ open }">
            <button type="button" aria-label="费用提示" aria-haspopup="dialog" :aria-expanded="open" class="inline-flex size-3.5 shrink-0 cursor-pointer items-center justify-center rounded-sm border-0 bg-transparent p-0 text-cp-warning-text outline-none focus-visible:ring-2 focus-visible:ring-cp-control-outline">
              <CircleAlert class="size-3" aria-hidden="true" />
            </button>
          </template>
          <p role="dialog" aria-label="费用提示" class="m-0 max-w-48 px-3 py-2 text-cp-xs leading-4 text-cp-text-secondary">部分费用</p>
        </BasePopover>
        <strong class="font-mono font-emphasis tabular-nums text-cp-text">{{ billing?.estimatedAmountDisplay ?? '—' }}</strong>
      </span>

      <template v-if="hasForecast">
        <span class="text-cp-text-tertiary">预测</span>
        <template v-if="forecastAvailable && forecast">
          <span class="inline-flex min-w-0 flex-wrap items-baseline gap-x-1.5 gap-y-0.5">
            <span class="font-mono tabular-nums text-cp-text-secondary">{{ forecast.estimatedTokens !== null ? `≈${forecast.estimatedTokensDisplay}` : '—' }}</span>
            <BasePopover v-if="forecastCaveat" trigger="hover-click" placement="top" :hover-delay="240" class="self-center">
              <template #trigger="{ open }">
                <button type="button" aria-label="预测提示" aria-haspopup="dialog" :aria-expanded="open" class="inline-flex size-3.5 shrink-0 cursor-pointer items-center justify-center rounded-sm border-0 bg-transparent p-0 text-cp-warning-text outline-none focus-visible:ring-2 focus-visible:ring-cp-control-outline">
                  <CircleAlert class="size-3" aria-hidden="true" />
                </button>
              </template>
              <p role="dialog" aria-label="预测提示" class="m-0 max-w-48 px-3 py-2 text-cp-xs leading-4 text-cp-text-secondary">{{ forecastCaveat }}</p>
            </BasePopover>
          </span>
          <span class="justify-self-end font-mono tabular-nums text-cp-text-secondary">{{ forecast.estimatedUsd !== null ? `≈${forecast.estimatedUsdDisplay}` : '—' }}</span>
        </template>
        <span v-else class="col-span-2 text-cp-text-tertiary" :title="loading ? '预测读取中' : error ? '预测读取失败' : forecast?.unavailableReason ?? '暂不可预测'">{{ loading ? '读取中' : error ? '失败' : '—' }}</span>
      </template>
    </div>

    <div v-if="account.capabilities.resetCredits" class="flex flex-wrap items-baseline gap-x-1.5 gap-y-0.5 text-cp-text-tertiary">
      <span>重置卡</span>
      <template v-if="resetCredits">
        <span class="font-emphasis tabular-nums text-cp-text-secondary">{{ resetCredits.availableCount }} 张</span>
        <template v-if="resetCredits.availableCount > 0">
          <time v-if="nextCredit?.expiresAtDisplay" :datetime="nextCredit.expiresAt ?? undefined" title="最早到期" class="tabular-nums text-cp-text-secondary">{{ nextCredit.expiresAtDisplay }}</time>
          <span v-else>到期未知</span>
        </template>
      </template>
      <span v-else>未读取</span>
      <button
        type="button"
        class="shrink-0 cursor-pointer rounded-cp-sm border-0 bg-cp-fill-quaternary px-1.5 py-0.5 text-[10px] leading-3 text-cp-link outline-none hover:bg-cp-fill-tertiary focus-visible:ring-2 focus-visible:ring-cp-control-outline disabled:cursor-wait disabled:opacity-60"
        :aria-label="resetCredits ? '更新重置卡' : '读取重置卡'"
        :aria-busy="resetLoading"
        :disabled="resetLoading || resetConsuming"
        @click.stop="loadCredits"
      >
        {{ resetLoading ? '读取中' : resetCredits ? '更新' : '读取' }}
      </button>
      <span v-if="resetPending || resetError" class="text-[10px] text-cp-warning-text">{{ resetPending ? '操作待确认' : '查询失败' }}</span>
    </div>

    <div v-if="recentUsageEntry" class="mt-0.5 flex min-w-0 items-end gap-2">
      <!-- 保留原有详情入口，容量与使用率独立显示在同一行 -->
      <div class="grid min-w-0 flex-1 grid-cols-[minmax(0,1fr)_auto] gap-x-2">
        <AccountQuotaSummaryEntry
          class="col-span-full col-start-1 row-start-1"
          :label="recentUsageEntry.label"
          :summary-label="additionalEntryCount === 0 && recentUsageEntry.windows.length === 1 ? recentUsageEntry.windows[0]?.windowLabelDisplay : undefined"
          :windows="recentUsageEntry.windows"
          :show-percentage="false"
        />
        <span class="z-1 col-start-2 row-start-1 flex items-baseline gap-2 self-start leading-3">
          <span v-if="currentUsageWindow" class="font-mono text-[10px] font-emphasis tabular-nums" :class="currentUsageTextClass" :title="`${currentUsageWindow.labelDisplay} 已用 ${currentUsageDisplay}`">{{ currentUsageDisplay }}</span>
          <AccountCapacityIndicator :capacity="account.capacity" />
        </span>
      </div>
      <span
        v-if="additionalEntryCount > 0"
        class="grid h-5 min-w-5 shrink-0 place-items-center rounded-cp bg-cp-fill-quaternary px-1.5 font-mono text-[9px] font-heavy tabular-nums text-cp-text-tertiary"
        :title="`另有 ${additionalEntryCount} 个额度组，可展开账号查看`"
      >
        +{{ additionalEntryCount }}
      </span>
    </div>
    <div v-else class="mt-0.5 flex items-center justify-between gap-2">
      <span v-if="account.authenticationKind === 'api_key'" class="text-cp-text-tertiary" title="上游额度未提供" aria-label="上游额度未提供">—</span>
      <AccountUsageWindow v-else class="min-w-0 flex-1" variant="compact" />
      <AccountCapacityIndicator :capacity="account.capacity" />
    </div>
  </div>
</template>
