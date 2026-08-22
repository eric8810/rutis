#!/usr/bin/env node
/**
 * rutis-bridge CLI wrapper: locates the platform package's rutis-dsh binary
 * (installed via optionalDependencies — only the matching platform lands in
 * node_modules) and execs it with the caller's arguments. `up` is the runner
 * subcommand; everything else passes through.
 */
import { spawnSync } from 'node:child_process'
import { createRequire } from 'node:module'

const require = createRequire(import.meta.url)

function platformPackageName() {
  const { platform, arch } = process
  const libc = platform === 'linux' ? (reportLinuxLibc() === 'musl' ? '-musl' : '-gnu') : ''
  return `rutis-bridge-${platform}-${arch}${libc}`
}

function reportLinuxLibc() {
  // glibc unless the report says otherwise; musl detection follows the
  // same heuristic family esbuild uses (report.longs is a string on musl).
  try {
    return process.report?.getReport()?.header?.glibcVersionRuntime === undefined ? 'musl' : 'glibc'
  } catch {
    return 'glibc'
  }
}

function resolveBinary() {
  const pkg = platformPackageName()
  try {
    const manifest = require(`${pkg}/package.json`)
    const bin = manifest.rutisBinary ?? 'rutis-dsh'
    return require.resolve(`${pkg}/${bin}${process.platform === 'win32' ? '.exe' : ''}`)
  } catch {
    return null
  }
}

const binary = resolveBinary()
if (binary === null) {
  console.error(`[rutis-bridge] no platform binary for ${process.platform}-${process.arch}.`)
  console.error('[rutis-bridge] reinstall rutis-bridge so npm fetches the matching platform package,')
  console.error('[rutis-bridge] or set RUTIS_DSH_BIN to an explicit rutis-dsh path/command.')
  process.exit(1)
}

const result = spawnSync(binary, process.argv.slice(2), { stdio: 'inherit' })
process.exit(result.status ?? 1)
