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
        for name in ('build.sh', 'Info.plist', 'Entitlements.plist'):
            for file in source.glob(name):
                shutil.copyfile(file, self.script.parent / name)

        self.bundle = self.repo / 'publish/V2t.app'
        self.tools = root / 'tools'
        self.tools.mkdir()
        self.mock_tool('uname', 'case "$1" in -s) echo Darwin;; -m) echo arm64;; esac')
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
    test "$*" != "${*#*--entitlements}"
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

    def test_build_creates_signed_menu_bar_bundle(self):
        result = self.build()
        self.assertEqual(result.returncode, 0, result.stderr)
        with (self.bundle / 'Contents/Info.plist').open('rb') as file:
            info = plistlib.load(file)
        executable = self.bundle / 'Contents/MacOS' / info['CFBundleExecutable']
        self.assertEqual(executable.read_text(), 'built executable')
        self.assertTrue(os.access(executable, os.X_OK))
        self.assertEqual(info['CFBundleIdentifier'], 'com.lawdalana.v2t')
        self.assertEqual(info['CFBundleShortVersionString'], '2.0.0')
        self.assertEqual(info['LSMinimumSystemVersion'], '12.0')
        self.assertIs(info['LSUIElement'], True)
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
        self.assertEqual(list(self.bundle.parent.iterdir()), [self.bundle])


if __name__ == '__main__':
    unittest.main()
