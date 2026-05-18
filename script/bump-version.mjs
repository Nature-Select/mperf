#!/usr/bin/env zx
/* eslint-disable */
// Bump the project version across every file that declares one.
//
// We don't rely on Tauri's "inherit from Cargo.toml when version is
// omitted" behavior because (a) it's implicit and the next maintainer
// has to know it, (b) Tauri config semantics shift across minor
// versions. Cheaper: keep every declaration explicit, drive them all
// from one script, fail loudly on drift.
//
// Files touched:
//   - Cargo.toml                                    (workspace.package.version)
//   - apps/desktop/src-tauri/tauri.conf.json        (bundle / installer metadata)
//   - package.json                                  (root pnpm workspace)
//   - apps/desktop/package.json                     (per-app pkg)
//
// The "关于" page in-app reads from CARGO_PKG_VERSION (Rust); installer
// names / OS "Application info" come from tauri.conf.json. Both matter.
//
// Usage: pnpm bump 0.0.1

import { readFileSync, writeFileSync } from 'node:fs'
import { join, dirname } from 'node:path'
import { fileURLToPath } from 'node:url'

$.verbose = false

const ROOT = join(dirname(fileURLToPath(import.meta.url)), '..')

const next = (process.argv[3] || '').trim()
if (!next) {
  console.error('usage: pnpm bump <new-version>     e.g. pnpm bump 0.0.1')
  process.exit(1)
}
// Permissive SemVer: x.y.z or x.y.z-prerelease. Rejects v-prefix, ranges,
// non-numeric major/minor/patch. Strict enough to catch typos.
const SEMVER = /^\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?$/
if (!SEMVER.test(next)) {
  console.error(`refusing to set version "${next}" — not a plain SemVer (e.g. 0.0.1, 1.2.3-beta.1)`)
  process.exit(1)
}

/// Bump Cargo workspace version. We do a regex replace rather than
/// parsing TOML because we don't want a TOML dependency in this script
/// and the workspace.package.version line is unambiguous.
function bumpCargo(p) {
  const src = readFileSync(p, 'utf8')
  // Match [workspace.package] header, then the first `version = "..."` after it.
  const re = /(\[workspace\.package\][^\[]*?version\s*=\s*")[^"]+(")/
  const m = src.match(re)
  if (!m) throw new Error(`could not find [workspace.package] version in ${p}`)
  const out = src.replace(re, `$1${next}$2`)
  writeFileSync(p, out)
  return m[0].match(/"([^"]+)"/)[1]
}

function bumpJson(p, field = 'version') {
  const json = JSON.parse(readFileSync(p, 'utf8'))
  const prior = json[field]
  json[field] = next
  // Preserve trailing newline — most editors / prettier expect it.
  writeFileSync(p, JSON.stringify(json, null, 2) + '\n')
  return prior
}

const changes = []
changes.push(['Cargo.toml (workspace)', bumpCargo(join(ROOT, 'Cargo.toml'))])
changes.push([
  'apps/desktop/src-tauri/tauri.conf.json',
  bumpJson(join(ROOT, 'apps/desktop/src-tauri/tauri.conf.json')),
])
changes.push(['package.json', bumpJson(join(ROOT, 'package.json'))])
changes.push([
  'apps/desktop/package.json',
  bumpJson(join(ROOT, 'apps/desktop/package.json')),
])

console.log(`bumped to ${next}:`)
for (const [file, prior] of changes) {
  console.log(`  ${prior.padStart(10)} → ${next.padEnd(10)}  ${file}`)
}
console.log('')
console.log('next steps:')
console.log('  1. git diff           # eyeball the changes')
console.log('  2. cargo check        # confirm Cargo.lock updates cleanly')
console.log(`  3. git commit -am "release: v${next}"`)
console.log(`  4. git tag v${next} && git push --tags`)
