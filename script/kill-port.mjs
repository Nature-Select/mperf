import { execSync } from 'node:child_process'

const port = process.argv[2]
if (!port || !/^\d+$/.test(port)) {
  console.error('usage: node kill-port.mjs <port>')
  process.exit(1)
}

try {
  if (process.platform === 'win32') {
    const out = execSync(`netstat -ano -p tcp | findstr :${port}`, { encoding: 'utf8' })
    const pids = new Set(
      out
        .split('\n')
        .map((l) => l.trim().split(/\s+/).pop())
        .filter((p) => /^\d+$/.test(p) && p !== '0'),
    )
    for (const pid of pids) {
      try {
        execSync(`taskkill /F /PID ${pid}`, { stdio: 'ignore' })
      } catch {}
    }
    if (pids.size > 0) console.error(`killed stale process(es) on port ${port}: ${[...pids].join(', ')}`)
  } else {
    const out = execSync(`lsof -ti tcp:${port} || true`, { encoding: 'utf8', shell: '/bin/sh' }).trim()
    if (out) {
      const pids = out.split('\n').filter(Boolean)
      execSync(`kill ${pids.join(' ')}`, { stdio: 'ignore' })
      console.error(`killed stale process(es) on port ${port}: ${pids.join(', ')}`)
    }
  }
} catch {
  // port was already free, or kill failed — let vite surface the real error
}
