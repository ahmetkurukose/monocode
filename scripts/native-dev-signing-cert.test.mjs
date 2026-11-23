import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { chmod, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { delimiter, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));

test("makes the local signing certificate once, in the keychain it is given", {
  skip: process.platform !== "darwin",
}, async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "monocode-signing-cert-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const bin = join(directory, "bin");
  await mkdir(bin);
  const calls = join(directory, "security.log");
  const trusted = join(directory, "trusted");
  // Never the real keychain: `security` notes its calls, and lists the
  // identity once it has been trusted.
  await writeFile(join(bin, "security"), `#!/bin/sh
printf '%s\\n' "$*" >> "${calls}"
case "$1" in
  find-identity) [ -f "${trusted}" ] && printf '  1) ABCD "Monocode Local Signing"\\n' ;;
  find-certificate) [ -f "${trusted}" ]; exit $? ;;
  add-trusted-cert) touch "${trusted}" ;;
esac
exit 0
`);
  await chmod(join(bin, "security"), 0o755);
  const run = () => spawnSync("bash", ["scripts/native-dev-signing-cert.sh", "--keychain", "/tmp/test.keychain-db"], {
    cwd: root,
    env: { ...process.env, PATH: `${bin}${delimiter}${process.env.PATH}` },
    encoding: "utf8",
  });

  const made = run();
  assert.equal(made.status, 0, made.stdout + made.stderr);
  assert.match(made.stdout, /Created "Monocode Local Signing"/);
  const log = await readFile(calls, "utf8");
  assert.match(log, /^import \S+identity\.p12 -k \/tmp\/test\.keychain-db -P \S+ -T \/usr\/bin\/codesign$/m);
  assert.match(log, /^add-trusted-cert -r trustRoot -p codeSign -k \/tmp\/test\.keychain-db \S+cert\.pem$/m);

  await rm(calls);
  const again = run();
  assert.equal(again.status, 0, again.stdout + again.stderr);
  assert.match(again.stdout, /already in \/tmp\/test\.keychain-db/);
  assert.doesNotMatch(await readFile(calls, "utf8"), /^(import|add-trusted-cert)/m);
});
