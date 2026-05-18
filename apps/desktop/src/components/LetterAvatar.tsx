/// First-letter avatar with a deterministic background color (hashed
/// from `seed`). Cheap, instant, no asset loading. Good enough to tell
/// apps apart at a glance in a long picker list.

const PALETTE = [
  '#f44336', // red
  '#e91e63', // pink
  '#9c27b0', // purple
  '#673ab7', // deep purple
  '#3f51b5', // indigo
  '#2196f3', // blue
  '#03a9f4', // light blue
  '#0097a7', // dark cyan (white on cyan-500 is borderline)
  '#00897b', // teal
  '#388e3c', // green
  '#ef6c00', // orange
  '#e64a19', // deep orange
  '#5d4037', // brown
  '#455a64', // blue grey
]

function hash(s: string): number {
  // djb2
  let h = 5381
  for (let i = 0; i < s.length; i++) {
    h = ((h << 5) + h + s.charCodeAt(i)) | 0
  }
  return Math.abs(h)
}

function firstChar(s: string): string {
  // `Array.from` iterates code points correctly for BMP CJK chars.
  return Array.from(s).find((c) => c.trim().length > 0) ?? '?'
}

export function LetterAvatar({
  label,
  seed,
  size = 20,
}: {
  label: string
  /// Hash seed (typically the package / bundle id).
  seed: string
  size?: number
}) {
  const color = PALETTE[hash(seed) % PALETTE.length]
  const ch = firstChar(label).toUpperCase()
  return (
    <span
      style={{
        display: 'inline-flex',
        alignItems: 'center',
        justifyContent: 'center',
        width: size,
        height: size,
        borderRadius: 4,
        background: color,
        color: 'white',
        fontSize: Math.round(size * 0.55),
        fontWeight: 600,
        flexShrink: 0,
        userSelect: 'none',
        lineHeight: 1,
      }}
    >
      {ch}
    </span>
  )
}
