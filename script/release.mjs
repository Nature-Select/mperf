#!/usr/bin/env zx
// Build a release bundle for the current host platform.
//
//   pnpm release
//
// 1. Ensure adb sidecar is staged (pulls if missing).
// 2. Run `pnpm tauri build`.
// 3. Print where the artifacts landed.

import { $, fs, path, chalk } from 'zx'

$.verbose = false

const repoRoot = path.resolve(import.meta.dirname, '..')
const binariesDir = path.join(repoRoot, 'apps/desktop/src-tauri/binaries')

const HOST_TRIPLE = {
  'darwin-arm64': 'aarch64-apple-darwin',
  'darwin-x64': 'x86_64-apple-darwin',
  'linux-x64': 'x86_64-unknown-linux-gnu',
  'win32-x64': 'x86_64-pc-windows-msvc',
}

const triple = HOST_TRIPLE[`${process.platform}-${process.arch}`]
if (!triple) {
  console.error(chalk.red(`unsupported host: ${process.platform}-${process.arch}`))
  process.exit(1)
}
const isWin = process.platform === 'win32'
const expectedBin = path.join(binariesDir, `adb-${triple}${isWin ? '.exe' : ''}`)

if (!(await fs.pathExists(expectedBin))) {
  console.log(chalk.yellow(`adb sidecar missing at ${expectedBin}; fetching…`))
  await $({ stdio: 'inherit' })`pnpm fetch:adb`
}
if (!(await fs.pathExists(expectedBin))) {
  console.error(chalk.red('adb sidecar still missing after fetch — aborting build'))
  process.exit(1)
}
console.log(chalk.green(`✓ adb sidecar: ${path.relative(repoRoot, expectedBin)}`))

console.log(chalk.cyan.bold('\nbuilding release bundle…'))
await $({ stdio: 'inherit' })`pnpm tauri build`

// Cargo workspace target lives at the repo root, not under src-tauri.
const bundleDir = path.join(repoRoot, 'target/release/bundle')
console.log(chalk.green.bold('\nDone.'))
console.log(`  artifacts:  ${path.relative(repoRoot, bundleDir)}/`)
console.log('')
if (await fs.pathExists(bundleDir)) {
  const entries = await fs.readdir(bundleDir)
  for (const fmt of entries) {
    const fmtDir = path.join(bundleDir, fmt)
    const files = await fs.readdir(fmtDir).catch(() => [])
    for (const f of files) {
      console.log(`    ${chalk.gray(fmt)}/${f}`)
    }
  }
}
