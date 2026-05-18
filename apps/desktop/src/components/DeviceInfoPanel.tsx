import { useQuery } from '@tanstack/react-query'
import { Spin, Typography } from '@arco-design/web-react'
import { DeviceInfoFull, Platform, Transport, getDeviceInfo } from '@/lib/ipc'
import styles from './DeviceInfoPanel.module.scss'

const { Text } = Typography

interface Props {
  deviceId: string
  platform: Platform
  /// Connection-state hint (e.g. "usb" / "network(...)"). Goes into the
  /// query key so wifi-only and USB queries are *separate* — when the
  /// user plugs in USB after selecting a wifi-only device, we want a
  /// fresh fetch instead of resurrecting the old (often hung) lockdown
  /// query. Optional for back-compat; falls back to keying on id alone.
  state?: string
  /// Transport currently in use. Shown as a "Connection" row at the
  /// top so the dropdown can stay clean (no inline USB/Wi-Fi badges).
  transport?: Transport
  /// Whether this entry can actually be sampled. iOS Wi-Fi is currently
  /// false (DTX needs USB tunnel) — we annotate the Connection row.
  usable?: boolean
}

export function DeviceInfoPanel({ deviceId, platform, state, transport, usable }: Props) {
  const { data, isLoading, isError } = useQuery({
    queryKey: ['device_info', deviceId, platform, state ?? ''],
    queryFn: () => getDeviceInfo(deviceId, platform),
    // Device info is essentially static — cache across refetches.
    staleTime: 5 * 60_000,
  })

  if (isLoading) {
    return (
      <div className={styles.panel}>
        <Spin size={14} />
      </div>
    )
  }
  if (isError || !data) {
    return (
      <div className={styles.panel}>
        <Text type="secondary" style={{ fontSize: 12 }}>
          Could not fetch device info.
        </Text>
      </div>
    )
  }

  const primary = buildPrimaryRows(data)
  const connection = transport
    ? `${transport === 'usb' ? 'USB' : 'Wi-Fi'}${usable === false ? ' · 不可采样' : ''}`
    : null
  return (
    <div className={styles.panel}>
      <div className={styles.primary}>
        {connection && <Field k="Connection" v={connection} />}
        {primary.map(([k, v]) => (
          <Field key={k} k={k} v={v} />
        ))}
      </div>
      {data.extra.length > 0 && (
        <div className={styles.extra}>
          {data.extra.map(([k, v]) => (
            <Field key={k} k={k} v={v} muted />
          ))}
        </div>
      )}
    </div>
  )
}

function Field({ k, v, muted = false }: { k: string; v: string; muted?: boolean }) {
  return (
    <div className={muted ? styles.fieldMuted : styles.field}>
      <span className={styles.fieldLabel}>{k}</span>
      <span className={styles.fieldValue}>{v}</span>
    </div>
  )
}

function buildPrimaryRows(d: DeviceInfoFull): Array<[string, string]> {
  const rows: Array<[string, string]> = []
  if (d.model) rows.push(['Model', d.model])
  if (d.manufacturer) rows.push(['Brand', d.manufacturer])
  if (d.os_version) {
    const label = d.platform === 'ios' ? 'iOS' : 'Android'
    rows.push([label, d.os_version])
  }
  if (d.build) rows.push(['Build', d.build])
  rows.push([d.platform === 'ios' ? 'UDID' : 'Serial', d.id])
  return rows
}
