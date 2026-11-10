import pathlib
import zipfile

root = pathlib.Path(__file__).resolve().parent.parent
source = root / 'examples' / 'simple-image'
destination = root / 'examples' / 'simple-image-1.0.0.npplugin'
with zipfile.ZipFile(destination, 'w', compression=zipfile.ZIP_DEFLATED) as package:
    for filename in sorted(source.rglob('*')):
        if filename.is_file():
            entry = zipfile.ZipInfo(filename.relative_to(source).as_posix(), (2026, 11, 4, 0, 0, 0))
            entry.compress_type = zipfile.ZIP_DEFLATED
            entry.external_attr = 0o100644 << 16
            package.writestr(entry, filename.read_bytes())
print(destination)
