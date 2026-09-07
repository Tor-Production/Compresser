#!/usr/bin/env python3
"""Build the mass measurement corpus, one directory per class.

The twenty-five images in ``samples/`` were chosen by hand to bracket the algorithm's behaviour.
They are good at that and useless for the questions findings 15 to 17 left open, all of which are
about *distributions*: does the best block size split by kind of image, how often does a tree or a
per-image grid choice actually pay, and where do interior alphabet gaps live. Twenty-five images
cannot answer any of those, and eight photographs choosing the same block size is not evidence that
photographs choose it.

So this fetches a few thousand, laid out the way ``brp_lab::corpus`` reads them::

    <root>/photo/kodak/          the eight the published figures quote, plus the rest of the suite
    <root>/photo/raw-crop/       crops from CC0 raw files, never through a lossy step
    <root>/synthetic/clipart/    CC0 vector clip art, rendered
    <root>/synthetic/generated/  this repository's own gen-samples output
    <root>/texture-ui/ambientcg/ CC0 material maps
    <root>/texture-ui/icons/     permissively licensed icon sets, rendered at several sizes
    <root>/screenshot/phone/     F-Droid listing screenshots
    <root>/screenshot/desktop/   Flathub listing screenshots
    <root>/photo-lossy/div2k/    crops from DIV2K -- a PROBE ROW, see below

**Nothing here is committed.** The corpus lives outside the repository; only this script does.

Why the photograph class is built from raw files
------------------------------------------------
``brp_imageio::is_supported_image`` refuses JPEG on purpose: a JPEG has had its high frequencies
quantised away and carries 8x8 DCT blocking, so every lossless codec scores far better on it than
on the photograph it came from. Almost every mass photograph dataset is JPEG-derived, and feeding
one in would mean transcoding past a guard the project installed deliberately -- on the very axis
being measured, because smoothed content prefers larger blocks.

So the photograph class is rendered from CC0 raw camera files and **cropped, never resized**: a
downscale smooths sensor noise the same way a lossy step does, while a crop preserves the pixel
statistics exactly and turns one 12-megapixel frame into several independent tiles at the geometry
the existing corpus already uses.

``photo-lossy/`` is the control for that decision, not part of the photograph class. DIV2K ships as
PNG but its sources are of mixed and undocumented provenance. It is measured as its own row so the
size of the confound is a number rather than an assumption. Never merge it into ``photo/``.

Requirements: Python 3.9+, and ImageMagick 7 on PATH (``magick``) with its ``raw`` and ``rsvg``
delegates, which is what decodes camera raw and renders SVG.

Usage::

    python scripts/fetch-corpus.py --root C:\\brp-corpus
    python scripts/fetch-corpus.py --root C:\\brp-corpus --classes photo --count 200
    python scripts/fetch-corpus.py --root C:\\brp-corpus --dry-run

It is resumable and idempotent: a class already holding its target is skipped, and a partially
fetched one is topped up. Re-running costs a few index requests and nothing else.
"""

from __future__ import annotations

import argparse
import io
import json
import random
import re
import shutil
import subprocess
import sys
import tarfile
import tempfile
import urllib.error
import urllib.request
import zipfile
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

USER_AGENT = "brp-corpus/1.0 (lossless codec measurement; contact via repository)"
TIMEOUT = 120
# The geometry the hand-built corpus uses, so a crop is comparable with a Kodak image.
TILE_W, TILE_H = 768, 512

# Every source, with the licence it is used under. Printed and written to the manifest, because a
# corpus whose provenance is not recorded is a corpus whose figures cannot be defended.
SOURCES = {
    "photo/kodak": ("Kodak True Color Image Suite", "released for unrestricted research use"),
    "photo/raw-crop": ("raw.pixls.us raw sample repository", "CC0"),
    "synthetic/clipart": ("openclipart.org", "CC0"),
    "synthetic/generated": ("this repository's gen-samples", "same licence as this repository"),
    "texture-ui/ambientcg": ("ambientCG material library", "CC0"),
    "texture-ui/icons": ("Bootstrap Icons (MIT), Material Design Icons (Apache-2.0)", "MIT / Apache-2.0"),
    "screenshot/phone": ("F-Droid app listing screenshots", "per-app free software licences"),
    "screenshot/desktop": ("Flathub app listing screenshots", "per-app free software licences"),
    "photo-lossy/div2k": ("DIV2K (ETH Zurich, NTIRE)", "research use; mixed source provenance"),
}


# ---------------------------------------------------------------------------------------------
# Plumbing
# ---------------------------------------------------------------------------------------------


def http(url: str, retries: int = 3) -> bytes:
    """One GET, with the retries a few thousand requests make necessary."""
    last = None
    for attempt in range(retries):
        try:
            req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
            with urllib.request.urlopen(req, timeout=TIMEOUT) as r:
                return r.read()
        except (urllib.error.URLError, OSError) as e:  # includes timeouts and resets
            last = e
    raise RuntimeError(f"{url}: {last}")


def http_json(url: str):
    return json.loads(http(url))


def magick(args: list[str]) -> bool:
    """One ImageMagick call. Returns False rather than raising: on a corpus this size a handful of
    inputs will always be broken, and one bad file must not end the run."""
    try:
        done = subprocess.run(
            ["magick", *args],
            stdout=subprocess.DEVNULL,
            stderr=subprocess.PIPE,
            timeout=300,
        )
        return done.returncode == 0
    except (OSError, subprocess.TimeoutExpired):
        return False


def have_magick() -> bool:
    return shutil.which("magick") is not None


def existing(directory: Path) -> int:
    if not directory.is_dir():
        return 0
    return sum(1 for p in directory.iterdir() if p.suffix.lower() in (".png", ".webp"))


def safe(name: str) -> str:
    return re.sub(r"[^A-Za-z0-9._-]+", "-", name).strip("-")[:80]


def parallel(items, fn, jobs: int) -> int:
    """Runs `fn` over `items`, counting the ones that produced a file. Exceptions are swallowed per
    item for the same reason `magick` does not raise."""
    done = 0
    with ThreadPoolExecutor(max_workers=jobs) as pool:
        for ok in pool.map(lambda it: _guard(fn, it), items):
            done += 1 if ok else 0
    return done


def _guard(fn, item) -> bool:
    try:
        return bool(fn(item))
    except Exception:
        return False


def tiles(width: int, height: int, count: int) -> list[tuple[int, int]]:
    """Up to `count` non-overlapping tile origins, spread over the frame rather than taken from one
    corner: adjacent tiles of a photograph are far more alike than distant ones, and a corpus of
    near-duplicates has fewer images in it than its file count claims."""
    cols = max(1, width // TILE_W)
    rows = max(1, height // TILE_H)
    if cols * rows == 0:
        return []
    picks = []
    total = cols * rows
    step = max(1, total // count)
    for i in range(0, total, step):
        if len(picks) >= count:
            break
        picks.append(((i % cols) * TILE_W, (i // cols) * TILE_H))
    return picks


def cut_tiles(source: Path, out_dir: Path, stem: str, count: int) -> int:
    """Crops `count` tiles out of an already-decoded image. The crop is exact -- no resampling."""
    probe = subprocess.run(
        ["magick", "identify", "-format", "%w %h", str(source)],
        capture_output=True,
        text=True,
        timeout=120,
    )
    if probe.returncode != 0 or not probe.stdout.strip():
        return 0
    try:
        width, height = (int(v) for v in probe.stdout.split()[:2])
    except ValueError:
        return 0

    written = 0
    for index, (x, y) in enumerate(tiles(width, height, count)):
        target = out_dir / f"{stem}-t{index}.png"
        if target.exists():
            written += 1
            continue
        ok = magick(
            [
                str(source),
                "-crop",
                f"{TILE_W}x{TILE_H}+{x}+{y}",
                "+repage",
                "-depth",
                "8",
                "-define",
                "png:compression-level=6",
                str(target),
            ]
        )
        written += 1 if ok and target.exists() else 0
    return written


# ---------------------------------------------------------------------------------------------
# photo/kodak -- the bridge to every figure this project has already published
# ---------------------------------------------------------------------------------------------


def fetch_kodak(out: Path, target: int, jobs: int) -> int:
    out.mkdir(parents=True, exist_ok=True)

    def one(n: int) -> bool:
        path = out / f"photo-kodim{n:02d}.png"
        if path.exists():
            return True
        path.write_bytes(http(f"https://r0k.us/graphics/kodak/kodak/kodim{n:02d}.png"))
        return True

    parallel(range(1, min(target, 24) + 1), one, jobs)
    return existing(out)


# ---------------------------------------------------------------------------------------------
# photo/raw-crop -- CC0 camera raw, rendered to 8 bit and cropped
# ---------------------------------------------------------------------------------------------

PIXLS = "https://raw.pixls.us/data/"
# A raw larger than this is a medium-format or high-resolution body; skipping them keeps the
# download bounded without biasing the *content*, only the sensor size.
MAX_RAW_BYTES = 60 * 1024 * 1024


def pixls_listing(url: str) -> list[str]:
    html = http(url).decode("utf-8", "replace")
    return [h for h in re.findall(r'href="([^"?][^"]*)"', html) if h not in ("/", "../")]


def fetch_raw_crops(out: Path, target: int, jobs: int, per_raw: int, keep_raw: bool) -> int:
    out.mkdir(parents=True, exist_ok=True)
    have = existing(out)
    if have >= target:
        return have

    print("    indexing raw.pixls.us ...", flush=True)
    makes = [m for m in pixls_listing(PIXLS) if m.endswith("/")]
    rng = random.Random(20260907)
    rng.shuffle(makes)

    # One file per camera model, so the class varies by sensor and scene rather than by frame.
    candidates: list[tuple[str, str]] = []
    needed_raws = (target - have) // max(1, per_raw) + 4
    for make in makes:
        if len(candidates) >= needed_raws:
            break
        try:
            models = [m for m in pixls_listing(PIXLS + make) if m.endswith("/")]
        except RuntimeError:
            continue
        rng.shuffle(models)
        for model in models[:6]:
            if len(candidates) >= needed_raws:
                break
            try:
                files = [f for f in pixls_listing(PIXLS + make + model) if not f.endswith("/")]
            except RuntimeError:
                continue
            if files:
                candidates.append((make + model, rng.choice(files)))
    print(f"    {len(candidates)} raw files selected across cameras", flush=True)

    def one(item: tuple[str, str]) -> bool:
        prefix, filename = item
        stem = safe(prefix.replace("/", "-") + "-" + Path(filename).stem)
        if (out / f"{stem}-t0.png").exists():
            return True
        url = PIXLS + prefix + filename
        with tempfile.TemporaryDirectory() as tmp:
            raw_path = Path(tmp) / safe(filename)
            try:
                req = urllib.request.Request(url, headers={"User-Agent": USER_AGENT})
                with urllib.request.urlopen(req, timeout=TIMEOUT) as r:
                    length = int(r.headers.get("Content-Length") or 0)
                    if length > MAX_RAW_BYTES:
                        return False
                    raw_path.write_bytes(r.read())
            except Exception:
                return False
            if keep_raw:
                keep_dir = out.parent / "raw-originals"
                keep_dir.mkdir(parents=True, exist_ok=True)
                shutil.copy2(raw_path, keep_dir / raw_path.name)
            # Render to 8-bit sRGB. This IS a narrowing of a deeper capture and is the one
            # deliberate information loss in the corpus; it is what an 8-bit codec would be given.
            decoded = Path(tmp) / "decoded.png"
            if not magick([str(raw_path), "-colorspace", "sRGB", "-depth", "8", str(decoded)]):
                return False
            return cut_tiles(decoded, out, stem, per_raw) > 0

    parallel(candidates, one, jobs)
    return existing(out)


# ---------------------------------------------------------------------------------------------
# synthetic/clipart and synthetic/generated
# ---------------------------------------------------------------------------------------------

# Renders at several sizes, because an icon and a poster are different content even from one file.
CLIPART_SIZES = [256, 512, 1024]


def fetch_clipart(out: Path, target: int, jobs: int) -> int:
    out.mkdir(parents=True, exist_ok=True)
    have = existing(out)
    if have >= target:
        return have
    rng = random.Random(20260907)
    ids = rng.sample(range(1000, 340000), min(target * 2, 40000))

    def one(cid: int) -> bool:
        size = CLIPART_SIZES[cid % len(CLIPART_SIZES)]
        target_path = out / f"openclipart-{cid}-{size}.png"
        if target_path.exists():
            return True
        try:
            svg = http(f"https://openclipart.org/download/{cid}", retries=1)
        except RuntimeError:
            return False
        if b"<svg" not in svg[:4000] and b"<?xml" not in svg[:200]:
            return False
        with tempfile.TemporaryDirectory() as tmp:
            src = Path(tmp) / "art.svg"
            src.write_bytes(svg)
            ok = magick(
                [
                    "-background",
                    "white",
                    "-density",
                    "144",
                    str(src),
                    "-resize",
                    f"{size}x{size}>",
                    "-flatten",
                    "-depth",
                    "8",
                    str(target_path),
                ]
            )
        return ok and target_path.exists()

    # Requested in slices so a high failure rate cannot leave the class short.
    index = 0
    while existing(out) < target and index < len(ids):
        batch = ids[index : index + max(64, (target - existing(out)) * 2)]
        index += len(batch)
        parallel(batch, one, jobs)
    return existing(out)


def fetch_generated(out: Path) -> int:
    """The repository's own generator, run once.

    Deliberately not seeded into hundreds of variants. `gen-samples` has no seed parameter, and
    adding one would produce images that are near-copies of each other -- which inflates the image
    count without adding information, and that is the exact failure this corpus exists to fix.
    Seventeen deterministic images are worth having as a bridge to every earlier finding; two
    hundred correlated ones are not.
    """
    out.mkdir(parents=True, exist_ok=True)
    if existing(out) > 0:
        return existing(out)
    repo = Path(__file__).resolve().parent.parent
    done = subprocess.run(
        ["cargo", "run", "-p", "brp-bench", "--release", "--bin", "gen-samples", "--", str(out)],
        cwd=repo,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.PIPE,
    )
    if done.returncode != 0:
        print("    gen-samples failed; is cargo on PATH?", file=sys.stderr)
    return existing(out)


# ---------------------------------------------------------------------------------------------
# texture-ui
# ---------------------------------------------------------------------------------------------

# Two maps per material, because they are genuinely different content: a colour map is photographic
# and a normal map is a smooth three-channel field that nothing else in the corpus resembles.
ACG_MAPS = ("_Color.png", "_NormalGL.png")


def fetch_ambientcg(out: Path, target: int, jobs: int) -> int:
    out.mkdir(parents=True, exist_ok=True)
    have = existing(out)
    if have >= target:
        return have
    wanted = (target - have) // len(ACG_MAPS) + 2
    index = http_json(
        "https://ambientcg.com/api/v2/full_json"
        f"?type=Material&limit={wanted}&include=downloadData&sort=popular"
    )

    def one(asset) -> bool:
        asset_id = asset.get("assetId")
        if not asset_id:
            return False
        if (out / f"{asset_id}_Color.png").exists():
            return True
        downloads = (
            asset.get("downloadFolders", {})
            .get("default", {})
            .get("downloadFiletypeCategories", {})
            .get("zip", {})
            .get("downloads", [])
        )
        link = next(
            (d.get("downloadLink") for d in downloads if (d.get("attribute") or "").startswith("1K-PNG")),
            None,
        )
        if not link:
            return False
        try:
            blob = http(link, retries=2)
        except RuntimeError:
            return False
        with zipfile.ZipFile(io.BytesIO(blob)) as archive:
            for name in archive.namelist():
                if not name.endswith(ACG_MAPS):
                    continue
                target_path = out / safe(Path(name).name)
                if target_path.exists():
                    continue
                target_path.write_bytes(archive.read(name))
        return True

    parallel(index.get("foundAssets", []), one, jobs)
    return existing(out)


ICON_SETS = [
    # (name, tarball or zip url, path filter, licence)
    (
        "bootstrap",
        "https://github.com/twbs/icons/releases/download/v1.11.3/bootstrap-icons-1.11.3.zip",
        "icons/",
    ),
    (
        "material",
        "https://codeload.github.com/Templarian/MaterialDesign-SVG/tar.gz/refs/heads/master",
        "/svg/",
    ),
]
ICON_SIZES = [64, 128, 256]


def fetch_icons(out: Path, target: int, jobs: int) -> int:
    """Icons are the smallest images in the corpus and the ones stage 1 and stage 0.5 should like
    most: few colours, large flat regions, and an alpha channel that is constant over most of it."""
    out.mkdir(parents=True, exist_ok=True)
    if existing(out) >= target:
        return existing(out)

    svgs: list[tuple[str, bytes]] = []
    for name, url, needle in ICON_SETS:
        try:
            blob = http(url, retries=2)
        except RuntimeError:
            print(f"    icon set {name} unavailable, skipping", file=sys.stderr)
            continue
        if url.endswith(".zip"):
            with zipfile.ZipFile(io.BytesIO(blob)) as archive:
                for member in archive.namelist():
                    if member.endswith(".svg") and needle in member:
                        svgs.append((f"{name}-{Path(member).stem}", archive.read(member)))
        else:
            with tarfile.open(fileobj=io.BytesIO(blob), mode="r:gz") as archive:
                for member in archive.getmembers():
                    if member.name.endswith(".svg") and needle in member.name:
                        handle = archive.extractfile(member)
                        if handle:
                            svgs.append((f"{name}-{Path(member.name).stem}", handle.read()))

    rng = random.Random(20260907)
    rng.shuffle(svgs)
    plan = [(stem, data, ICON_SIZES[i % len(ICON_SIZES)]) for i, (stem, data) in enumerate(svgs)]
    plan = plan[: max(0, target - existing(out))]

    def one(item) -> bool:
        stem, data, size = item
        target_path = out / f"{safe(stem)}-{size}.png"
        if target_path.exists():
            return True
        with tempfile.TemporaryDirectory() as tmp:
            src = Path(tmp) / "icon.svg"
            src.write_bytes(data)
            ok = magick(
                [
                    "-background",
                    "none",
                    "-density",
                    "384",
                    str(src),
                    "-resize",
                    f"{size}x{size}",
                    "-depth",
                    "8",
                    str(target_path),
                ]
            )
        return ok and target_path.exists()

    parallel(plan, one, jobs)
    return existing(out)


# ---------------------------------------------------------------------------------------------
# screenshot
# ---------------------------------------------------------------------------------------------


def fetch_fdroid(out: Path, target: int, jobs: int) -> int:
    out.mkdir(parents=True, exist_ok=True)
    have = existing(out)
    if have >= target:
        return have
    print("    downloading the F-Droid index (about 56 MB) ...", flush=True)
    index = http_json("https://f-droid.org/repo/index-v2.json")

    shots: list[tuple[str, str]] = []
    for package, entry in (index.get("packages") or {}).items():
        phone = ((entry.get("metadata") or {}).get("screenshots") or {}).get("phone") or {}
        for _lang, entries in phone.items():
            for item in entries or []:
                name = item.get("name")
                if name and name.lower().endswith(".png"):
                    shots.append((package, name))
                    break  # one per app: two screens of one app are near-duplicates
            break
    rng = random.Random(20260907)
    rng.shuffle(shots)

    def one(item: tuple[str, str]) -> bool:
        package, name = item
        target_path = out / f"{safe(package)}.png"
        if target_path.exists():
            return True
        try:
            target_path.write_bytes(http("https://f-droid.org/repo" + name, retries=1))
        except RuntimeError:
            return False
        return True

    parallel(shots[: (target - have) * 2], one, jobs)
    return existing(out)


def fetch_flathub(out: Path, target: int, jobs: int) -> int:
    out.mkdir(parents=True, exist_ok=True)
    have = existing(out)
    if have >= target:
        return have
    apps = http_json("https://flathub.org/api/v2/appstream")
    rng = random.Random(20260907)
    rng.shuffle(apps)

    def one(app_id: str) -> bool:
        target_path = out / f"{safe(app_id)}.png"
        if target_path.exists():
            return True
        try:
            entry = http_json(f"https://flathub.org/api/v2/appstream/{app_id}")
        except (RuntimeError, json.JSONDecodeError):
            return False
        for shot in entry.get("screenshots") or []:
            for size in shot.get("sizes") or []:
                src = size.get("src") or ""
                if src.endswith("_orig.png"):
                    try:
                        target_path.write_bytes(http(src, retries=1))
                        return True
                    except RuntimeError:
                        return False
        return False

    parallel(apps[: (target - have) * 3], one, jobs)
    return existing(out)


# ---------------------------------------------------------------------------------------------
# photo-lossy -- the probe row, never part of the photograph class
# ---------------------------------------------------------------------------------------------

DIV2K = "https://data.vision.ee.ethz.ch/cvl/DIV2K/DIV2K_valid_HR.zip"


def fetch_div2k(out: Path, target: int, per_image: int) -> int:
    """One 449 MB archive of a hundred 2K photographs, cropped the same way `photo/raw-crop` is.

    The validation split rather than the 3.3 GB training split on purpose: this is a probe for how
    much lossy provenance moves the answer, and a hundred distinct scenes measure that as well as
    eight hundred would, for a seventh of the download.
    """
    out.mkdir(parents=True, exist_ok=True)
    if existing(out) >= target:
        return existing(out)
    print("    downloading DIV2K_valid_HR (about 449 MB) ...", flush=True)
    blob = http(DIV2K, retries=2)
    with tempfile.TemporaryDirectory() as tmp:
        with zipfile.ZipFile(io.BytesIO(blob)) as archive:
            names = [n for n in archive.namelist() if n.lower().endswith(".png")]
            for name in names:
                if existing(out) >= target:
                    break
                stem = safe(Path(name).stem)
                if (out / f"{stem}-t0.png").exists():
                    continue
                extracted = Path(tmp) / "page.png"
                extracted.write_bytes(archive.read(name))
                cut_tiles(extracted, out, stem, per_image)
    return existing(out)


# ---------------------------------------------------------------------------------------------
# Driver
# ---------------------------------------------------------------------------------------------


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Build the BRP mass measurement corpus outside the repository.",
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    parser.add_argument("--root", default=r"C:\brp-corpus", help="corpus root (default: %(default)s)")
    parser.add_argument(
        "--classes",
        default="photo,synthetic,texture-ui,screenshot,photo-lossy",
        help="comma-separated classes to fetch",
    )
    parser.add_argument("--count", type=int, default=1000, help="target images per class")
    parser.add_argument("--jobs", type=int, default=8, help="parallel downloads")
    parser.add_argument("--per-raw", type=int, default=4, help="crops taken from each raw frame")
    parser.add_argument("--keep-raw", action="store_true", help="keep the downloaded raw originals")
    parser.add_argument("--dry-run", action="store_true", help="print the plan and stop")
    args = parser.parse_args()

    root = Path(args.root)
    wanted = [c.strip() for c in args.classes.split(",") if c.strip()]

    print(f"corpus root: {root}")
    print(f"classes:     {', '.join(wanted)}")
    print(f"target:      {args.count} images per class\n")
    print("sources and licences")
    for key, (name, licence) in SOURCES.items():
        if key.split("/")[0] in wanted:
            print(f"  {key:<24} {name}  [{licence}]")
    print()

    if args.dry_run:
        print("dry run: nothing fetched")
        return 0
    if not have_magick():
        print("ImageMagick ('magick') is not on PATH; it decodes raw and renders SVG.", file=sys.stderr)
        return 1

    n = args.count
    results: dict[str, int] = {}

    if "photo" in wanted:
        print("photo")
        results["photo/kodak"] = fetch_kodak(root / "photo" / "kodak", 24, args.jobs)
        print(f"    kodak      {results['photo/kodak']}")
        results["photo/raw-crop"] = fetch_raw_crops(
            root / "photo" / "raw-crop", max(0, n - 24), args.jobs, args.per_raw, args.keep_raw
        )
        print(f"    raw-crop   {results['photo/raw-crop']}")

    if "synthetic" in wanted:
        print("synthetic")
        results["synthetic/generated"] = fetch_generated(root / "synthetic" / "generated")
        print(f"    generated  {results['synthetic/generated']}")
        results["synthetic/clipart"] = fetch_clipart(
            root / "synthetic" / "clipart", max(0, n - results["synthetic/generated"]), args.jobs
        )
        print(f"    clipart    {results['synthetic/clipart']}")

    if "texture-ui" in wanted:
        print("texture-ui")
        results["texture-ui/ambientcg"] = fetch_ambientcg(
            root / "texture-ui" / "ambientcg", min(300, n // 3), args.jobs
        )
        print(f"    ambientcg  {results['texture-ui/ambientcg']}")
        results["texture-ui/icons"] = fetch_icons(
            root / "texture-ui" / "icons", max(0, n - results["texture-ui/ambientcg"]), args.jobs
        )
        print(f"    icons      {results['texture-ui/icons']}")

    if "screenshot" in wanted:
        print("screenshot")
        results["screenshot/phone"] = fetch_fdroid(root / "screenshot" / "phone", n // 2, args.jobs)
        print(f"    phone      {results['screenshot/phone']}")
        results["screenshot/desktop"] = fetch_flathub(
            root / "screenshot" / "desktop", n - results["screenshot/phone"], args.jobs
        )
        print(f"    desktop    {results['screenshot/desktop']}")

    if "photo-lossy" in wanted:
        print("photo-lossy  (a probe row, never merged into photo/)")
        results["photo-lossy/div2k"] = fetch_div2k(root / "photo-lossy" / "div2k", n, 8)
        print(f"    div2k      {results['photo-lossy/div2k']}")

    manifest = root / "manifest.json"
    root.mkdir(parents=True, exist_ok=True)
    manifest.write_text(
        json.dumps(
            {
                "counts": results,
                "sources": {k: {"source": v[0], "licence": v[1]} for k, v in SOURCES.items()},
                "tile": [TILE_W, TILE_H],
                "note": "photo-lossy is JPEG-derived and is a control row, not a photograph class",
            },
            indent=2,
        ),
        encoding="utf-8",
    )

    total = sum(results.values())
    print(f"\n{total} images under {root}; provenance written to {manifest}")
    print("\nNow run, from the repository:")
    print(rf"  $env:BRP_SAMPLE=100; cargo run -p brp-lab --release --bin block-sweep -- {root}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
