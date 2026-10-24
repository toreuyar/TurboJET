/**
 * Real-backend browser harness for the Discovery journey (TJ-026).
 *
 * Starts the actual tj-api binary, the actual SQLite database/workspace and a
 * local scripted OpenAI-compatible model. No live provider, paid call, forge,
 * deployment or production state is used. A local scripted success is NOT live
 * supported-model proof.
 */
import { spawn, type ChildProcess } from 'node:child_process';
import fs from 'node:fs';
import net from 'node:net';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { writeNeutralPlanningPackage } from './neutral-planning-package';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const REPO = path.resolve(HERE, '../../..');
const API_BINARY = process.env.TJ_API_BINARY || path.join(REPO, 'target/debug/tj-api');
const WEBUI = process.env.TJ_WEBUI_PATH || path.join(REPO, 'webui/dist/index.html');
const MASTER_KEY = '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef';

async function freePort(): Promise<number> {
  return new Promise((resolve, reject) => {
    const server = net.createServer();
    server.unref();
    server.on('error', reject);
    server.listen(0, '127.0.0.1', () => {
      const address = server.address();
      const port = typeof address === 'object' && address ? address.port : 0;
      server.close(() => resolve(port));
    });
  });
}

async function waitFor(url: string, timeoutMs = 20000): Promise<void> {
  const deadline = Date.now() + timeoutMs;
  let last = '';
  while (Date.now() < deadline) {
    try {
      const response = await fetch(url);
      last = `${response.status}`;
      if (response.ok) return;
    } catch (error) {
      last = (error as Error).message;
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  throw new Error(`timed out waiting for ${url} (${last})`);
}

function copyConfig(target: string) {
  fs.mkdirSync(target, { recursive: true });
  // Discovery is a self-contained shipped package.
  const discovery = path.join(REPO, 'workflows', 'discovery');
  const targetPackages = path.join(target, 'workflows');
  fs.mkdirSync(targetPackages, { recursive: true });
  fs.cpSync(discovery, path.join(targetPackages, 'discovery'), { recursive: true });
  // Planning is synthesized in the disposable backend state; no repository
  // fixture or ignored-local package is read.
  writeNeutralPlanningPackage(path.join(targetPackages, 'planning'));
}

export class BackendHarness {
  root: string;
  baseUrl = '';
  modelUrl = '';
  private backend?: ChildProcess;
  private model?: ChildProcess;
  private configPath = '';
  private env: NodeJS.ProcessEnv = {};

  private constructor(root: string) {
    this.root = root;
  }

  static async start(options: { packagePath?: string } = {}): Promise<BackendHarness> {
    if (!fs.existsSync(API_BINARY)) {
      throw new Error(`tj-api binary missing at ${API_BINARY}; run cargo build -p tj-api first`);
    }
    if (!fs.existsSync(WEBUI)) {
      throw new Error(`built UI missing at ${WEBUI}; run npm run build in webui first`);
    }
    const root = fs.mkdtempSync(path.join(os.tmpdir(), 'tj-browser-'));
    const harness = new BackendHarness(root);
    const configDir = path.join(root, 'config');
    copyConfig(configDir);
    const modelPort = await freePort();
    harness.modelPort = modelPort;
    harness.modelUrl = `http://127.0.0.1:${modelPort}`;
    harness.model = spawn(process.env.PYTHON || 'python3', [path.join(REPO, 'scripts/discovery_scripted_model.py'), String(modelPort)], {
      stdio: ['ignore', fs.openSync(path.join(root, 'model.log'), 'a'), fs.openSync(path.join(root, 'model.err'), 'a')],
    });
    await waitFor(`${harness.modelUrl}/v1/models`);

    const port = await freePort();
    harness.baseUrl = `http://127.0.0.1:${port}`;
    harness.configPath = path.join(configDir, 'turbojet.json');
    const home = path.join(root, 'home');
    fs.mkdirSync(home, { recursive: true });
    fs.writeFileSync(harness.configPath, JSON.stringify({
      listen_addr: `127.0.0.1:${port}`,
      log_level: 'warn',
      database_path: path.join(home, 'state.db'),
      workspace_base: path.join(home, 'workspaces'),
      workflow_sources: options.packagePath ? [{
        id: 'browser-fixture',
        path: options.packagePath,
        kind: 'package',
      }] : [{
        id: 'browser-fixtures',
        path: 'config/workflows',
        kind: 'directory',
      }],
      providers: [{
        id: 'scripted', type: 'custom', api_key_env: 'TJ_E2E_PROVIDER_KEY',
        base_url: harness.modelUrl, default_model: 'scripted-model',
        rate_limit_retries: 2, transient_retries: 2, retry_base_ms: 20,
      }],
      agents: [{
        id: 'chief', provider_id: 'scripted', model: 'scripted-model', default: true,
        enabled: true, tools: [], system_prompt: 'Local scripted fixture. Never claims live evidence.',
      }],
      budgets: {
        question_rounds_per_stage: 6, question_rounds_per_run: 12, model_calls: 40,
        tool_calls: 40, output_corrections: 6, interaction_nudges: 6, active_secs: 180,
      },
      activity_retention_days: 30,
      run_timeout_secs: 120,
    }, null, 2));
    harness.env = {
      ...process.env,
      TURBOJET_HOME: home,
      TURBOJET_WEBUI_PATH: WEBUI,
      TURBOJET_MASTER_KEY: MASTER_KEY,
      TJ_E2E_PROVIDER_KEY: 'local-scripted-key',
    };
    await harness.spawnBackend();
    return harness;
  }

  private modelPort = 0;

  private async spawnBackend() {
    const stdout = fs.openSync(path.join(this.root, 'backend.log'), 'a');
    this.backend = spawn(API_BINARY, [this.configPath], { env: this.env, stdio: ['ignore', stdout, stdout] });
    await waitFor(`${this.baseUrl}/api/health`, 30000);
  }

  async setMode(mode: string) {
    const response = await fetch(`${this.modelUrl}/__mode`, {
      method: 'POST',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ mode }),
    });
    if (!response.ok) throw new Error(`cannot set scripted model mode: ${response.status}`);
  }

  async restart() {
    if (!this.backend) throw new Error('backend is not running');
    const exited = new Promise<void>((resolve) => this.backend!.once('exit', () => resolve()));
    this.backend.kill('SIGKILL');
    await exited;
    await this.spawnBackend();
  }

  async stop() {
    for (const child of [this.backend, this.model]) {
      if (child && !child.killed) {
        try { child.kill('SIGKILL'); } catch { /* already gone */ }
      }
    }
    this.backend = undefined;
    this.model = undefined;
    fs.rmSync(this.root, { recursive: true, force: true });
  }
}
