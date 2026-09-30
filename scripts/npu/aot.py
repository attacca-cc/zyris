"""AOT-compile the three graphs for every Qualcomm SoC LiteRT 2.2.0 lists; one bundle per SoC."""
import hashlib, json, pathlib, shutil, sys

from ai_edge_litert.aot import aot_compile as aot_lib
from ai_edge_litert.aot.vendors.qualcomm import target as qnn

import whisper_kv as kv

SRC = pathlib.Path("out/whisper-small")
OUT = pathlib.Path("out/bundles")
GRAPHS = ("encoder", "cross", "decoder")
# The HTP generation each SoC's compiled graphs run on; the skel and stub that go with them.
# Phones only: LiteRT's own AI-pack export treats the SA automotive parts as non-Android targets.
HTP = {"SM8450": 69, "SM8475": 69, "SM8550": 73, "SM8650": 75, "SM8750": 79, "SM8845": 81, "SM8850": 81}

def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for block in iter(lambda: f.read(1 << 20), b""):
            h.update(block)
    return h.hexdigest()

def main(only):
    manifest_path = OUT / "manifest.json"
    manifest = {"model": "whisper-small", "revision": kv.WHISPER_REVISION, "litert": "2.2.0",
                "qairt": "2.47.0.260601", "socs": {}, "failed": {}}
    if only and manifest_path.exists():  # a partial run adds to what an earlier run found
        manifest = json.loads(manifest_path.read_text())
    for soc in qnn.SocModel:
        if soc == qnn.SocModel.ALL or soc.value.startswith("SA") or (only and soc.value not in only):
            continue
        name = soc.value
        dest = OUT / f"whisper-small-npu-{name}"
        tmp = OUT / f"_tmp-{name}"
        offload, error = {}, None
        try:
            for graph in GRAPHS:
                result = aot_lib.aot_compile(str(SRC / f"{graph}.tflite"), output_dir=str(tmp), target=qnn.Target(soc), keep_going=True)
                if result.failed_backends:
                    raise RuntimeError(f"{graph}: {result.failed_backends[0][1]}")
                _, model = result.models_with_backend[0]
                (tmp / "ok").mkdir(parents=True, exist_ok=True)
                shutil.copy(model.path, tmp / "ok" / f"{graph}.tflite")
                s = model.partition_stats.subgraph_stats[0]
                offload[graph] = [s.num_ops_offloaded, s.num_total_ops]
            if name not in HTP:
                raise RuntimeError("compiled, but its HTP generation is not in the table")
        except Exception as e:  # no bundle at all, never one with a graph missing
            error = str(e)
        manifest["socs"].pop(name, None)
        manifest["failed"].pop(name, None)
        if error:
            manifest["failed"][name] = error[:300]
            shutil.rmtree(tmp, ignore_errors=True)
            shutil.rmtree(dest, ignore_errors=True)
            print(name, "FAILED", error[:120], flush=True)
            continue
        shutil.rmtree(dest, ignore_errors=True)
        dest.mkdir(parents=True)
        for graph in GRAPHS:
            shutil.move(str(tmp / "ok" / f"{graph}.tflite"), dest / f"{graph}.tflite")
        shutil.rmtree(tmp, ignore_errors=True)
        for extra in ("generation_config.json", "tokenizer.json", "shapes.json"):
            shutil.copy(SRC / extra, dest / extra)
        v = HTP[name]
        # No QNN libraries in a bundle: the QAIRT licence allows them only inside the app, never as
        # a standalone download (LICENSING.md). The bundle names its generation; the APK carries it.
        files = [{"name": f.name, "bytes": f.stat().st_size, "sha256": sha256(f)} for f in sorted(dest.iterdir())]
        manifest["socs"][name] = {"htp": v, "files": files, "offload": offload}
        print(name, "ok", offload, sum(f["bytes"] for f in files) // 2**20, "MiB", flush=True)
    manifest_path.write_text(json.dumps(manifest, indent=2))

def rescan():
    """Rebuild manifest.json's file lists from the bundles on disk, keeping offload and failures."""
    manifest_path = OUT / "manifest.json"
    manifest = json.loads(manifest_path.read_text())
    for name, entry in manifest["socs"].items():
        dest = OUT / f"whisper-small-npu-{name}"
        for lib in dest.glob("libQnn*.so"):
            lib.unlink()
        entry["files"] = [{"name": f.name, "bytes": f.stat().st_size, "sha256": sha256(f)} for f in sorted(dest.iterdir())]
    manifest_path.write_text(json.dumps(manifest, indent=2))

SHORT_GRAPHS = ("encoder-10s", "cross-10s", "decoder-10s")
SHORT_RELEASE, FIRST_RELEASE = "npu-models-2", "npu-models-1"

def short(only):
    """Add the ten-second set to the bundles on disk, leaving every uploaded file as it is."""
    manifest_path = OUT / "manifest.json"
    manifest = json.loads(manifest_path.read_text())
    short_files = {f"{g}.tflite" for g in SHORT_GRAPHS}
    # Only a set check_tflite.py passed: it reads the clips as the thirty-second set does.
    stamp = SRC / "checked.json"
    checked = json.loads(stamp.read_text()) if stamp.exists() else {}
    for f in sorted(short_files):
        if checked.get(f) != sha256(SRC / f):
            raise SystemExit(f"{f} has not passed check_tflite.py; run it first")
    for name, entry in sorted(manifest["socs"].items()):
        if only and name not in only:
            continue
        dest = OUT / f"whisper-small-npu-{name}"
        kept = [f for f in entry["files"] if f["name"] not in short_files]
        for f in kept:  # what npu-models-1 serves: these bytes, or stop
            f.setdefault("release", FIRST_RELEASE)
            if sha256(dest / f["name"]) != f["sha256"]:
                raise SystemExit(f"{name}/{f['name']} is not the file npu-models-1 serves")
        tmp = OUT / f"_tmp-{name}"
        added = []
        for graph in SHORT_GRAPHS:
            result = aot_lib.aot_compile(str(SRC / f"{graph}.tflite"), output_dir=str(tmp), target=qnn.Target(qnn.SocModel(name)), keep_going=True)
            if result.failed_backends:
                raise SystemExit(f"{name} {graph}: {result.failed_backends[0][1][:300]}")
            _, model = result.models_with_backend[0]
            path = dest / f"{graph}.tflite"
            shutil.copy(model.path, path)
            s = model.partition_stats.subgraph_stats[0]
            entry["offload"][graph] = [s.num_ops_offloaded, s.num_total_ops]
            added.append({"name": path.name, "bytes": path.stat().st_size, "sha256": sha256(path), "release": SHORT_RELEASE})
        shutil.rmtree(tmp, ignore_errors=True)
        entry["files"] = sorted(kept + added, key=lambda f: f["name"])
        print(name, "short ok", {g: entry["offload"][g] for g in SHORT_GRAPHS}, flush=True)
        manifest_path.write_text(json.dumps(manifest, indent=2))  # after each SoC: a long run can stop

if __name__ == "__main__":
    if sys.argv[1:2] == ["--short"]:
        short(set(sys.argv[2:]))
    else:
        main(set(sys.argv[1:]))
