#!/usr/bin/env python3
"""Create a PF1007L boot-profile trial with a 64 MiB default CMA pool."""
import argparse
import hashlib
import os
from pathlib import Path
import re
import secrets
import shutil
import struct
import subprocess
import zlib


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def run(*args, **kwargs):
    return subprocess.run(args, check=True, **kwargs)


def make_profile(bundle, output, profile, cma_mib=64, accelerometer=False, gpu_devfreq=False):
    payloads = ('zImage', 'initramfs.cpio.gz', 'sun8i-a33-pf1007l-stage1.dtb', 'boot-control.scr')
    if not re.fullmatch(r'arch-usb-[a-z0-9][a-z0-9-]{0,39}', profile):
        raise ValueError('Invalid profile ID')
    if output.is_relative_to(Path(__file__).resolve().parents[1]):
        raise ValueError('Output must be outside the source checkout')
    if {p.name for p in bundle.iterdir()} != {*payloads, 'SHA256SUMS'}:
        raise ValueError('Input must be an installed five-file boot bundle')
    pins = {}
    for line in (bundle / 'SHA256SUMS').read_text().splitlines():
        match = re.fullmatch(r'([0-9a-f]{64})  (.+)', line)
        if not match or match[2] not in payloads or match[2] in pins:
            raise ValueError('Invalid input checksum manifest')
        pins[match[2]] = match[1]
    if set(pins) != set(payloads):
        raise ValueError('Incomplete input checksum manifest')
    for name in payloads:
        source = bundle / name
        if source.is_symlink() or not source.is_file() or digest(source) != pins[name]:
            raise ValueError('Input payload is not a matching regular file')
    raw = (bundle / 'boot-control.scr').read_bytes()
    header = bytearray(raw[:64])
    magic, header_crc, _, size, _, _, data_crc = struct.unpack('>7I', header[:28])
    header[4:8] = bytes(4)
    if magic != 0x27051956 or zlib.crc32(header) != header_crc or len(raw) != 64 + size or zlib.crc32(raw[64:]) != data_crc:
        raise ValueError('Invalid legacy U-Boot script header or checksum')
    script_size, terminator = struct.unpack('>2I', raw[64:72])
    if terminator or script_size != len(raw) - 72:
        raise ValueError('Only single-part legacy scripts are supported')
    script = raw[72:].decode('ascii')
    old_profiles = set(re.findall(r'(arch-usb-[a-z0-9-]+)/zImage', script))
    old_builds = re.findall(r'(?m)^setenv build (working-control-[0-9a-f]{16})$', script)
    if len(old_profiles) != 1 or len(old_builds) != 1:
        raise ValueError('Launcher must identify one profile and build')
    old_profile = old_profiles.pop()
    if profile == old_profile:
        raise ValueError('Use a new immutable profile ID')
    dt_name = payloads[2]
    dt = bundle / dt_name
    for prop in ('#address-cells', '#size-cells'):
        if subprocess.check_output(['fdtget', '-t', 'x', str(dt), '/', prop], text=True).strip() != '1':
            raise ValueError('This recipe requires the PF1007L 32-bit device tree')
    if 'reserved-memory' in subprocess.check_output(['fdtget', '-l', str(dt), '/'], text=True).splitlines():
        raise ValueError('Existing reserved-memory policy needs manual review')
    output.mkdir(parents=True, exist_ok=False)
    for name in payloads[:3]:
        shutil.copyfile(bundle / name, output / name)
    patched = str(output / dt_name)
    run('fdtput', '-c', patched, '/reserved-memory')
    for prop in ('#address-cells', '#size-cells'):
        run('fdtput', '-t', 'x', patched, '/reserved-memory', prop, '1')
    run('fdtput', patched, '/reserved-memory', 'ranges')
    node = '/reserved-memory/linux,cma'
    run('fdtput', '-c', patched, node)
    run('fdtput', '-t', 's', patched, node, 'compatible', 'shared-dma-pool')
    run('fdtput', patched, node, 'reusable')
    run('fdtput', '-t', 'x', patched, node, 'size', f'{cma_mib * 1024 * 1024:x}')
    run('fdtput', '-t', 'x', patched, node, 'alignment', '1000000')
    run('fdtput', patched, node, 'linux,cma-default')
    if accelerometer:
        controller = '/soc/i2c@1c2b000'
        run('fdtget', patched, controller, 'status', stdout=subprocess.DEVNULL)
        run('fdtput', '-t', 's', patched, controller, 'status', 'okay')
        sensor = controller + '/accelerometer@19'
        run('fdtput', '-c', patched, sensor)
        run('fdtput', '-t', 's', patched, sensor, 'compatible', 'silan,sc7a20')
        run('fdtput', '-t', 'x', patched, sensor, 'reg', '19')
    if gpu_devfreq:
        table = '/opp-table-gpu'
        expected = {'opp-144000000', 'opp-240000000', 'opp-384000000'}
        children = set(subprocess.check_output(['fdtget', '-l', patched, table], text=True).splitlines())
        if children != expected:
            raise ValueError('GPU scaling requires the original A33 frequency table')
        for child in children:
            properties = set(subprocess.check_output(['fdtget', '-p', patched, table + '/' + child], text=True).splitlines())
            if properties != {'opp-hz'}:
                raise ValueError('GPU frequency table contains unreviewed voltage or other policy')
            cells = subprocess.check_output(['fdtget', '-t', 'x', patched, table + '/' + child, 'opp-hz'], text=True).split()
            if cells != ['0', f'{int(child[4:]):x}']:
                raise ValueError('GPU frequency table does not match its node names')
        phandle = subprocess.check_output(['fdtget', '-t', 'x', patched, table, 'phandle'], text=True).strip()
        run('fdtput', '-t', 'x', patched, '/soc/gpu@1c40000', 'operating-points-v2', phandle)
    build = 'working-control-' + secrets.token_hex(8)
    script = script.replace(old_profile + '/', profile + '/').replace(old_builds[0], build)
    script = script.replace(pins[dt_name], digest(output / dt_name))
    script = script.replace(hex(dt.stat().st_size), hex((output / dt_name).stat().st_size))
    source_script = output.parent / (output.name + '.cmd')
    source_script.write_text(script)
    run('mkimage', '-A', 'arm', '-O', 'linux', '-T', 'script', '-C', 'none', '-n', 'PF1007L CMA64 trial',
        '-d', str(source_script), str(output / 'boot-control.scr'), env={**os.environ, 'SOURCE_DATE_EPOCH': '0'})
    (output / 'SHA256SUMS').write_text(''.join(f'{digest(output / name)}  {name}\n' for name in payloads))
    print(f'Profile: {profile}\nBuild: {build}\nBundle: {output}')


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--bundle', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--profile-id', required=True)
    parser.add_argument('--cma-mib', type=int, choices=(32, 64, 128), default=64)
    parser.add_argument('--accelerometer', action='store_true', help='Enable PF1007L TWI1 and SC7A20 at 0x19')
    parser.add_argument('--gpu-devfreq', action='store_true', help='Restore the original 144/240/384 MHz A33 GPU scaling table')
    args = parser.parse_args()
    make_profile(args.bundle.resolve(), args.output.resolve(), args.profile_id, args.cma_mib, args.accelerometer, args.gpu_devfreq)
