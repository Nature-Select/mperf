import { Select } from '@arco-design/web-react'
import { Device } from '@/lib/ipc'

const { Option, OptGroup } = Select

/// Compose the stable React/Select key for a device. iOS surfaces the
/// same UDID twice (USB + Wi-Fi) per PerfDog convention, so id alone
/// isn't unique — we include transport.
export function deviceKey(d: Device): string {
  return `${d.platform}:${d.id}:${d.transport}`
}

export function deviceFromKey(key: string, devices: Device[]): Device | undefined {
  return devices.find((d) => deviceKey(d) === key)
}

/// One-character transport indicator. Inline tags clutter a narrow
/// dropdown; a single suffix in muted text reads cleanly. Detailed
/// transport info lives in the 设备 tab.
function transportSuffix(d: Device): string {
  return d.transport === 'usb' ? 'USB' : 'Wi-Fi'
}

export interface DeviceSelectorProps {
  devices: Device[]
  selectedKey: string | null
  onChange: (key: string | null) => void
  loading?: boolean
  /// Locks the dropdown while a session is recording — switching device
  /// mid-session is invalid.
  disabled?: boolean
}

export function DeviceSelector({
  devices,
  selectedKey,
  onChange,
  loading,
  disabled,
}: DeviceSelectorProps) {
  const androidDevices = devices.filter((d) => d.platform === 'android')
  const iosDevices = devices.filter((d) => d.platform === 'ios')

  const renderOption = (d: Device) => {
    const label = d.model || d.id
    return (
      <Option key={deviceKey(d)} value={deviceKey(d)} disabled={!d.usable}>
        <span
          style={{
            display: 'inline-flex',
            alignItems: 'baseline',
            width: '100%',
            gap: 6,
          }}
        >
          <span
            style={{
              fontWeight: 500,
              overflow: 'hidden',
              textOverflow: 'ellipsis',
              whiteSpace: 'nowrap',
            }}
          >
            {label}
          </span>
          <span
            style={{
              marginLeft: 'auto',
              color: 'var(--color-text-3)',
              fontSize: 11,
              fontFamily: 'ui-monospace, SFMono-Regular, monospace',
            }}
          >
            {transportSuffix(d)}
          </span>
        </span>
      </Option>
    )
  }

  return (
    <Select
      size="small"
      placeholder={devices.length === 0 ? 'No devices' : 'Select device'}
      style={{ width: '100%' }}
      value={selectedKey ?? undefined}
      onChange={(v) => onChange((v as string | undefined) ?? null)}
      loading={loading}
      disabled={disabled}
      allowClear
    >
      {androidDevices.length > 0 && (
        <OptGroup label="Android">{androidDevices.map(renderOption)}</OptGroup>
      )}
      {iosDevices.length > 0 && (
        <OptGroup label="iOS">{iosDevices.map(renderOption)}</OptGroup>
      )}
    </Select>
  )
}
