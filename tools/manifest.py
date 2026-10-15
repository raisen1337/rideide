"""Writes what a release publishes beside dist/rkit-setup.exe, after build-installer.ps1:

- dist/apps.json, which rkit reads to show, for every app on its own, the version there is,
  where its program is, its SHA-256 and size, and its changelog; and the programs themselves;
- dist/latest.json, which ride reads to update itself (and the rest) with the setup program;
- dist/notes.md, the release's notes: the newest changelog entry of each app.

    python tools/manifest.py
"""
import hashlib
import json
import os
import re
import shutil
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
REPO = os.environ.get('GITHUB_REPOSITORY', 'raisen1337/rideide')
# The app, its crate's folder, and its changelog.
APPS = [
    ('ride', 'app', 'CHANGELOG.md'),
    ('rterm', 'rterm', 'rterm/CHANGELOG.md'),
    ('rkit', 'rkit', 'rkit/CHANGELOG.md'),
]


def version_of(manifest):
    text = open(os.path.join(ROOT, manifest), encoding='utf-8').read()
    package = text.split('[package]', 1)[-1].split('\n[', 1)[0]
    found = re.search(r'^version\s*=\s*"([^"]+)"', package, re.M)
    if found:
        return found.group(1)
    # `version.workspace = true`: the suite's version.
    return version_of('Cargo.toml') if manifest != 'Cargo.toml' else re.search(
        r'^version\s*=\s*"([^"]+)"', text, re.M).group(1)


def changelog(path, limit=20):
    """The entries of a changelog, newest first: `## x.y.z` headings and what follows."""
    text = open(os.path.join(ROOT, path), encoding='utf-8').read()
    entries = []
    for part in re.split(r'^## ', text, flags=re.M)[1:]:
        head, _, body = part.partition('\n')
        version = head.strip()
        if re.fullmatch(r'\d+\.\d+(\.\d+)?', version):
            entries.append({'version': version, 'notes': body.strip()})
    return entries[:limit]


def main():
    suite = version_of('Cargo.toml')
    dist = os.path.join(ROOT, 'dist')
    os.makedirs(dist, exist_ok=True)
    apps = {}
    for name, crate, log in APPS:
        exe = os.path.join(ROOT, 'target', 'release', f'{name}.exe')
        data = open(exe, 'rb').read()
        shutil.copyfile(exe, os.path.join(dist, f'{name}.exe'))
        version = version_of(f'{crate}/Cargo.toml')
        entries = changelog(log)
        if not entries or entries[0]['version'] != version:
            sys.exit(f'{log} has no entry for {name} {version} at the top')
        apps[name] = {
            'version': version,
            'url': f'https://github.com/{REPO}/releases/download/v{suite}/{name}.exe',
            'sha256': hashlib.sha256(data).hexdigest(),
            'size': len(data),
            'changelog': entries,
        }
    manifest = {'suite': suite, 'apps': apps}
    with open(os.path.join(dist, 'apps.json'), 'w', encoding='utf-8', newline='\n') as f:
        json.dump(manifest, f, indent=1)

    # ride updates itself with the setup program, when ride's own version is newer.
    setup = open(os.path.join(dist, 'rkit-setup.exe'), 'rb').read()
    latest = {
        'version': apps['ride']['version'],
        'url': f'https://github.com/{REPO}/releases/download/v{suite}/rkit-setup.exe',
        'sha256': hashlib.sha256(setup).hexdigest(),
        'size': len(setup),
    }
    with open(os.path.join(dist, 'latest.json'), 'w', encoding='utf-8', newline='\n') as f:
        json.dump(latest, f, indent=1)

    notes = '\n\n'.join(
        f"### {name} {app['version']}\n\n{app['changelog'][0]['notes']}" for name, app in apps.items()
    )
    with open(os.path.join(dist, 'notes.md'), 'w', encoding='utf-8', newline='\n') as f:
        f.write(notes + '\n')
    print('apps.json:', ', '.join(f'{n} {a["version"]}' for n, a in apps.items()), f'(suite {suite})')


if __name__ == '__main__':
    main()
