<script setup lang="ts">
import type { AccountInsightIdentity } from '../composables/useAccountInsights'
import type { Account } from '@/api'

import { computed } from 'vue'
import { useI18n } from 'vue-i18n'

import AccountPlanBadge from '@/components/account/AccountPlanBadge.vue'
import { stablePresetVisualToneClass } from '@/utils/color'
import { useAccountInsights } from '../composables/useAccountInsights'
import { accountIdentityLabel } from '../utils/accountIdentity'
import AccountNotesPopover from './AccountNotesPopover.vue'

type AccountIdentity = AccountInsightIdentity & Pick<Account, 'email' | 'planType' | 'planTypeDisplay'>
  & Partial<Pick<Account, 'notes' | 'name' | 'capabilities' | 'upstream'>>

const props = withDefaults(
  defineProps<{
    account: AccountIdentity
    size?: 'md' | 'lg'
    showPlan?: boolean
    showNotes?: boolean
    showSubscription?: boolean
    metaPosition?: 'title' | 'secondary'
    metaSize?: 'xs' | 'sm'
  }>(),
  {
    size: 'md',
    showPlan: false,
    showNotes: false,
    showSubscription: false,
    metaPosition: 'title',
    metaSize: 'sm',
  },
)

const { t } = useI18n({ useScope: 'global' })
const insights = useAccountInsights()
const personalState = computed(() => insights.personalInfo(props.account))
const personalInfo = computed(() => personalState.value.info)
const subscription = computed(() => personalInfo.value?.subscription)
const subscriptionVisible = computed(() => props.showSubscription && props.account.capabilities?.subscription)

const emailText = computed(() => props.account.email?.trim())
const displayTitle = computed(() => {
  const label = accountIdentityLabel(props.account)
  return props.account.authenticationKind !== 'api_key' && label === emailText.value
    ? label.split('@')[0]
    : label
})

const visibleNotes = computed(() => props.showNotes ? props.account.notes : undefined)

const identityTitle = computed(() => {
  const email = emailText.value
  return email && email !== displayTitle.value
    ? `${displayTitle.value}\n${email}`
    : displayTitle.value
})

const secondaryText = computed(() => props.account.authenticationKind !== 'api_key' && emailText.value !== displayTitle.value
  ? emailText.value
  : undefined)

const secondaryClass = computed(() => props.size === 'lg'
  ? 'mt-1 text-z-sm text-z-text-secondary'
  : 'mt-0.5 font-mono text-z-xs text-z-text-quaternary')

const initial = computed(() => displayTitle.value.slice(0, 1).toUpperCase())

const avatarSizeClass = computed(() =>
  props.size === 'lg'
    ? 'size-10 text-z-xl'
    : 'size-9 text-z',
)

const metaGapClass = computed(() => props.metaSize === 'xs'
  ? 'gap-1'
  : 'gap-1.5')

const avatarToneClass = computed(() => {
  const identity = props.account.id || props.account.email || displayTitle.value
  return stablePresetVisualToneClass(identity)
})
</script>

<template>
  <div class="flex min-w-0 items-center gap-3">
    <span
      class="inline-flex shrink-0 items-center justify-center rounded-lg font-extrabold"
      :class="[avatarSizeClass, avatarToneClass]"
    >
      {{ initial }}
    </span>
    <div class="min-w-0 flex-1">
      <div class="flex min-w-0 items-center gap-2">
        <span class="min-w-0 truncate text-z font-heavy text-z-text" :title="identityTitle">
          {{ displayTitle }}
        </span>
        <span
          v-if="metaPosition === 'title' && (showPlan || $slots.meta)"
          class="inline-flex shrink-0 items-center justify-end"
          :class="metaGapClass"
        >
          <slot name="meta" />
          <AccountPlanBadge v-if="showPlan" :upstream="account.upstream" :authentication-kind="account.authenticationKind" :plan-type="account.planType" :plan-type-display="account.planTypeDisplay" :size="metaSize" />
        </span>
      </div>
      <div
        v-if="metaPosition === 'secondary' && (showPlan || $slots.meta)"
        class="mt-0.5 inline-flex min-w-0 items-center"
        :class="metaGapClass"
      >
        <slot name="meta" />
        <AccountPlanBadge v-if="showPlan" :upstream="account.upstream" :authentication-kind="account.authenticationKind" :plan-type="account.planType" :plan-type-display="account.planTypeDisplay" :size="metaSize" />
      </div>
      <AccountNotesPopover v-else-if="visibleNotes" :notes="visibleNotes" class="mt-0.5" />
      <div v-else-if="secondaryText" class="truncate font-emphasis" :class="secondaryClass">
        {{ secondaryText }}
      </div>
      <div v-if="subscriptionVisible" class="mt-0.5 flex flex-wrap items-baseline gap-x-1.5 text-z-xs leading-4 text-z-text-tertiary">
        <time v-if="subscription?.expiresAtDisplay" :datetime="subscription.expiresAt" class="tabular-nums text-z-text-secondary">
          {{ subscription.expiresAtDisplay }}
        </time>
        <span v-else>{{ t('accounts.insights.expiryUnknown') }}</span>
        <button
          type="button"
          class="shrink-0 cursor-pointer rounded-z-sm border-0 bg-z-fill-quaternary px-1.5 py-0.5 text-[10px] leading-3 text-z-link outline-none hover:bg-z-fill-tertiary focus-visible:ring-2 focus-visible:ring-z-control-outline disabled:cursor-wait disabled:opacity-60"
          :aria-label="personalInfo ? t('accounts.insights.refreshSubscription') : t('accounts.insights.readSubscription')"
          :aria-busy="personalState.loading"
          :disabled="personalState.loading"
          @click.stop="insights.loadPersonalInfo(account)"
        >
          {{ personalState.loading ? t('common.status.loading') : personalInfo ? t('accounts.insights.update') : t('accounts.insights.read') }}
        </button>
        <span v-if="personalState.error" role="status" class="text-[10px] text-z-warning-text" :title="personalState.error">{{ personalInfo ? t('accounts.insights.updateFailed') : t('accounts.insights.readFailed') }}</span>
      </div>
    </div>
  </div>
</template>
