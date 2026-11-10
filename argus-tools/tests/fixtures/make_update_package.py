#!/usr/bin/env python3
"""Rebuilds tests/fixtures/update-f7-test.tgz the way the firmware's scripts/update.py and
scripts/flipper/assets/tarball.py build a real update package: a USTAR tar of the resources,
compressed with the C heatshrink tool (-w 13 -l 6) behind the 7-byte "HSDS" header as
resources.ths, inside f7-update-<version>/ in a gzip'd tar.

Usage: make_update_package.py HEATSHRINK_BINARY FIXTURES_DIR
The FAPs are copied from Argus_Firmware's build (GPLv3 like this repo).
"""
import io
import os
import struct
import subprocess
import sys
import tarfile


def sanitize(ti):
    ti.uid = ti.gid = 0
    ti.mtime = 0
    ti.uname = ti.gname = "furippa"
    return ti


def main():
    hs, fixtures = sys.argv[1], sys.argv[2]
    resources = io.BytesIO()
    with tarfile.open(fileobj=resources, mode="w:", format=tarfile.USTAR_FORMAT) as t:
        for arc, src in [
            ("apps/Games/snake_game.fap", "snake_game.fap"),
            ("apps/Sub-GHz/subghz_jammer_detect.fap", "subghz_jammer_detect.fap"),
            ("apps_data/js_app/plugins/js_gui__loading.fal", "js_gui__loading.fal"),
        ]:
            data = open(os.path.join(fixtures, src), "rb").read()
            ti = sanitize(tarfile.TarInfo(arc))
            ti.size = len(data)
            t.addfile(ti, io.BytesIO(data))
        manifest = b"V:0\nT:1700000000\nD:apps\n"
        ti = sanitize(tarfile.TarInfo("Manifest"))
        ti.size = len(manifest)
        t.addfile(ti, io.BytesIO(manifest))
    plain = resources.getvalue()
    compressed = subprocess.run([hs, "-e", "-w", "13", "-l", "6"], input=plain, capture_output=True, check=True).stdout
    ths = struct.pack("<IBBB", 0x53445348, 1, 13, 6) + compressed
    open(os.path.join(fixtures, "resources-c.ths"), "wb").write(ths)
    open(os.path.join(fixtures, "resources-plain.tar"), "wb").write(plain)

    pkg = io.BytesIO()
    with tarfile.open(fileobj=pkg, mode="w:gz", format=tarfile.USTAR_FORMAT) as t:
        for name, data in [
            ("f7-update-test/update.fuf", b"Filetype: Flipper firmware upgrade configuration\nVersion: 2\nInfo: test\nTarget: 7\nResources: resources.ths\n"),
            ("f7-update-test/resources.ths", ths),
        ]:
            ti = sanitize(tarfile.TarInfo(name))
            ti.size = len(data)
            t.addfile(ti, io.BytesIO(data))
    open(os.path.join(fixtures, "update-f7-test.tgz"), "wb").write(pkg.getvalue())


if __name__ == "__main__":
    main()
