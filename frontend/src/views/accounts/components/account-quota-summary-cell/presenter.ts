import type { AccountQuotaWindowEntry } from '../../constants'
import type { Account, AccountQuotaWindow, AccountResetCredit } from '@/api'

type AccountModelUsage = Account['usage']['models'][number]

export function weeklyResetRemaining(window: AccountQuotaWindow | undefined, now: number) {
  if (window?.windowSeconds !== 7 * 24 * 60 * 60 || !window.resetAt)
    return null
  const resetAt = Date.parse(window.resetAt)
  if (!Number.isFinite(resetAt))
    return null
  const hours = Math.max(0, Math.floor((resetAt - now) / (60 * 60 * 1000)))
  return `${Math.floor(hours / 24)}d${hours % 24}h`
}

export function soonestResetCredit(credits: readonly AccountResetCredit[]) {
  let soonest: AccountResetCredit | undefined
  let soonestTimestamp = Number.POSITIVE_INFINITY
  for (const credit of credits) {
    if (credit.status !== 'available' || !credit.expiresAt)
      continue
    const timestamp = Date.parse(credit.expiresAt)
    if (Number.isFinite(timestamp) && timestamp < soonestTimestamp) {
      soonest = credit
      soonestTimestamp = timestamp
    }
  }
  return soonest
}

/**
 * 根据账号最近一次模型请求选择对应额度组。
 * 没有专属额度的模型统一回退到 Codex 通用额度。
 */
export function recentlyUsedQuotaEntry(
  entries: readonly AccountQuotaWindowEntry[],
  models: readonly AccountModelUsage[],
) {
  const recentModel = latestModelUsage(models)
  if (recentModel) {
    const modelKey = quotaIdentity(recentModel.model)
    const modelEntry = entries.find(entry => quotaEntryIdentities(entry).has(modelKey))
    if (modelEntry)
      return modelEntry
  }

  return entries.find(entry => entry.windows.some(window => window.limitId === 'codex'))
    ?? entries[0]
}

/** 摘要优先使用当前占用最高的窗口，占用未知时保留首个窗口的重置时间 */
export function representativeQuotaWindow(entry: AccountQuotaWindowEntry | undefined) {
  return entry?.windows.reduce<AccountQuotaWindow | undefined>((representative, window) => {
    if (typeof window.usedPercent !== 'number')
      return representative
    if (
      !representative
      || typeof representative.usedPercent !== 'number'
      || window.usedPercent > representative.usedPercent
    ) {
      return window
    }
    return representative
  }, undefined) ?? entry?.windows[0]
}

function latestModelUsage(models: readonly AccountModelUsage[]) {
  let latest: AccountModelUsage | undefined
  let latestTimestamp = Number.NEGATIVE_INFINITY

  for (const model of models) {
    const timestamp = Date.parse(model.lastUsedAt)
    if (!Number.isFinite(timestamp) || timestamp <= latestTimestamp)
      continue
    latest = model
    latestTimestamp = timestamp
  }

  return latest
}

function quotaEntryIdentities(entry: AccountQuotaWindowEntry) {
  const identities = new Set<string>()
  addQuotaIdentity(identities, entry.label)
  for (const window of entry.windows) {
    addQuotaIdentity(identities, window.limitId)
    addQuotaIdentity(identities, window.limitName)
  }
  return identities
}

function addQuotaIdentity(identities: Set<string>, value: string | null) {
  const identity = quotaIdentity(value)
  if (identity)
    identities.add(identity)
}

function quotaIdentity(value: string | null) {
  return value?.trim().toLowerCase().replace(/[^a-z0-9]+/g, '') ?? ''
}
