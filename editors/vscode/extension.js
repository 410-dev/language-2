// VS Code client of the language-2 language server (`language-2 lsp`, spec 15.4).
//
// The server executable comes from the `language-2.server.path` setting, the PATH, or the
// newest SDK installed under the SDK home. On Windows a running executable cannot be replaced,
// so the server is started from a copy: rebuilding or reinstalling the toolchain stays possible
// while the editor is open ("language-2: Restart Language Server" picks up the new build).

const vscode = require('vscode');
const fs = require('fs');
const os = require('os');
const path = require('path');
const { LanguageClient, TransportKind } = require('vscode-languageclient/node');

const EXE = process.platform === 'win32' ? 'language-2.exe' : 'language-2';

/** @type {LanguageClient | undefined} */
let client;
/** @type {vscode.OutputChannel | undefined} */
let output;

function isFile(p) {
  try {
    return fs.statSync(p).isFile();
  } catch {
    return false;
  }
}

/** SDK homes to search, most specific first. */
function sdkHomes() {
  const homes = [];
  if (process.env.L2_HOME) {
    homes.push(process.env.L2_HOME);
  }
  if (process.platform === 'win32') {
    if (process.env.LOCALAPPDATA) {
      homes.push(path.join(process.env.LOCALAPPDATA, 'language-2'));
    }
    homes.push(path.join(os.homedir(), 'AppData', 'Local', 'language-2'));
  }
  homes.push(path.join(os.homedir(), '.language-2'));
  return [...new Set(homes)];
}

function isDir(p) {
  try {
    return fs.statSync(p).isDirectory();
  } catch {
    return false;
  }
}

/** Expands `%VAR%`, `${env:VAR}`, `$VAR` / `${VAR}` and a leading `~`. */
function expand(p) {
  const env = (name) => {
    const key = Object.keys(process.env).find((k) => k.toLowerCase() === name.toLowerCase());
    return key ? process.env[key] : undefined;
  };
  let s = p.trim().replace(/^"(.*)"$/, '$1');
  s = s.replace(/%([^%]+)%/g, (m, n) => env(n) ?? m);
  s = s.replace(/\$\{env:([^}]+)\}/g, (m, n) => env(n) ?? m);
  s = s.replace(/\$\{([A-Za-z_][A-Za-z0-9_]*)\}|\$([A-Za-z_][A-Za-z0-9_]*)/g, (m, a, b) => env(a || b) ?? m);
  if (s === '~' || s.startsWith('~/') || s.startsWith('~\\')) {
    s = path.join(os.homedir(), s.slice(1));
  }
  return s;
}

/** The newest `sdk/<version>/bin/language-2` under an SDK home. */
function newestSdkServer(home) {
  const sdk = path.join(home, 'sdk');
  let versions = [];
  try {
    versions = fs.readdirSync(sdk).filter((v) => /^\d+$/.test(v)).sort((a, b) => Number(b) - Number(a));
  } catch (e) {
    if (output) {
      output.appendLine(`cannot list ${sdk}: ${e}`);
    }
  }
  for (const v of versions) {
    const p = path.join(sdk, v, 'bin', EXE);
    try {
      fs.statSync(p);
    } catch (e) {
      if (output) {
        output.appendLine(`cannot read ${p}: ${e}`);
      }
    }
    if (isFile(p)) {
      return p;
    }
  }
  return undefined;
}

/**
 * The executable named by the `language-2.server.path` setting. The setting may also name a
 * directory: one holding the executable, an SDK version directory (`.../sdk/1`) or the SDK home.
 */
function configuredServer(setting) {
  let p = expand(setting);
  if (!path.isAbsolute(p) && vscode.workspace.workspaceFolders && vscode.workspace.workspaceFolders.length > 0) {
    p = path.join(vscode.workspace.workspaceFolders[0].uri.fsPath, p);
  }
  if (isFile(p)) {
    return p;
  }
  if (isDir(p)) {
    for (const c of [path.join(p, EXE), path.join(p, 'bin', EXE)]) {
      if (isFile(c)) {
        return c;
      }
    }
    return newestSdkServer(p);
  }
  if (process.platform === 'win32' && isFile(p + '.exe')) {
    return p + '.exe';
  }
  return undefined;
}

/** The language-2 executable to run, or undefined. */
function findServer() {
  const setting = vscode.workspace.getConfiguration('language-2').get('server.path');
  if (setting) {
    const p = configuredServer(setting);
    if (p) {
      return p;
    }
    output.appendLine(`language-2.server.path "${setting}" is not a language-2 executable (expanded: ${expand(setting)}); searching PATH and the SDK home instead`);
    vscode.window.showWarningMessage(`language-2: language-2.server.path("${setting}")에서 실행 파일을 찾지 못해 자동으로 찾습니다.`);
  }
  for (const dir of (process.env.PATH || '').split(path.delimiter)) {
    if (dir && isFile(path.join(dir, EXE))) {
      return path.join(dir, EXE);
    }
  }
  for (const home of sdkHomes()) {
    const p = newestSdkServer(home);
    if (p) {
      return p;
    }
    output.appendLine(`no SDK toolchain under ${path.join(home, 'sdk', '<version>', 'bin', EXE)}`);
  }
  output.appendLine(`${EXE} is not on PATH either`);
  // what this process sees of the SDK home's parent (diagnostics)
  for (const home of sdkHomes()) {
    const parent = path.dirname(home);
    try {
      const names = fs.readdirSync(parent);
      output.appendLine(`${parent}: ${names.length} entries, ${names.includes(path.basename(home)) ? 'includes' : 'does not include'} ${path.basename(home)}`);
    } catch (e) {
      output.appendLine(`cannot list ${parent}: ${e}`);
    }
  }
  return undefined;
}

/** On Windows, a copy of `exe` in the extension's storage (one per build of the executable). */
function shadowCopy(context, exe) {
  if (process.platform !== 'win32') {
    return exe;
  }
  try {
    const st = fs.statSync(exe);
    const dir = path.join(context.globalStorageUri.fsPath, 'server');
    fs.mkdirSync(dir, { recursive: true });
    const name = `language-2-${st.size}-${Math.floor(st.mtimeMs)}.exe`;
    const copy = path.join(dir, name);
    if (!isFile(copy)) {
      fs.copyFileSync(exe, copy);
    }
    for (const old of fs.readdirSync(dir)) {
      if (old !== name) {
        try {
          fs.unlinkSync(path.join(dir, old));
        } catch {
          // still running in another window
        }
      }
    }
    return copy;
  } catch (e) {
    output.appendLine(`could not copy ${exe}: ${e}`);
    return exe;
  }
}

async function start(context) {
  const config = vscode.workspace.getConfiguration('language-2');
  if (!config.get('server.enabled')) {
    return;
  }
  const exe = findServer();
  if (!exe) {
    const choice = await vscode.window.showWarningMessage(
      'language-2: 언어 서버로 쓸 language-2 실행 파일을 찾지 못했습니다. `language-2 sdk install`로 SDK를 설치하거나 `language-2.server.path`를 설정하세요. 찾아본 위치는 출력 창에 있습니다.',
      '출력 보기',
      '설정 열기'
    );
    if (choice === '출력 보기') {
      output.show(true);
    } else if (choice) {
      vscode.commands.executeCommand('workbench.action.openSettings', 'language-2.server.path');
    }
    return;
  }
  output.appendLine(`starting ${exe} lsp`);
  const serverOptions = { command: shadowCopy(context, exe), args: ['lsp'], transport: TransportKind.stdio };
  const clientOptions = {
    documentSelector: [{ scheme: 'file', language: 'language-2' }],
    outputChannel: output,
  };
  client = new LanguageClient('language-2', 'language-2', serverOptions, clientOptions);
  try {
    await client.start();
  } catch (e) {
    output.appendLine(`the language server did not start: ${e}`);
    vscode.window.showErrorMessage(`language-2: 언어 서버를 시작하지 못했습니다 (${exe}).`);
    client = undefined;
  }
}

async function stop() {
  if (client) {
    const c = client;
    client = undefined;
    try {
      await c.stop();
    } catch {
      // already gone
    }
  }
}

exports.activate = async function activate(context) {
  output = vscode.window.createOutputChannel('language-2');
  context.subscriptions.push(output);
  context.subscriptions.push(
    vscode.commands.registerCommand('language-2.restartServer', async () => {
      await stop();
      await start(context);
    }),
    vscode.commands.registerCommand('language-2.showServerOutput', () => output.show(true)),
    vscode.workspace.onDidChangeConfiguration((e) => {
      if (e.affectsConfiguration('language-2.server')) {
        vscode.commands.executeCommand('language-2.restartServer');
      }
    })
  );
  await start(context);
};

exports.deactivate = function deactivate() {
  return stop();
};
