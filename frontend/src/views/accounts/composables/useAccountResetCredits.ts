import type { AccountInsightIdentity } from './useAccountInsights'
import type { AccountResetCredit } from '@/api'
import { ZNotification } from '@codex-proxy/ui'
import { computed, shallowReactive, shallowRef, watch } from 'vue'

import {
  consumeAccountResetCredit,
  getAccountResetCredits,
} from '@/api'
import { ApiError } from '@/api/request'
import { errorMessage, generateRequestId } from '@/utils/operation'
import { accountInsightKey } from './useAccountInsights'

interface PendingResetCreditOperation {
  accountId: string
  creditId?: string
  credit?: AccountResetCredit
  redeemRequestId: string
  hasTransportFailure: boolean
}

interface ResetCreditsSnapshot {
  identity: string
  credits: AccountResetCredit[]
  availableCount: number
}

interface ResetCreditsSession {
  accountId: string
  identity: string
  snapshot: ResetCreditsSnapshot | null
  pendingOperation: PendingResetCreditOperation | null
  consuming: boolean
  loading: boolean
  loadError: string
  loadIdentity: string
  loadSequence: number
  loadController?: AbortController
  loadPromise?: Promise<void>
}

// 库存仍以主动查询的上游结果为准；未决操作和消费锁必须跨展开行卸载存续。
const sessionsByAccountId = shallowReactive(new Map<string, ResetCreditsSession>())

function getResetCreditsSession(accountId: string) {
  let session = sessionsByAccountId.get(accountId)
  if (!session) {
    session = shallowReactive<ResetCreditsSession>({
      accountId,
      identity: '',
      snapshot: null,
      pendingOperation: null,
      consuming: false,
      loading: false,
      loadError: '',
      loadIdentity: '',
      loadSequence: 0,
    })
    sessionsByAccountId.set(accountId, session)
  }
  return session
}

function useResetCreditsSession(account: () => AccountInsightIdentity) {
  const session = computed(() => getResetCreditsSession(account().id))
  watch([session, () => accountInsightKey(account())], ([target, identity]) => {
    // 换绑后隐藏旧库存，但未决消费和幂等标识仍属于原有内部账号会话
    target.identity = identity
  }, { immediate: true, flush: 'sync' })
  return session
}

export function useAccountResetCreditsSnapshot(account: () => AccountInsightIdentity) {
  const session = useResetCreditsSession(account)
  return {
    snapshot: computed(() => session.value.snapshot?.identity === session.value.identity ? session.value.snapshot : null),
    loading: computed(() => session.value.loading),
    consuming: computed(() => session.value.consuming),
    loadError: computed(() => session.value.loadIdentity === session.value.identity ? session.value.loadError : ''),
    ambiguous: computed(() => session.value.pendingOperation?.hasTransportFailure === true),
    loadCredits: () => session.value.consuming
      ? Promise.resolve()
      : loadSessionCredits(session.value),
  }
}

function loadSessionCredits(session: ResetCreditsSession, silent = false) {
  if (session.loadPromise)
    return session.loadPromise
  // 行内按钮和弹窗共享同一次读取，不取消彼此的请求或改动消费会话
  const promise = readSessionCredits(session, silent).finally(() => {
    if (session.loadPromise === promise)
      session.loadPromise = undefined
  })
  session.loadPromise = promise
  return promise
}

async function readSessionCredits(session: ResetCreditsSession, silent: boolean) {
  const sequence = ++session.loadSequence
  const identity = session.identity
  session.loadController?.abort()
  const controller = new AbortController()
  session.loadController = controller
  session.loading = true
  session.loadError = ''
  session.loadIdentity = identity
  try {
    const result = await getAccountResetCredits({ accountId: session.accountId }, { silent, signal: controller.signal })
    if (sequence !== session.loadSequence)
      return
    session.snapshot = {
      identity,
      credits: result.credits,
      availableCount: Math.max(0, result.availableCount),
    }
  }
  catch (error: unknown) {
    if (sequence === session.loadSequence)
      session.loadError = errorMessage(error)
  }
  finally {
    if (sequence === session.loadSequence)
      session.loading = false
  }
}

export function useAccountResetCredits(options: {
  account: () => AccountInsightIdentity
  onConsumed: (accountId: string) => void
  capabilities: () => { consumeResetCredit: boolean }
}) {
  const session = useResetCreditsSession(options.account)
  const snapshot = computed(() => session.value.snapshot?.identity === session.value.identity ? session.value.snapshot : null)
  const credits = computed(() => snapshot.value?.credits ?? [])
  const availableCount = computed(() => snapshot.value?.availableCount ?? 0)
  const hasSnapshot = computed(() => snapshot.value !== null)
  const loading = computed(() => session.value.loading)
  const consuming = computed(() => session.value.consuming)
  const loadError = computed(() => session.value.loadIdentity === session.value.identity ? session.value.loadError : '')
  const showConfirm = shallowRef(false)
  const selectedCreditId = shallowRef('')
  const pendingOperation = computed(() => session.value.pendingOperation)

  const availableCredits = computed(() =>
    credits.value.filter(credit => credit.status === 'available'),
  )
  const selectedCredit = computed(() =>
    availableCredits.value.find(credit => credit.id === selectedCreditId.value),
  )
  const consumptionCredit = computed(() =>
    pendingOperation.value ? pendingOperation.value.credit : selectedCredit.value,
  )
  const ambiguous = computed(() => pendingOperation.value?.hasTransportFailure === true)
  const canStartConsume = computed(() =>
    hasSnapshot.value && !loadError.value && availableCount.value > 0
    && (selectedCredit.value !== undefined || availableCredits.value.length === 0),
  )
  const canRequestConsume = computed(() =>
    options.capabilities().consumeResetCredit && (ambiguous.value || canStartConsume.value),
  )

  function reconcileSelectedCredit() {
    const operation = pendingOperation.value
    if (operation) {
      selectedCreditId.value = operation.creditId ?? ''
      return
    }

    if (!selectedCredit.value)
      selectedCreditId.value = ''
  }

  function selectCredit(creditId: string) {
    if (pendingOperation.value || consuming.value)
      return
    if (!availableCredits.value.some(credit => credit.id === creditId))
      return
    selectedCreditId.value = creditId
  }

  function applyConfirmedConsumption(target: ResetCreditsSession, operation: PendingResetCreditOperation) {
    const snapshot = target.snapshot
    if (!snapshot)
      return
    const creditIndex = snapshot.credits.findIndex(credit => credit.id === operation.creditId)
    const nextCredits = creditIndex < 0
      ? snapshot.credits
      : snapshot.credits.filter((_, index) => index !== creditIndex)
    target.snapshot = {
      ...snapshot,
      credits: nextCredits,
      availableCount: Math.max(0, snapshot.availableCount - 1),
    }
  }

  function requestConsume() {
    if (consuming.value || loading.value || !canRequestConsume.value)
      return
    showConfirm.value = true
  }

  function cancelConsume() {
    if (consuming.value)
      return
    showConfirm.value = false
  }

  async function confirmConsume(): Promise<boolean> {
    const target = session.value
    if (!options.capabilities().consumeResetCredit || !showConfirm.value || target.consuming || target.loading)
      return false

    const credit = selectedCredit.value
    // 确认发送时才建立操作；仅打开或取消确认弹窗不占用账号的消费状态。
    const operation = target.pendingOperation ?? (canStartConsume.value
      ? {
          accountId: target.accountId,
          creditId: credit?.id,
          credit,
          redeemRequestId: generateRequestId(),
          hasTransportFailure: false,
        }
      : null)
    if (!operation)
      return false
    target.pendingOperation = operation
    target.consuming = true
    try {
      // 不可逆消费由当前会话区分已确认失败与结果未知，不能先弹出可重试的通用错误。
      const result = await consumeAccountResetCredit({
        accountId: operation.accountId,
        creditId: operation.creditId,
        redeemRequestId: operation.redeemRequestId,
      }, { silent: true })
      const confirmed = result.code === 'reset'
        || (result.code === 'already_redeemed' && operation.hasTransportFailure)
      target.pendingOperation = null
      if (!confirmed) {
        ZNotification.error({ message: resetResultMessage(result.code) })
        await loadSessionCredits(target, true)
        return false
      }

      const successMessage = result.code === 'already_redeemed'
        ? '上次重置已完成'
        : '额度已重置'
      // 消费已确认就结束交互，账号页负责额度回读，避免收起展开行后丢失更新。
      if (session.value === target)
        showConfirm.value = false
      options.onConsumed(operation.accountId)
      ZNotification.success({ message: successMessage })
      applyConfirmedConsumption(target, operation)
      await loadSessionCredits(target, true)
      return true
    }
    catch (error: unknown) {
      if (isAmbiguousConsumeError(error)) {
        target.pendingOperation = {
          ...operation,
          hasTransportFailure: true,
        }
        ZNotification.warning({ message: '消费结果暂不确定，重试会复用同一个请求标识', ...({ duration: 5000 }) })
      }
      else {
        target.pendingOperation = null
        ZNotification.error({ message: errorMessage(error, '额度重置失败') })
        await loadSessionCredits(target, true)
      }
      return false
    }
    finally {
      target.consuming = false
    }
  }

  watch(
    [session, () => session.value.identity],
    () => {
      selectedCreditId.value = ''
      showConfirm.value = false
    },
    { immediate: true, flush: 'sync' },
  )

  watch([credits, pendingOperation], reconcileSelectedCredit, { immediate: true, flush: 'sync' })
  watch(consuming, (isConsuming) => {
    if (!isConsuming)
      showConfirm.value = false
  }, { flush: 'sync' })

  return {
    credits,
    availableCredits,
    availableCount,
    selectedCreditId,
    consumptionCredit,
    canRequestConsume,
    hasSnapshot,
    loading,
    consuming,
    loadError,
    ambiguous,
    showConfirm,
    loadCredits: () => loadSessionCredits(session.value),
    selectCredit,
    requestConsume,
    cancelConsume,
    confirmConsume,
  }
}

function isAmbiguousConsumeError(error: unknown) {
  if (!(error instanceof ApiError))
    return false
  return error.code === 50202
    || error.status === 0
    || error.status === 408
    || error.kind === 'timeout'
    || error.kind === 'network'
}

function resetResultMessage(code: string) {
  switch (code) {
    case 'already_redeemed':
      return '该重置操作已被处理，请先刷新重置卡列表'
    case 'no_credit':
      return '当前没有可用的主动重置卡'
    case 'nothing_to_reset':
      return '当前额度窗口不需要重置'
    default:
      return `上游未执行额度重置：${code}`
  }
}
