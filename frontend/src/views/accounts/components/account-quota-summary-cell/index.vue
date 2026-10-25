<script setup lang="ts">
import type { Account } from '@/api'

import { ZPopover } from '@codex-proxy/ui'
import { CircleAlert } from '@lucide/vue'
import { computed, toRef } from 'vue'
import { useI18n } from 'vue-i18n'
import AccountUsageWindow from '@/components/account/account-usage-window/index.vue'
import { quotaWindowLabel, quotaWindowPresentation } from '@/components/account/account-usage-window/presenter'
import { useAccountQuotaForecast } from '../../composables/useAccountQuotaForecast'
import { useAccountResetCreditsSnapshot } from '../../composables/useAccountResetCredits'
import { groupedAccountQuotaWindows, usageWindowLabels, visibleSummaryQuotaWindows } from '../../constants'
import { useStatusClock } from '../account-status-badge/useStatusClock'
import AccountCapacityIndicator from '../AccountCapacityIndicator.vue'
import AccountQuotaSummaryEntry from './Entry.vue'
import { recentlyUsedQuotaEntry, representativeQuotaWindow, soonestResetCredit, weeklyResetRemaining } from './presenter'

const props = defineProps<{
  account: Account
}>()

const emit = defineEmits<{ showConcurrency: [] }>()
const { t } = useI18n({ useScope: 'global' })

const localUsageOnly = computed(() => props.account.authenticationKind === 'api_key' && !props.account.upstream?.discovered)

// 互联列表只展示此 Key 的消费限额，池内各账号的百分比不能代表整个上游
const quotaWindows = computed(() => props.account.upstream?.discovered
  ? props.account.quota.windows.filter(window => window.amount)
  : props.account.quota.windows.filter(window => window.windowSeconds !== 5 * 60 * 60))
const visibleQuotaWindows = computed(() => visibleSummaryQuotaWindows(quotaWindows.value))
const summaryEntries = computed(() => props.account.upstream?.discovered && visibleQuotaWindows.value.length
  ? [
      {
        key: 'upstream-key',
        label: t('accounts.quota.general'),
        windows: visibleQuotaWindows.value,
      },
    ]
  : groupedAccountQuotaWindows(visibleQuotaWindows.value))
const recentUsageEntry = computed(() => recentlyUsedQuotaEntry(
  summaryEntries.value,
  props.account.usage.models,
))
const currentUsageWindow = computed(() => representativeQuotaWindow(recentUsageEntry.value))
const currentUsageDisplay = computed(() => currentUsageWindow.value?.usedPercentDisplay ?? '—')
const tokenUsageTitle = computed(() => props.account.authenticationKind === 'api_key'
  ? t('accounts.quota.localTokens')
  : t('accounts.quota.totalTokensFor', { windowLabelDisplay: usageWindowLabels.value[props.account.usage.windowKind] }))
const currentUsageTextClass = computed(() => currentUsageWindow.value
  ? quotaWindowPresentation(currentUsageWindow.value, '2px').percentTextClass
  : 'text-z-text-quaternary')
const additionalEntryCount = computed(() => Math.max(summaryEntries.value.length - 1, 0))
const billing = computed(() => props.account.usage.costs.find(cost => cost.currency.toUpperCase() === 'USD'))
const hasForecast = computed(() => props.account.authenticationKind !== 'api_key')
const { report, loading, error } = useAccountQuotaForecast(toRef(() => props.account), hasForecast)
const forecast = computed(() => report.value?.forecasts.find(item => item.period === 'weekly'))
const now = useStatusClock()
const resetRemaining = computed(() => weeklyResetRemaining(currentUsageWindow.value, now.value.getTime()))
const forecastAvailable = computed(() => forecast.value?.source
  && !forecast.value.unavailableReason
  && Date.parse(forecast.value.source.resetAt) > now.value.getTime()
  && (forecast.value.estimatedTokens !== null || forecast.value.estimatedUsd !== null))
const forecastCaveat = computed(() => [
  forecast.value?.lowSample
    ? t('accounts.insights.lowSample')
    : '',
  forecast.value?.incompleteTokens || forecast.value?.incompleteCost
    ? t('accounts.insights.incompleteData')
    : '',
].filter(Boolean).join(' · '))
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
  <div class="box-border grid min-h-16.5 w-full min-w-0 content-center gap-1 py-1.5 text-z-xs leading-4">
    <div class="grid grid-cols-[auto_minmax(0,1fr)_auto] items-baseline gap-x-1.5 gap-y-0.5">
      <span class="text-z-text-tertiary" :title="tokenUsageTitle">{{ t('common.units.tokens') }}</span>
      <strong class="font-mono font-heavy tabular-nums text-z-text" :title="tokenUsageTitle">{{ account.usage.totalTokensDisplay }}</strong>
      <span class="inline-flex items-baseline gap-1 justify-self-end" :title="usageWindowLabels[account.usage.windowKind]">
        <ZPopover v-if="billing && account.usage.costEstimateStatus === 'partial'" trigger="hover-click" placement="top" :hover-delay="240" class="self-center">
          <template #reference="{ open }">
            <button type="button" :aria-label="t('accounts.insights.costHelp')" aria-haspopup="dialog" :aria-expanded="open" class="inline-flex size-3.5 shrink-0 cursor-pointer items-center justify-center rounded-sm border-0 bg-transparent p-0 text-z-warning-text outline-none focus-visible:ring-2 focus-visible:ring-z-control-outline">
              <CircleAlert class="size-3" aria-hidden="true" />
            </button>
          </template>
          <p role="dialog" :aria-label="t('accounts.insights.costHelp')" class="m-0 max-w-48 px-3 py-2 text-z-xs leading-4 text-z-text-secondary">{{ t('accounts.insights.partialCost') }}</p>
        </ZPopover>
        <strong class="font-mono font-emphasis tabular-nums text-z-text">{{ billing?.estimatedAmountDisplay ?? '—' }}</strong>
      </span>

      <template v-if="hasForecast">
        <span class="text-z-text-tertiary">{{ t('accounts.insights.forecast') }}</span>
        <template v-if="forecastAvailable && forecast">
          <span class="inline-flex min-w-0 flex-wrap items-baseline gap-x-1.5 gap-y-0.5">
            <span class="font-mono tabular-nums text-z-text-secondary">{{ forecast.estimatedTokens !== null ? `≈${forecast.estimatedTokensDisplay}` : '—' }}</span>
            <ZPopover v-if="forecastCaveat" trigger="hover-click" placement="top" :hover-delay="240" class="self-center">
              <template #reference="{ open }">
                <button type="button" :aria-label="t('accounts.insights.forecastHelp')" aria-haspopup="dialog" :aria-expanded="open" class="inline-flex size-3.5 shrink-0 cursor-pointer items-center justify-center rounded-sm border-0 bg-transparent p-0 text-z-warning-text outline-none focus-visible:ring-2 focus-visible:ring-z-control-outline">
                  <CircleAlert class="size-3" aria-hidden="true" />
                </button>
              </template>
              <p role="dialog" :aria-label="t('accounts.insights.forecastHelp')" class="m-0 max-w-48 px-3 py-2 text-z-xs leading-4 text-z-text-secondary">{{ forecastCaveat }}</p>
            </ZPopover>
          </span>
          <span class="justify-self-end font-mono tabular-nums text-z-text-secondary">{{ forecast.estimatedUsd !== null ? `≈${forecast.estimatedUsdDisplay}` : '—' }}</span>
        </template>
        <span v-else class="col-span-2 text-z-text-tertiary" :title="loading ? t('accounts.quotaForecast.loadingQuotaForecast') : error || forecast?.unavailableReason || t('accounts.quotaForecast.unpredictableAtMoment')">{{ loading ? t('common.status.loading') : error ? t('common.status.failed') : '—' }}</span>
      </template>
    </div>

    <div v-if="account.capabilities.resetCredits" class="flex flex-wrap items-baseline gap-x-1.5 gap-y-0.5 text-z-text-tertiary">
      <span>{{ t('accounts.insights.resetCredits') }}</span>
      <template v-if="resetCredits">
        <span class="font-emphasis tabular-nums text-z-text-secondary">{{ t('accounts.insights.creditCount', { count: resetCredits.availableCount }) }}</span>
        <template v-if="resetCredits.availableCount > 0">
          <time v-if="nextCredit?.expiresAtDisplay" :datetime="nextCredit.expiresAt ?? undefined" :title="t('accounts.insights.earliestExpiry')" class="tabular-nums text-z-text-secondary">{{ nextCredit.expiresAtDisplay }}</time>
          <span v-else>{{ t('accounts.insights.expiryUnknown') }}</span>
        </template>
      </template>
      <span v-else>{{ t('accounts.insights.notRead') }}</span>
      <button
        type="button"
        class="shrink-0 cursor-pointer rounded-z-sm border-0 bg-z-fill-quaternary px-1.5 py-0.5 text-[10px] leading-3 text-z-link outline-none hover:bg-z-fill-tertiary focus-visible:ring-2 focus-visible:ring-z-control-outline disabled:cursor-wait disabled:opacity-60"
        :aria-label="resetCredits ? t('accounts.quotaReset.refreshCredits') : t('accounts.insights.readResetCredits')"
        :aria-busy="resetLoading"
        :disabled="resetLoading || resetConsuming"
        @click.stop="loadCredits"
      >
        {{ resetLoading ? t('common.status.loading') : resetCredits ? t('accounts.insights.update') : t('accounts.insights.read') }}
      </button>
      <span v-if="resetPending || resetError" class="text-[10px] text-z-warning-text">{{ resetPending ? t('accounts.insights.operationPending') : t('accounts.insights.readFailed') }}</span>
    </div>

    <div v-if="localUsageOnly" class="grid min-w-0 gap-1.5">
      <div class="flex min-w-0 items-start justify-between gap-2 leading-3">
        <span class="truncate text-[10px] font-bold text-z-text-quaternary">{{ usageWindowLabels[account.usage.windowKind] }}</span>
        <AccountCapacityIndicator :capacity="account.capacity" @show-details="emit('showConcurrency')" />
      </div>
      <div class="h-1 w-full rounded-full bg-z-success" aria-hidden="true" />
    </div>
    <div v-else-if="recentUsageEntry" class="mt-0.5 flex min-w-0 items-end gap-2">
      <!-- 保留原有详情入口，容量与使用率独立显示在同一行 -->
      <div class="grid min-w-0 flex-1 grid-cols-[minmax(0,1fr)_auto] gap-x-2">
        <AccountQuotaSummaryEntry
          class="col-span-full col-start-1 row-start-1"
          :label="recentUsageEntry.label"
          :summary-label="additionalEntryCount === 0 && recentUsageEntry.windows.length === 1 && recentUsageEntry.windows[0] ? quotaWindowLabel(recentUsageEntry.windows[0], false) : undefined"
          :windows="recentUsageEntry.windows"
          :show-percentage="false"
        />
        <span class="z-1 col-start-2 row-start-1 flex items-baseline gap-2 self-start leading-3">
          <span v-if="resetRemaining" class="font-mono text-[10px] tabular-nums text-z-text-tertiary" :title="t('accounts.insights.resetTimeHelp', { resetAtDisplay: currentUsageWindow?.resetAtDisplay })" :aria-label="t('accounts.insights.resetRemaining', { remaining: resetRemaining })">{{ resetRemaining }}</span>
          <span v-if="currentUsageWindow" class="font-mono text-[10px] font-emphasis tabular-nums" :class="currentUsageTextClass" :title="t('accounts.insights.quotaUsed', { window: quotaWindowLabel(currentUsageWindow), usedPercent: currentUsageDisplay })">{{ currentUsageDisplay }}</span>
          <AccountCapacityIndicator :capacity="account.capacity" @show-details="emit('showConcurrency')" />
        </span>
      </div>
      <span
        v-if="additionalEntryCount > 0"
        class="grid h-5 min-w-5 shrink-0 place-items-center rounded-z bg-z-fill-quaternary px-1.5 font-mono text-[9px] font-heavy tabular-nums text-z-text-tertiary"
        :title="t('accounts.quota.moreGroups', { count: additionalEntryCount })"
      >
        +{{ additionalEntryCount }}
      </span>
    </div>
    <div v-else class="mt-0.5 flex items-center justify-between gap-2">
      <span v-if="account.authenticationKind === 'api_key'" class="text-z-text-tertiary" :title="t('accounts.quota.unavailableHelp')" :aria-label="t('accounts.quota.unavailableHelp')">—</span>
      <AccountUsageWindow v-else class="min-w-0 flex-1" variant="compact" />
      <AccountCapacityIndicator :capacity="account.capacity" @show-details="emit('showConcurrency')" />
    </div>
  </div>
</template>
