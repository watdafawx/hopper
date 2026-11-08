"""Builds the scratch data root for the memory ceiling check.
usage: python make_scratch.py <root> [n_big_mods=1156]
Layout: <root>/data/settings.json, <root>/instances/{Big Pack, Small1..7}, <root>/icons/*.png
Everything is fake. Nothing outside <root> is touched."""
import sys, os, json, random, zipfile, io
from PIL import Image, ImageDraw

root = os.path.abspath(sys.argv[1])
n_big = int(sys.argv[2]) if len(sys.argv) > 2 else 1156
random.seed(7)
os.makedirs(f"{root}/data", exist_ok=True)
inst_root = f"{root}/instances"
icons = f"{root}/icons"
os.makedirs(inst_root, exist_ok=True); os.makedirs(icons, exist_ok=True)
json.dump({"instances_root": inst_root, "theme": 1, "setup_done": True}, open(f"{root}/data/settings.json", "w"))

def icon(size, seed):
    r = random.Random(seed)
    a = tuple(r.randrange(40, 255) for _ in range(3)); b = tuple(r.randrange(40, 255) for _ in range(3))
    im = Image.new("RGBA", (size, size))
    d = ImageDraw.Draw(im)
    for y in range(size):
        t = y / size
        d.line([(0, y), (size, y)], fill=tuple(int(a[i]*(1-t)+b[i]*t) for i in range(3)) + (255,))
    for _ in range(6):
        x, y = r.randrange(size), r.randrange(size); s = r.randrange(size // 8, size // 3)
        d.ellipse([x, y, x + s, y + s], fill=tuple(r.randrange(255) for _ in range(3)) + (255,))
    return im

def png(im):
    b = io.BytesIO(); im.save(b, "PNG"); return b.getvalue()

def jar(path, modid, with_logo, seed):
    toml = f'modLoader="javafml"\nloaderVersion="[2,)"\nlicense="MIT"\n[[mods]]\nmodId="{modid}"\nversion="1.0.0"\ndisplayName="Fake {modid}"\nauthors="Scratch"\n' + ('logoFile="logo.png"\n' if with_logo else '')
    with zipfile.ZipFile(path, "w", zipfile.ZIP_DEFLATED) as z:
        z.writestr("META-INF/neoforge.mods.toml", toml)
        if with_logo: z.writestr("logo.png", png(icon(128, seed)))

def make(name, n, tracked, big):
    d = f"{inst_root}/{name}"; os.makedirs(f"{d}/mods", exist_ok=True)
    addons = []
    for i in range(n):
        fn = f"fake{i:04d}-1.0.0.jar"; mid = f"fake{name.lower().replace(' ','')}{i:04d}"
        jar(f"{d}/mods/{fn}", mid, i % 3 == 0, i)
        if i < tracked:
            ic = f"{icons}/{name.replace(' ','_')}_{i:04d}.png"
            if big or i < 20: icon(256, i * 31 + len(name)).save(ic)
            addons.append({"project_id": 100000 + i, "file_id": 900000 + i, "file_name": fn, "name": f"Fake Mod {i:04d}",
                "icon": "file:///" + ic.replace("\\", "/"), "page": f"https://example.invalid/m/{i}", "author": "Scratch",
                "enabled": True, "from_pack": i < tracked - 30, "env": "both", "class_id": 6,
                "file_date": "2026-04-09T21:20:09.987Z", "date_added": "2026-09-07T08:52:31.6715993Z",
                "release_type": 1, "locked": False, "mr_project": "", "mr_version": "", "mr_sha1": ""})
    hj = {"name": name, "game_version": "1.21.1", "loader": "neoforge-21.1.259", "pack": None,
          "imported_from_cf": 1793218467, "format": 5, "addons": addons}
    json.dump(hj, open(f"{d}/hopper.json", "w"), indent=1)
    print(name, n, "jars", os.path.getsize(f"{d}/hopper.json") // 1024, "KB hopper.json")

make("Big Pack", n_big, n_big - 40, True)
for k in range(1, 8): make(f"Small{k}", 12 + k, 12 + k, False)
