import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { access, chmod, mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises";
import { tmpdir } from "node:os";
import { delimiter, join } from "node:path";
import { test } from "node:test";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("../", import.meta.url));

test("Finder launches use development data for dev bundles and installed data for release bundles", {
  skip: process.platform === "win32",
}, async (t) => {
  const directory = await mkdtemp(join(tmpdir(), "monocode-bundle-data-"));
  t.after(() => rm(directory, { recursive: true, force: true }));
  const bin = join(directory, "bin");
  const target = join(directory, "target with spaces");
  await mkdir(bin);
  // Exercise packaging without signing or opening an app.
  for (const [name, script] of [
    ["uname", "#!/bin/sh\nprintf 'Darwin\\n'\n"],
    ["codesign", "#!/bin/sh\nexit 0\n"],
  ]) {
    await writeFile(join(bin, name), script);
    await chmod(join(bin, name), 0o755);
  }
  // Stands in for the framework cef-dll-sys fetches with the CEF SDK.
  const framework = join(directory, "sdk/Chromium Embedded Framework.framework");
  await mkdir(join(framework, "Libraries"), { recursive: true });
  await writeFile(join(framework, "Chromium Embedded Framework"), "framework");
  await writeFile(join(framework, "Libraries/libcef_sandbox.dylib"), "sandbox");
  const env = { ...process.env, PATH: `${bin}${delimiter}${process.env.PATH}`, MONOCODE_CEF_FRAMEWORK: framework };
  for (const [profile, override, mode] of [
    ["native-dev", undefined, "dev-data"],
    ["release", undefined, "monocode-data"],
    ["release", "dev-data", "dev-data"],
    ["native-dev", "monocode-data", "monocode-data"],
  ]) {
    await t.test(`${profile}: ${override ?? "default"}`, async () => {
      const output = join(target, profile);
      await mkdir(output, { recursive: true });
      const executable = join(output, "monocode-gpui");
      await writeFile(executable, "#!/bin/sh\nprintf '%s\\n' \"$@\"\n");
      await chmod(executable, 0o755);
      const helper = join(output, "monocode-cef-helper");
      await writeFile(helper, "#!/bin/sh\nexit 0\n");
      await chmod(helper, 0o755);
      const ptyExec = join(output, "monocode-pty-exec");
      await writeFile(ptyExec, "#!/bin/sh\nexit 0\n");
      await chmod(ptyExec, 0o755);
      const args = ["scripts/bundle-native-macos.sh", profile, target, "0.16.0"];
      if (override) args.push(override);
      const packaged = spawnSync("bash", args, {
        cwd: root,
        env,
        encoding: "utf8",
      });
      assert.ifError(packaged.error);
      assert.equal(packaged.status, 0, packaged.stdout + packaged.stderr);
      const bundle = join(output, "Monocode Native.app/Contents");
      const plist = await readFile(join(bundle, "Info.plist"), "utf8");
      assert.ok(plist.includes(`<key>MonocodeDataMode</key><string>${mode}</string>`));
      // macOS ends an app that touches Bluetooth, the camera or the microphone
      // without saying why; Chromium does for passkeys and website requests.
      for (const key of ["NSBluetoothAlwaysUsageDescription", "NSCameraUsageDescription", "NSMicrophoneUsageDescription", "NSLocationUsageDescription"]) {
        assert.ok(plist.includes(`<key>${key}</key><string>`), key);
      }
      // macOS rejects menu bar items from a process that exec'd after launch.
      assert.ok(plist.includes("<key>CFBundleExecutable</key><string>monocode-gpui</string>"));
      await assert.rejects(access(join(bundle, "MacOS/monocode-launcher")));
      const launched = spawnSync(join(bundle, "MacOS/monocode-gpui"), ["--cwd", "a project with spaces"], {
        encoding: "utf8",
      });
      assert.ifError(launched.error);
      assert.equal(launched.status, 0, launched.stderr);
      assert.deepEqual(launched.stdout.trim().split("\n"), ["--cwd", "a project with spaces"]);
      // Chromium's sandbox lets its helpers read only inside the app.
      assert.equal(
        await readFile(join(bundle, "Frameworks/Chromium Embedded Framework.framework/Chromium Embedded Framework"), "utf8"),
        "framework",
      );
      await access(join(bundle, "Frameworks/Chromium Embedded Framework.framework/Libraries/libcef_sandbox.dylib"));
      // Chromium derives the other helpers' names from the main one's.
      for (const variant of ["", " (GPU)", " (Renderer)", " (Plugin)", " (Alerts)"]) {
        const name = `Monocode Native Helper${variant}`;
        const helperApp = join(bundle, "Frameworks", `${name}.app/Contents`);
        await access(join(helperApp, "MacOS", name));
        const helperPlist = await readFile(join(helperApp, "Info.plist"), "utf8");
        assert.ok(helperPlist.includes(`<key>CFBundleExecutable</key><string>${name}</string>`));
        assert.ok(helperPlist.includes("<key>LSUIElement</key><true/>"));
      }
    });
  }
  await t.test("a chosen name names the bundle and what the Finder shows, not the app's identity", async () => {
    const output = join(target, "release");
    const packaged = spawnSync("bash", ["scripts/bundle-native-macos.sh", "release", target, "0.16.0"], {
      cwd: root,
      env: { ...env, MONOCODE_NATIVE_APP_NAME: "SOCode" },
      encoding: "utf8",
    });
    assert.ifError(packaged.error);
    assert.equal(packaged.status, 0, packaged.stdout + packaged.stderr);
    const plist = await readFile(join(output, "SOCode.app/Contents/Info.plist"), "utf8");
    assert.ok(plist.includes("<key>CFBundleName</key><string>SOCode</string>"));
    assert.ok(plist.includes("<key>CFBundleDisplayName</key><string>SOCode</string>"));
    assert.ok(plist.includes("<key>CFBundleIdentifier</key><string>dev.usemono.gpui</string>"));
    assert.ok(plist.includes("<key>CFBundleExecutable</key><string>monocode-gpui</string>"));
    await access(join(output, "SOCode.app/Contents/Frameworks/SOCode Helper (Renderer).app/Contents/MacOS/SOCode Helper (Renderer)"));
  });
});
