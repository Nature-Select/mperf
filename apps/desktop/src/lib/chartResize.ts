/// Attach a ResizeObserver to a chart's host element + a window resize
/// fallback, and return a cleanup function. Use inside the same
/// useEffect that creates the uPlot instance:
///
///   const cleanup = attachChartResize(hostRef.current, onResize)
///   return () => { cleanup(); plotRef.current?.destroy() }
///
/// ResizeObserver catches box changes from *any* layout shift —
/// window resize, sidebar drag, parent flex reflow — which the older
/// `window.resize` listener misses (window.resize only fires on the
/// outer window dimensions, not on internal layout changes like a
/// resizable sidebar pushing the content panel narrower).
export function attachChartResize(
  host: HTMLElement | null,
  onResize: () => void,
): () => void {
  if (!host) {
    // No host yet — return a no-op cleanup so caller pattern stays uniform.
    window.addEventListener('resize', onResize)
    return () => window.removeEventListener('resize', onResize)
  }
  const ro = new ResizeObserver(() => onResize())
  ro.observe(host)
  // Keep window.resize as a belt-and-braces fallback. ResizeObserver
  // is universal in modern browsers / webviews so this is rarely the
  // path that fires, but it's harmless.
  window.addEventListener('resize', onResize)
  return () => {
    ro.disconnect()
    window.removeEventListener('resize', onResize)
  }
}
