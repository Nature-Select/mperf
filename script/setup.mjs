#!/usr/bin/env zx
// One-shot environment check + install for new developers.
//
//   pnpm setup
//
// Verifies the host toolchain (Rust, Node, pnpm), installs JS deps, and
// stages the platform-tools `adb` binary for the current OS via
// fetch-binaries.mjs. Prints any missing prereq with clear install hints
// instead of trying to install heavy toolchains itself.

import { $, fs, path, chalk } from 'zx'

$.verbose = false

const repoRoot = path.resolve(import.meta.dirname, '..')
let failed = 0

function fail(label, msg, hint) {
  failed += 1
  console.log(`${chalk.red('✗')} ${chalk.bold(label)}: ${msg}`)
  if (hint) console.log(`  ${chalk.gray(hint)}`)
}

function ok(label, msg) {
  console.log(`${chalk.green('✓')} ${chalk.bold(label)}: ${msg}`)
}

async function which(cmd) {
  try {
    const r = await $`command -v ${cmd}`
    return r.stdout.trim() || null
  } catch {
    return null
  }
}

async function tryVersion(cmd, args = ['--version']) {
  try {
    const r = await $`${cmd} ${args}`
    return r.stdout.trim()
  } catch {
    return null
  }
}

console.log(chalk.cyan.bold('\nmperf · environment check\n'))

// ---- Rust ----
const cargoPath = await which('cargo')
if (!cargoPath) {
  fail(
    'rust',
    'cargo not found',
    "install via:  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh",
  )
} else {
  const v = await tryVersion('rustc')
  ok('rust', v ?? cargoPath)
}

// ---- Node ----
const nodeV = await tryVersion('node')
if (!nodeV) {
  fail('node', 'not found', 'install Node ≥ 20 (https://nodejs.org or via nvm)')
} else {
  const major = Number(nodeV.replace(/^v/, '').split('.')[0])
  if (major < 20) {
    fail('node', `${nodeV} (need ≥ 20)`, 'upgrade via nvm or nodejs.org')
  } else {
    ok('node', nodeV)
  }
}

// ---- pnpm ----
const pnpmV = await tryVersion('pnpm')
if (!pnpmV) {
  fail('pnpm', 'not found', 'enable via `corepack enable pnpm` (ships with Node)')
} else {
  ok('pnpm', pnpmV)
}

// ---- Platform-specific iOS prereqs ----
if (process.platform === 'linux') {
  const usbmuxd = await which('usbmuxd')
  if (!usbmuxd) {
    console.log(
      `${chalk.yellow('!')} ${chalk.bold('usbmuxd')}: not installed (iOS support disabled until you install it)`,
    )
    console.log(`  ${chalk.gray('debian/ubuntu: sudo apt-get install -y usbmuxd')}`)
  } else {
    ok('usbmuxd', usbmuxd)
  }
}
if (process.platform === 'win32') {
  console.log(
    `${chalk.yellow('!')} ${chalk.bold('Apple Mobile Device Support')}: required for iOS on Windows`,
  )
  console.log(`  ${chalk.gray('install iTunes or AMDS from apple.com')}`)
}

if (failed > 0) {
  console.log(chalk.red(`\n${failed} prerequisite check(s) failed. Fix above, then re-run \`pnpm setup\`.`))
  process.exit(1)
}

// ---- pnpm install ----
console.log(chalk.cyan.bold('\ninstalling JS deps…'))
await $({ stdio: 'inherit' })`pnpm install`

// ---- fetch adb ----
console.log(chalk.cyan.bold('\nfetching adb sidecar…'))
await $({ stdio: 'inherit' })`pnpm fetch:adb`

console.log(chalk.green.bold('\nReady. Next:'))
console.log(`  ${chalk.cyan('pnpm tauri dev')}        # run the app in dev mode`)
console.log(`  ${chalk.cyan('pnpm tauri build')}      # produce a release bundle for this platform`)
console.log(`  ${chalk.cyan('pnpm test')}             # run Rust tests`)
console.log('')
console.log(chalk.gray('Connecting devices:'))
console.log(chalk.gray('  Android:  enable USB debugging, authorize the host on first plug-in'))
console.log(chalk.gray('  iOS:      trust this computer on the device; Developer Mode for iOS 16+'))
console.log('')
