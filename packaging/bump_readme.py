#!/usr/bin/env python3
import re
import sys


ver, sha = sys.argv[1], sys.argv[2]
with open("README.md") as f:
    s = f.read()
s = re.sub(r"releases/download/v[0-9.]+/", f"releases/download/v{ver}/", s)
s = re.sub(r"rnetapplet-[0-9.]+-x86_64-linux", f"rnetapplet-{ver}-x86_64-linux", s)
s = re.sub(r"rnetapplet-[0-9.]+-aarch64-linux", f"rnetapplet-{ver}-aarch64-linux", s)
s = re.sub(r'echo "\w{64}  rnetapplet\.tar\.gz"', f'echo "{sha}  rnetapplet.tar.gz"', s)
with open("README.md", "w") as f:
    f.write(s)