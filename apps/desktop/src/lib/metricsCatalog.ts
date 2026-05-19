/// Static catalog of metrics the user can pick to record.
///
/// Granularity: one entry per **chart card** in LiveView, not per
/// underlying `MetricKind`. Toggling an entry shows/hides the entire
/// card. This trades fine-grained per-line control for the picker UI
/// staying in lock-step with what's actually rendered — `cpu_usage`
/// hides the CPU card regardless of whether App CPU or Total CPU lines
/// are drawn inside it.
///
/// `implemented: true` means a sampler emits data AND a chart card
/// renders it today (see `apps/desktop/src/components/Live*Chart.tsx`).
/// `implemented: false` items either have no sampler yet, no chart yet,
/// or both — they render disabled in the picker so users see the
/// roadmap without misclicking.
/// `previewable: true` lets disabled items be toggled anyway, used for
/// UI-only placeholders like the ScreenTab strip.

export type Platform = 'android' | 'ios'

export type MetricCategory =
  | 'capture'
  | 'frame'
  | 'cpu'
  | 'memory'
  | 'gpu'
  | 'thermal'
  | 'battery'
  | 'network'
  | 'misc'

export interface MetricItem {
  /// Stable key persisted in localStorage. Renaming requires a
  /// `STORAGE_KEY` version bump in `useMetricsSelection`.
  id: string
  label: string
  description: string
  category: MetricCategory
  /// Short abbreviation rendered inside the colored avatar chip.
  abbr: string
  implemented: boolean
  previewable?: boolean
  /// Platforms that actually surface this metric at runtime. Items work
  /// on every platform listed here once `implemented: true`.
  platforms: Platform[]
}

export interface CategoryDef {
  id: MetricCategory
  label: string
  color: string
}

export const CATEGORIES: CategoryDef[] = [
  // 'capture' sits at the top because ScreenShot / StartupTiming are
  // session-level setup concerns, not per-tick metrics — PerfDog puts
  // them above every per-frame measurement and so do we.
  { id: 'capture', label: '采集', color: '#F59E0B' },
  { id: 'frame', label: '帧率', color: '#4FACFE' },
  { id: 'cpu', label: 'CPU', color: '#37C8AB' },
  { id: 'memory', label: '内存', color: '#A78BFA' },
  { id: 'gpu', label: 'GPU', color: '#FF8A3D' },
  { id: 'thermal', label: '温度', color: '#F4516C' },
  { id: 'battery', label: '电池', color: '#67C23A' },
  { id: 'network', label: '网络', color: '#1FB6C1' },
  { id: 'misc', label: '其他', color: '#909399' },
]

export const METRICS: MetricItem[] = [
  // ─── Capture ─────────────────────────────────────────────────────────
  {
    id: 'screenshot',
    label: 'ScreenShot',
    description: '采集期间按固定间隔抓取屏幕截图（PerfDog 默认 2s 一张，后端暂未实现）。',
    category: 'capture',
    abbr: 'SS',
    implemented: false,
    previewable: true,
    platforms: ['android', 'ios'],
  },
  {
    id: 'startup_timing',
    label: 'Startup Timing',
    description: '应用启动各阶段耗时 TTID / TTFD（后端暂未实现）。',
    category: 'capture',
    abbr: 'ST',
    implemented: false,
    previewable: true,
    platforms: ['android', 'ios'],
  },

  // ─── Frame ───────────────────────────────────────────────────────────
  // Maps to LiveFpsChart, which plots FPS plus (Android-only) Frame
  // Time, Jank tiers and Stutter from the same FpsSampler. We don't
  // split these in the picker since they're a single derived family.
  {
    id: 'frame',
    label: 'Frame',
    description:
      'FPS、Frame Time、Jank（Small / Normal / Big）、Stutter。Android 走 gfxinfo + SurfaceView 取 max；iOS 仅屏幕级 FPS（DTX 不暴露每帧时间，无 jank）。',
    category: 'frame',
    abbr: 'F',
    implemented: true,
    platforms: ['android', 'ios'],
  },

  // ─── CPU ─────────────────────────────────────────────────────────────
  {
    id: 'cpu_usage',
    label: 'CPU Usage',
    description: '整机 CPU 占用 + 目标进程 CPU 占用（双线）。',
    category: 'cpu',
    abbr: 'CPU',
    implemented: true,
    platforms: ['android', 'ios'],
  },
  {
    id: 'cpu_core',
    label: 'CPU Core Usage',
    description: '每个逻辑核心的独立占用率。',
    category: 'cpu',
    abbr: 'CC',
    implemented: true,
    platforms: ['android', 'ios'],
  },
  {
    id: 'cpu_thread',
    label: 'Thread CPU Usage',
    description: '目标进程内每个线程的 CPU 占用（暂未实现）。',
    category: 'cpu',
    abbr: 'TC',
    implemented: false,
    platforms: ['android', 'ios'],
  },
  {
    id: 'cpu_clock',
    label: 'CPU Clock',
    description: '运行时每核心实时频率（暂未实现）。',
    category: 'cpu',
    abbr: 'CK',
    implemented: false,
    platforms: ['android', 'ios'],
  },
  {
    id: 'cpu_freq_limits',
    label: 'CPU Frequency Limits',
    description: '调度器对每核心的频率上限（暂未实现）。',
    category: 'cpu',
    abbr: 'FL',
    implemented: false,
    platforms: ['android'],
  },

  // ─── Memory ──────────────────────────────────────────────────────────
  // App-process memory only (Android PSS / iOS physFootprint). System
  // memory has a sampler but no chart card, so it's not surfaced here.
  {
    id: 'memory',
    label: 'Memory Usage',
    description: 'Android: PSS；iOS: physFootprint（与 Xcode 一致）。',
    category: 'memory',
    abbr: 'M',
    implemented: true,
    platforms: ['android', 'ios'],
  },
  {
    id: 'memory_detail',
    label: 'Memory Detail',
    description: 'Native / Java / Graphics / Code 分项内存（暂未实现）。',
    category: 'memory',
    abbr: 'MD',
    implemented: false,
    platforms: ['android', 'ios'],
  },

  // ─── GPU ─────────────────────────────────────────────────────────────
  {
    id: 'gpu',
    label: 'GPU Usage',
    description:
      'Android: Adreno KGSL / Mali devfreq；iOS: Tiler / Renderer / Device 三元组（DTX graphics.opengl）。',
    category: 'gpu',
    abbr: 'G',
    implemented: true,
    platforms: ['android', 'ios'],
  },
  {
    id: 'gpu_clock',
    label: 'GPU Clock',
    description: '运行时 GPU 频率（暂未实现）。',
    category: 'gpu',
    abbr: 'GK',
    implemented: false,
    platforms: ['android', 'ios'],
  },
  {
    id: 'gpu_counter',
    label: 'GPU Counter',
    description: '厂商私有 GPU 性能计数器，需要 Mali / Adreno SDK（暂未实现）。',
    category: 'gpu',
    abbr: 'GC',
    implemented: false,
    platforms: ['android'],
  },

  // ─── Thermal ─────────────────────────────────────────────────────────
  // LiveTemperatureChart shows CPU temp + battery temp together on
  // Android. iOS IORegistry isn't exposed, so the whole card is gated
  // off for iOS in LiveView regardless of selection.
  {
    id: 'temperature',
    label: 'Temperature',
    description: 'CPU 温度（/sys/class/thermal 取最高）+ 电池温度。iOS 不开放，仅 Android。',
    category: 'thermal',
    abbr: 'T',
    implemented: true,
    platforms: ['android'],
  },
  {
    id: 'thermal_status',
    label: 'Thermal Status',
    description: '整机散热状态 DEFAULT / WARM / HOT / SEVERE（暂未实现）。',
    category: 'thermal',
    abbr: 'TS',
    implemented: false,
    platforms: ['android'],
  },

  // ─── Battery ─────────────────────────────────────────────────────────
  // Battery sampler (level / temp / voltage) exists on Android but
  // there's no dedicated chart card yet — battery temp piggy-backs on
  // the Temperature chart. Marked unimplemented to honestly signal
  // "nothing new to see by toggling this".
  {
    id: 'battery',
    label: 'Battery',
    description: '剩余电量百分比 + 电压（图表未实现，电池温度已并入 Temperature）。',
    category: 'battery',
    abbr: 'B',
    implemented: false,
    platforms: ['android'],
  },

  // ─── Network ─────────────────────────────────────────────────────────
  {
    id: 'network',
    label: 'Network',
    description: '每秒上下行字节（暂未实现）。',
    category: 'network',
    abbr: 'N',
    implemented: false,
    platforms: ['android', 'ios'],
  },

  // ─── Misc ────────────────────────────────────────────────────────────
  {
    id: 'brightness',
    label: 'Brightness',
    description: '屏幕亮度（暂未实现）。',
    category: 'misc',
    abbr: 'Br',
    implemented: false,
    platforms: ['android', 'ios'],
  },
]

export function categoryOf(id: MetricCategory): CategoryDef {
  return CATEGORIES.find((c) => c.id === id) ?? CATEGORIES[CATEGORIES.length - 1]
}

/// Default selection on a fresh install: cross-platform chart cards.
/// `temperature` is intentionally left off — it's Android-only, so
/// pre-selecting it would inflate the "已选 N/M" counter on an iOS
/// device for a card that never renders. Android users can flip it on
/// once; iOS users skip the cosmetic mismatch.
/// Capture items also stay off — they're previewable placeholders,
/// not something a new user expects to see populated.
export const DEFAULT_SELECTED_IDS = new Set<string>([
  'frame',
  'cpu_usage',
  'cpu_core',
  'memory',
  'gpu',
])
