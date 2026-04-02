#!/usr/bin/env node

// Copyright 2026 Circle Internet Group, Inc. All rights reserved.
//
// SPDX-License-Identifier: Apache-2.0
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//      http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

import fs from 'fs'
import path from 'path'
import { spawn } from 'child_process'

class ProcessManager {
  constructor({ pidfile, command, args, clean, prepare, daemon }) {
    this.pidfile = pidfile
    this.command = command
    this._getArgs = args
    this._clean = clean
    this._prepare = prepare
    this._daemon = daemon
  }

  stop = async (options = {}) => {
    if (!fs.existsSync(this.pidfile(options))) {
      return
    }
    const pid = parseInt(fs.readFileSync(this.pidfile(options), 'utf8'), 10)
    if (isNaN(pid)) {
      fs.unlinkSync(this.pidfile(options))
      return
    }
    if (options.processId != null && options.processId !== pid) {
      return
    }
    console.log(`Killing process ${pid}`)
    try {
      process.kill(pid, 'SIGTERM')
    } catch (e) {
      if (e.code !== 'ESRCH') {
        // ESRCH: No such process
        console.error(`Failed to kill process ${pid}: ${e.message}`)
      }
    } finally {
      fs.unlinkSync(this.pidfile(options))
    }
  }
  _check_not_running = (options = {}) => {
    if (fs.existsSync(this.pidfile(options))) {
      const pid = fs.readFileSync(this.pidfile(options), 'utf8')
      throw new Error(`Process already running on ${pid}`)
    }
    return true
  }
  clean = async (options = {}) => this._check_not_running(options) && (await this._clean?.(options))
  start = async (options = {}) => {
    this._check_not_running(options) && (await this._prepare?.(options))
    return new Promise((resolve) => {
      const child = spawn(this.command, this._getArgs(options), { stdio: 'inherit' })
      child.on('spawn', () => fs.writeFileSync(this.pidfile(options), `${child.pid}`))
      ;['exit', 'SIGINT', 'SIGTERM'].forEach((signal) => {
        process.on(signal, () => {
          this.stop({ ...options, processId: child.pid })
          resolve()
        })
      })
      child.on('exit', () => resolve())
    })
  }
  daemon = async (options = {}) => {
    this._check_not_running(options) && (await this._prepare?.(options))
    await this._daemon?.(options)
  }
}

const rootdir = path.normalize(path.join(import.meta.dirname, '..'))
process.chdir(rootdir)

const localdevManager = new ProcessManager({
  pidfile: (options = {}) => path.join(rootdir, `datadir/${options.network ?? 'localdev'}/app.pid`),
  command: 'cargo',
  args: (options = {}) => {
    const network = options.network ?? 'localdev'
    const chain_or_genesis = options.genesis ?? `arc-${network}`
    const blockTime = options.blockTime ?? '200ms'
    const launchArgs = [
      'run',
      ...(options.frozen ? ['--frozen'] : []),
      ...(options.offline ? ['--offline'] : []),
      '--package',
      'arc-node-execution',
      '--bin',
      'arc-node-execution',
      '--',
      'node',
      `--chain=${chain_or_genesis}`,
      `--config=${path.join(rootdir, `assets/localdev/reth.toml`)}`,
      `--datadir=${path.join(rootdir, `datadir/${network}`)}`,
      `--ipcpath=${path.join(rootdir, `datadir/${network}/reth.ipc`)}`,
      '--dev',
      ...(blockTime === '0' ? [] : [`--dev.block-time=${blockTime}`]),
      '--disable-discovery',
      '--http',
      '--http.api=all',
      `--http.port=${options.port ?? 8545}`,
      '--metrics=8080',
      '--rpc.txfeecap=1000',
      '--invalid-tx-list-enable',
      '--arc.denylist.enabled',
    ]
    return launchArgs
  },
  prepare: (options = {}) => {
    const network = options.network ?? 'localdev'
    fs.mkdirSync(path.join(rootdir, `datadir/${network}`), { recursive: true })
  },
  clean: async (options = {}) => {
    const network = options.network ?? 'localdev'
    fs.rmSync(path.join(rootdir, `datadir/${network}`), { recursive: true, force: true })
  },
  daemon: async (options = {}) => {
    const network = options.network ?? 'localdev'
    const port = options.port ?? 8545
    const logfile = path.join(rootdir, `datadir/${network}/log.out`)
    const out = fs.openSync(logfile, 'a')
    const launchArgs = [process.argv[1], 'start', `--network=${network}`, `--port=${port}`]
    if (options.frozen) {
      launchArgs.push('--frozen')
    }
    if (options.offline) {
      launchArgs.push('--offline')
    }
    if (options.blockTime) {
      launchArgs.push(`--block-time=${options.blockTime}`)
    }
    if (options.genesis) {
      launchArgs.push(`--genesis=${options.genesis}`)
    }

    const child = spawn(process.argv[0], launchArgs, {
      stdio: ['ignore', out, out],
      detached: true,
    })
    child.unref()

    process.stdout.write('Waiting for reth to start...')
    for (let i = 0; i < (options.healthy_retry ?? 130); i++) {
      try {
        const res = await fetch(`http://127.0.0.1:${port}`, {
          method: 'POST',
          headers: { 'Content-Type': 'application/json' },
          body: JSON.stringify({ jsonrpc: '2.0', method: 'eth_blockNumber' }),
        })
        if (res.ok) {
          process.stdout.write('\n')
          console.log('reth started')
          process.exit(0)
        }
      } catch (e) {}
      process.stdout.write('.')
      await new Promise((resolve) => setTimeout(resolve, 1000))
    }
    process.stdout.write('\n')
    throw new Error('reth failed to start')
  },
})

try {
  let options = {}
  const args = []
  for (const arg of process.argv.slice(2)) {
    if (arg.startsWith('--')) {
      const tokens = arg.split('=', 2)
      switch (tokens[0]) {
        case '--offline':
          options.offline = true
          break
        case '--frozen':
          options.frozen = true
          break
        case '--healthy-retry':
          options.healthy_retry = parseInt(tokens[1], 10)
          if (isNaN(options.healthy_retry)) {
            throw new Error(`Invalid value for --healthy-retry: ${tokens[1]}`)
          }
          break
        case '--network':
          options.network = tokens[1]
          break
        case '--port':
          options.port = parseInt(tokens[1])
          if (isNaN(options.port)) {
            throw new Error(`Invalid value for --port: ${tokens[1]}`)
          }
          break
        case '--block-time':
          options.blockTime = tokens[1]
          break
        case '--genesis':
          options.genesis = tokens[1]
      }
      continue
    }
    args.push(arg)
  }
  // check the arguments
  for (const arg of args) {
    const action = localdevManager[arg]
    if (!action) {
      console.log('Usage: localdev.js start|stop|clean|daemon --network=<network>')
      process.exit(1)
    }
  }
  for (const arg of args) {
    const action = localdevManager[arg]
    if (!action) {
      console.log('Usage: localdev.js start|stop|clean|daemon --network=<network>')
      process.exit(1)
    }
    await action(options)
  }
} catch (e) {
  console.error(e)
  process.exit(1)
}
