import { Select } from '@arco-design/web-react'
import { AppInfo } from '@/lib/ipc'
import { LetterAvatar } from '@/components/LetterAvatar'

const { Option } = Select

// Module-level constants so each Option doesn't allocate two fresh
// style objects per render — at 500+ apps that was visibly slow during
// dropdown scroll (every scroll-triggered re-render = ~1000 new objects).
const OPTION_ROW_STYLE: React.CSSProperties = {
  display: 'inline-flex',
  alignItems: 'center',
  gap: 6,
  overflow: 'hidden',
}
const OPTION_LABEL_STYLE: React.CSSProperties = {
  overflow: 'hidden',
  textOverflow: 'ellipsis',
  whiteSpace: 'nowrap',
}

// Arco 2.x virtual-list config. `threshold: 60` keeps the simple
// (no-virtual) path for small app lists where the windowing overhead
// would dominate; once a typical phone (~200–600 launchable apps) tips
// over the threshold, virtualization keeps the dropdown to ~10 mounted
// rows regardless of list size, which fixes the scroll jank that came
// from rendering and re-laying out hundreds of DOM nodes per scroll
// frame. `itemHeight: 32` matches Arco's mini-size option height.
const VIRTUAL_LIST_PROPS = {
  threshold: 60,
  height: 240,
  itemHeight: 32,
}

export interface AppSelectorProps {
  apps: AppInfo[]
  value: string | null
  onChange: (id: string | null) => void
  loading?: boolean
  /// Disabled while recording (target can't change mid-session) or
  /// when no device is picked yet.
  disabled?: boolean
}

export function AppSelector({ apps, value, onChange, loading, disabled }: AppSelectorProps) {
  return (
    <Select
      size="small"
      placeholder={disabled && !apps.length ? '先选设备' : 'Select app'}
      showSearch
      allowClear
      style={{ width: '100%' }}
      loading={loading}
      disabled={disabled}
      value={value ?? undefined}
      onChange={(v) => onChange((v as string | undefined) ?? null)}
      virtualListProps={VIRTUAL_LIST_PROPS}
      filterOption={(input, option) => {
        const props = (option as { props?: { children?: unknown; value?: unknown } })?.props
        const text = `${String(props?.children ?? '')} ${String(props?.value ?? '')}`.toLowerCase()
        return text.includes(input.toLowerCase())
      }}
    >
      {apps.map((a) => (
        <Option key={a.id} value={a.id}>
          <span style={OPTION_ROW_STYLE}>
            <LetterAvatar label={a.label} seed={a.id} size={16} />
            <span style={OPTION_LABEL_STYLE}>
              {a.label === a.id ? a.id : `${a.label}`}
            </span>
          </span>
        </Option>
      ))}
    </Select>
  )
}
