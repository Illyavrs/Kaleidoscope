import { createHash, createPrivateKey, createPublicKey, sign } from 'node:crypto';
import { spawn } from 'node:child_process';
import { createReadStream } from 'node:fs';
import { copyFile, lstat, mkdir, readFile, readdir, realpath, writeFile } from 'node:fs/promises';
import { dirname, isAbsolute, relative, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

export const MAX_ARCHIVE_BYTES = 536_870_912;
const MAX_FILE_BYTES = 268_435_456;
const MAX_JSON_BYTES = 1_048_576;
const MAX_PROCESS_OUTPUT = 16_384;
const SPKI_PREFIX = Buffer.from('302a300506032b6570032100', 'hex');
const SCRIPT_DIRECTORY = dirname(fileURLToPath(import.meta.url));
const DEFAULT_WORKSPACE = resolve(SCRIPT_DIRECTORY, '../../..');
const BRIDGE_NAME = 'osu.Game.Rulesets.Transfiguration.dll';

export const GUI_FILES = Object.freeze([
  'Kaleidoscope.exe',
  BRIDGE_NAME,
  'artifact.json',
  'compatibility.json',
  'THIRD-PARTY-NOTICES.md',
  'LICENSE',
  'README.txt',
  'Transfiguration-README.md',
  'licenses/DotNet.txt',
  'licenses/Harmony.txt',
  'licenses/Iced.txt',
  'licenses/Mono.Cecil.txt',
  'licenses/MonoMod.txt',
  'Kaleidoscope-THIRD-PARTY-NOTICES.md',
  'licenses/Nunito.txt',
  'licenses/NotoSansTC.txt',
  'package.json',
]);

function assert(condition, message) {
  if (!condition) throw new Error(message);
}

export function validateVersion(version) {
  assert(typeof version === 'string' && version.length <= 64, 'Invalid release version.');
  const result = /^(0|[1-9]\d*)\.(0|[1-9]\d*)\.(0|[1-9]\d*)$/.exec(version);
  assert(result !== null && result[0] === version, 'Invalid release version.');
  assert(result.slice(1).every((part) => BigInt(part) <= 18_446_744_073_709_551_615n), 'Release version exceeds the native semantic-version limit.');
  return version;
}

export function validateRepository(repository) {
  assert(typeof repository === 'string' && !/\s/.test(repository) && /^[A-Za-z0-9][A-Za-z0-9-]{0,38}\/[A-Za-z0-9][A-Za-z0-9._-]{0,99}$/.test(repository), 'Invalid GitHub repository.');
  return repository;
}

function validateCommit(value) {
  assert(typeof value === 'string' && value.length === 40 && /^[0-9a-f]{40}$/.test(value), 'Invalid component commit.');
  return value;
}

function validateHash(value) {
  assert(typeof value === 'string' && value.length === 64 && /^[0-9a-f]{64}$/.test(value), 'Invalid SHA-256.');
  return value;
}

function exactFileHashes(files) {
  assert(files !== null && typeof files === 'object' && !Array.isArray(files), 'Invalid package file hashes.');
  const names = Object.keys(files);
  assert(names.length === GUI_FILES.length && GUI_FILES.every((name) => Object.hasOwn(files, name)), 'Package inventory does not match the GUI package.');
  return Object.fromEntries(GUI_FILES.map((name) => [name, validateHash(files[name])]));
}

export function buildManifest(input) {
  const version = validateVersion(input.version);
  const files = exactFileHashes(input.files);
  assert(Number.isSafeInteger(input.bytes) && input.bytes > 0 && input.bytes <= MAX_ARCHIVE_BYTES, 'Archive size exceeds the update limit.');
  return {
    protocol: 1,
    version,
    platform: 'windows-x64',
    repository: validateRepository(input.repository),
    tag: `v${version}`,
    asset: `Kaleidoscope-${version}-windows-x64.zip`,
    bytes: input.bytes,
    sha256: validateHash(input.sha256),
    launcher_sha256: files['Kaleidoscope.exe'],
    bridge_sha256: files[BRIDGE_NAME],
    bridge_version: validateVersion(input.bridgeVersion),
    kaleidoscope_commit: validateCommit(input.kaleidoscopeCommit),
    transfiguration_commit: validateCommit(input.transfigurationCommit),
    files,
  };
}

export function createSignedEnvelope(manifest, signingKeyPem, publicKeyHex) {
  const manifestKeys = ['protocol', 'version', 'platform', 'repository', 'tag', 'asset', 'bytes', 'sha256', 'launcher_sha256', 'bridge_sha256', 'bridge_version', 'kaleidoscope_commit', 'transfiguration_commit', 'files'];
  assert(manifest !== null && typeof manifest === 'object' && !Array.isArray(manifest) && Object.keys(manifest).length === manifestKeys.length && manifestKeys.every((name) => Object.hasOwn(manifest, name)), 'Invalid update manifest fields.');
  const canonical = buildManifest({ version: manifest.version, bridgeVersion: manifest.bridge_version, repository: manifest.repository, bytes: manifest.bytes, sha256: manifest.sha256, kaleidoscopeCommit: manifest.kaleidoscope_commit, transfigurationCommit: manifest.transfiguration_commit, files: manifest.files });
  assert(manifest.protocol === 1 && manifest.platform === canonical.platform && manifest.tag === canonical.tag && manifest.asset === canonical.asset && manifest.launcher_sha256 === canonical.launcher_sha256 && manifest.bridge_sha256 === canonical.bridge_sha256, 'Invalid update manifest bindings.');
  assert(typeof publicKeyHex === 'string' && publicKeyHex.length === 64 && /^[0-9a-f]{64}$/.test(publicKeyHex), 'Missing or invalid configured update public key.');
  let privateKey;
  try {
    privateKey = createPrivateKey(signingKeyPem);
  } catch {
    throw new Error('The update signing key is invalid.');
  }
  assert(privateKey.asymmetricKeyType === 'ed25519', 'The update signing key must use Ed25519.');
  const actualPublic = createPublicKey(privateKey).export({ type: 'spki', format: 'der' });
  assert(actualPublic.length === 44 && actualPublic.subarray(0, 12).equals(SPKI_PREFIX), 'The update public key encoding is invalid.');
  assert(actualPublic.subarray(12).toString('hex') === publicKeyHex, 'The signing key does not match the configured update public key.');
  // This exact byte sequence is the signed protocol payload. Consumers verify
  // it before parsing JSON, rather than serializing the parsed object again.
  const bytes = Buffer.from(JSON.stringify(canonical), 'utf8');
  assert(bytes.length <= MAX_JSON_BYTES, 'Update manifest exceeds the size limit.');
  const signature = sign(null, bytes, privateKey);
  assert(signature.length === 64, 'The signing key produced an invalid signature.');
  return { manifest: bytes.toString('base64'), signature: signature.toString('base64') };
}

function containsPath(parent, child) {
  const remainder = relative(parent, child);
  return remainder === '' || (!remainder.startsWith(`..${sep}`) && remainder !== '..' && !isAbsolute(remainder));
}

export async function assertUnlinkedPath(path, { exists = true } = {}) {
  const full = resolve(path);
  let cursor = full;
  let first = true;
  while (true) {
    try {
      const item = await lstat(cursor);
      assert(!item.isSymbolicLink(), 'Update input or output contains a symbolic link or junction.');
    } catch (error) {
      if (!(first && !exists && error.code === 'ENOENT')) throw error;
    }
    const parent = dirname(cursor);
    if (parent === cursor) break;
    cursor = parent;
    first = false;
  }
  if (exists) {
    const physical = await realpath(full);
    assert(physical.toLowerCase() === full.toLowerCase(), 'Update input path does not resolve to its declared location.');
  }
  return full;
}

async function sha256(path) {
  const hash = createHash('sha256');
  for await (const chunk of createReadStream(path)) hash.update(chunk);
  return hash.digest('hex');
}

export async function collectPackageHashes(directory) {
  const root = await assertUnlinkedPath(directory);
  assert((await lstat(root)).isDirectory(), 'Package input must be a directory.');
  const names = [];
  async function walk(base, prefix = '') {
    for (const entry of await readdir(base, { withFileTypes: true })) {
      const name = `${prefix}${entry.name}`;
      const path = resolve(base, entry.name);
      const item = await lstat(path);
      assert(!item.isSymbolicLink(), 'Package files cannot be symbolic links or junctions.');
      if (item.isDirectory()) {
        assert(name === 'licenses', 'Package contains an unexpected directory.');
        await walk(path, `${name}/`);
      } else {
        assert(item.isFile() && GUI_FILES.includes(name), 'Package contains an unexpected file.');
        assert(item.size > 0 && item.size <= MAX_FILE_BYTES, 'Package file is empty or exceeds the size limit.');
        if (name.endsWith('.json')) assert(item.size <= MAX_JSON_BYTES, 'Package JSON exceeds the size limit.');
        names.push({ name, path, size: item.size });
      }
    }
  }
  await walk(root);
  assert(names.length === GUI_FILES.length && GUI_FILES.every((name) => names.some((entry) => entry.name === name)), 'Package inventory does not match the GUI package.');
  assert(names.reduce((total, entry) => total + entry.size, 0) <= MAX_ARCHIVE_BYTES, 'Uncompressed package exceeds the update limit.');
  const records = {};
  for (const entry of names) records[entry.name] = await sha256(entry.path);
  return exactFileHashes(records);
}

async function boundedProcess(command, args, options = {}) {
  return await new Promise((resolveProcess, reject) => {
    const env = { ...(options.env ?? process.env) };
    delete env.KALEIDOSCOPE_UPDATE_SIGNING_KEY_PEM;
    delete env.KALEIDOSCOPE_UPDATE_SIGNING_KEY_FILE;
    delete env.GH_TOKEN;
    delete env.GITHUB_TOKEN;
    const child = spawn(command, args, { windowsHide: true, shell: false, stdio: ['ignore', 'pipe', 'pipe'], ...options, env });
    let output = '';
    let size = 0;
    let failure;
    const timer = setTimeout(() => { failure = new Error('Release verification timed out.'); child.kill(); }, 60_000);
    function consume(chunk, keep) {
      size += chunk.length;
      if (size > MAX_PROCESS_OUTPUT) { failure = new Error('Release verification output exceeds the limit.'); child.kill(); }
      else if (keep) output += chunk.toString('utf8');
    }
    child.stdout.on('data', (chunk) => consume(chunk, true));
    child.stderr.on('data', (chunk) => consume(chunk, false));
    child.on('error', () => { clearTimeout(timer); reject(new Error('Could not start the release verifier.')); });
    child.on('close', (code) => {
      clearTimeout(timer);
      if (failure) reject(failure);
      else if (code !== 0) reject(new Error('Release verification failed.'));
      else resolveProcess(output);
    });
  });
}

async function verifyComponentPins(workspaceRoot, kaleidoscopeCommit, transfigurationCommit) {
  for (const [folder, commit] of [['tools/kaleidoscope', kaleidoscopeCommit], ['tools/transfiguration', transfigurationCommit]]) {
    const cwd = await assertUnlinkedPath(resolve(workspaceRoot, folder));
    const actual = (await boundedProcess('git', ['rev-parse', 'HEAD'], { cwd })).trim();
    assert(actual === validateCommit(commit), 'The source checkout does not match the declared component commit.');
    const changes = await boundedProcess('git', ['status', '--porcelain', '--untracked-files=normal'], { cwd });
    assert(changes.trim() === '', 'Update source components must be committed and clean.');
  }
}

async function verifyGuiPackage(directory, archivePath, workspaceRoot) {
  const modulePath = await assertUnlinkedPath(resolve(workspaceRoot, 'tools/kaleidoscope/scripts/launcher-package.psm1'));
  // Parameters cross a process boundary through environment variables, never
  // interpolated PowerShell source. The existing verifier admits production
  // GUI packages only, checks the ZIP's bounded entries and compares all bytes.
  await boundedProcess('pwsh', ['-NoProfile', '-NonInteractive', '-Command',
    "$ErrorActionPreference = 'Stop'; Import-Module -Name $env:KALEIDOSCOPE_RELEASE_VERIFIER -Force; Test-KaleidoscopePackage -Directory $env:KALEIDOSCOPE_RELEASE_DIRECTORY | Out-Null; Test-KaleidoscopePackageArchive -ArchivePath $env:KALEIDOSCOPE_RELEASE_ARCHIVE -Directory $env:KALEIDOSCOPE_RELEASE_DIRECTORY | Out-Null"],
  { env: { ...process.env, KALEIDOSCOPE_RELEASE_VERIFIER: modulePath, KALEIDOSCOPE_RELEASE_DIRECTORY: directory, KALEIDOSCOPE_RELEASE_ARCHIVE: archivePath } });
}

export function validateCompiledUpdateConfig(config, expected) {
  assert(config !== null && typeof config === 'object' && !Array.isArray(config), 'Invalid compiled update configuration.');
  assert(Object.keys(config).length === 6 && ['protocol', 'configured', 'version', 'bridge_version', 'repository', 'public_key'].every((key) => Object.hasOwn(config, key)), 'Invalid compiled update configuration fields.');
  assert(config.protocol === 1 && config.configured === true && config.version === expected.version && config.bridge_version === expected.bridgeVersion && config.repository === expected.repository && config.public_key === expected.publicKeyHex, 'The launcher update channel, component versions or key do not match this release.');
}

async function readSigningKey(environment, outputDirectory, packageDirectory) {
  const pem = environment.KALEIDOSCOPE_UPDATE_SIGNING_KEY_PEM;
  const keyFile = environment.KALEIDOSCOPE_UPDATE_SIGNING_KEY_FILE;
  assert(Boolean(pem) !== Boolean(keyFile), 'Configure exactly one update signing key source.');
  if (pem) {
    assert(Buffer.byteLength(pem, 'utf8') <= 16_384, 'The update signing key exceeds the size limit.');
    return pem;
  }
  const path = await assertUnlinkedPath(keyFile);
  assert(!containsPath(outputDirectory, path) && !containsPath(packageDirectory, path), 'The update signing key must remain outside release payload and output directories.');
  const item = await lstat(path);
  assert(item.isFile() && item.size > 0 && item.size <= 16_384, 'The update signing key file is invalid.');
  return await readFile(path, 'utf8');
}

export async function createUpdateRelease(options, environment = process.env) {
  assert(process.platform === 'win32', 'GUI release verification requires Windows PowerShell 7.');
  const version = validateVersion(options.version);
  const bridgeVersion = validateVersion(options.bridgeVersion);
  const repository = validateRepository(options.repository);
  assert(repository === environment.KALEIDOSCOPE_UPDATE_REPOSITORY, 'The release repository differs from the configured update channel.');
  const publicKeyHex = environment.KALEIDOSCOPE_UPDATE_PUBLIC_KEY;
  assert(typeof publicKeyHex === 'string' && publicKeyHex.length === 64 && /^[0-9a-f]{64}$/.test(publicKeyHex), 'Missing or invalid configured update public key.');
  const workspaceRoot = await assertUnlinkedPath(options.workspaceRoot ?? DEFAULT_WORKSPACE);
  const packageDirectory = await assertUnlinkedPath(options.packageDirectory);
  const archivePath = await assertUnlinkedPath(options.archivePath);
  const outputDirectory = await assertUnlinkedPath(options.outputDirectory, { exists: false });
  assert(!containsPath(packageDirectory, outputDirectory) && !containsPath(outputDirectory, packageDirectory) && !containsPath(outputDirectory, archivePath), 'Release output must be separate from package inputs.');
  let outputExists = false;
  try { await lstat(outputDirectory); outputExists = true; } catch (error) { if (error.code !== 'ENOENT') throw error; }
  assert(!outputExists, 'Release output already exists; choose a fresh directory.');
  const archive = await lstat(archivePath);
  assert(archive.isFile() && archive.size > 0 && archive.size <= MAX_ARCHIVE_BYTES, 'Update archive is invalid or exceeds the size limit.');
  await verifyComponentPins(workspaceRoot, options.kaleidoscopeCommit, options.transfigurationCommit);
  const before = await collectPackageHashes(packageDirectory);
  const archiveHash = await sha256(archivePath);
  await verifyGuiPackage(packageDirectory, archivePath, workspaceRoot);
  const diagnostic = await boundedProcess(resolve(packageDirectory, 'Kaleidoscope.exe'), ['--verify-update-config']);
  let compiled;
  try { compiled = JSON.parse(diagnostic); } catch { throw new Error('Invalid compiled update configuration.'); }
  validateCompiledUpdateConfig(compiled, { version, bridgeVersion, repository, publicKeyHex });
  const after = await collectPackageHashes(packageDirectory);
  assert(JSON.stringify(before) === JSON.stringify(after) && archiveHash === await sha256(archivePath) && archive.size === (await lstat(archivePath)).size, 'The verified package changed during release preparation.');
  const manifest = buildManifest({ ...options, version, bridgeVersion, repository, bytes: archive.size, sha256: archiveHash, files: after });
  const signingKey = await readSigningKey(environment, outputDirectory, packageDirectory);
  const envelope = createSignedEnvelope(manifest, signingKey, publicKeyHex);
  await mkdir(outputDirectory);
  const outputArchive = resolve(outputDirectory, manifest.asset);
  await copyFile(archivePath, outputArchive, 1); // COPYFILE_EXCL: existing releases are never overwritten.
  assert(await sha256(outputArchive) === archiveHash && (await lstat(outputArchive)).size === manifest.bytes, 'The release archive changed during staging.');
  await writeFile(resolve(outputDirectory, 'updates.json'), `${JSON.stringify(envelope)}\n`, { encoding: 'utf8', flag: 'wx' });
  return { directory: outputDirectory, asset: manifest.asset, manifest: 'updates.json', version, bridge_version: bridgeVersion, repository, bytes: manifest.bytes, sha256: archiveHash };
}

function commandLine(argv) {
  const allowed = new Set(['version', 'bridge-version', 'repository', 'package-directory', 'archive', 'output-directory', 'kaleidoscope-commit', 'transfiguration-commit', 'workspace-root']);
  const values = {};
  for (let i = 0; i < argv.length; i += 2) {
    const key = argv[i]?.startsWith('--') ? argv[i].slice(2) : '';
    assert(allowed.has(key) && argv[i + 1] && !Object.hasOwn(values, key), 'Invalid or duplicate release arguments.');
    values[key] = argv[i + 1];
  }
  for (const key of allowed) if (key !== 'workspace-root') assert(Object.hasOwn(values, key), 'Missing required release arguments.');
  return { version: values.version, bridgeVersion: values['bridge-version'], repository: values.repository, packageDirectory: values['package-directory'], archivePath: values.archive, outputDirectory: values['output-directory'], kaleidoscopeCommit: values['kaleidoscope-commit'], transfigurationCommit: values['transfiguration-commit'], workspaceRoot: values['workspace-root'] };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const result = await createUpdateRelease(commandLine(process.argv.slice(2)));
    process.stdout.write(`${JSON.stringify(result)}\n`);
  } catch (error) {
    // Key material and verifier output are deliberately never included here.
    process.stderr.write(`${error.message}\n`);
    process.exitCode = 1;
  }
}
