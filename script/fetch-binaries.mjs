#!/usr/bin/env zx
// Download Android platform-tools (adb) for the current host platform and
// stage it at `apps/desktop/src-tauri/binaries/adb-<target-triple>` so the
// Tauri sidecar mechanism can pick it up at bundle time.
//
// Usage:
//   pnpm fetch:adb            # current host platform
//   pnpm fetch:adb --all       # macOS arm64 + macOS x64 + Linux x64 + Windows x64

import { $, fs, path, chalk, argv } from 'zx'

$.verbose = false

const repoRoot = path.resolve(import.meta.dirname, '..')
const binariesDir = path.join(repoRoot, 'apps/desktop/src-tauri/binaries')

// Map host platform → Rust target triple (matches what Tauri's externalBin expects).
const HOST_TRIPLE = {
  'darwin-arm64': 'aarch64-apple-darwin',
  'darwin-x64': 'x86_64-apple-darwin',
  'linux-x64': 'x86_64-unknown-linux-gnu',
  'win32-x64': 'x86_64-pc-windows-msvc',
}

const PLATFORM_TOOLS_URL = {
  darwin: 'https://dl.google.com/android/repository/platform-tools-latest-darwin.zip',
  linux: 'https://dl.google.com/android/repository/platform-tools-latest-linux.zip',
  win32: 'https://dl.google.com/android/repository/platform-tools-latest-windows.zip',
}

const TARGETS_ALL = [
  { os: 'darwin', triple: 'aarch64-apple-darwin' },
  { os: 'darwin', triple: 'x86_64-apple-darwin' },
  { os: 'linux', triple: 'x86_64-unknown-linux-gnu' },
  { os: 'win32', triple: 'x86_64-pc-windows-msvc' },
]

async function fetchForOs(os, triple) {
  const isWin = os === 'win32'
  const url = PLATFORM_TOOLS_URL[os]
  if (!url) throw new Error(`no platform-tools URL for os=${os}`)

  const tmpZip = path.join('/tmp', `platform-tools-${os}.zip`)
  const tmpDir = path.join('/tmp', `platform-tools-extract-${os}`)

  console.log(chalk.cyan(`→ ${triple}: downloading platform-tools (${os})…`))
  await fetchToFile(url, tmpZip)

  console.log(chalk.cyan(`→ ${triple}: extracting…`))
  await fs.remove(tmpDir).catch(() => {})
  await fs.ensureDir(tmpDir)
  await $`unzip -q -o ${tmpZip} -d ${tmpDir}`

  await fs.ensureDir(binariesDir)
  const binaryName = isWin ? 'adb.exe' : 'adb'
  const outputName = `adb-${triple}${isWin ? '.exe' : ''}`
  const src = path.join(tmpDir, 'platform-tools', binaryName)
  const dst = path.join(binariesDir, outputName)
  await fs.copy(src, dst, { overwrite: true })
  if (!isWin) await $`chmod +x ${dst}`
  console.log(chalk.green(`✓ ${path.relative(repoRoot, dst)}`))

  // Windows adb needs two DLLs side by side. Tauri's externalBin doesn't
  // copy arbitrary extras — we ship them via `resources` in tauri.conf.
  if (isWin) {
    for (const dll of ['AdbWinApi.dll', 'AdbWinUsbApi.dll']) {
      const dsrc = path.join(tmpDir, 'platform-tools', dll)
      const ddst = path.join(binariesDir, dll)
      if (await fs.pathExists(dsrc)) {
        await fs.copy(dsrc, ddst, { overwrite: true })
        console.log(chalk.green(`✓ ${path.relative(repoRoot, ddst)}`))
      }
    }
  }
}

async function fetchToFile(url, outPath) {
  const res = await fetch(url, { redirect: 'follow' })
  if (!res.ok) throw new Error(`fetch ${url}: ${res.status} ${res.statusText}`)
  const buf = Buffer.from(await res.arrayBuffer())
  await fs.writeFile(outPath, buf)
}

const all = argv.all === true || argv._?.includes('--all')

if (all) {
  for (const t of TARGETS_ALL) {
    await fetchForOs(t.os, t.triple)
  }
} else {
  const key = `${process.platform}-${process.arch}`
  const triple = HOST_TRIPLE[key]
  if (!triple) {
    console.error(chalk.red(`unsupported host: ${key}`))
    process.exit(1)
  }
  await fetchForOs(process.platform, triple)
}

console.log(chalk.bold('\nDone. The binary is staged for Tauri externalBin.'))
