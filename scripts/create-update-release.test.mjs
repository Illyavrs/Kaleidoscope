import assert from 'node:assert/strict';
import { createHash, generateKeyPairSync, verify } from 'node:crypto';
import { mkdtemp, mkdir, rm, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import test from 'node:test';
import { GUI_FILES, MAX_ARCHIVE_BYTES, assertUnlinkedPath, buildManifest, collectPackageHashes, createSignedEnvelope, validateCompiledUpdateConfig, validateRepository, validateVersion } from './create-update-release.mjs';

const hash = (text) => createHash('sha256').update(text).digest('hex');
const hashes = () => Object.fromEntries(GUI_FILES.map((name) => [name, hash(name)]));
const input = () => ({ version: '1.2.3', bridgeVersion: '0.1.0', repository: 'Illyaverse/Kaleidoscope', bytes: 1234, sha256: hash('ZIP'), kaleidoscopeCommit: 'a'.repeat(40), transfigurationCommit: 'b'.repeat(40), files: hashes() });

function syntheticKey() {
  // Tests create ephemeral in-memory keys only. They never write a release key
  // or sign a compiled launcher, game library, or production package.
  const { privateKey, publicKey } = generateKeyPairSync('ed25519');
  return { privateKey: privateKey.export({ type: 'pkcs8', format: 'pem' }), publicKey, publicHex: publicKey.export({ type: 'spki', format: 'der' }).subarray(12).toString('hex') };
}

async function packageFixture(run) {
  const directory = await mkdtemp(join(tmpdir(), 'kaleidoscope-update-synthetic-'));
  try {
    await mkdir(join(directory, 'package'));
    await mkdir(join(directory, 'package', 'licenses'));
    for (const name of GUI_FILES) await writeFile(join(directory, 'package', name), `synthetic: ${name}\n`);
    await run(directory, join(directory, 'package'));
  } finally {
    // One native filesystem API owns this verified temporary test directory.
    // No user/client/dependency/release directory crosses this boundary.
    assert(directory.startsWith(join(tmpdir(), 'kaleidoscope-update-synthetic-')));
    await rm(directory, { recursive: true, force: true });
  }
}

test('manifest binds an exact complete GUI inventory, component pins and official asset name', () => {
  const manifest = buildManifest(input());
  assert.deepEqual(Object.keys(manifest), ['protocol', 'version', 'platform', 'repository', 'tag', 'asset', 'bytes', 'sha256', 'launcher_sha256', 'bridge_sha256', 'bridge_version', 'kaleidoscope_commit', 'transfiguration_commit', 'files']);
  assert.equal(manifest.tag, 'v1.2.3');
  assert.equal(manifest.asset, 'Kaleidoscope-1.2.3-windows-x64.zip');
  assert.equal(manifest.platform, 'windows-x64');
  assert.deepEqual(Object.keys(manifest.files), GUI_FILES);
  assert.equal(manifest.launcher_sha256, manifest.files['Kaleidoscope.exe']);
  assert.equal(manifest.bridge_sha256, manifest.files['osu.Game.Rulesets.Transfiguration.dll']);
  assert.equal(manifest.bridge_version, '0.1.0');
  assert.equal(manifest.files['package.json'], hash('package.json'));
});

test('signed envelope verifies exact UTF-8 bytes and rejects tampering', () => {
  const key = syntheticKey();
  const manifest = buildManifest(input());
  const envelope = createSignedEnvelope(manifest, key.privateKey, key.publicHex);
  assert.deepEqual(Object.keys(envelope), ['manifest', 'signature']);
  const payload = Buffer.from(envelope.manifest, 'base64');
  const signature = Buffer.from(envelope.signature, 'base64');
  assert.equal(signature.length, 64);
  assert.equal(payload.toString('utf8'), JSON.stringify(manifest));
  assert.equal(verify(null, payload, key.publicKey, signature), true);
  assert.equal(verify(null, Buffer.from(`${payload.toString('utf8')} `), key.publicKey, signature), false);
  assert.deepEqual(createSignedEnvelope(manifest, key.privateKey, key.publicHex), envelope);
});

test('wrong, missing and non-Ed25519 signing identities are rejected', () => {
  const key = syntheticKey();
  const another = syntheticKey();
  const manifest = buildManifest(input());
  assert.throws(() => createSignedEnvelope(manifest, key.privateKey, another.publicHex), /does not match/);
  assert.throws(() => createSignedEnvelope(manifest, key.privateKey, ''), /public key/);
  assert.throws(() => createSignedEnvelope(manifest, 'not a key', key.publicHex), /invalid/);
  const rsa = generateKeyPairSync('rsa', { modulusLength: 2048 }).privateKey.export({ type: 'pkcs8', format: 'pem' });
  assert.throws(() => createSignedEnvelope(manifest, rsa, key.publicHex), /Ed25519/);
});

test('the signer itself rejects extra fields and inconsistent signed bindings', () => {
  const key = syntheticKey();
  const manifest = buildManifest(input());
  const missingBridgeVersion = { ...manifest }; delete missingBridgeVersion.bridge_version;
  for (const changed of [missingBridgeVersion, { ...manifest, bridge_version: '0.1.0-beta.1' }, { ...manifest, extra: true }, { ...manifest, protocol: 2 }, { ...manifest, platform: 'linux-x64' }, { ...manifest, tag: 'v1.2.2' }, { ...manifest, asset: 'different.zip' }, { ...manifest, launcher_sha256: 'a'.repeat(64) }, { ...manifest, bridge_sha256: 'b'.repeat(64) }]) assert.throws(() => createSignedEnvelope(changed, key.privateKey, key.publicHex));
});

test('canonical stable versions match native semver and reject prerelease/build suffixes', () => {
  for (const version of ['0.1.0', '2.0.0', '18446744073709551615.0.0']) assert.equal(validateVersion(version), version);
  for (const version of ['', 'v1.0.0', '1.0', '01.0.0', '1.0.0-01', '2.0.0-alpha.1', '2.0.0-0', '2.0.0+build.4', '18446744073709551616.0.0', '0.18446744073709551616.0', '0.0.18446744073709551616', '../1.0.0', '1.0.0\n', '1.0.0;pwsh', '1.0.0/evil']) assert.throws(() => validateVersion(version));
});

test('repository validation rejects URLs, credentials and path ambiguity', () => {
  for (const repository of ['Illyaverse/Kaleidoscope', 'owner/repo.name']) assert.equal(validateRepository(repository), repository);
  for (const repository of ['', 'https://github.com/owner/repo', 'owner/repo/extra', 'owner/../repo', 'owner@token/repo', 'owner/repo?x', ' owner/repo', 'owner/repo\n']) assert.throws(() => validateRepository(repository));
});

test('manifest rejects missing, extra, invalid and mismatched inventory hashes', () => {
  const missing = hashes(); delete missing['package.json'];
  assert.throws(() => buildManifest({ ...input(), files: missing }));
  assert.throws(() => buildManifest({ ...input(), files: { ...hashes(), 'UpdateHelper.exe': hash('extra') } }));
  assert.throws(() => buildManifest({ ...input(), files: { ...hashes(), 'Kaleidoscope.exe': 'A'.repeat(64) } }));
  assert.throws(() => buildManifest({ ...input(), files: [] }));
  assert.throws(() => buildManifest({ ...input(), kaleidoscopeCommit: 'a'.repeat(39) }));
  assert.throws(() => buildManifest({ ...input(), transfigurationCommit: 'B'.repeat(40) }));
  assert.throws(() => buildManifest({ ...input(), kaleidoscopeCommit: `${'a'.repeat(40)}\n` }));
  assert.throws(() => buildManifest({ ...input(), sha256: `${'a'.repeat(64)}\n` }));
  for (const bridgeVersion of [undefined, '', 'v0.1.0', '0.1.0+build', '0.1.0-rc.1', '18446744073709551616.0.0']) assert.throws(() => buildManifest({ ...input(), bridgeVersion }));
});

test('archive bounds reject zero, oversized, fractional and unsafe lengths', () => {
  assert.equal(buildManifest({ ...input(), bytes: MAX_ARCHIVE_BYTES }).bytes, MAX_ARCHIVE_BYTES);
  for (const bytes of [0, -1, 1.5, NaN, Infinity, MAX_ARCHIVE_BYTES + 1, Number.MAX_SAFE_INTEGER + 1, '123']) assert.throws(() => buildManifest({ ...input(), bytes }));
});

test('compiled channel diagnostic must match the signed release and contain no extra fields', () => {
  const expected = { version: '1.2.3', bridgeVersion: '0.1.0', repository: 'Illyaverse/Kaleidoscope', publicKeyHex: 'a'.repeat(64) };
  const config = { protocol: 1, configured: true, version: expected.version, bridge_version: expected.bridgeVersion, repository: expected.repository, public_key: expected.publicKeyHex };
  validateCompiledUpdateConfig(config, expected);
  const oldDiagnostic = { ...config }; delete oldDiagnostic.bridge_version;
  for (const changed of [oldDiagnostic, { ...config, bridge_version: '0.0.9' }, { ...config, configured: false }, { ...config, version: '1.2.2' }, { ...config, repository: 'other/repo' }, { ...config, public_key: 'b'.repeat(64) }, { ...config, protocol: 2 }, { ...config, extra: true }]) assert.throws(() => validateCompiledUpdateConfig(changed, expected));
});

test('file collection hashes every exact permitted payload including package.json', async () => {
  await packageFixture(async (_directory, packageDirectory) => {
    const files = await collectPackageHashes(packageDirectory);
    assert.deepEqual(Object.keys(files), GUI_FILES);
    for (const name of GUI_FILES) assert.equal(files[name], hash(`synthetic: ${name}\n`));
  });
});

test('file collection rejects extra payloads and directories', async () => {
  await packageFixture(async (_directory, packageDirectory) => {
    await writeFile(join(packageDirectory, 'UpdateHelper.exe'), 'synthetic');
    await assert.rejects(() => collectPackageHashes(packageDirectory), /unexpected file/);
  });
  await packageFixture(async (_directory, packageDirectory) => {
    await mkdir(join(packageDirectory, 'nested'));
    await assert.rejects(() => collectPackageHashes(packageDirectory), /unexpected directory/);
  });
});

test('file collection rejects missing and empty payloads', async () => {
  await packageFixture(async (_directory, packageDirectory) => {
    await rm(join(packageDirectory, 'package.json'));
    await assert.rejects(() => collectPackageHashes(packageDirectory), /inventory/);
  });
  await packageFixture(async (_directory, packageDirectory) => {
    await writeFile(join(packageDirectory, 'Kaleidoscope.exe'), '');
    await assert.rejects(() => collectPackageHashes(packageDirectory), /empty/);
  });
});

test('linked directory inputs and linked file ancestors are rejected', async () => {
  await packageFixture(async (directory, packageDirectory) => {
    const link = join(directory, 'linked-package');
    // A directory junction is available to ordinary Windows users, without
    // enabling Developer Mode or requiring symbolic-link privileges.
    await symlink(packageDirectory, link, process.platform === 'win32' ? 'junction' : 'dir');
    await assert.rejects(() => collectPackageHashes(link), /symbolic link or junction/);
    await assert.rejects(() => assertUnlinkedPath(join(link, 'Kaleidoscope.exe')), /symbolic link or junction/);
  });
});
