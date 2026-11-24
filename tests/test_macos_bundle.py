import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import tempfile
import unittest


class MacBundleTests(unittest.TestCase):
    def setUp(self):
        temp = tempfile.TemporaryDirectory()
        self.addCleanup(temp.cleanup)
        root = Path(temp.name)
        self.repo = root / 'v2t source'
        self.script = self.repo / 'app/v2t-macos/build.sh'
        self.script.parent.mkdir(parents=True)
        source = Path(__file__).resolve().parents[1] / 'app/v2t-macos'
        for name in ('build.sh', 'Info.plist', 'RecordInfo.plist', 'Entitlements.plist'):
            for file in source.glob(name):
                shutil.copyfile(file, self.script.parent / name)

        self.bundle = self.repo / 'publish/V2t.app'
        self.launcher = self.repo / 'publish/V2t Record.app'
        self.tools = root / 'tools'
        self.tools.mkdir()
        self.mock_tool('uname', 'case "$1" in -s) echo Darwin;; -m) echo arm64;; esac')
        self.mock_tool('plutil', r'''
python3 - "${@: -1}" <<'PY'
import plistlib
import sys
with open(sys.argv[1], 'rb') as file:
    print(plistlib.load(file)['CFBundleIdentifier'])
PY
''')
        self.mock_tool('mv', r'''
if [[ ${FAIL_LAUNCHER_PUBLISH:-0} = 1 && $1 = */.v2t-macos.*/V2t\ Record.app && $2 = */publish/V2t\ Record.app ]]; then
    exit 1
fi
exec /bin/mv "$@"
''')
        self.mock_tool('cargo', r'''
test "$MACOSX_DEPLOYMENT_TARGET" = 12.0
test "$*" != "${*#*--locked}"
test "$*" != "${*#*aarch64-apple-darwin}"
while [ "$1" != '--target-dir' ]; do shift; done
target="$2/aarch64-apple-darwin/release"
mkdir -p "$target"
printf 'built executable' > "$target/V2t"
chmod +x "$target/V2t"
''')
        self.mock_tool('codesign', r'''
if [ "$1" = '--verify' ]; then
    test -f "${@: -1}/Contents/_CodeSignature/CodeResources"
else
    test "${FAIL_SIGN:-0}" = 0
    bundle="${@: -1}"
    if [[ $bundle = */V2t\ Record.app ]]; then
        test "${FAIL_LAUNCHER_SIGN:-0}" = 0
        test "$*" = "${*#*--entitlements}"
    else
        test "$*" != "${*#*--entitlements}"
    fi
    mkdir "${@: -1}/Contents/_CodeSignature"
    touch "${@: -1}/Contents/_CodeSignature/CodeResources"
fi
''')

    def mock_tool(self, name, body):
        tool = self.tools / name
        tool.write_text('#!/bin/bash\nset -euo pipefail\n' + body + '\n')
        tool.chmod(0o700)

    def build(self, **env):
        return subprocess.run(
            ['bash', str(self.script)],
            env={**os.environ, 'PATH': f'{self.tools}:{os.environ["PATH"]}', **env},
            text=True,
            capture_output=True,
            check=False,
        )

    def test_build_creates_signed_desktop_bundle(self):
        result = self.build()
        self.assertEqual(result.returncode, 0, result.stderr)
        with (self.bundle / 'Contents/Info.plist').open('rb') as file:
            info = plistlib.load(file)
        executable = self.bundle / 'Contents/MacOS' / info['CFBundleExecutable']
        self.assertEqual(executable.read_text(), 'built executable')
        self.assertTrue(os.access(executable, os.X_OK))
        self.assertEqual(info['CFBundleIdentifier'], 'com.lawdalana.v2t')
        self.assertEqual(info['CFBundleShortVersionString'], '2.2.1')
        self.assertEqual(info['LSMinimumSystemVersion'], '12.0')
        self.assertFalse(info.get('LSUIElement', False))
        self.assertTrue(info['NSMicrophoneUsageDescription'])
        with (self.script.parent / 'Entitlements.plist').open('rb') as file:
            self.assertEqual(plistlib.load(file), {'com.apple.security.device.audio-input': True})
        self.assertTrue((self.bundle / 'Contents/_CodeSignature/CodeResources').is_file())

    def test_failed_signing_preserves_an_existing_bundle(self):
        initial = self.build()
        self.assertEqual(initial.returncode, 0, initial.stderr)
        existing = self.bundle / 'Contents/MacOS/V2t'
        existing.write_text('previous executable')
        failed = self.build(FAIL_SIGN='1')
        self.assertNotEqual(failed.returncode, 0)
        self.assertEqual(existing.read_text(), 'previous executable')
        self.assertEqual(set(self.bundle.parent.iterdir()), {self.bundle, self.launcher})

    def test_build_creates_a_separate_recording_launcher(self):
        result = self.build()
        self.assertEqual(result.returncode, 0, result.stderr)
        with (self.launcher / 'Contents/Info.plist').open('rb') as file:
            info = plistlib.load(file)
        self.assertEqual(info['CFBundleIdentifier'], 'com.lawdalana.v2t.record')
        self.assertEqual(info['CFBundleDisplayName'], 'V2t Record')
        self.assertEqual(info['CFBundleShortVersionString'], '2.2.1')
        self.assertFalse(info.get('LSUIElement', False))
        self.assertTrue(os.access(self.launcher / 'Contents/MacOS' / info['CFBundleExecutable'], os.X_OK))
        self.assertTrue((self.launcher / 'Contents/_CodeSignature/CodeResources').is_file())
        self.assertTrue((self.bundle / 'Contents/Resources/Record.v2t-action').is_file())

    def test_failed_launcher_signing_preserves_both_bundles(self):
        initial = self.build()
        self.assertEqual(initial.returncode, 0, initial.stderr)
        main = self.bundle / 'Contents/MacOS/V2t'
        launcher = self.launcher / 'Contents/MacOS/V2t'
        main.write_text('previous main')
        launcher.write_text('previous launcher')
        failed = self.build(FAIL_LAUNCHER_SIGN='1')
        self.assertNotEqual(failed.returncode, 0, failed.stderr)
        self.assertEqual(main.read_text(), 'previous main')
        self.assertEqual(launcher.read_text(), 'previous launcher')

    def test_failed_launcher_publish_restores_both_bundles(self):
        initial = self.build()
        self.assertEqual(initial.returncode, 0, initial.stderr)
        main = self.bundle / 'Contents/MacOS/V2t'
        launcher = self.launcher / 'Contents/MacOS/V2t'
        main.write_text('previous main')
        launcher.write_text('previous launcher')

        failed = self.build(FAIL_LAUNCHER_PUBLISH='1')
        self.assertNotEqual(failed.returncode, 0, failed.stderr)
        self.assertEqual(main.read_text(), 'previous main')
        self.assertEqual(launcher.read_text(), 'previous launcher')

    def test_refuses_to_replace_an_unrelated_launcher(self):
        self.launcher.mkdir(parents=True)
        info = self.launcher / 'Contents/Info.plist'
        info.parent.mkdir()
        other = {'CFBundleIdentifier': 'com.example.other'}
        info.write_bytes(plistlib.dumps(other))

        result = self.build()
        self.assertNotEqual(result.returncode, 0, result.stderr)
        self.assertEqual(plistlib.loads(info.read_bytes()), other)
        self.assertFalse(self.bundle.exists())


if __name__ == '__main__':
    unittest.main()
