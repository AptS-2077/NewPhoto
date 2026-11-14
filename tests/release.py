import copy
import hashlib
import importlib.util
import io
import json
import pathlib
import struct
import tarfile
import tempfile
import unittest

specification = importlib.util.spec_from_file_location('release_package', pathlib.Path(__file__).resolve().parent.parent / 'scripts/package-release.py')
package = importlib.util.module_from_spec(specification)
specification.loader.exec_module(package)


class ReleaseSafety(unittest.TestCase):
    def setUp(self):
        self.target = 'x86_64-unknown-linux-gnu'
        self.tag = 'v0.1.0-beta.1'
        self.source_sha = 'a' * 40
        self.prefix = 'newphoto-0.1.0-beta.1-linux-x86_64/'
        binary = bytearray(64)
        binary[:6] = b'\x7fELF\x02\x01'
        struct.pack_into('<H', binary, 18, 62)
        self.files = {'newphoto': bytes(binary), 'LICENSE': b'license', 'docs/api.md': b'api', 'README.md': b'readme', '.env.example': b'DATABASE_URL=\n'}
        self.files.update({'web/dist/' + name: b'fixture' for name in package.WEB_FILES})
        manifest = {'version': self.tag[1:], 'tag': self.tag, 'source_commit': self.source_sha, 'target': self.target, 'sha256': {name: hashlib.sha256(content).hexdigest() for name, content in self.files.items()}}
        self.files['BUILD-INFO.json'] = json.dumps(manifest).encode()

    def archive(self, directory, files=None, symlink=False):
        path = pathlib.Path(directory) / 'package.tar.gz'
        with tarfile.open(path, 'w:gz') as archive:
            for name, content in (files or self.files).items():
                entry = tarfile.TarInfo(self.prefix + name)
                entry.mode = 0o755 if name == 'newphoto' else 0o644
                if symlink and name == 'newphoto':
                    entry.type = tarfile.SYMTYPE
                    entry.linkname = '/etc/passwd'
                    archive.addfile(entry)
                else:
                    entry.size = len(content)
                    archive.addfile(entry, io.BytesIO(content))
        return path

    def verify(self, path):
        return package.verify_archive(path, self.target, self.tag, self.source_sha)

    def test_valid_inventory_and_provenance(self):
        with tempfile.TemporaryDirectory() as directory:
            self.assertEqual(self.verify(self.archive(directory))['source_commit'], self.source_sha)

    def test_private_file_is_not_allowed(self):
        files = {**self.files, 'secrets/master.key': b'private'}
        with tempfile.TemporaryDirectory() as directory, self.assertRaises(ValueError):
            self.verify(self.archive(directory, files))

    def test_traversal_is_not_allowed(self):
        files = {**self.files, '../outside': b'escape'}
        with tempfile.TemporaryDirectory() as directory, self.assertRaises(ValueError):
            self.verify(self.archive(directory, files))

    def test_tampered_asset_is_rejected(self):
        files = {**self.files, 'web/dist/app.js': b'changed after packaging'}
        with tempfile.TemporaryDirectory() as directory, self.assertRaises(ValueError):
            self.verify(self.archive(directory, files))

    def test_wrong_architecture_is_rejected(self):
        files = copy.deepcopy(self.files)
        binary = bytearray(files['newphoto'])
        struct.pack_into('<H', binary, 18, 183)
        files['newphoto'] = bytes(binary)
        with tempfile.TemporaryDirectory() as directory, self.assertRaises(ValueError):
            self.verify(self.archive(directory, files))

    def test_symlink_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory, self.assertRaises(ValueError):
            self.verify(self.archive(directory, symlink=True))

    def test_secret_signature_is_rejected(self):
        with self.assertRaises(ValueError):
            package.scan({'README.md': b'-----BEGIN PRIVATE KEY-----'})

    def test_wrong_source_revision_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory, self.assertRaises(ValueError):
            package.verify_archive(self.archive(directory), self.target, self.tag, 'b' * 40)


if __name__ == '__main__':
    unittest.main()
